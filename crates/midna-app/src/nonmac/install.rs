//! First-launch install off macOS. `src/install.rs` registers a LaunchAgent with SMAppService
//! and links the CLI into `~/.local/bin`; there's no installer or login item here yet, so this
//! build always runs in dev mode: if midnad isn't answering, the `midnad` next to this
//! executable is started directly (opt out: `MIDNA_NO_SPAWN=1`). Same API as `src/install.rs`,
//! so `lifecycle` doesn't care which one it has; the installed-only calls report that they
//! aren't available.
//!
//! All functions here block; `lifecycle` runs them off the UI thread.
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

const NOT_YET: &str = "not available on this platform yet";

#[derive(Clone, Debug, PartialEq)]
pub enum Mode {
    Dev,
    #[allow(dead_code)]
    Installed { bundle: PathBuf },
}

/// Always dev mode: there's no installed layout to detect yet.
pub fn mode() -> Mode {
    Mode::Dev
}

#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub enum LoginItem {
    /// Dev mode: midnad is started directly.
    Dev,
    Checking,
    Enabled,
    /// Registered, but the human must switch it on in System Settings ▸ Login Items.
    RequiresApproval,
    NotRegistered,
    /// macOS 12: a plain `~/Library/LaunchAgents` plist.
    Legacy,
    Failed(String),
}

pub fn read_plist(_path: &Path) -> Option<Value> {
    None
}

/// Not macOS.
pub fn macos_version() -> (u32, u32) {
    (0, 0)
}

pub fn login_status(_bundle: &Path) -> LoginItem {
    LoginItem::Failed(NOT_YET.into())
}

pub fn register(_bundle: &Path) -> LoginItem {
    LoginItem::Failed(NOT_YET.into())
}

pub fn unregister(_bundle: &Path) -> Result<(), String> {
    Err(NOT_YET.into())
}

pub fn open_login_items() {}

pub fn kickstart(_bundle: &Path) {}

/// The midnad executable's file name.
fn exe_name(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// `bin/current/midnad` under MIDNA_HOME (what upgrades run).
pub fn current_daemon(home: &Path) -> PathBuf {
    home.join("bin").join("current").join(exe_name("midnad"))
}

pub fn install_daemon(_bundle: &Path, _home: &Path) -> Result<PathBuf, String> {
    Err(NOT_YET.into())
}

pub fn socket_alive(socket: &Path) -> bool {
    midna_proto::ipc::UnixStream::connect(socket).is_ok()
}

/// Dev mode: start the `midnad` next to this executable, detached from this process (it
/// outlives the app, as the daemon must), and don't wait for it.
pub fn spawn_dev_daemon() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let midnad = exe.with_file_name(exe_name("midnad"));
    if !midnad.is_file() {
        return Err(format!("no midnad next to {}", exe.display()));
    }
    let mut cmd = Command::new(&midnad);
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    detach(&mut cmd);
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// No console window, and not in this process's group (so it survives the app and ⌃C).
#[cfg(windows)]
fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS};
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

// ------------------------------------------------------------------ midna CLI for agents

#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub enum CliLink {
    Dev,
    /// `link` -> `target`, and `link`'s directory is on PATH.
    Linked {
        link: PathBuf,
    },
    /// The link dir isn't on the login shell's PATH; Settings offers to install it anyway.
    NotOnPath {
        dir: PathBuf,
    },
    /// Something that isn't ours already lives at `link`; left alone.
    Conflict {
        link: PathBuf,
    },
    Failed(String),
}

/// Midna Dev: `<its home>/cli`. Else `MIDNA_CLI_LINK_DIR` (tests), else `~/.local/bin`.
pub fn cli_link_dir() -> PathBuf {
    if let Some(h) = midna_proto::paths::DEV_HOME {
        return Path::new(h).join("cli");
    }
    std::env::var_os("MIDNA_CLI_LINK_DIR").map(PathBuf::from).unwrap_or_else(|| midna_proto::paths::user_home().join(".local").join("bin"))
}

/// The link dir was chosen for us (Midna Dev, tests): link there whether or not it's on PATH.
pub fn cli_link_dir_pinned() -> bool {
    midna_proto::paths::is_dev() || std::env::var_os("MIDNA_CLI_LINK_DIR").is_some()
}

/// The user's PATH. A Windows app inherits it from Explorer, with no login shell to ask.
pub fn login_shell_path() -> Option<String> {
    std::env::var("PATH").ok()
}

/// Linking the CLI needs a symlink (a privilege on Windows) or a copy that upgrades keep in
/// step; neither is done yet.
pub fn link_cli(_home: &Path, _shell_path: Option<&str>, _force_dir: bool) -> CliLink {
    CliLink::Failed(NOT_YET.into())
}

// ------------------------------------------------------------------ relaunch

/// Start `app` (the executable) again once this process has exited. The caller quits right
/// after.
#[cfg(windows)]
pub fn relaunch_after_exit(app: &Path) -> Result<(), String> {
    let script = format!(
        "Wait-Process -Id {} -Timeout 30 -ErrorAction SilentlyContinue; Start-Process -FilePath '{}'",
        std::process::id(),
        app.display().to_string().replace('\'', "''")
    );
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    detach(&mut cmd);
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(not(windows))]
pub fn relaunch_after_exit(_app: &Path) -> Result<(), String> {
    Err(NOT_YET.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_dev_with_the_daemon_beside_the_app() {
        assert_eq!(mode(), Mode::Dev);
        let d = current_daemon(Path::new("home"));
        assert!(d.starts_with(Path::new("home").join("bin").join("current")));
        assert_eq!(d.file_stem().unwrap(), "midnad");
    }
}
