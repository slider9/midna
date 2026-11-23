//! `midna-pty.exe --home <MIDNA_HOME> --session <id>`: the keeper for one terminal.
//!
//! It serves `\\.\pipe\midna-pty-<home hash>-<session id>` ([`crate::names::pipe_name`]):
//! current user only, local clients only, one client at a time, and only a client running
//! `midnad.exe` from `MIDNA_HOME\bin` or from the keeper's own folder. The first client sends
//! `spawn` (the shell's argv, folder, environment and size never go on the command line); later
//! ones attach to the same shell. Once the shell runs, the keeper records itself in
//! `MIDNA_HOME\pty\<session id>.json`; it exits once the shell has exited and a client has been
//! told the code (`exited`), or once the shell has ended after `hangup` or `kill`, and removes
//! the record then. What it does goes to `MIDNA_HOME\pty\keeper.log`.
use crate::names::{self, Record};
use crate::proto::{self, PROTOCOL, Reply, Request};
use crate::shell::{self, Shell};
use crate::win::{self, Pipe, owned, raw, wide};
use crate::console;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FlushFileBuffers, PIPE_ACCESS_DUPLEX, ReadFile};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcess, PROCESS_DUP_HANDLE, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    WaitForSingleObject,
};

/// How long a keeper with no shell waits for the client that started it.
const FIRST_CLIENT: Duration = Duration::from_secs(30);
/// After `hangup`, how long the shell has to exit before its job is ended.
const HANGUP_GRACE: Duration = Duration::from_secs(5);
/// How long closing the pseudoconsole may take before the keeper reads the output itself.
const CLOSE_STALL: Duration = Duration::from_secs(2);
/// The shell's last output gets this long to reach the output pipe before the console closes.
const LAST_PAINT: Duration = Duration::from_millis(150);

/// Exit codes.
const BAD_ARGS: i32 = 2;
const PIPE_TAKEN: i32 = 3;

pub fn run(args: &[String]) -> i32 {
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let (Some(home), Some(session)) = (arg("--home"), arg("--session")) else {
        eprintln!("usage: midna-pty --home <MIDNA_HOME> --session <id>\nmidnad starts it, one per terminal; it isn't meant to be run by hand.");
        return BAD_ARGS;
    };
    let home = PathBuf::from(home);
    if !home.is_absolute() || !names::valid_session(&session) {
        eprintln!("midna-pty: --home must be absolute and --session letters, digits, - and _");
        return BAD_ARGS;
    }
    console::ignore_console_interrupts();
    let log = Log::open(&home, &session);
    let pipe_name = names::pipe_name(&home, &session);
    let pipe = match create_pipe(&pipe_name) {
        Ok(p) => p,
        Err(e) => {
            log.line(&format!("can't serve {pipe_name}: {e}"));
            return PIPE_TAKEN;
        }
    };
    let exe = std::env::current_exe().ok().map(|p| midna_proto::paths::canonical(&p).unwrap_or(p)).unwrap_or_default();
    let k = Arc::new(Keeper {
        home,
        session,
        pipe_name,
        pipe,
        own_dir: exe.parent().map(Path::to_path_buf).unwrap_or_default(),
        exe,
        log,
        shell: OnceLock::new(),
        exit_code: Mutex::new(None),
        conn: Mutex::new(None),
        ending: AtomicBool::new(false),
        collected: AtomicBool::new(false),
        finishing: AtomicBool::new(false),
        closing: AtomicBool::new(false),
        closed: (Mutex::new(false), Condvar::new()),
        draining: AtomicBool::new(false),
    });
    k.log.line(&format!("up, build {}, ConPTY {}", midna_proto::VERSION, shell::conpty().dll.as_ref().map_or("Windows' own".into(), |d| d.display().to_string())));
    k.serve_forever()
}

