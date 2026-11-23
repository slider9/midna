//! midnad's side: starting a keeper and talking to one.
use crate::names;
use crate::proto::{self, PROTOCOL, Reply, Request, Snapshot};
use crate::win::{self, Pipe, owned, raw, wide};
use std::io;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, GENERIC_READ, GENERIC_WRITE, WAIT_TIMEOUT};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT};
use windows_sys::Win32::System::IO::CancelIoEx;
use windows_sys::Win32::System::Pipes::WaitNamedPipeW;
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CreateProcessW, DETACHED_PROCESS, GetExitCodeProcess, PROCESS_INFORMATION, STARTUPINFOW,
    WaitForSingleObject,
};

/// How long a call waits for its reply.
const CALL: Duration = Duration::from_secs(30);

/// A keeper process just started.
pub struct Launched {
    pub process: OwnedHandle,
    pub pid: u32,
    /// It left midnad's job (CREATE_BREAKAWAY_FROM_JOB). False when the job doesn't allow
    /// that: the keeper then ends with the job.
    pub breakaway: bool,
}

/// Start `exe` (a `midna-pty.exe`) as the keeper of `session` in `home`. It inherits no
/// handles (std's `Command` would pass it every inheritable one midnad has, such as the pipe a
/// parent reads midnad's output from, which then never closes), has no console of its own
/// (DETACHED_PROCESS: CREATE_NO_WINDOW would give each keeper a console host it never uses) and
/// leaves midnad's job when the job allows it. Its folder is the one it runs from. It isn't put
/// in a new process group: that would make it ignore Ctrl+C, and the shell it starts with it.
pub fn launch(exe: &Path, home: &Path, session: &str) -> io::Result<Launched> {
    if !names::valid_session(session) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("session id {session:?}")));
    }
    let exe_s = exe.to_string_lossy().into_owned();
    let line = crate::shell::command_line(&[exe_s, "--home".into(), home.to_string_lossy().into_owned(), "--session".into(), session.into()]);
    let app = wide(exe.as_os_str());
    let dir = exe.parent().map(|d| wide(d.as_os_str()));
    let spawn = |flags: u32| -> io::Result<(OwnedHandle, u32)> {
        let mut cmdline = wide(&line);
        // SAFETY: NUL-terminated strings that outlive the call; the handles returned are ours.
        unsafe {
            let mut si: STARTUPINFOW = std::mem::zeroed();
            si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
            let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
            let ok = CreateProcessW(
                app.as_ptr(),
                cmdline.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                flags,
                std::ptr::null(),
                dir.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
                &si,
                &mut pi,
            );
            if ok == 0 {
                return Err(win::last_error());
            }
            drop(owned(pi.hThread));
            Ok((owned(pi.hProcess)?, pi.dwProcessId))
        }
    };
    let base = DETACHED_PROCESS;
    match spawn(base | CREATE_BREAKAWAY_FROM_JOB) {
        Ok((process, pid)) => Ok(Launched { process, pid, breakaway: true }),
        // midnad is in a job that doesn't allow breaking away.
        Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
            let (process, pid) = spawn(base)?;
            Ok(Launched { process, pid, breakaway: false })
        }
        Err(e) => Err(e),
    }
}

