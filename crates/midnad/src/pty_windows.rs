//! Windows terminals: each runs in its own keeper, `midna-pty.exe` (crates/midna-pty), which owns
//! the terminal's pseudoconsole (ConPTY), its shell and the shell's job, so the terminal can
//! outlive midnad. midnad starts a keeper per new terminal, attaches to it and then reads, writes
//! and waits on the copies of the shell's pipes and process the keeper gave it, as it did with
//! its own pseudoconsole. Resizes, hang-ups and force closes are messages to the keeper.
//!
//! The keeper closes the pseudoconsole once the shell exits, so the output pipe ends and the
//! reader drains, and it puts every process the shell starts in the shell's job, so a force
//! close ends them too, like a hang-up of the process group does on Unix. A keeper that dies
//! takes its pseudoconsole with it: that terminal ends with "terminal host exited".
//!
//! The keeper comes from beside midnad.exe ([`keeper_exe`]); it runs `conpty.dll` from beside
//! itself when it's there, else Windows' own.
use super::ENV_SCRUB;
use midna_pty::client::{self, Client, Spawn};
use std::collections::HashMap;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_NO_DATA, GetLastError, HANDLE, STILL_ACTIVE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, INFINITE, OpenProcess, PROCESS_TERMINATE, TerminateProcess, WaitForMultipleObjects, WaitForSingleObject,
};

/// The newer ConPTY's files, which go wherever midnad.exe goes (`install::install_self`; the
/// app's bundle has them beside its midnad.exe), so the keeper beside it runs them.
pub use midna_pty::shell::CONPTY_FILES as FILES;
pub use midna_pty::shell::command_line;
pub use midna_proto::cmd_shim::which;

/// What [`Master::wait_exit`] reports, in place of a signal, for a terminal whose keeper
/// died while its shell ran.
pub const HOST_EXITED: i32 = -1;

/// How long a new terminal waits for the last keeper of the same session to be gone (an agent
/// that exited and is replaced by a shell): the keeper's pipe is named after the session.
const PREVIOUS_KEEPER: Duration = Duration::from_secs(5);

/// How long a terminal that's dropped while its shell runs waits for the keeper to take the
/// hang-up.
const DROP_HANGUP: Duration = Duration::from_secs(2);

/// Whose terminal this is: its keeper is named after `sid` in `home`. A `tied` terminal ends
/// with this process: its keeper is put in a job that ends it, its shell and everything they
/// started when this process exits (a midnad inside another program, such as a test, whose
/// terminals mustn't outlive it). Otherwise the keeper outlives midnad, for the next one.
pub struct Owner<'a> {
    pub home: &'a Path,
    pub sid: &'a str,
    pub tied: bool,
}

/// A terminal, through its keeper: the copies of its pipes and the shell's process.
pub struct Master {
    client: Client,
    keeper: Arc<OwnedHandle>,
    input: OwnedHandle,
    output: OwnedHandle,
    process: OwnedHandle,
    shell_pid: u32,
    /// A hang-up or kill was sent.
    ended: AtomicBool,
}

