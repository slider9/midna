//! The few process, clock and file primitives midnad needs from the OS, behind one API for
//! macOS (and other Unix) and Windows. Everything that only makes sense on one of them (the
//! same-PID upgrade, launchd, code signatures) lives in its own module instead.
//!
//! Windows has no signals or process groups. A "process group" here is the process and its
//! descendants (what a terminal's shell or a trigger's command started), hang-up, terminate and
//! kill all end the processes, and stop/continue suspend and resume their threads.
use std::fs::OpenOptions;
use std::path::Path;
use std::process::{Command, ExitStatus};

/// Signal numbers midna reports in exit statuses (the same on macOS and Linux; a Windows exit
/// never carries one).
pub const SIGHUP: i32 = 1;
pub const SIGINT: i32 = 2;

/// What separates PATH's entries (Windows paths carry a drive colon).
#[cfg(unix)]
pub const PATH_SEP: char = ':';
#[cfg(windows)]
pub const PATH_SEP: char = ';';

/// What to tell a process (or a process group, see [`signal_group`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sig {
    Hup,
    Term,
    Kill,
    Stop,
    Cont,
}

/// Is `pid` still running (including as someone else's process)?
pub fn alive(pid: i32) -> bool {
    pid > 0 && imp::alive(pid)
}

/// Send `sig` to one process.
pub fn signal(pid: i32, sig: Sig) -> bool {
    pid > 0 && imp::signal(pid, sig)
}

/// Send `sig` to the group `pgid` leads: its POSIX process group on Unix, the process and its
/// descendants on Windows.
pub fn signal_group(pgid: i32, sig: Sig) -> bool {
    pgid > 1 && imp::signal_group(pgid, sig)
}

/// Start `cmd` in its own group (a new POSIX session, or a new Windows process group with no
/// console window), so a timeout or a close can stop everything it started.
pub fn new_group(cmd: &mut Command) {
    imp::new_group(cmd)
}

/// The signal that ended a process (Unix only; Windows processes only have exit codes).
pub fn exit_signal(status: &ExitStatus) -> Option<i32> {
    imp::exit_signal(status)
}

/// 32 random bits from the OS.
pub fn random_u32() -> u32 {
    imp::random_u32()
}

/// When this boot started, unix seconds (0 when the OS won't say).
pub fn boot_time() -> i64 {
    imp::boot_time()
}

/// Monotonic nanoseconds that don't advance while the machine sleeps.
pub fn uptime_ns() -> u64 {
    imp::uptime_ns()
}

/// Make `path` readable by this user only (0600 for a file, 0700 for a folder). Windows: a
/// no-op, since midna's files live under `%LOCALAPPDATA%`, which only the user (and admins)
/// can open.
pub fn set_private(path: &Path) -> std::io::Result<()> {
    imp::set_private(path)
}

/// Create files from `opts` readable by this user only (mode 0600 on Unix).
pub fn private_file(opts: &mut OpenOptions) -> &mut OpenOptions {
    imp::private_file(opts)
}

/// A POSIX `sh` for midna's own wrapper scripts (`sh -c '…'`): `/bin/sh` on Unix; Git for
/// Windows' `bash.exe` on Windows, which Claude Code needs there anyway.
pub fn posix_sh() -> String {
    imp::posix_sh()
}

/// The POSIX session `pid` belongs to (`getsid`); None on Windows, which has no sessions.
pub fn session_of(pid: i32) -> Option<i32> {
    imp::session_of(pid)
}