struct Keeper {
    home: PathBuf,
    session: String,
    pipe_name: String,
    /// The pipe's one instance, reused for each client in turn.
    pipe: Pipe,
    exe: PathBuf,
    own_dir: PathBuf,
    log: Log,
    shell: OnceLock<Arc<Shell>>,
    exit_code: Mutex<Option<u32>>,
    /// The connected client. Everything written to the pipe is written holding this lock.
    conn: Mutex<Option<Conn>>,
    /// `hangup` or `kill` was asked for: exit once the shell has, told or not.
    ending: AtomicBool,
    /// A client was sent `exited`.
    collected: AtomicBool,
    finishing: AtomicBool,
    closing: AtomicBool,
    closed: (Mutex<bool>, Condvar),
    draining: AtomicBool,
}

struct Conn {
    /// It took copies of the pipes (`attach`) on this connection, so it reads the output.
    attached: bool,
    exited_sent: bool,
}

impl Keeper {
    fn serve_forever(self: &Arc<Self>) -> i32 {
        loop {
            let wait = if self.shell.get().is_none() { Some(FIRST_CLIENT) } else { None };
            let pid = match self.accept(wait) {
                Ok(pid) => pid,
                Err(e) if self.shell.get().is_none() => {
                    self.log.line(&format!("no client started a shell: {e}"));
                    return 1;
                }
                Err(e) => {
                    self.log.line(&format!("accept: {e}"));
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
            };
            match self.allowed(pid) {
                Ok(()) => {
                    *self.conn.lock().unwrap_or_else(|e| e.into_inner()) = Some(Conn { attached: false, exited_sent: false });
                    self.serve(pid);
                    *self.conn.lock().unwrap_or_else(|e| e.into_inner()) = None;
                }
                Err(why) => self.log.line(&format!("refused pid {pid}: {why}")),
            }
            unsafe { DisconnectNamedPipe(self.pipe.handle()) };
            if self.shell.get().is_none() {
                self.log.line("the client left before starting a shell");
                return 1;
            }
            self.maybe_exit();
        }
    }

    /// Wait for the next client: its pid.
    fn accept(&self, wait: Option<Duration>) -> std::io::Result<u32> {
        let h = self.pipe.handle();
        // SAFETY: the OVERLAPPED and its event outlive the operation, which is waited for (or
        // cancelled and then waited for) before they go.
        unsafe {
            let ev = owned(CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()))?;
            let mut ov: OVERLAPPED = std::mem::zeroed();
            ov.hEvent = raw(&ev);
            if ConnectNamedPipe(h, &mut ov) == 0 {
                match GetLastError() {
                    ERROR_PIPE_CONNECTED => {}
                    ERROR_IO_PENDING => {
                        let ms = wait.map_or(INFINITE, |d| d.as_millis() as u32);
                        if WaitForSingleObject(raw(&ev), ms) != WAIT_OBJECT_0 {
                            CancelIoEx(h, &ov);
                            let mut n = 0;
                            GetOverlappedResult(h, &ov, &mut n, 1);
                            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "nobody connected"));
                        }
                        let mut n = 0;
                        if GetOverlappedResult(h, &ov, &mut n, 0) == 0 {
                            return Err(win::last_error());
                        }
                    }
                    // Connected and gone again before we looked.
                    ERROR_NO_DATA => {
                        DisconnectNamedPipe(h);
                        return Err(std::io::Error::other("the client left at once"));
                    }
                    e => return Err(std::io::Error::from_raw_os_error(e as i32)),
                }
            }
            let mut pid = 0u32;
            if GetNamedPipeClientProcessId(h, &mut pid) == 0 {
                return Err(win::last_error());
            }
            Ok(pid)
        }
    }

    /// A client must be midnad from this home's `bin` (any version) or from beside the keeper.
    fn allowed(&self, pid: u32) -> Result<(), String> {
        let image = win::process_image(pid).ok_or("can't see its program")?;
        let real = midna_proto::paths::canonical(&image).unwrap_or(image);
        let name = real.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !name.eq_ignore_ascii_case(names::CLIENT_EXE) {
            return Err(format!("{} isn't {}", real.display(), names::CLIENT_EXE));
        }
        let bin = self.home.join("bin");
        let bin = midna_proto::paths::canonical(&bin).unwrap_or(bin);
        let dir = real.parent().unwrap_or(Path::new(""));
        if inside(dir, &bin) || same_dir(dir, &self.own_dir) {
            return Ok(());
        }
        Err(format!("{} is neither in {} nor beside the keeper", real.display(), bin.display()))
    }

    fn serve(self: &Arc<Self>, pid: u32) {
        loop {
            let v = match proto::read_frame(&mut &self.pipe) {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return,
                Err(e) => {
                    self.log.line(&format!("pid {pid}: {e}"));
                    return;
                }
            };
            let reply = match proto::parse_request(v) {
                Ok(req) => {
                    let kind = req.kind();
                    self.handle(req, pid).unwrap_or_else(|message| Reply::Error { request: kind.into(), message })
                }
                Err(reply) => reply,
            };
            if let Reply::Error { request, message } = &reply {
                self.log.line(&format!("pid {pid}: {request}: {message}"));
            }
            let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
            if proto::write_frame(&mut &self.pipe, &reply).is_err() {
                return;
            }
            self.tell_exit(&mut conn);
        }
    }

    /// Send `exited` on this connection, once, if the shell has exited.
    fn tell_exit(&self, conn: &mut Option<Conn>) {
        let Some(code) = *self.exit_code.lock().unwrap_or_else(|e| e.into_inner()) else { return };
        let Some(c) = conn.as_mut().filter(|c| !c.exited_sent) else { return };
        c.exited_sent = true;
        if proto::write_frame(&mut &self.pipe, &Reply::Exited { code }).is_ok() {
            self.collected.store(true, Ordering::SeqCst);
        }
    }

    fn handle(self: &Arc<Self>, req: Request, client: u32) -> Result<Reply, String> {
        let shell = || self.shell.get().cloned().ok_or_else(|| "no shell yet: send spawn first".to_string());
        match req {
            Request::Hello { .. } => {
                let s = self.shell.get();
                let exit_code = *self.exit_code.lock().unwrap_or_else(|e| e.into_inner());
                Ok(Reply::Hello {
                    protocol: PROTOCOL,
                    build: midna_proto::VERSION.into(),
                    pid: std::process::id(),
                    shell_pid: s.map(|s| s.pid),
                    running: s.is_some() && exit_code.is_none(),
                    exit_code,
                })
            }
            Request::Spawn { argv, cwd, env, cols, rows } => {
                if self.shell.get().is_some() {
                    return Err("this keeper already has a shell".into());
                }
                if cols == 0 || rows == 0 {
                    return Err(format!("a {cols}x{rows} terminal"));
                }
                let s = Arc::new(shell::spawn(&argv, &cwd, &env, cols, rows).map_err(|e| e.to_string())?);
                let pid = s.pid;
                let _ = self.shell.set(s);
                self.log.line(&format!("shell {pid}: {}", argv.first().map(String::as_str).unwrap_or_default()));
                if let Err(e) = self.write_record(pid) {
                    self.log.line(&format!("can't write {}: {e}", names::record_path(&self.home, &self.session).display()));
                }
                let k = self.clone();
                std::thread::spawn(move || k.watch_shell());
                Ok(Reply::Spawn { shell_pid: pid })
            }
            Request::Attach { pid } => {
                if pid != client {
                    return Err(format!("attach copies handles into the connected client ({client}) only, not {pid}"));
                }
                let s = shell()?;
                let reply = attach(&s, pid).map_err(|e| e.to_string())?;
                if let Some(c) = self.conn.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                    c.attached = true;
                }
                Ok(reply)
            }
            Request::Resize { cols, rows } => {
                if cols == 0 || rows == 0 {
                    return Err(format!("a {cols}x{rows} terminal"));
                }
                shell()?.resize(cols, rows).map_err(|e| e.to_string())?;
                Ok(Reply::Resize {})
            }
            Request::Snapshot => Ok(Reply::Snapshot(console::snapshot(&*shell()?)?)),
            Request::Hangup => {
                let s = shell()?;
                self.ending.store(true, Ordering::SeqCst);
                self.log.line(&format!("hangup from pid {client}"));
                self.close_console(!self.client_reads());
                let k = self.clone();
                std::thread::spawn(move || {
                    if unsafe { WaitForSingleObject(raw(&s.process), HANGUP_GRACE.as_millis() as u32) } != WAIT_OBJECT_0 {
                        k.log.line("the shell didn't exit after the hangup; ending its job");
                        s.kill(1);
                    }
                });
                Ok(Reply::Hangup {})
            }
            Request::Kill => {
                let s = shell()?;
                self.ending.store(true, Ordering::SeqCst);
                self.log.line(&format!("kill from pid {client}"));
                s.kill(1);
                Ok(Reply::Kill {})
            }
        }
    }

    fn client_reads(&self) -> bool {
        self.conn.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|c| c.attached)
    }

    fn watch_shell(self: Arc<Self>) {
        let Some(s) = self.shell.get().cloned() else { return };
        let mut code = 0u32;
        unsafe {
            WaitForSingleObject(raw(&s.process), INFINITE);
            GetExitCodeProcess(raw(&s.process), &mut code);
        }
        *self.exit_code.lock().unwrap_or_else(|e| e.into_inner()) = Some(code);
        self.log.line(&format!("shell {} exited {code}", s.pid));
        std::thread::sleep(LAST_PAINT);
        self.close_console(false);
        self.tell_exit(&mut self.conn.lock().unwrap_or_else(|e| e.into_inner()));
        self.maybe_exit();
    }

    /// Close the pseudoconsole off the control thread, once. With `drain`, read and drop the
    /// output meanwhile (nobody else is reading it); without, only if closing stalls and nobody
    /// is reading then: Windows' own ConPTY waits for room in a full output pipe.
    fn close_console(self: &Arc<Self>, drain: bool) {
        let Some(s) = self.shell.get().cloned() else { return };
        if drain {
            self.drain();
        }
        if self.closing.swap(true, Ordering::SeqCst) {
            return;
        }
        let k = self.clone();
        std::thread::spawn(move || {
            console::close(&s);
            let (done, cv) = &k.closed;
            *done.lock().unwrap_or_else(|e| e.into_inner()) = true;
            cv.notify_all();
        });
        if !drain {
            let k = self.clone();
            std::thread::spawn(move || {
                if !k.wait_closed(CLOSE_STALL) && !k.client_reads() {
                    k.log.line("closing the console stalled; reading its output");
                    k.drain();
                }
            });
        }
    }

    fn wait_closed(&self, d: Duration) -> bool {
        let (done, cv) = &self.closed;
        let g = done.lock().unwrap_or_else(|e| e.into_inner());
        *cv.wait_timeout_while(g, d, |closed| !*closed).unwrap_or_else(|e| e.into_inner()).0
    }

    /// Read the output pipe and drop what's there, until it ends.
    fn drain(&self) {
        let Some(s) = self.shell.get().cloned() else { return };
        if self.draining.swap(true, Ordering::SeqCst) {
            return;
        }
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 << 10];
            loop {
                let mut n = 0u32;
                if unsafe { ReadFile(raw(&s.output), buf.as_mut_ptr(), buf.len() as u32, &mut n, std::ptr::null_mut()) } == 0 || n == 0 {
                    break;
                }
            }
        });
    }

    fn maybe_exit(self: &Arc<Self>) {
        let exited = self.exit_code.lock().unwrap_or_else(|e| e.into_inner()).is_some();
        if exited && (self.collected.load(Ordering::SeqCst) || self.ending.load(Ordering::SeqCst)) {
            self.finish();
        }
    }

    fn finish(self: &Arc<Self>) {
        if self.finishing.swap(true, Ordering::SeqCst) {
            return;
        }
        if !self.wait_closed(CLOSE_STALL) {
            if !self.client_reads() {
                self.drain();
            }
            if !self.wait_closed(CLOSE_STALL) {
                self.log.line("the console still hasn't closed; exiting anyway");
            }
        }
        let _ = std::fs::remove_file(names::record_path(&self.home, &self.session));
        // Let the client read what it was sent (`exited`) before the pipe goes.
        let h = self.pipe.handle() as usize;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            unsafe { FlushFileBuffers(h as HANDLE) };
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(Duration::from_secs(2));
        self.log.line("exits");
        std::process::exit(0);
    }

    fn write_record(&self, shell_pid: u32) -> std::io::Result<()> {
        let r = Record {
            pid: std::process::id(),
            pipe: self.pipe_name.clone(),
            protocol: PROTOCOL,
            build: midna_proto::VERSION.into(),
            shell_pid,
            exe: self.exe.display().to_string(),
        };
        let path = names::record_path(&self.home, &self.session);
        std::fs::create_dir_all(names::pty_dir(&self.home))?;
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec_pretty(&r).map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, &path)
    }
}

