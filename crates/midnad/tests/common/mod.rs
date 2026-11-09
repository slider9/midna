//! Test harness: an in-process midnad on a temp MIDNA_HOME. This test binary is configured as
//! the "GUI", so plain connections are human; `agent()` connections downgrade themselves.
#![allow(dead_code)]
use midna_proto::{Client, ClientError, RpcError};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static N: AtomicU32 = AtomicU32::new(0);

pub struct TestDaemon {
    handle: Option<midnad::Handle>,
    pub home: PathBuf,
}

impl TestDaemon {
    pub fn start() -> TestDaemon {
        TestDaemon::start_with(|_| {})
    }

    /// Like `start`, with the config adjusted first (e.g. `agent_bin`).
    pub fn start_with(f: impl FnOnce(&mut midnad::Config)) -> TestDaemon {
        #[cfg(windows)]
        git_on_path();
        // Short path: Unix socket paths are limited to 104 bytes on macOS (108 on Windows).
        let home = tmp().join(format!("midna-t-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&home);
        let mut cfg = midnad::Config::for_home(home.clone());
        cfg.app_path = Some(std::env::current_exe().unwrap().to_string_lossy().into_owned());
        f(&mut cfg);
        let handle = midnad::start(cfg).expect("start daemon");
        TestDaemon { handle: Some(handle), home }
    }

    /// Stop midnad and start a new one on the same home (state.json, the event log).
    pub fn restart(&mut self) {
        if let Some(h) = self.handle.take() {
            h.shutdown();
        }
        let mut cfg = midnad::Config::for_home(self.home.clone());
        cfg.app_path = Some(std::env::current_exe().unwrap().to_string_lossy().into_owned());
        self.handle = Some(midnad::start(cfg).expect("restart daemon"));
    }

    pub fn daemon(&self) -> std::sync::Arc<midnad::daemon::Daemon> {
        self.handle.as_ref().expect("running").daemon.clone()
    }

    pub fn socket(&self) -> PathBuf {
        self.home.join("midnad.sock")
    }

    pub fn human(&self) -> Client {
        let mut c = Client::connect(self.socket()).expect("connect");
        c.set_caller(None);
        c
    }

    pub fn agent(&self, session: Option<&str>) -> Client {
        Client::connect(self.socket()).expect("connect").as_agent(session.map(str::to_string))
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            h.shutdown();
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

pub fn call(c: &mut Client, m: &str, p: Value) -> Value {
    c.call_value(m, p).unwrap_or_else(|e| panic!("{m} failed: {e}"))
}

pub fn call_err(c: &mut Client, m: &str, p: Value) -> RpcError {
    match c.call_value(m, p) {
        Err(ClientError::Rpc(e)) => e,
        other => panic!("{m}: expected an RPC error, got {other:?}"),
    }
}

/// Poll `f` until it returns Some, or panic after `secs`.
pub fn wait_for<T>(secs: u64, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let t0 = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        if t0.elapsed() > Duration::from_secs(secs) {
            panic!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Open a plain sh session in [`tmp`] (no login shell, no user dotfiles).
pub fn open_sh(c: &mut Client) -> String {
    let s = call(c, "session.open", json!({ "kind": "shell", "cwd": tmp(), "command": [sh()] }));
    s["id"].as_str().unwrap().to_string()
}

/// The folder tests open terminals in and keep scratch files in. Unix: `/tmp`, as written, so
/// the paths tests compare stay what they always were. Windows: the user's temp folder,
/// canonical (`paths::canonical`, the form midnad stores cwds in). Either way it's short, so a
/// home under it keeps `midnad.sock` inside the 104/108-byte socket path limit.
pub fn tmp() -> PathBuf {
    #[cfg(unix)]
    return PathBuf::from("/tmp");
    #[cfg(windows)]
    return midna_proto::paths::canonical(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
}

/// [`tmp`] as a string, for `format!` and JSON.
pub fn tmp_s() -> String {
    tmp().to_string_lossy().into_owned()
}

/// A POSIX sh for terminals and `sh -c`. Unix: `/bin/sh`. Windows: Git for Windows' own
/// `usr\bin\sh.exe` (its 8.3 path, see [`short`]), so a terminal is still named "sh" and reads
/// no dotfiles. Not Git's `bin\sh.exe`: that launcher starts an interactive bash, with
/// `/etc/bash.bashrc`, the user's `.bashrc` and a prompt that sets the title.
pub fn sh() -> String {
    #[cfg(unix)]
    return "/bin/sh".into();
    #[cfg(windows)]
    {
        git_on_path();
        posix_bin("sh")
    }
}

/// A path as one word typed at [`sh`]: as is on Unix (what the tests always typed), single-quoted
/// on Windows, where its backslashes would be escapes.
pub fn sh_word(p: impl AsRef<std::path::Path>) -> String {
    let p = p.as_ref().display().to_string();
    if cfg!(windows) { format!("'{p}'") } else { p }
}

/// Windows stands in for `nc -U` (Git Bash has none) with this test binary: copied under
/// another name, so midnad doesn't take it for the GUI, and run as `EXE --exact
/// helper_send_line --ignored --nocapture`. A file that uses it declares
/// `#[test] #[ignore] fn helper_send_line() { common::send_line() }`.
pub fn send_line_exe() -> std::path::PathBuf {
    static EXE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    EXE.get_or_init(|| {
        let me = std::env::current_exe().unwrap();
        // Next to the original, where any DLL it needs is found too.
        let copy = me.with_file_name(format!("{}-nc{}", me.file_stem().unwrap().to_string_lossy(), std::env::consts::EXE_SUFFIX));
        if let Err(e) = std::fs::copy(&me, &copy) {
            assert!(copy.is_file(), "copy {} to {}: {e}", me.display(), copy.display());
        }
        copy
    })
    .clone()
}

/// The `nc -U` stand-in's body: sends the line `MIDNA_TEST_LINE` to `MIDNA_TEST_SOCKET` (else
/// `MIDNA_SOCKET`) and prints the reply. Does nothing in a normal test run.
pub fn send_line() {
    let Ok(line) = std::env::var("MIDNA_TEST_LINE") else { return };
    let sock = std::env::var("MIDNA_TEST_SOCKET").or_else(|_| std::env::var("MIDNA_SOCKET")).unwrap();
    let mut s = midna_proto::ipc::UnixStream::connect(sock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    std::io::Write::write_all(&mut s, format!("{line}\n").as_bytes()).unwrap();
    let mut reply = String::new();
    let _ = std::io::BufRead::read_line(&mut std::io::BufReader::new(s), &mut reply);
    println!("{reply}");
}

/// A POSIX tool as a program to start (`cat`, `echo`, `sleep`): `/bin/{name}` on Unix, Git for
/// Windows' `usr\bin\{name}.exe` on Windows.
pub fn posix_bin(name: &str) -> String {
    #[cfg(unix)]
    return format!("/bin/{name}");
    #[cfg(windows)]
    {
        // posix_sh is <git>\bin\bash.exe.
        let bash = std::path::PathBuf::from(midnad::sys::posix_sh());
        match bash.parent().and_then(|b| b.parent()) {
            Some(git) if bash.is_absolute() => short(git.join("usr").join("bin").join(format!("{name}.exe"))).to_string_lossy().into_owned(),
            _ => format!("{name}.exe"),
        }
    }
}

/// Python 3: `python3` on Unix; `python` on Windows, the name python.org's installer puts on PATH.
pub fn python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

/// A Python script to start as a program (a fake agent). Unix: the script itself, by its
/// shebang. Windows: a `.cmd` in [`tmp`] that runs it with [`python`].
pub fn python_bin(script: &str) -> String {
    if cfg!(unix) {
        return script.into();
    }
    let stem = std::path::Path::new(script).file_stem().unwrap().to_string_lossy().into_owned();
    let cmd = tmp().join(format!("midna-py-{}-{stem}.cmd", std::process::id()));
    std::fs::write(&cmd, format!("@{} \"{script}\" %*\r\n", python())).unwrap();
    cmd.to_string_lossy().into_owned()
}

/// A runnable sh script at `path` (a fake agent, status script or command) that runs `body`,
/// returned as the program to start.
///
/// Unix: `path` itself, `#!/bin/sh` and mode 0755. Windows starts nothing by its shebang, so
/// the sh goes to `path` with a `.sh` extension and a `.cmd` beside it runs that with Git
/// Bash; the `.cmd` is returned. Its arguments pass through cmd.exe, so a newline ends them,
/// `%VAR%` expands, and an unquoted `'` (one with no space or `"` in its argument) confuses
/// Git Bash's own parsing of the command line.
pub fn script(path: impl AsRef<std::path::Path>, body: &str) -> String {
    let path = path.as_ref();
    #[cfg(unix)]
    {
        std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }
    #[cfg(windows)]
    {
        let (sh, cmd) = (path.with_extension("sh"), path.with_extension("cmd"));
        std::fs::write(&sh, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::write(&cmd, format!("@\"{}\" \"{}\" %*\r\n", midnad::sys::posix_sh(), sh.display())).unwrap();
        cmd.to_string_lossy().into_owned()
    }
}

/// Remove what [`script`] wrote.
pub fn remove_script(path: impl AsRef<std::path::Path>) {
    let path = path.as_ref();
    let _ = std::fs::remove_file(path);
    #[cfg(windows)]
    for ext in ["sh", "cmd"] {
        let _ = std::fs::remove_file(path.with_extension(ext));
    }
}

pub fn read(c: &mut Client, id: &str) -> String {
    call(c, "session.read", json!({ "id": id, "lines": 200 }))["text"].as_str().unwrap().to_string()
}

/// Windows: `p` in its 8.3 form (`C:\PROGRA~1\Git\…`), which has no spaces. midnad runs a
/// one-word command that contains whitespace through the login shell, as a command line, and
/// Git lives under `Program Files`. Unchanged where the volume keeps no short names.
#[cfg(windows)]
fn short(p: std::path::PathBuf) -> std::path::PathBuf {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetShortPathNameW(long: *const u16, short: *mut u16, len: u32) -> u32;
    }
    let wide: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut buf = vec![0u16; 1024];
    let n = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if n == 0 || n >= buf.len() {
        return p;
    }
    std::path::PathBuf::from(std::ffi::OsString::from_wide(&buf[..n]))
}

/// Windows: put Git's `usr\bin` first on this process's PATH (once), so [`sh`] finds `sleep`,
/// `cat` and the rest: a terminal's PATH ends with midnad's own. Run from Git Bash it's there
/// already. And make [`sh`] midnad's login shell (`MIDNA_SHELL`), which runs one-line commands
/// (`run_command`, a one-string `command`): the tests write those for a POSIX shell, as on Unix,
/// and otherwise the login shell is whatever `SHELL` the test runner has (Git Bash's bash), or
/// PowerShell. Windows keeps the environment behind a lock of its own, so setting it while other
/// threads run is sound there.
#[cfg(windows)]
pub fn git_on_path() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        unsafe { std::env::set_var("MIDNA_SHELL", posix_bin("sh")) };
        let usr_bin = std::path::PathBuf::from(posix_bin("sh")).parent().unwrap().to_path_buf();
        let key = |p: &std::path::Path| midna_proto::paths::canonical(p).map(|c| c.to_string_lossy().to_lowercase()).ok();
        let path = std::env::var_os("PATH").unwrap_or_default();
        if std::env::split_paths(&path).any(|p| key(&p).is_some() && key(&p) == key(&usr_bin)) {
            return;
        }
        let joined = std::env::join_paths(std::iter::once(usr_bin).chain(std::env::split_paths(&path))).unwrap();
        unsafe { std::env::set_var("PATH", joined) };
    });
}
