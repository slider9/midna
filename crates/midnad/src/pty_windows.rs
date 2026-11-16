//! Windows terminals: a pseudoconsole (ConPTY, Windows 10 1809+) per session.
//!
//! The app runs attached to a pseudoconsole whose conhost turns its console calls into VT on
//! the output pipe and the VT we write on the input pipe into console input. Unlike a Unix pty,
//! the output pipe doesn't end when the app exits: conhost lives until the pseudoconsole is
//! closed, so the reaper closes it once the process is gone ([`Master::child_exited`]).
//!
//! Every session's processes go in a job object, so a close (or a force close) ends what the
//! shell started too, like a hang-up of the process group does on Unix.
//!
//! Which ConPTY: `conpty.dll` next to midnad.exe when it's there ([`conpty`]), else Windows' own.
use super::ENV_SCRUB;
use std::ffi::OsString;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_BROKEN_PIPE, ERROR_NO_DATA, GetLastError, HANDLE, INVALID_HANDLE_VALUE, STILL_ACTIVE, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Console::{COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
    InitializeProcThreadAttributeList, OpenProcess, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
};

/// The newer ConPTY's files, which go wherever midnad.exe goes (`install::install_self`; the
/// app's bundle has them beside its midnad.exe). `conpty.dll` starts the `OpenConsole.exe`
/// beside it for every terminal.
pub const FILES: [&str; 2] = ["conpty.dll", "OpenConsole.exe"];

/// The pseudoconsole calls. Windows' own (kernel32) is as old as the Windows it ships with: on
/// Windows 10 it drops dim text (Claude's placeholder then reads as typed), clears a new
/// terminal's screen and keeps an app's keyboard and mouse modes to itself. Microsoft ships the
/// current one, the one Windows Terminal runs, as `conpty.dll` with an `OpenConsole.exe` beside
/// it (NuGet `Microsoft.Windows.Console.ConPTY`; VS Code and WezTerm bundle it too). Its three
/// calls take what kernel32's do. `MIDNA_CONPTY=system` uses Windows' own regardless, and
/// `MIDNA_CONPTY=<path to a conpty.dll>` that one (tests and trials).
pub struct Conpty {
    create: unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32,
    resize: unsafe extern "system" fn(HPCON, COORD) -> i32,
    close: unsafe extern "system" fn(HPCON),
    /// The `conpty.dll` in use (None: Windows' own).
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
                Ok(exe) => exe.with_file_name(FILES[0]),
                Err(_) => return system,
            },
        };
        if !(dll.is_file() && dll.with_file_name(FILES[1]).is_file()) {
            return system;
        }
        match load_conpty(&dll) {
            Some(api) => api,
            None => {
                eprintln!("midnad: {} has no pseudoconsole calls; using Windows' own", dll.display());
                system
            }
        }
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

/// A session's pseudoconsole: its input and output pipes, the app's process and its job.
pub struct Master {
    /// None once closed (hang-up, or the app exited).
    console: Mutex<Option<HPCON>>,
    input: HANDLE,
    output: HANDLE,
    process: HANDLE,
    job: HANDLE,
}

// The handles are plain kernel handles, usable from any thread; `console` is behind a lock.
unsafe impl Send for Master {}
unsafe impl Sync for Master {}

impl Drop for Master {
    fn drop(&mut self) {
        self.close_console();
        unsafe {
            for h in [self.input, self.output, self.process, self.job] {
                if !h.is_null() {
                    CloseHandle(h);
                }
            }
        }
    }
}

/// Where DA/DSR replies go: the input pipe (not owned; the [`Master`] outlives the engine).
#[derive(Clone, Copy)]
pub struct Writer(isize);

impl Writer {
    pub fn write(&self, data: &[u8]) {
        write_handle(self.0 as HANDLE, data);
    }
}

fn write_handle(h: HANDLE, mut b: &[u8]) -> bool {
    while !b.is_empty() {
        let mut n = 0u32;
        let ok = unsafe { WriteFile(h, b.as_ptr(), b.len().min(u32::MAX as usize) as u32, &mut n, std::ptr::null_mut()) };
        if ok == 0 || n == 0 {
            return false;
        }
        b = &b[n as usize..];
    }
    true
}

impl Master {
    /// Write everything, in order (a full pipe blocks until conhost reads).
    pub fn write_all(&self, b: &[u8]) -> bool {
        write_handle(self.input, b)
    }

    pub fn writer(&self) -> Writer {
        Writer(self.input as isize)
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        if let Some(hpc) = *self.console.lock().unwrap_or_else(|e| e.into_inner()) {
            unsafe { (conpty().resize)(hpc, COORD { X: cols as i16, Y: rows as i16 }) };
        }
    }

    /// Reads block (no readiness poll); the reader pause only matters to Unix upgrades.
    pub fn readable(&self, _timeout_ms: i32) -> bool {
        true
    }