/// Copy the shell's pipes and process into `pid`.
fn attach(s: &Shell, pid: u32) -> std::io::Result<Reply> {
    // SAFETY: handles we own, copied into a process we opened; a copy made before a failure is
    // closed again in that process.
    unsafe {
        let target = owned(OpenProcess(PROCESS_DUP_HANDLE, 0, pid))?;
        let mut made: Vec<HANDLE> = vec![];
        let mut dup = |h: HANDLE, access: u32, options: u32| -> std::io::Result<u64> {
            let mut out: HANDLE = std::ptr::null_mut();
            if DuplicateHandle(GetCurrentProcess(), h, raw(&target), &mut out, access, 0, options) == 0 {
                let e = win::last_error();
                for m in made.drain(..) {
                    DuplicateHandle(raw(&target), m, std::ptr::null_mut(), std::ptr::null_mut(), 0, 0, DUPLICATE_CLOSE_SOURCE);
                }
                return Err(e);
            }
            made.push(out);
            Ok(out as u64)
        };
        let input = dup(raw(&s.input), 0, DUPLICATE_SAME_ACCESS)?;
        let output = dup(raw(&s.output), 0, DUPLICATE_SAME_ACCESS)?;
        let process = dup(raw(&s.process), PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0)?;
        Ok(Reply::Attach { input, output, process, shell_pid: s.pid })
    }
}