impl Drop for Master {
    /// A terminal dropped with its shell still running is hung up, as closing its pseudoconsole
    /// did; midnad handing its terminals over never drops them.
    fn drop(&mut self) {
        if !self.ended.load(Ordering::SeqCst) && self.child_alive() {
            let _ = self.client.hangup();
            self.wait_keeper(DROP_HANGUP);
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

fn raw(h: &OwnedHandle) -> HANDLE {
    h.as_raw_handle()
}

impl Master {
    /// Write everything, in order (a full pipe blocks until the console host reads).
    pub fn write_all(&self, b: &[u8]) -> bool {
        write_handle(raw(&self.input), b)
    }

    pub fn writer(&self) -> Writer {
        Writer(raw(&self.input) as isize)
    }

    /// Once the shell has exited the keeper has closed the pseudoconsole and says so; nothing
    /// to resize then.
    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.client.resize(cols, rows);
    }

    /// Reads block (no readiness poll); the reader pause only matters to Unix upgrades.
    pub fn readable(&self, _timeout_ms: i32) -> bool {
        true
    }

    /// Ok(0) once the console host has closed its end (the keeper closed the pseudoconsole, or
    /// died).
    pub fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut n = 0u32;
        let ok = unsafe { ReadFile(raw(&self.output), buf.as_mut_ptr(), buf.len().min(u32::MAX as usize) as u32, &mut n, std::ptr::null_mut()) };
        if ok == 0 {
            let e = unsafe { GetLastError() };
            if e == ERROR_BROKEN_PIPE || e == ERROR_NO_DATA {
                return Ok(0);
            }
            return Err(std::io::Error::from_raw_os_error(e as i32));
        }
        Ok(n as usize)
    }

    /// Hang up: the keeper closes the pseudoconsole, which sends every attached process
    /// CTRL_CLOSE_EVENT (the console's hang-up), and ends the shell's job if the shell hasn't
    /// exited a few seconds later. Off this thread, as closing a pseudoconsole was.
    pub fn hangup(self: &Arc<Self>) {
        self.ended.store(true, Ordering::SeqCst);
        let m = self.clone();
        let _ = std::thread::Builder::new().name("pty-hangup".into()).spawn(move || {
            let _ = m.client.hangup();
        });
    }

    /// The shell exited. The keeper lets the console host paint its last output, then closes the
    /// pseudoconsole so the output pipe ends and the reader drains.
    pub fn child_exited(&self) {}

    /// End every process in the session's job (a force close). With the keeper gone, the shell
    /// at least.
    pub fn kill_all(&self) {
        self.ended.store(true, Ordering::SeqCst);
        if self.client.kill().is_err() && self.child_alive() {
            self.terminate_shell();
        }
    }

    /// Is the shell itself still running?
    pub fn child_alive(&self) -> bool {
        let mut code = 0u32;
        unsafe { GetExitCodeProcess(raw(&self.process), &mut code) != 0 && code == STILL_ACTIVE as u32 }
    }

    /// Wait up to `timeout` for the keeper to exit: true once it has.
    pub fn wait_keeper(&self, timeout: Duration) -> bool {
        unsafe { WaitForSingleObject(raw(&self.keeper), timeout.as_millis().min(INFINITE as u128 - 1) as u32) == WAIT_OBJECT_0 }
    }

    /// Block until the shell exits: `(exit code, None)`. Windows has no signals; a process ended
    /// by TerminateProcess reports the code it was given. If the keeper dies first, the shell
    /// has lost its console: it's ended, and the result is `(None, Some(HOST_EXITED))`.
    pub fn wait_exit(&self) -> (Option<i32>, Option<i32>) {
        let both = [raw(&self.process), raw(&self.keeper)];
        let r = unsafe { WaitForMultipleObjects(2, both.as_ptr(), 0, INFINITE) };
        // The keeper exits only after its shell has, unless it was killed or crashed.
        if r != WAIT_OBJECT_0 && unsafe { WaitForSingleObject(raw(&self.process), 0) } == WAIT_TIMEOUT {
            eprintln!("midnad: the keeper of shell {} exited while the shell ran", self.shell_pid);
            self.terminate_shell();
            return (None, Some(HOST_EXITED));
        }
        let mut code = 0u32;
        if unsafe { GetExitCodeProcess(raw(&self.process), &mut code) } == 0 {
            return (None, None);
        }
        (Some(code as i32), None)
    }

    /// End the shell itself, without its keeper. Its pid is still its own: we hold its process.
    fn terminate_shell(&self) {
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, self.shell_pid);
            if !h.is_null() {
                TerminateProcess(h, 1);
                drop(OwnedHandle::from_raw_handle(h));
            }
        }
    }
}

pub struct Spawned {
    pub master: Master,
    pub pid: i32,
}

/// The keeper to start: `midna-pty.exe` beside this program, or, for a test inside cargo's
/// `deps` folder, beside the binaries one folder up.
pub fn keeper_exe() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let beside = exe.with_file_name(midna_pty::names::EXE);
    if beside.is_file() {
        return Ok(beside);
    }
    let dir = exe.parent().unwrap_or(Path::new(""));
    if dir.file_name().is_some_and(|n| n == "deps")
        && let Some(up) = dir.parent().map(|d| d.join(midna_pty::names::EXE)).filter(|p| p.is_file())
    {
        return Ok(up);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("{} (the terminal keeper) isn't beside {}; build it with cargo build -p midna-pty", midna_pty::names::EXE, exe.display()),
    ))
}

