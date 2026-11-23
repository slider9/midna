//! PTY spawn / resize / write. Spawning uses `std::process::Command` (PATH lookup, env, cwd)
//! with a `pre_exec` that makes the slave the controlling terminal of a new session. Windows
//! runs each terminal's pseudoconsole in a keeper process instead (`pty_windows.rs`), behind the
//! same [`Master`] / [`Spawned`] API.
#[cfg(unix)]
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd, RawFd};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::process::{Command, Stdio};
use std::sync::Mutex;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(windows)]
#[path = "pty_windows.rs"]
mod windows;
#[cfg(windows)]
pub use windows::{FILES as CONPTY_FILES, HOST_EXITED, Master, Owner, Spawned, Writer, command_line, conpty_dll, end_leftover_keepers, keeper_exe, spawn, which};

/// The most unread input a terminal holds. macOS stops taking a write here, wherever in the
/// write that falls.
#[cfg(unix)]
const INPUT_ROOM: usize = 1022;
/// How long a write waits for an app that has stopped reading its input.
#[cfg(unix)]
const ROOM_WAIT: Duration = Duration::from_secs(2);

/// Inherited variables that would confuse programs in a fresh terminal (e.g. Claude Code
/// refusing to start "nested" because midnad was launched from inside a Claude session).
/// Verified against Claude Code 2.1.288 run from inside another Claude session: an inherited
/// `CLAUDE_CODE_CHILD_SESSION` turns transcript saving off, and the parent's session id,
/// messaging socket and pid leak into the child. Host-terminal markers (Saggar, iTerm,
/// Terminal.app) would make the user's global hooks attribute a midna agent to the wrong
/// terminal. User configuration (e.g. `CLAUDE_CODE_ENABLE_TODO_TOOLS`) is kept.
pub(crate) const ENV_SCRUB: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "AI_AGENT",
    "CLAUDE_HANDOFF_BATON",
    "CLAUDE_HANDOFF_WRAPPER",
    "CLAUDE_HANDOFF_ARGV",
    "CODEX_THREAD_ID",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    // Set only on what a trigger starts; inherited by midnad, they'd mute every terminal's
    // stream publishes.
    "MIDNA_TRIGGER",
    "MIDNA_DELIVERY",
    // Says the agent it's set for carries midna's per-launch hooks (`rpc::session::mark_injected`);
    // inherited by a midnad started from such an agent, it'd stand down every terminal's global
    // hooks.
    "MIDNA_HOOKS_INJECTED",
    "SAGGAR",
    "SAGGAR_SESSION",
    "SAGGAR_CLI_DIR",
    "TERM_SESSION_ID",
    "ITERM_SESSION_ID",
];

/// Serializes openpty+spawn so a concurrently spawning child can't inherit another's fds.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

/// Hold off PTY spawns (an upgrade clears FD_CLOEXEC on every master while it hands off; a
/// child spawned meanwhile would inherit them).
pub fn spawn_lock() -> std::sync::MutexGuard<'static, ()> {
    SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(unix)]
pub struct Spawned {
    pub master: Master,
    pub pid: i32,
}

/// Owns the PTY master fd; closed when the last holder (reader, engine, writers) drops it.
#[cfg(unix)]
pub struct Master(RawFd);

#[cfg(unix)]
impl Drop for Master {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

/// Where DA/DSR replies go: the master fd (not owned; the [`Master`] outlives the engine).
#[cfg(unix)]
#[derive(Clone, Copy)]
pub struct Writer(RawFd);

#[cfg(unix)]
impl Writer {
    pub fn write(&self, data: &[u8]) {
        unsafe { libc::write(self.0, data.as_ptr() as *const _, data.len()) };
    }
}

#[cfg(unix)]
impl Master {
    /// Take ownership of a master fd (one kept across an upgrade).
    pub fn from_raw(fd: RawFd) -> Master {
        Master(fd)
    }