/// The shell to start, as `spawn` takes it.
#[derive(Clone, Debug)]
pub struct Spawn {
    pub argv: Vec<String>,
    pub cwd: String,
    /// The whole environment, not changes to the keeper's.
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

pub struct Started {
    pub client: Client,
    pub keeper: Launched,
    pub shell_pid: u32,
}

/// Start a keeper and its shell: [`launch`], connect, `hello`, `spawn`. The client stays
/// connected; drop it (or the process) and the keeper keeps the shell for the next one.
pub fn start(exe: &Path, home: &Path, session: &str, shell: Spawn) -> io::Result<Started> {
    start_in(exe, home, session, shell, None)
}

/// [`start`], with the keeper put in `job` before it starts the shell, so the shell, its console
/// host and everything they start are in it too (a job that ends them all when it closes ties
/// the terminal to the process holding it).
pub fn start_in(exe: &Path, home: &Path, session: &str, shell: Spawn, job: Option<&OwnedHandle>) -> io::Result<Started> {
    let keeper = launch(exe, home, session)?;
    let fail = |keeper: &Launched, e: io::Error| {
        // Don't leave a keeper that never got a shell (it would exit on its own, later).
        unsafe { windows_sys::Win32::System::Threading::TerminateProcess(raw(&keeper.process), 1) };
        e
    };
    if let Some(job) = job
        && unsafe { windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(raw(job), raw(&keeper.process)) } == 0
    {
        return Err(fail(&keeper, io::Error::other(format!("can't put the keeper in its job: {}", win::last_error()))));
    }
    let client = Client::connect_launched(&names::pipe_name(home, session), &keeper, Duration::from_secs(10)).map_err(|e| fail(&keeper, e))?;
    let hello = client.hello().map_err(|e| fail(&keeper, e))?;
    if hello.protocol < PROTOCOL {
        return Err(fail(&keeper, io::Error::other(format!("{} speaks protocol {}", exe.display(), hello.protocol))));
    }
    let shell_pid = client.spawn(shell).map_err(|e| fail(&keeper, e))?;
    Ok(Started { client, keeper, shell_pid })
}

/// What `hello` says.
#[derive(Clone, Debug, PartialEq)]
pub struct Hello {
    pub protocol: u32,
    pub build: String,
    pub pid: u32,
    pub shell_pid: Option<u32>,
    pub running: bool,
    pub exit_code: Option<u32>,
}

/// The shell's handles, copied into this process by `attach`.
pub struct Attached {
    /// Write the terminal's input here.
    pub input: OwnedHandle,
    /// Read the terminal's output here; it ends once the keeper closes the pseudoconsole.
    pub output: OwnedHandle,
    /// The shell's process: wait on it, read its exit code.
    pub process: OwnedHandle,
    pub shell_pid: u32,
}

#[derive(Default)]
struct ExitState {
    code: Option<u32>,
    closed: bool,
}

/// A connection to a keeper. Calls are one at a time; `exited` is collected as it arrives.
pub struct Client {
    pipe: Arc<Pipe>,
    replies: Mutex<mpsc::Receiver<Reply>>,
    call: Mutex<()>,
    exit: Arc<(Mutex<ExitState>, Condvar)>,
    stop: Arc<AtomicBool>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Client {
    /// Connect to the keeper serving `pipe_name`, waiting up to `timeout` for it to be there and
    /// free (it serves one client at a time).
    pub fn connect(pipe_name: &str, timeout: Duration) -> io::Result<Client> {
        Self::open(pipe_name, None, timeout)
    }

    fn connect_launched(pipe_name: &str, keeper: &Launched, timeout: Duration) -> io::Result<Client> {
        Self::open(pipe_name, Some(keeper), timeout)
    }

    fn open(pipe_name: &str, keeper: Option<&Launched>, timeout: Duration) -> io::Result<Client> {
        let name = wide(pipe_name);
        let deadline = Instant::now() + timeout;
        let h = loop {
            // SECURITY_IDENTIFICATION: whoever serves the pipe may learn who we are, never act
            // as us.
            let h = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    std::ptr::null_mut(),
                )
            };
            if let Ok(h) = owned(h) {
                break h;
            }
            let e = win::last_error();
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(e.kind(), format!("{pipe_name}: {e}")));
            }
            if let Some(k) = keeper
                && unsafe { WaitForSingleObject(raw(&k.process), 0) } != WAIT_TIMEOUT
            {
                let mut code = 0u32;
                unsafe { GetExitCodeProcess(raw(&k.process), &mut code) };
                return Err(io::Error::other(format!("the keeper exited ({code}) before serving {pipe_name}; see pty\\keeper.log")));
            }
            match e.raw_os_error().map(|c| c as u32) {
                Some(ERROR_PIPE_BUSY) => unsafe {
                    WaitNamedPipeW(name.as_ptr(), left.as_millis().min(1000) as u32);
                },
                Some(ERROR_FILE_NOT_FOUND) => std::thread::sleep(Duration::from_millis(20)),
                _ => return Err(io::Error::new(e.kind(), format!("{pipe_name}: {e}"))),
            }
        };
        let pipe = Arc::new(Pipe::new(h));
        let (tx, rx) = mpsc::channel();
        let exit: Arc<(Mutex<ExitState>, Condvar)> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let reader = {
            let (pipe, exit, stop) = (pipe.clone(), exit.clone(), stop.clone());
            std::thread::Builder::new().name("midna-pty-client".into()).spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let Ok(v) = proto::read_frame(&mut &*pipe) else { break };
                    match proto::parse_reply(v) {
                        Some(Reply::Exited { code }) => {
                            exit.0.lock().unwrap_or_else(|e| e.into_inner()).code = Some(code);
                            exit.1.notify_all();
                        }
                        Some(r) => {
                            if tx.send(r).is_err() {
                                break;
                            }
                        }
                        None => {}
                    }
                }
                exit.0.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
                exit.1.notify_all();
            })?
        };
        Ok(Client { pipe, replies: Mutex::new(rx), call: Mutex::new(()), exit, stop, reader: Some(reader) })
    }

    fn call(&self, req: Request) -> io::Result<Reply> {
        let _one = self.call.lock().unwrap_or_else(|e| e.into_inner());
        let kind = req.kind();
        proto::write_frame(&mut &*self.pipe, &req)?;
        let reply = match self.replies.lock().unwrap_or_else(|e| e.into_inner()).recv_timeout(CALL) {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => return Err(io::Error::new(io::ErrorKind::TimedOut, format!("no reply to {kind}"))),
            // A client the keeper won't serve is disconnected at once.
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(io::Error::new(io::ErrorKind::BrokenPipe, format!("the keeper closed the connection ({kind})"))),
        };
        match reply {
            Reply::Error { message, .. } => Err(io::Error::other(format!("{kind}: {message}"))),
            r => Ok(r),
        }
    }

    pub fn hello(&self) -> io::Result<Hello> {
        match self.call(Request::Hello { protocol: PROTOCOL, build: midna_proto::VERSION.into() })? {
            Reply::Hello { protocol, build, pid, shell_pid, running, exit_code } => Ok(Hello { protocol, build, pid, shell_pid, running, exit_code }),
            r => Err(unexpected(r)),
        }
    }

    /// Start the shell: its pid.
    pub fn spawn(&self, s: Spawn) -> io::Result<u32> {
        match self.call(Request::Spawn { argv: s.argv, cwd: s.cwd, env: s.env, cols: s.cols, rows: s.rows })? {
            Reply::Spawn { shell_pid } => Ok(shell_pid),
            r => Err(unexpected(r)),
        }
    }

    pub fn attach(&self) -> io::Result<Attached> {
        match self.call(Request::Attach { pid: std::process::id() })? {
            // SAFETY: the keeper made these handles in this process for us alone.
            Reply::Attach { input, output, process, shell_pid } => unsafe {
                let h = |v: u64| OwnedHandle::from_raw_handle(v as usize as _);
                Ok(Attached { input: h(input), output: h(output), process: h(process), shell_pid })
            },
            r => Err(unexpected(r)),
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        self.call(Request::Resize { cols, rows }).map(drop)
    }

    pub fn snapshot(&self) -> io::Result<Snapshot> {
        match self.call(Request::Snapshot)? {
            Reply::Snapshot(s) => Ok(s),
            r => Err(unexpected(r)),
        }
    }

    pub fn hangup(&self) -> io::Result<()> {
        self.call(Request::Hangup { end_job: false }).map(drop)
    }

    /// Hang up, and have the keeper end the shell's job once the shell has exited too, so
    /// nothing it started is left (midnad stopping).
    pub fn hangup_ending_job(&self) -> io::Result<()> {
        self.call(Request::Hangup { end_job: true }).map(drop)
    }

    pub fn kill(&self) -> io::Result<()> {
        self.call(Request::Kill).map(drop)
    }

    /// The shell's exit code once the keeper has sent it, waiting up to `timeout`. None if it
    /// hasn't come by then or the connection ended without it.
    pub fn wait_exited(&self, timeout: Duration) -> Option<u32> {
        let (m, cv) = &*self.exit;
        let g = m.lock().unwrap_or_else(|e| e.into_inner());
        let g = cv.wait_timeout_while(g, timeout, |s| s.code.is_none() && !s.closed).unwrap_or_else(|e| e.into_inner()).0;
        g.code
    }

    /// The keeper closed the connection (it refused us, or it exited).
    pub fn closed(&self) -> bool {
        self.exit.0.lock().unwrap_or_else(|e| e.into_inner()).closed
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // The reader waits in a read on the pipe; cancel it (again, if it started another
        // between our looks) until it has stopped, so the handle closes now.
        self.stop.store(true, Ordering::SeqCst);
        if let Some(reader) = self.reader.take() {
            while !reader.is_finished() {
                unsafe { CancelIoEx(self.pipe.handle(), std::ptr::null()) };
                std::thread::sleep(Duration::from_millis(2));
            }
            let _ = reader.join();
        }
    }
}

fn unexpected(r: Reply) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("unexpected reply {r:?}"))
}

/// The version folders live keepers of `home` run from, from their records in `MIDNA_HOME\pty`:
/// each record's pid, if that process is running `midna-pty.exe` (a pid left by a keeper that
/// was killed may belong to another program by now).
pub fn live_keeper_dirs(home: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = vec![];
    for (_, r) in names::records(home) {
        let Some(image) = win::process_image(r.pid) else { continue };
        if !image.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(names::EXE)) {
            continue;
        }
        if let Some(dir) = image.parent() {
            let dir = midna_proto::paths::canonical(dir).unwrap_or_else(|_| dir.to_path_buf());
            if !out.contains(&dir) {
                out.push(dir);
            }
        }
    }
    out
}