    /// Ok(0) once conhost has closed its end (the pseudoconsole was closed).
    pub fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut n = 0u32;
        let ok = unsafe { ReadFile(self.output, buf.as_mut_ptr(), buf.len().min(u32::MAX as usize) as u32, &mut n, std::ptr::null_mut()) };
        if ok == 0 {
            let e = unsafe { GetLastError() };
            if e == ERROR_BROKEN_PIPE || e == ERROR_NO_DATA {
                return Ok(0);
            }
            return Err(std::io::Error::from_raw_os_error(e as i32));
        }
        Ok(n as usize)
    }

    /// Hang up: close the pseudoconsole, which sends every attached process CTRL_CLOSE_EVENT
    /// (the console's hang-up; Windows ends them if they don't exit within a few seconds).
    /// ClosePseudoConsole can block until conhost has flushed, so it runs off this thread.
    pub fn hangup(self: &std::sync::Arc<Self>) {
        let m = self.clone();
        let _ = std::thread::Builder::new().name("pty-hangup".into()).spawn(move || m.close_console());
    }

    /// The app exited: let conhost paint its last output, then close the pseudoconsole so the
    /// output pipe ends and the reader drains.
    pub fn child_exited(&self) {
        std::thread::sleep(Duration::from_millis(150));
        self.close_console();
    }

    fn close_console(&self) {
        let hpc = self.console.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(hpc) = hpc {
            unsafe { (conpty().close)(hpc) };
        }
    }

    /// End every process in the session's job (a force close).
    pub fn kill_all(&self) {
        if !self.job.is_null() {
            unsafe { TerminateJobObject(self.job, 1) };
        }
    }

    /// Is the app itself still running?
    pub fn child_alive(&self) -> bool {
        let mut code = 0u32;
        unsafe { GetExitCodeProcess(self.process, &mut code) != 0 && code == STILL_ACTIVE as u32 }
    }
}

pub struct Spawned {
    pub master: Master,
    pub pid: i32,
}

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
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

/// The extensions `which` tries for a name without one (PATHEXT, else the usual four).
fn path_exts() -> Vec<String> {
    std::env::var("PATHEXT")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .filter(|e| !e.is_empty())
        .map(|e| e.to_ascii_lowercase())
        .collect()
}

fn with_exts(p: &Path) -> Option<PathBuf> {
    if p.extension().is_some() && p.is_file() {
        return Some(p.to_path_buf());
    }
    path_exts().iter().map(|e| PathBuf::from(format!("{}{e}", p.display()))).find(|c| c.is_file())
}

/// Resolve a program against `path` (`;`-separated) and PATHEXT.
pub fn which(prog: &str, path: &str) -> Option<String> {
    let p = Path::new(prog);
    if prog.contains(['\\', '/', ':']) {
        return with_exts(p).map(|p| p.to_string_lossy().into_owned());
    }
    path.split(';').filter(|d| !d.is_empty()).find_map(|d| with_exts(&Path::new(d).join(prog))).map(|p| p.to_string_lossy().into_owned())
}

