//! The terminal the keeper holds: a pseudoconsole (ConPTY), its pipes, the shell and its job.
//! This is midnad's `pty_windows::spawn`, moved here so the pseudoconsole's owner can outlive
//! midnad.
//!
//! The shell runs attached to a pseudoconsole whose console host turns its console calls into VT
//! on the output pipe and the VT written to the input pipe into console input. The output pipe
//! doesn't end when the shell exits: the host lives until the pseudoconsole is closed.
//!
//! Every process the shell starts goes in its job, so a force close ends them too.
use crate::win::{owned, raw, wide};
use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::OwnedHandle;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, InitializeProcThreadAttributeList,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute,
};

/// The output pipe's buffer. A handover leaves it unread for about a second, and at 8 MB a
/// program printing flat out never blocked in that time on either ConPTY (T164's spike; 1 MB
/// still blocked for half a second). The input pipe keeps the default size.
pub const OUTPUT_PIPE: u32 = 8 << 20;

/// The newer ConPTY's files, beside the keeper as they are beside midnad.
pub const CONPTY_FILES: [&str; 2] = ["conpty.dll", "OpenConsole.exe"];

/// The pseudoconsole calls: `conpty.dll` beside this executable when it's there with its
/// `OpenConsole.exe`, else Windows' own (kernel32). `MIDNA_CONPTY=system` uses Windows' own
/// regardless, and `MIDNA_CONPTY=<path to a conpty.dll>` that one, as in midnad.
pub struct Conpty {
    create: unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32,
    resize: unsafe extern "system" fn(HPCON, COORD) -> i32,
    close: unsafe extern "system" fn(HPCON),
    pub dll: Option<PathBuf>,
}

pub fn conpty() -> &'static Conpty {
    static API: OnceLock<Conpty> = OnceLock::new();
    API.get_or_init(|| {
        let system = Conpty { create: CreatePseudoConsole, resize: ResizePseudoConsole, close: ClosePseudoConsole, dll: None };
        let dll = match std::env::var_os("MIDNA_CONPTY") {
            Some(v) if v == "system" => return system,
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => match std::env::current_exe() {
                Ok(exe) => exe.with_file_name(CONPTY_FILES[0]),
                Err(_) => return system,
            },
        };
        if !(dll.is_file() && dll.with_file_name(CONPTY_FILES[1]).is_file()) {
            return system;
        }
        load_conpty(&dll).unwrap_or(system)
    })
}

fn load_conpty(dll: &Path) -> Option<Conpty> {
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW};
    let path = wide(dll.as_os_str());
    // SAFETY: a NUL-terminated path; the library stays loaded for the process's life, and each
    // export has the signature of the kernel32 call it stands in for.
    unsafe {
        let lib = LoadLibraryExW(path.as_ptr(), std::ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH);
        if lib.is_null() {
            return None;
        }
        let create = GetProcAddress(lib, c"ConptyCreatePseudoConsole".as_ptr().cast())?;
        let resize = GetProcAddress(lib, c"ConptyResizePseudoConsole".as_ptr().cast())?;
        let close = GetProcAddress(lib, c"ConptyClosePseudoConsole".as_ptr().cast())?;
        Some(Conpty {
            create: std::mem::transmute::<unsafe extern "system" fn() -> isize, unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32>(create),
            resize: std::mem::transmute::<unsafe extern "system" fn() -> isize, unsafe extern "system" fn(HPCON, COORD) -> i32>(resize),
            close: std::mem::transmute::<unsafe extern "system" fn() -> isize, unsafe extern "system" fn(HPCON)>(close),
            dll: Some(dll.to_path_buf()),
        })
    }
}

/// A running shell in its pseudoconsole.
pub struct Shell {
    /// None once closed.
    console: Mutex<Option<HPCON>>,
    pub input: OwnedHandle,
    pub output: OwnedHandle,
    pub process: OwnedHandle,
    pub job: Option<OwnedHandle>,
    pub pid: u32,
}