/// The `conpty.dll` a new terminal's keeper runs, as it picks one (None: Windows' own).
pub fn conpty_dll() -> Option<PathBuf> {
    let dll = match std::env::var_os("MIDNA_CONPTY") {
        Some(v) if v == "system" => return None,
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => keeper_exe().ok()?.with_file_name(FILES[0]),
    };
    (dll.is_file() && dll.with_file_name(FILES[1]).is_file()).then_some(dll)
}

/// The environment the shell gets, whole: ours minus [`ENV_SCRUB`], with `env` applied.
fn environment(env: &[(String, String)]) -> Vec<(String, String)> {
    let mut vars: Vec<(String, String)> = std::env::vars_os()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
        .filter(|(k, _)| !ENV_SCRUB.iter().any(|s| k.eq_ignore_ascii_case(s)))
        .filter(|(k, _)| !env.iter().any(|(n, _)| k.eq_ignore_ascii_case(n)))
        .collect();
    vars.extend(env.iter().cloned());
    vars
}

/// The job tied terminals' keepers go in: it ends them when this process exits. Processes in it
/// may still leave on purpose, as from a terminal's own job.
fn tie_job() -> Option<&'static OwnedHandle> {
    static JOB: OnceLock<Option<OwnedHandle>> = OnceLock::new();
    JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return None;
        }
        let job = OwnedHandle::from_raw_handle(job);
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        SetInformationJobObject(raw(&job), JobObjectExtendedLimitInformation, &info as *const _ as *const _, std::mem::size_of_val(&info) as u32);
        Some(job)
    })
    .as_ref()
}

/// Each session's last keeper in this process, by its pipe name.
fn keepers() -> &'static Mutex<HashMap<String, Arc<OwnedHandle>>> {
    static K: OnceLock<Mutex<HashMap<String, Arc<OwnedHandle>>>> = OnceLock::new();
    K.get_or_init(Default::default)
}