/// The environment block: ours minus [`ENV_SCRUB`], with `env` applied, sorted by name ignoring
/// case (names are case-insensitive on Windows), as `K=V\0…\0\0` UTF-16.
fn env_block(env: &[(String, String)]) -> Vec<u16> {
    let mut vars: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(k, _)| !ENV_SCRUB.iter().any(|s| k.to_string_lossy().eq_ignore_ascii_case(s)))
        .filter(|(k, _)| !env.iter().any(|(n, _)| k.to_string_lossy().eq_ignore_ascii_case(n)))
        .collect();
    vars.extend(env.iter().map(|(k, v)| (OsString::from(k), OsString::from(v))));
    vars.sort_by_key(|(k, _)| k.to_string_lossy().to_uppercase());
    let mut block = vec![];
    for (k, v) in vars {
        block.extend(k.encode_wide());
        block.push('=' as u16);
        block.extend(v.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

fn pipe() -> std::io::Result<(HANDLE, HANDLE)> {
    let (mut r, mut w): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
    if unsafe { CreatePipe(&mut r, &mut w, std::ptr::null(), 0) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((r, w))
}

fn job() -> HANDLE {
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return job;
        }
        // A process may still leave the session on purpose (CREATE_BREAKAWAY_FROM_JOB), like a
        // Unix process that calls setsid.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as *const _, std::mem::size_of_val(&info) as u32);
        job
    }
}

pub fn spawn(argv: &[String], cwd: &str, env: &[(String, String)], cols: u16, rows: u16) -> std::io::Result<Spawned> {
    let Some(prog) = argv.first() else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command"));
    };
    let path = env.iter().find(|(k, _)| k.eq_ignore_ascii_case("PATH")).map(|(_, v)| v.clone()).unwrap_or_else(crate::sys::user_path);
    let exe = which(prog, &path).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, format!("{prog}: not found on PATH")))?;
    let lower = exe.to_ascii_lowercase();
    // A batch file runs through cmd.exe (CreateProcess can't start one with a pseudoconsole
    // and quoted arguments on its own).
    let line = if lower.ends_with(".cmd") || lower.ends_with(".bat") {
        let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into());
        let mut inner = vec![exe.clone()];
        inner.extend(argv[1..].iter().cloned());
        format!("{} /d /s /c \"{}\"", command_line(&[comspec]), command_line(&inner))
    } else {
        let mut all = vec![exe.clone()];
        all.extend(argv[1..].iter().cloned());
        command_line(&all)
    };
    let cwd = if Path::new(cwd).is_dir() { PathBuf::from(cwd) } else { midna_proto::paths::user_home() };
    let _g = super::spawn_lock();
    unsafe {
        let (in_read, in_write) = pipe()?;
        let (out_read, out_write) = match pipe() {
            Ok(p) => p,
            Err(e) => {
                CloseHandle(in_read);
                CloseHandle(in_write);
                return Err(e);
            }
        };
        let mut hpc: HPCON = 0;
        let hr = (conpty().create)(COORD { X: cols as i16, Y: rows as i16 }, in_read, out_write, 0, &mut hpc);
        // conhost holds its own copies now.
        CloseHandle(in_read);
        CloseHandle(out_write);
        if hr < 0 {
            CloseHandle(in_write);
            CloseHandle(out_read);
            return Err(std::io::Error::from_raw_os_error(hr));
        }
        let mut master = Master { console: Mutex::new(Some(hpc)), input: in_write, output: out_read, process: std::ptr::null_mut(), job: job() };
        let mut size = 0usize;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        let mut attrs = vec![0u8; size];
        let list = attrs.as_mut_ptr() as *mut core::ffi::c_void;
        if InitializeProcThreadAttributeList(list, 1, 0, &mut size) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let ok = UpdateProcThreadAttribute(list, 0, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize, hpc as *const _, std::mem::size_of::<HPCON>(), std::ptr::null_mut(), std::ptr::null());
        if ok == 0 {
            let e = std::io::Error::last_os_error();
            DeleteProcThreadAttributeList(list);
            return Err(e);
        }
        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        // Invalid std handles, so the app uses the pseudoconsole and never inherits midnad's
        // own (redirected) stdio.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
        si.lpAttributeList = list;
        let mut cmdline = wide(std::ffi::OsStr::new(&line));
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
        let err = std::io::Error::last_os_error();
        DeleteProcThreadAttributeList(list);
        if created == 0 {
            return Err(std::io::Error::new(err.kind(), format!("{exe}: {err}")));
        }
        // In the job before it runs, so nothing it starts escapes.
        if !master.job.is_null() {
            AssignProcessToJobObject(master.job, pi.hProcess);
        }
        ResumeThread(pi.hThread);
        CloseHandle(pi.hThread);
        master.process = pi.hProcess;
        Ok(Spawned { master, pid: pi.dwProcessId as i32 })
    }
}

/// Block until `pid` exits: `(exit code, None)`. Windows has no signals; a process ended by
/// TerminateProcess reports the code it was given.
pub fn wait_exit(pid: i32) -> (Option<i32>, Option<i32>) {
    unsafe {
        let h = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
        if h.is_null() {
            return (None, None);
        }
        let mut code = None;
        if WaitForSingleObject(h, INFINITE) == WAIT_OBJECT_0 {
            let mut c = 0u32;
            if GetExitCodeProcess(h, &mut c) != 0 {
                code = Some(c as i32);
            }
        }
        CloseHandle(h);
        (code, None)
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
    fn which_finds_system_programs_with_pathext() {
        let path = std::env::var("PATH").unwrap();
        let cmd = which("cmd", &path).expect("cmd.exe on PATH");
        assert!(cmd.to_ascii_lowercase().ends_with("cmd.exe"), "{cmd}");
        assert!(which("surely-not-a-program-midna", &path).is_none());
    }

    #[test]
    fn a_console_app_runs_in_the_pseudoconsole() {
        let argv: Vec<String> = ["cmd", "/d", "/c", "echo midna-conpty-ok"].iter().map(|s| s.to_string()).collect();
        let sp = spawn(&argv, ".", &[], 80, 24).expect("spawn");
        let m = std::sync::Arc::new(sp.master);
        let reader = {
            let m = m.clone();
            std::thread::spawn(move || {
                let (mut all, mut buf) = (Vec::new(), [0u8; 4096]);
                while let Ok(n) = m.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    all.extend_from_slice(&buf[..n]);
                }
                String::from_utf8_lossy(&all).into_owned()
            })
        };
        let (code, _) = wait_exit(sp.pid);
        assert_eq!(code, Some(0));
        m.child_exited();
        let out = reader.join().unwrap();
        assert!(out.contains("midna-conpty-ok"), "{out:?}");
    }
}