// The handles are plain kernel handles, usable from any thread; the console is behind a lock.
unsafe impl Send for Shell {}
unsafe impl Sync for Shell {}

impl Shell {
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let console = self.console.lock().unwrap_or_else(|e| e.into_inner());
        let Some(hpc) = *console else { return Err(io::Error::other("the pseudoconsole is closed")) };
        let hr = unsafe { (conpty().resize)(hpc, COORD { X: cols as i16, Y: rows as i16 }) };
        if hr < 0 { Err(io::Error::from_raw_os_error(hr)) } else { Ok(()) }
    }

    /// Close the pseudoconsole: every process attached to it gets CTRL_CLOSE_EVENT, and the
    /// output pipe ends once the host has written what it has. Windows' own ConPTY blocks here
    /// while the output pipe is full, so it's called off the control thread, with something
    /// reading the pipe.
    pub fn close_console(&self) {
        let hpc = self.console.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(hpc) = hpc {
            unsafe { (conpty().close)(hpc) };
        }
    }

    pub fn console_open(&self) -> bool {
        self.console.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    /// End every process in the job.
    pub fn kill(&self, code: u32) {
        if let Some(job) = &self.job {
            unsafe { TerminateJobObject(raw(job), code) };
        }
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        self.close_console();
    }
}

/// One argument quoted the way the MSVC runtime (and Rust's std) splits a command line.
fn quote_arg(arg: &str, out: &mut String) {
    if !arg.is_empty() && !arg.chars().any(|c| matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '"')) {
        out.push_str(arg);
        return;
    }
    out.push('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            c => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
}

pub fn command_line(argv: &[String]) -> String {
    let mut s = String::new();
    for (i, a) in argv.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        quote_arg(a, &mut s);
    }
    s
}