    pub fn raw(&self) -> RawFd {
        self.0
    }

    pub fn writer(&self) -> Writer {
        Writer(self.0)
    }

    pub fn write_all(&self, b: &[u8]) -> bool {
        write_all(self.0, b)
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        resize(self.0, cols, rows)
    }

    /// Poll (not a blocking read), so the reader notices a pause within `timeout_ms`.
    pub fn readable(&self, timeout_ms: i32) -> bool {
        let mut pfd = libc::pollfd { fd: self.0, events: libc::POLLIN, revents: 0 };
        let r = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        !(r == 0 || (r < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted))
    }

    /// Ok(0) or an error (EIO) once the child side is gone.
    pub fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = unsafe { libc::read(self.0, buf.as_mut_ptr() as *mut _, buf.len()) };
        if n < 0 { Err(std::io::Error::last_os_error()) } else { Ok(n as usize) }
    }

    /// The terminal's foreground process group, if any.
    pub fn foreground_group(&self) -> Option<i32> {
        let fg = unsafe { libc::tcgetpgrp(self.0) };
        (fg > 1).then_some(fg)
    }

    /// A Unix pty ends by itself when its last process closes the slave.
    pub fn child_exited(&self) {}
}

#[cfg(unix)]
fn set_cloexec(fd: RawFd) {
    unsafe {
        let f = libc::fcntl(fd, libc::F_GETFD);
        libc::fcntl(fd, libc::F_SETFD, f | libc::FD_CLOEXEC);
    }
}

/// Waits between tries when the daemon is out of file handles: handles that are closing (a
/// terminal ending, a CLI call finishing) free up in that time.
#[cfg(unix)]
const EXHAUSTED_RETRIES: [Duration; 3] = [Duration::from_millis(100), Duration::from_millis(300), Duration::from_millis(1000)];

/// Start `argv` on a new PTY. Out of file handles (EMFILE/ENFILE), it tries again a few times,
/// then says so in the error rather than a bare "Too many open files".
#[cfg(unix)]
pub fn spawn(argv: &[String], cwd: &str, env: &[(String, String)], cols: u16, rows: u16) -> std::io::Result<Spawned> {
    let mut waits = EXHAUSTED_RETRIES.iter();
    loop {
        match spawn_once(argv, cwd, env, cols, rows) {
            Err(e) if crate::fd_limit::exhausted(&e) => match waits.next() {
                Some(w) => std::thread::sleep(*w),
                None => {
                    let e = crate::fd_limit::explain(e);
                    eprintln!("midnad: could not start {}: {e}", argv.first().map_or("", String::as_str));
                    return Err(e);
                }
            },
            r => return r,
        }
    }
}