fn create_pipe(name: &str) -> std::io::Result<Pipe> {
    let sec = win::user_only()?;
    let wname = wide(name);
    // FIRST_PIPE_INSTANCE: if anyone already serves this name, fail rather than share it.
    let h = unsafe {
        CreateNamedPipeW(
            wname.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            64 << 10,
            64 << 10,
            0,
            &sec.attrs,
        )
    };
    Ok(Pipe::new(owned(h)?))
}

fn folded(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

fn same_dir(a: &Path, b: &Path) -> bool {
    !b.as_os_str().is_empty() && folded(a) == folded(b)
}

fn inside(dir: &Path, root: &Path) -> bool {
    let (d, r) = (folded(dir), folded(root));
    !r.is_empty() && (d == r || d.starts_with(&format!("{r}\\")))
}

/// `MIDNA_HOME\pty\keeper.log`, shared by every keeper of the home; restarted once it passes 1 MB.
struct Log {
    file: Mutex<Option<std::fs::File>>,
    session: String,
}

impl Log {
    fn open(home: &Path, session: &str) -> Log {
        let dir = names::pty_dir(home);
        let path = dir.join("keeper.log");
        let _ = std::fs::create_dir_all(&dir);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1 << 20) {
            let _ = std::fs::rename(&path, dir.join("keeper.log.old"));
        }
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok();
        Log { file: Mutex::new(file), session: session.into() }
    }

    fn line(&self, msg: &str) {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or_default();
        if let Some(f) = self.file.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let _ = f.write_all(format!("{secs:.3} [{}] {}: {msg}\n", std::process::id(), self.session).as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_folder_is_inside_bin_or_the_keepers_own() {
        let bin = Path::new(r"C:\Home\bin");
        assert!(inside(Path::new(r"c:\home\BIN\0.1.0-abcd"), bin));
        assert!(inside(Path::new(r"C:\Home\bin"), bin));
        assert!(!inside(Path::new(r"C:\Home\binary\x"), bin));
        assert!(!inside(Path::new(r"C:\Home"), bin));
        assert!(same_dir(Path::new(r"C:\t\debug\"), Path::new(r"c:\T\Debug")));
        assert!(!same_dir(Path::new(r"C:\t"), Path::new("")));
    }
}