pub fn spawn(owner: &Owner, argv: &[String], cwd: &str, env: &[(String, String)], cols: u16, rows: u16) -> std::io::Result<Spawned> {
    let Some(prog) = argv.first() else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command"));
    };
    // Found on the PATH midnad looks on (the terminal's, else the user's as it is now); the
    // keeper takes the program found as it is.
    let path = env.iter().find(|(k, _)| k.eq_ignore_ascii_case("PATH")).map(|(_, v)| v.clone()).unwrap_or_else(crate::sys::user_path);
    let argv = midna_proto::cmd_shim::resolve(argv, &path).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, format!("{prog}: not found on PATH")))?;
    let exe = keeper_exe()?;
    let job = if owner.tied { Some(tie_job().ok_or_else(|| std::io::Error::other("can't make the job that ties terminals to midnad"))?) } else { None };
    let pipe = midna_pty::names::pipe_name(owner.home, owner.sid);
    let _g = super::spawn_lock();
    let previous = keepers().lock().unwrap_or_else(|e| e.into_inner()).get(&pipe).cloned();
    if let Some(p) = previous {
        unsafe { WaitForSingleObject(raw(&p), PREVIOUS_KEEPER.as_millis() as u32) };
    }
    let shell = Spawn { argv, cwd: cwd.to_string(), env: environment(env), cols, rows };
    let started = client::start_in(&exe, owner.home, owner.sid, shell, job).map_err(|e| std::io::Error::new(e.kind(), format!("terminal keeper: {e}")))?;
    let keeper = Arc::new(started.keeper.process);
    {
        let mut k = keepers().lock().unwrap_or_else(|e| e.into_inner());
        k.retain(|_, h| unsafe { WaitForSingleObject(raw(h), 0) } == WAIT_TIMEOUT);
        k.insert(pipe, keeper.clone());
    }
    let a = match started.client.attach() {
        Ok(a) => a,
        Err(e) => {
            let _ = started.client.kill();
            return Err(std::io::Error::new(e.kind(), format!("terminal keeper: {e}")));
        }
    };
    let master = Master { client: started.client, keeper, input: a.input, output: a.output, process: a.process, shell_pid: a.shell_pid, ended: AtomicBool::new(false) };
    Ok(Spawned { master, pid: a.shell_pid as i32 })
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
    fn the_shell_gets_our_environment_scrubbed_with_the_terminals_on_top() {
        // SAFETY: this test's own variables; no other test reads them.
        unsafe {
            std::env::set_var("MIDNA_PTY_ENV_TEST", "ours");
            std::env::set_var("CLAUDECODE", "1");
        }
        let env = environment(&[("midna_pty_env_test".into(), "theirs".into()), ("TERM".into(), "xterm-256color".into())]);
        let get = |k: &str| env.iter().filter(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str()).collect::<Vec<_>>();
        assert_eq!(get("MIDNA_PTY_ENV_TEST"), ["theirs"]);
        assert!(get("CLAUDECODE").is_empty());
        assert_eq!(get("TERM"), ["xterm-256color"]);
        assert!(!get("SystemRoot").is_empty());
    }

    fn home(name: &str) -> PathBuf {
        let h = std::env::temp_dir().join(format!("midnad-pty-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        std::fs::create_dir_all(&h).unwrap();
        h
    }

    fn read_all(m: &Arc<Master>) -> std::thread::JoinHandle<String> {
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
    }

    #[test]
    fn a_console_app_runs_in_a_keeper() {
        let h = home("app");
        let argv: Vec<String> = ["cmd", "/d", "/c", "echo midna-conpty-ok& exit 3"].iter().map(|s| s.to_string()).collect();
        let sp = spawn(&Owner { home: &h, sid: "app", tied: true }, &argv, ".", &[], 80, 24).expect("spawn");
        let m = Arc::new(sp.master);
        let reader = read_all(&m);
        assert_eq!(m.wait_exit(), (Some(3), None));
        let out = reader.join().unwrap();
        assert!(out.contains("midna-conpty-ok"), "{out:?}");
        // The keeper goes once it has told us.
        assert!(m.wait_keeper(Duration::from_secs(10)), "the keeper stayed");
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn a_killed_keeper_ends_its_terminal() {
        let h = home("host");
        let argv: Vec<String> = ["cmd", "/d"].iter().map(|s| s.to_string()).collect();
        let sp = spawn(&Owner { home: &h, sid: "host", tied: true }, &argv, ".", &[], 80, 24).expect("spawn");
        let m = Arc::new(sp.master);
        let reader = read_all(&m);
        assert!(m.child_alive());
        unsafe { TerminateProcess(raw(&m.keeper), 9) };
        assert_eq!(m.wait_exit(), (None, Some(HOST_EXITED)));
        assert!(!m.child_alive(), "the shell outlived its keeper");
        reader.join().unwrap();
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn a_hangup_ends_the_shell_and_its_keeper() {
        let h = home("hup");
        let argv: Vec<String> = ["cmd", "/d"].iter().map(|s| s.to_string()).collect();
        let sp = spawn(&Owner { home: &h, sid: "hup", tied: true }, &argv, ".", &[], 80, 24).expect("spawn");
        let m = Arc::new(sp.master);
        let reader = read_all(&m);
        m.hangup();
        let (code, signal) = m.wait_exit();
        assert!(code.is_some() && signal.is_none(), "{code:?} {signal:?}");
        assert!(m.wait_keeper(Duration::from_secs(10)), "the keeper stayed");
        reader.join().unwrap();
        // The next terminal of the session gets the name.
        let again = spawn(&Owner { home: &h, sid: "hup", tied: true }, &["cmd".to_string(), "/d".into(), "/c".into(), "exit 0".into()], ".", &[], 80, 24).expect("again");
        assert_eq!(again.master.wait_exit(), (Some(0), None));
        let _ = std::fs::remove_dir_all(&h);
    }
}