#[cfg(unix)]
fn spawn_once(argv: &[String], cwd: &str, env: &[(String, String)], cols: u16, rows: u16) -> std::io::Result<Spawned> {
    let Some((prog, args)) = argv.split_first() else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command"));
    };
    let _g = SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut m, mut s) = (0, 0);
    let mut ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    if unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null_mut(), &mut ws) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    set_cloexec(m);
    set_cloexec(s);
    // Owned until the child is running, so every early return (a failed dup when out of handles)
    // closes both ends.
    let master = unsafe { OwnedFd::from_raw_fd(m) };
    // Command dup2s these onto 0/1/2 (dup2 clears CLOEXEC on the copies).
    let slave = unsafe { OwnedFd::from_raw_fd(s) };
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .current_dir(if std::path::Path::new(cwd).is_dir() { cwd } else { "/" })
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    for k in ENV_SCRUB {
        cmd.env_remove(k);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    unsafe {
        cmd.pre_exec(|| {
            // Only async-signal-safe calls here.
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::ioctl(0, libc::TIOCSCTTY as _, 0);
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    // The reaper waits on the pid (not the Child), so adopted sessions after an upgrade work the
    // same way. Dropping a Child neither waits nor kills.
    Ok(Spawned { master: Master(master.into_raw_fd()), pid: child.id() as i32 })
}

#[cfg(unix)]
pub fn resize(fd: RawFd, cols: u16, rows: u16) {
    let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) };
}

/// Bytes the app hasn't read from its input yet.
#[cfg(unix)]
fn unread_input(fd: RawFd) -> Option<usize> {
    let mut n: libc::c_int = 0;
    (unsafe { libc::ioctl(fd, libc::FIONREAD, &mut n) } == 0).then_some(n as usize)
}

/// Wait for room for all of `len` bytes. A key's escape sequence that doesn't fit is cut where
/// the room ends: an app that's behind reads `ESC [` of `ESC [ 3 ~`, gives up on it and takes the
/// `3~` that follows for typing.
#[cfg(unix)]
fn wait_for_room(fd: RawFd, len: usize) {
    let by = Instant::now() + ROOM_WAIT;
    while unread_input(fd).is_some_and(|n| n + len > INPUT_ROOM) && Instant::now() < by {
        std::thread::sleep(Duration::from_micros(200));
    }
}

/// Write everything (the master fd is blocking; retries on EINTR/EAGAIN), in one piece if it can
/// be.
#[cfg(unix)]
pub fn write_all(fd: RawFd, mut b: &[u8]) -> bool {
    if b.len() <= INPUT_ROOM {
        wait_for_room(fd, b.len());
    }
    while !b.is_empty() {
        let n = unsafe { libc::write(fd, b.as_ptr() as *const _, b.len()) };
        if n <= 0 {
            let e = std::io::Error::last_os_error();
            if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) {
                std::thread::sleep(std::time::Duration::from_micros(200));
                continue;
            }
            return false;
        }
        b = &b[n as usize..];
    }
    true
}

/// Resolve a program name against PATH (for error messages before spawning).
#[cfg(unix)]
pub fn which(prog: &str, path: &str) -> Option<String> {
    if prog.contains('/') {
        return std::path::Path::new(prog).exists().then(|| prog.to_string());
    }
    path.split(':').map(|d| format!("{d}/{prog}")).find(|p| {
        std::fs::metadata(p).map(|m| std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0).unwrap_or(false)
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    /// A pty in raw mode, as a full-screen app sets it: (master, slave).
    fn raw_pty() -> (OwnedFd, OwnedFd) {
        let (mut master, mut slave) = (-1, -1);
        let made = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
        assert_eq!(made, 0);
        let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            libc::tcgetattr(slave.as_raw_fd(), &mut t);
            libc::cfmakeraw(&mut t);
            libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &t);
        }
        (master, slave)
    }

    #[test]
    fn a_burst_of_keys_reaches_a_slow_app_whole() {
        const KEY: &[u8] = b"\x1b[3~";
        const PRESSES: usize = 2000;
        let (master, slave) = raw_pty();
        let fd = master.as_raw_fd();
        let writer = std::thread::spawn(move || (0..PRESSES).all(|_| write_all(fd, KEY)));
        let (mut buf, mut total) = ([0u8; 4096], 0);
        while total < PRESSES * KEY.len() {
            std::thread::sleep(Duration::from_millis(2));
            let n = unsafe { libc::read(slave.as_raw_fd(), buf.as_mut_ptr() as *mut _, buf.len()) };
            assert!(n > 0);
            assert_eq!(n as usize % KEY.len(), 0, "a read ended inside a key, {total} bytes in");
            total += n as usize;
        }
        assert!(writer.join().unwrap());
    }

    #[test]
    fn an_app_that_stops_reading_doesnt_hold_a_write_forever() {
        let (master, _slave) = raw_pty();
        let fd = master.as_raw_fd();
        assert!(write_all(fd, &[b'x'; INPUT_ROOM - 2]));
        let start = Instant::now();
        wait_for_room(fd, 4);
        assert!((ROOM_WAIT..ROOM_WAIT * 2).contains(&start.elapsed()));
    }
}