/// `env` as an environment block: sorted by name ignoring case (names are case-insensitive on
/// Windows), `K=V\0…\0\0` UTF-16. Later duplicates win.
fn env_block(env: &[(String, String)]) -> Vec<u16> {
    let mut vars: Vec<(OsString, OsString)> = vec![];
    for (k, v) in env {
        vars.retain(|(n, _)| !n.to_string_lossy().eq_ignore_ascii_case(k));
        vars.push((OsString::from(k), OsString::from(v)));
    }
    vars.sort_by_key(|(k, _)| k.to_string_lossy().to_uppercase());
    let mut block = vec![];
    for (k, v) in vars {
        block.extend(k.encode_wide());
        block.push('=' as u16);
        block.extend(v.encode_wide());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

fn pipe(size: u32) -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (mut r, mut w): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
    if unsafe { CreatePipe(&mut r, &mut w, std::ptr::null(), size) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((owned(r)?, owned(w)?))
}

fn job() -> Option<OwnedHandle> {
    unsafe {
        let job = owned(CreateJobObjectW(std::ptr::null(), std::ptr::null())).ok()?;
        // A process may still leave the session on purpose (CREATE_BREAKAWAY_FROM_JOB), like a
        // Unix process that calls setsid. The keeper holds the only handle, so if it dies, all
        // that's still in the job (the shell and what it started) goes with it.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(raw(&job), JobObjectExtendedLimitInformation, &info as *const _ as *const _, std::mem::size_of_val(&info) as u32);
        Some(job)
    }
}

/// Start `argv` in a new pseudoconsole of `cols` by `rows`.
pub fn spawn(argv: &[String], cwd: &str, env: &[(String, String)], cols: u16, rows: u16) -> io::Result<Shell> {
    let Some(prog) = argv.first() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty command"));
    };
    let path = env.iter().rev().find(|(k, _)| k.eq_ignore_ascii_case("PATH")).map(|(_, v)| v.clone()).or_else(|| std::env::var("PATH").ok()).unwrap_or_default();
    let all = midna_proto::cmd_shim::resolve(argv, &path).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{prog}: not found on PATH")))?;
    let exe = all[0].clone();
    // A batch file runs through cmd.exe (CreateProcess can't start one with a pseudoconsole and
    // quoted arguments on its own). cmd ends its arguments at a newline and acts on `|<>&` in
    // them, so a shim (npm's claude.cmd) has been swapped for what it runs above, and only
    // another batch file gets here.
    let line = if midna_proto::cmd_shim::is_batch(&exe) {
        let comspec = env.iter().rev().find(|(k, _)| k.eq_ignore_ascii_case("ComSpec")).map(|(_, v)| v.clone()).unwrap_or_else(|| "cmd.exe".into());
        format!("{} /d /s /c \"{}\"", command_line(&[comspec]), command_line(&all))
    } else {
        command_line(&all)
    };
    let cwd = if Path::new(cwd).is_dir() { PathBuf::from(cwd) } else { midna_proto::paths::user_home() };
    let (in_read, in_write) = pipe(0)?;
    let (out_read, out_write) = pipe(OUTPUT_PIPE)?;
    let mut hpc: HPCON = 0;
    let hr = unsafe { (conpty().create)(COORD { X: cols as i16, Y: rows as i16 }, raw(&in_read), raw(&out_write), 0, &mut hpc) };
    // The console host holds its own copies now.
    drop(in_read);
    drop(out_write);
    if hr < 0 {
        return Err(io::Error::from_raw_os_error(hr));
    }
    let console = Mutex::new(Some(hpc));
    let job = job();
    unsafe {
        let mut size = 0usize;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        let mut attrs = vec![0u8; size];
        let list = attrs.as_mut_ptr() as *mut core::ffi::c_void;
        let close = |e: io::Error| {
            (conpty().close)(hpc);
            Err(e)
        };
        if InitializeProcThreadAttributeList(list, 1, 0, &mut size) == 0 {
            return close(io::Error::last_os_error());
        }
        let ok = UpdateProcThreadAttribute(list, 0, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize, hpc as *const _, std::mem::size_of::<HPCON>(), std::ptr::null_mut(), std::ptr::null());
        if ok == 0 {
            let e = io::Error::last_os_error();
            DeleteProcThreadAttributeList(list);
            return close(e);
        }
        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        // Invalid std handles, so the shell uses the pseudoconsole and never inherits the
        // keeper's own (it has none anyway).
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
        si.lpAttributeList = list;
        let mut cmdline = wide(&line);
        let envb = env_block(env);
        let cwdw = wide(cwd.as_os_str());
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let created = CreateProcessW(
            std::ptr::null(),
            cmdline.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
            envb.as_ptr() as *const _,
            cwdw.as_ptr(),
            &si.StartupInfo,
            &mut pi,
        );
        let err = io::Error::last_os_error();
        DeleteProcThreadAttributeList(list);
        if created == 0 {
            return close(io::Error::new(err.kind(), format!("{exe}: {err}")));
        }
        // In the job before it runs, so nothing it starts escapes.
        if let Some(job) = &job {
            AssignProcessToJobObject(raw(job), pi.hProcess);
        }
        ResumeThread(pi.hThread);
        drop(owned(pi.hThread));
        Ok(Shell { console, input: in_write, output: out_read, process: owned(pi.hProcess)?, job, pid: pi.dwProcessId })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_like_the_msvc_runtime() {
        let v = |a: &[&str]| command_line(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(v(&["a", "b c", ""]), r#"a "b c" """#);
        assert_eq!(v(&[r#"say "hi""#]), r#""say \"hi\"""#);
        assert_eq!(v(&[r"C:\dir with space\"]), r#""C:\dir with space\\""#);
        assert_eq!(v(&[r"C:\plain\path"]), r"C:\plain\path");
    }

    #[test]
    fn the_environment_block_is_sorted_and_the_last_duplicate_wins() {
        let env = [("b", "2"), ("A", "1"), ("PATH", "x"), ("Path", "y")].map(|(k, v)| (k.to_string(), v.to_string()));
        let block = env_block(&env);
        let text = String::from_utf16_lossy(&block);
        assert_eq!(text, "A=1\0b=2\0Path=y\0\0");
        assert_eq!(env_block(&[]), vec![0, 0]);
    }
}