/// Windows: the PATH a program started now gets, read fresh. That's the system's and then the
/// user's from the registry, followed by whatever midnad's own PATH adds. Installers write to
/// the registry, so a `claude` installed while midnad runs is found without restarting it. It
/// stands in for macOS's login shell, which re-reads the user's PATH for every terminal.
#[cfg(windows)]
pub fn user_path() -> String {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    let machine = imp::reg_string(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment", "Path");
    let user = imp::reg_string(HKEY_CURRENT_USER, "Environment", "Path");
    let own = std::env::var("PATH").ok();
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<&str> = vec![];
    for dir in [&machine, &user, &own].into_iter().flatten().flat_map(|p| p.split(';')) {
        let dir = dir.trim();
        if !dir.is_empty() && seen.insert(dir.trim_end_matches('\\').to_ascii_lowercase()) {
            out.push(dir);
        }
    }
    out.join(";")
}

#[cfg(unix)]
mod imp {
    use super::Sig;
    use std::fs::OpenOptions;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::path::Path;
    use std::process::{Command, ExitStatus};

    fn raw(sig: Sig) -> libc::c_int {
        match sig {
            Sig::Hup => libc::SIGHUP,
            Sig::Term => libc::SIGTERM,
            Sig::Kill => libc::SIGKILL,
            Sig::Stop => libc::SIGSTOP,
            Sig::Cont => libc::SIGCONT,
        }
    }

    pub fn alive(pid: i32) -> bool {
        unsafe { libc::kill(pid, 0) == 0 } || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    pub fn signal(pid: i32, sig: Sig) -> bool {
        unsafe { libc::kill(pid, raw(sig)) == 0 }
    }

    pub fn signal_group(pgid: i32, sig: Sig) -> bool {
        unsafe { libc::kill(-pgid, raw(sig)) == 0 }
    }

    pub fn new_group(cmd: &mut Command) {
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
                Ok(())
            });
        }
    }

    pub fn exit_signal(status: &ExitStatus) -> Option<i32> {
        status.signal()
    }

    #[cfg(target_os = "macos")]
    pub fn random_u32() -> u32 {
        unsafe { libc::arc4random() }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn random_u32() -> u32 {
        let mut b = [0u8; 4];
        unsafe { libc::getrandom(b.as_mut_ptr().cast(), 4, 0) };
        u32::from_ne_bytes(b)
    }

    #[cfg(target_os = "macos")]
    pub fn boot_time() -> i64 {
        let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
        let mut size = std::mem::size_of::<libc::timeval>();
        let mut mib = [libc::CTL_KERN, libc::KERN_BOOTTIME];
        let r = unsafe { libc::sysctl(mib.as_mut_ptr(), 2, &mut tv as *mut _ as *mut libc::c_void, &mut size, std::ptr::null_mut(), 0) };
        if r == 0 { tv.tv_sec } else { 0 }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn boot_time() -> i64 {
        let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
        if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) } != 0 {
            return 0;
        }
        midna_proto::time::now_unix() - ts.tv_sec
    }

    #[cfg(target_os = "macos")]
    pub fn uptime_ns() -> u64 {
        unsafe extern "C" {
            fn clock_gettime_nsec_np(id: libc::clockid_t) -> u64;
        }
        unsafe { clock_gettime_nsec_np(libc::CLOCK_UPTIME_RAW) }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn uptime_ns() -> u64 {
        let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }

    pub fn set_private(path: &Path) -> std::io::Result<()> {
        let mode = if path.is_dir() { 0o700 } else { 0o600 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }

    pub fn private_file(opts: &mut OpenOptions) -> &mut OpenOptions {
        opts.mode(0o600)
    }

    pub fn posix_sh() -> String {
        "/bin/sh".into()
    }

    pub fn session_of(pid: i32) -> Option<i32> {
        let sid = unsafe { libc::getsid(pid) };
        (sid > 0).then_some(sid)
    }
}

#[cfg(windows)]
mod imp {
    use super::Sig;
    use std::fs::OpenOptions;
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::{Command, ExitStatus};
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError, HANDLE, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SUSPEND_RESUME,
        PROCESS_TERMINATE, TerminateProcess,
    };

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtSuspendProcess(process: HANDLE) -> i32;
        fn NtResumeProcess(process: HANDLE) -> i32;
    }

    /// An open process handle, closed on drop.
    pub struct Proc(pub HANDLE);
    impl Proc {
        pub fn open(pid: i32, access: u32) -> Option<Proc> {
            let h = unsafe { OpenProcess(access, 0, pid as u32) };
            (!h.is_null()).then_some(Proc(h))
        }
    }
    impl Drop for Proc {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    pub fn alive(pid: i32) -> bool {
        match Proc::open(pid, PROCESS_QUERY_LIMITED_INFORMATION) {
            Some(p) => {
                let mut code = 0u32;
                unsafe { GetExitCodeProcess(p.0, &mut code) != 0 && code == STILL_ACTIVE as u32 }
            }
            // A protected or elevated process we may not open still exists.
            None => (unsafe { GetLastError() }) == ERROR_ACCESS_DENIED,
        }
    }

    pub fn signal(pid: i32, sig: Sig) -> bool {
        match sig {
            Sig::Hup | Sig::Term | Sig::Kill => Proc::open(pid, PROCESS_TERMINATE).is_some_and(|p| unsafe { TerminateProcess(p.0, 1) } != 0),
            Sig::Stop => Proc::open(pid, PROCESS_SUSPEND_RESUME).is_some_and(|p| unsafe { NtSuspendProcess(p.0) } >= 0),
            Sig::Cont => Proc::open(pid, PROCESS_SUSPEND_RESUME).is_some_and(|p| unsafe { NtResumeProcess(p.0) } >= 0),
        }
    }

    /// `pgid` and everything under it, deepest first for an end (so a parent doesn't respawn a
    /// child it lost), the root first for stop/continue.
    pub fn signal_group(pgid: i32, sig: Sig) -> bool {
        let table = crate::procs::snapshot();
        let (mut pids, mut i) = (vec![pgid], 0);
        while i < pids.len() && pids.len() < 4096 {
            let parent = pids[i];
            pids.extend(table.iter().filter(|&(&p, &(pp, _))| pp == parent && p != parent && !pids.contains(&p)).map(|(&p, _)| p).collect::<Vec<_>>());
            i += 1;
        }
        if matches!(sig, Sig::Hup | Sig::Term | Sig::Kill) {
            pids.reverse();
        }
        let mut any = false;
        for p in pids {
            any |= signal(p, sig);
        }
        any
    }

    pub fn new_group(cmd: &mut Command) {
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    pub fn exit_signal(_: &ExitStatus) -> Option<i32> {
        None
    }

    pub fn random_u32() -> u32 {
        let mut b = [0u8; 4];
        unsafe { windows_sys::Win32::Security::Cryptography::ProcessPrng(b.as_mut_ptr(), 4) };
        u32::from_ne_bytes(b)
    }

    pub fn boot_time() -> i64 {
        let up_ms = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() };
        midna_proto::time::now_unix() - (up_ms / 1000) as i64
    }

    pub fn uptime_ns() -> u64 {
        let mut t = 0u64;
        unsafe { windows_sys::Win32::System::WindowsProgramming::QueryUnbiasedInterruptTime(&mut t) };
        t.saturating_mul(100)
    }

    pub fn set_private(_: &Path) -> std::io::Result<()> {
        Ok(())
    }

    pub fn private_file(opts: &mut OpenOptions) -> &mut OpenOptions {
        opts
    }

    /// Git for Windows' bash: `MIDNA_POSIX_SH`, then the usual install folders, then `bash.exe`
    /// on PATH (which may be WSL's launcher in System32, so it comes last).
    pub fn posix_sh() -> String {
        if let Some(s) = std::env::var_os("MIDNA_POSIX_SH").filter(|s| !s.is_empty()) {
            return s.to_string_lossy().into_owned();
        }
        let mut roots = vec![];
        for k in ["ProgramFiles", "ProgramW6432", "LOCALAPPDATA"] {
            if let Some(v) = std::env::var_os(k) {
                let base = std::path::PathBuf::from(v);
                roots.push(if k == "LOCALAPPDATA" { base.join("Programs").join("Git") } else { base.join("Git") });
            }
        }
        roots
            .into_iter()
            .map(|r| r.join("bin").join("bash.exe"))
            .find(|p| p.is_file())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "bash.exe".into())
    }

    pub fn session_of(_: i32) -> Option<i32> {
        None
    }

    /// A string value from the registry, `%VAR%`s expanded (RegGetValueW does that for
    /// REG_EXPAND_SZ). None when it isn't there.
    pub fn reg_string(root: windows_sys::Win32::System::Registry::HKEY, key: &str, name: &str) -> Option<String> {
        use windows_sys::Win32::System::Registry::{RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW};
        let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let (key, name) = (wide(key), wide(name));
        // An environment variable is at most 32767 characters.
        let mut buf = vec![0u16; 32768];
        let mut bytes = (buf.len() * 2) as u32;
        let r = unsafe {
            RegGetValueW(root, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut bytes)
        };
        if r != 0 {
            return None;
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..n]))
    }
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn user_path_is_the_registrys_first_then_ours_once_each() {
        let path = super::user_path();
        let dirs: Vec<String> = path.split(';').map(|d| d.trim_end_matches('\\').to_ascii_lowercase()).collect();
        let windir = std::env::var("SystemRoot").unwrap().to_ascii_lowercase();
        assert!(dirs.contains(&format!(r"{windir}\system32")), "{path}");
        // expanded, not %SystemRoot%
        assert!(!path.contains('%'), "{path}");
        let mut unique = dirs.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), dirs.len(), "{path}");
        // everything midnad's own PATH had is still there
        for d in std::env::var("PATH").unwrap().split(';').filter(|d| !d.trim().is_empty()) {
            assert!(dirs.contains(&d.trim().trim_end_matches('\\').to_ascii_lowercase()), "{d} missing from {path}");
        }
    }
}
