//! First-launch install off macOS. Same API as `src/install.rs`, so `lifecycle` doesn't care
//! which one it has.
//!
//! Windows: a packaged build (`packaging/build-windows.sh`) is a folder, by default
//! `%LOCALAPPDATA%\Programs\Midna`, holding `midna-app.exe`, `midnad.exe`, `midna.exe` and
//! [`BUNDLE_MARKER`]. Run from there, the app installs midnad into `MIDNA_HOME\bin` (`midnad
//! --install-self`), starts it at login with a value under `HKCU\…\Run` (`midna-app.exe
//! --start-daemon`, which starts it with no window and exits), copies the `midna` CLI into
//! `~\.local\bin` and adds itself to the Start menu and Settings ▸ Apps. Anything else (a `cargo` build, Linux) is dev mode: if midnad isn't
//! answering, the `midnad` next to this executable is started directly (opt out:
//! `MIDNA_NO_SPAWN=1`), and the installed-only calls report that they aren't available.
//!
//! All functions here block; `lifecycle` runs them off the UI thread.
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(not(windows))]
const NOT_YET: &str = "not available on this platform yet";

/// A packaged Windows build has this file next to `midna-app.exe`.
pub const BUNDLE_MARKER: &str = "midna-bundle.txt";

#[derive(Clone, Debug, PartialEq)]
pub enum Mode {
    Dev,
    #[allow(dead_code)]
    Installed { bundle: PathBuf },
}

/// Installed when this executable is in a packaged build and `MIDNA_DEV` isn't set.
pub fn mode() -> Mode {
    if std::env::var("MIDNA_DEV").is_ok_and(|v| !v.is_empty() && v != "0") {
        return Mode::Dev;
    }
    match bundle_of(&std::env::current_exe().unwrap_or_default()) {
        Some(bundle) => Mode::Installed { bundle },
        None => Mode::Dev,
    }
}

/// The packaged build `exe` is in (its folder, with [`BUNDLE_MARKER`] in it). Windows only.
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    (cfg!(windows) && dir.join(BUNDLE_MARKER).is_file()).then(|| dir.to_path_buf())
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

/// Not macOS. (Windows reads its own version: `updater::windows_version`.)
#[cfg_attr(windows, allow(dead_code))]
pub fn macos_version() -> (u32, u32) {
    (0, 0)
}

#[cfg(not(windows))]
pub fn login_status(_bundle: &Path) -> LoginItem {
    LoginItem::Failed(NOT_YET.into())
}

#[cfg(not(windows))]
pub fn register(_bundle: &Path) -> LoginItem {
    LoginItem::Failed(NOT_YET.into())
}

#[cfg(not(windows))]
pub fn unregister(_bundle: &Path) -> Result<(), String> {
    Err(NOT_YET.into())
}

#[cfg(not(windows))]
pub fn open_login_items() {}

#[cfg(not(windows))]
pub fn kickstart(_bundle: &Path) {}

#[cfg(not(windows))]
pub fn install_daemon(_bundle: &Path, _home: &Path) -> Result<PathBuf, String> {
    Err(NOT_YET.into())
}

/// The login entry: a value under `HKCU\…\Run` that runs `midna-app.exe --start-daemon`.
/// Settings ▸ Apps ▸ Startup and Task Manager's Startup tab list it, and switching it off there
/// is kept in `StartupApproved\Run` (the Run value stays).
#[cfg(windows)]
mod login {
    use super::*;
    use crate::winreg;

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

    /// The Run value's name. Midna Dev has its own, so it never touches the installed Midna's.
    pub fn name() -> &'static str {
        if midna_proto::paths::is_dev() { "Midna Dev" } else { "Midna" }
    }

    pub fn command(bundle: &Path) -> String {
        format!("\"{}\" --start-daemon", bundle.join(exe_name("midna-app")).display())
    }

    /// Switched off in Startup apps: the first byte of its `StartupApproved` value is odd (2 and
    /// 6 are on, 3 and 7 off).
    pub fn switched_off() -> bool {
        winreg::get_binary(APPROVED, name()).and_then(|b| b.first().copied()).is_some_and(|b| b & 1 == 1)
    }

    pub fn status(bundle: &Path) -> LoginItem {
        match winreg::get_string(RUN, name()) {
            // Another copy's (an older install somewhere else): `register` points it here.
            Some(cmd) if cmd != command(bundle) => LoginItem::NotRegistered,
            Some(_) if switched_off() => LoginItem::RequiresApproval,
            Some(_) => LoginItem::Enabled,
            None => LoginItem::NotRegistered,
        }
    }

    pub fn register(bundle: &Path) -> Result<(), String> {
        winreg::set_string(RUN, Some(name()), &command(bundle)).map_err(|e| format!("{RUN}\\{}: {e}", name()))
    }

    pub fn unregister() -> Result<(), String> {
        winreg::delete(RUN, name()).map_err(|e| format!("{RUN}\\{}: {e}", name()))
    }
}

#[cfg(windows)]
pub fn login_status(bundle: &Path) -> LoginItem {
    login::status(bundle)
}

/// Write the login entry, and start midnad now if it isn't answering (as launchd does when a
/// LaunchAgent is registered). A login entry switched off in Startup apps stays off.
#[cfg(windows)]
pub fn register(bundle: &Path) -> LoginItem {
    if let Err(e) = login::register(bundle) {
        return LoginItem::Failed(e);
    }
    let status = login::status(bundle);
    if status == LoginItem::Enabled && !socket_alive(&midna_proto::paths::socket_path()) {
        kickstart(bundle);
    }
    status
}

#[cfg(windows)]
pub fn unregister(_bundle: &Path) -> Result<(), String> {
    login::unregister()
}

/// Settings ▸ Apps ▸ Startup, where a login entry that was switched off is switched on.
#[cfg(windows)]
pub fn open_login_items() {
    crate::opener::open("ms-settings:startupapps");
}

/// Start the installed midnad (`bin\current`'s), with no window, and wait until it answers or
/// gives up (midnad waits up to 5 s for its socket).
#[cfg(windows)]
pub fn kickstart(_bundle: &Path) {
    if let Err(e) = start_daemon() {
        crate::lifecycle::log(&format!("start midnad: {e}"));
    }
}

#[cfg(windows)]
pub fn start_daemon() -> Result<(), String> {
    let home = midna_proto::paths::midna_home();
    let midnad = current_daemon(&home);
    if !midnad.is_file() {
        return Err(format!("no installed midnad at {}", midnad.display()));
    }
    let mut cmd = Command::new(&midnad);
    cmd.arg("--home").arg(&home).arg("--socket").arg(midna_proto::paths::socket_path());
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    detach(&mut cmd);
    match cmd.status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("{} exited with {s}; see {}", midnad.display(), home.join("midnad.log").display())),
        Err(e) => Err(format!("{}: {e}", midnad.display())),
    }
}

/// The newer ConPTY's files, which go with midnad (midnad's `pty::CONPTY_FILES`).
#[cfg(windows)]
const CONPTY_FILES: [&str; 2] = ["conpty.dll", "OpenConsole.exe"];

/// Install the bundle's midnad (and the `midna` CLI and ConPTY files beside it) into
/// `home\bin\<version>-<hash>` and point `bin\current` at it (`midnad --install-self`), unless
/// that build is already there.
#[cfg(windows)]
pub fn install_daemon(bundle: &Path, home: &Path) -> Result<PathBuf, String> {
    let src = bundle.join(exe_name("midnad"));
    if !src.is_file() {
        return Err(format!("no midnad in {}", bundle.display()));
    }
    let current = current_daemon(home);
    let beside = |name: &str| same_file(&bundle.join(name), &current.with_file_name(name));
    if same_file(&src, &current) && beside(&exe_name("midna")) && CONPTY_FILES.iter().all(|f| !bundle.join(f).is_file() || beside(f)) {
        return Ok(current);
    }
    let mut cmd = Command::new(&src);
    cmd.arg("--install-self").arg("--home").arg(home);
    cmd.stdin(std::process::Stdio::null());
    detach(&mut cmd);
    let out = cmd.output().map_err(|e| format!("{}: {e}", src.display()))?;
    if !out.status.success() {
        return Err(format!("midnad --install-self: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    crate::lifecycle::log(String::from_utf8_lossy(&out.stdout).trim());
    Ok(current_daemon(home))
}

/// The Start menu entry: `Midna.lnk` (Midna Dev: `Midna Dev.lnk`) in the user's Start menu
/// Programs folder.
#[cfg(windows)]
pub fn start_menu_entry() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA").filter(|v| !v.is_empty())?;
    Some(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs").join(format!("{}.lnk", login::name())))
}

/// Add this build to the Start menu, or point an entry that runs another copy (one since moved
/// or deleted) at this one. An entry already pointing here is left as it is.
#[cfg(windows)]
pub fn add_to_start_menu(bundle: &Path) -> Result<(), String> {
    let lnk = start_menu_entry().ok_or("no %APPDATA%")?;
    let exe = bundle.join(exe_name("midna-app"));
    if points_at(&lnk, &exe) {
        return Ok(());
    }
    midna_proto::paths::guard_write(&lnk).map_err(|e| e.to_string())?;
    if let Some(dir) = lnk.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    shortcut(&lnk, &exe, bundle).map_err(|e| format!("{}: {e}", lnk.display()))
}

/// Does the shortcut at `lnk` run `exe`?
#[cfg(windows)]
fn points_at(lnk: &Path, exe: &Path) -> bool {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IPersistFile, STGM_READ};
    use windows::Win32::UI::Shell::{IShellLinkW, SLGP_RAWPATH, ShellLink};
    use windows::core::{HSTRING, Interface};
    if !lnk.is_file() {
        return false;
    }
    // SAFETY: COM on this thread, then plain interface calls into a buffer we own.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let target = (|| -> windows::core::Result<String> {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.cast::<IPersistFile>()?.Load(&HSTRING::from(lnk.as_os_str()), STGM_READ)?;
            let mut buf = [0u16; 1024];
            link.GetPath(&mut buf, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)?;
            Ok(String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())]))
        })();
        target.is_ok_and(|t| midna_proto::paths::same_path(&t, &exe.to_string_lossy()))
    }
}

#[cfg(windows)]
fn shortcut(lnk: &Path, exe: &Path, dir: &Path) -> windows::core::Result<()> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IPersistFile};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{HSTRING, Interface};
    // SAFETY: COM on this thread (already initialized is fine), then plain interface calls.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(exe.as_os_str()))?;
        link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
        link.SetDescription(&HSTRING::from("A terminal built for working with AI agents"))?;
        link.cast::<IPersistFile>()?.Save(&HSTRING::from(lnk.as_os_str()), true)
    }
}

/// Take this build's Start menu entry away (`--uninstall`).
#[cfg(windows)]
pub fn remove_from_start_menu() -> Option<PathBuf> {
    let lnk = start_menu_entry()?;
    std::fs::remove_file(&lnk).ok().map(|()| lnk)
}

/// The entry in Settings ▸ Apps (and Control Panel's Programs and Features): a key under
/// `HKCU\…\Uninstall` named for the build's bundle id, so Midna Dev's never touches the
/// installed Midna's. Its Uninstall button runs `midna-app.exe --uninstall`; there's nothing to
/// modify or repair.
#[cfg(windows)]
mod apps_entry {
    use super::*;
    use crate::winreg;

    pub fn key() -> String {
        let id = if midna_proto::paths::is_dev() { format!("{}.dev", midna_proto::paths::BUNDLE_ID) } else { midna_proto::paths::BUNDLE_ID.to_string() };
        format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{id}")
    }

    pub enum Data {
        Text(String),
        Number(u32),
    }

    /// What the entry holds for the build in `bundle`, `size_kb` big.
    pub fn values(bundle: &Path, size_kb: u32) -> Vec<(&'static str, Data)> {
        let exe = bundle.join(exe_name("midna-app"));
        vec![
            ("DisplayName", Data::Text(super::login::name().into())),
            ("DisplayVersion", Data::Text(midna_proto::VERSION.into())),
            ("Publisher", Data::Text("Morgan Hunt".into())),
            ("DisplayIcon", Data::Text(format!("{},0", exe.display()))),
            ("InstallLocation", Data::Text(bundle.display().to_string())),
            ("UninstallString", Data::Text(format!("\"{}\" --uninstall", exe.display()))),
            ("QuietUninstallString", Data::Text(format!("\"{}\" --uninstall", exe.display()))),
            ("NoModify", Data::Number(1)),
            ("NoRepair", Data::Number(1)),
            ("EstimatedSize", Data::Number(size_kb)),
        ]
    }

    pub fn write(key: &str, values: Vec<(&'static str, Data)>) -> std::io::Result<()> {
        for (name, data) in values {
            match data {
                Data::Text(s) => winreg::set_string(key, Some(name), &s)?,
                Data::Number(n) => winreg::set_dword(key, name, n)?,
            }
        }
        Ok(())
    }

    /// The size of the files in `dir`, in KB.
    pub fn size_kb(dir: &Path) -> u32 {
        fn bytes(dir: &Path) -> u64 {
            let sizes = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| match e.metadata() {
                Ok(m) if m.is_dir() => bytes(&e.path()),
                Ok(m) => m.len(),
                Err(_) => 0,
            });
            sizes.sum()
        }
        u32::try_from(bytes(dir).div_ceil(1024)).unwrap_or(u32::MAX)
    }
}

/// Add this build to Settings ▸ Apps, or bring its entry up to date (version, folder).
#[cfg(windows)]
pub fn add_to_apps(bundle: &Path) -> Result<(), String> {
    let key = apps_entry::key();
    apps_entry::write(&key, apps_entry::values(bundle, apps_entry::size_kb(bundle))).map_err(|e| format!("{key}: {e}"))
}

/// Take this build's entry out of Settings ▸ Apps (`--uninstall`). True when there was one.
#[cfg(windows)]
pub fn remove_from_apps() -> Result<bool, String> {
    let key = apps_entry::key();
    let had = crate::winreg::get_string(&key, "UninstallString").is_some();
    crate::winreg::delete_key(&key).map(|()| had).map_err(|e| format!("{key}: {e}"))
}

/// Same size and bytes.
#[cfg(windows)]
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) if x.len() == y.len() => std::fs::read(a).ok().is_some_and(|a| std::fs::read(b).ok().is_some_and(|b| a == b)),
        _ => false,
    }
}

/// The executable's file name (`.exe` on Windows).
fn exe_name(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// The installed midnad (what upgrades and the login entry run): `bin/current/midnad`, or on
/// Windows `midnad.exe` in the folder that the `bin\current` file names.
#[cfg(not(windows))]
pub fn current_daemon(home: &Path) -> PathBuf {
    home.join("bin").join("current").join(exe_name("midnad"))
}

#[cfg(windows)]
pub fn current_daemon(home: &Path) -> PathBuf {
    let root = home.join("bin");
    let name = std::fs::read_to_string(root.join("current")).unwrap_or_default();
    root.join(name.trim()).join(exe_name("midnad"))
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

/// A console with no window (midnad's console programs share it instead of each flashing a
/// window of its own), and not in this process's group (so it survives the app and ⌃C).
#[cfg(windows)]
fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
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

#[cfg(not(windows))]
pub fn link_cli(_home: &Path, _shell_path: Option<&str>, _force_dir: bool) -> CliLink {
    CliLink::Failed(NOT_YET.into())
}

/// Where the CLI goes: `midna.exe` in [`cli_link_dir`].
pub fn cli_link() -> PathBuf {
    cli_link_dir().join(exe_name("midna"))
}

/// Windows: a symlink needs a privilege (or Developer Mode), so the CLI is a copy of the
/// installed `midna.exe`, made again whenever that one changes. A copy keeps its source's time,
/// so one of a different size or time is out of date. A running copy can't be replaced, only
/// renamed: it's moved aside, and removed next time. `force_dir`: install even when the
/// folder isn't on PATH (the human asked in Settings).
#[cfg(windows)]
pub fn link_cli(home: &Path, shell_path: Option<&str>, force_dir: bool) -> CliLink {
    let dir = cli_link_dir();
    let on_path = cli_link_dir_pinned() || shell_path.is_some_and(|p| dir_on_path(&dir, p));
    copy_cli(&current_daemon(home).with_file_name(exe_name("midna")), &dir, on_path, force_dir)
}

/// Copy `src` to `dir\midna.exe` (see [`link_cli`]).
#[cfg(windows)]
fn copy_cli(src: &Path, dir: &Path, on_path: bool, force_dir: bool) -> CliLink {
    let link = dir.join(exe_name("midna"));
    let Some(want) = std::fs::metadata(src).ok().filter(|m| m.is_file()) else {
        return CliLink::Failed(format!("no installed CLI at {}", src.display()));
    };
    let reply = |link: PathBuf| if on_path { CliLink::Linked { link } } else { CliLink::NotOnPath { dir: dir.to_path_buf() } };
    if let Ok(have) = std::fs::metadata(&link) {
        if have.len() == want.len() && have.modified().ok() == want.modified().ok() {
            return reply(link);
        }
        if !is_our_cli(&link) {
            return CliLink::Conflict { link };
        }
    }
    if !on_path && !force_dir {
        return CliLink::NotOnPath { dir: dir.to_path_buf() };
    }
    if let Err(e) = midna_proto::paths::guard_write(&link).and_then(|()| std::fs::create_dir_all(dir)) {
        return CliLink::Failed(format!("{}: {e}", dir.display()));
    }
    let name = exe_name("midna");
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten().filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with(&name) && n.ends_with(".old"))) {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let tmp = dir.join(format!("{name}.{}.tmp", std::process::id()));
    if let Err(e) = std::fs::copy(src, &tmp) {
        let _ = std::fs::remove_file(&tmp);
        return CliLink::Failed(format!("{}: {e}", tmp.display()));
    }
    let aside = dir.join(format!("{name}.{}.old", std::process::id()));
    if link.exists() && std::fs::rename(&link, &aside).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return CliLink::Failed(format!("{} is in use", link.display()));
    }
    if let Err(e) = std::fs::rename(&tmp, &link) {
        let _ = std::fs::rename(&aside, &link);
        let _ = std::fs::remove_file(&tmp);
        return CliLink::Failed(e.to_string());
    }
    let _ = std::fs::remove_file(&aside);
    reply(link)
}

/// Remove our CLI copy (`--uninstall`). Something else at that path is left alone.
#[cfg(windows)]
pub fn unlink_cli() -> Option<PathBuf> {
    let link = cli_link();
    (is_our_cli(&link) && std::fs::remove_file(&link).is_ok()).then_some(link)
}

/// Is `dir` one of `path`'s folders (`;`-separated, compared without case)?
#[cfg(windows)]
fn dir_on_path(dir: &Path, path: &str) -> bool {
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).replace('/', "\\").to_lowercase();
    let want = norm(dir);
    std::env::split_paths(path).any(|p| !p.as_os_str().is_empty() && norm(&p) == want)
}

/// Is the file at `link` a midna CLI (an earlier copy of ours)? Every build names the
/// installed Midna's bundle id, which nothing else does.
#[cfg(windows)]
fn is_our_cli(link: &Path) -> bool {
    let id = midna_proto::paths::BUNDLE_ID.as_bytes();
    std::fs::read(link).is_ok_and(|b| b.windows(id.len()).any(|w| w == id))
}

// ------------------------------------------------------------------ relaunch

/// Start `app` (the executable, or the packaged build's folder) again once this process has
/// exited. The caller quits right after.
#[cfg(windows)]
pub fn relaunch_after_exit(app: &Path) -> Result<(), String> {
    let exe = if app.is_dir() { app.join(exe_name("midna-app")) } else { app.to_path_buf() };
    let app = exe.as_path();
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
    fn a_cargo_build_is_dev_mode() {
        assert_eq!(mode(), Mode::Dev);
        assert_eq!(bundle_of(&std::env::current_exe().unwrap()), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn the_daemon_is_under_bin_current() {
        let d = current_daemon(Path::new("home"));
        assert!(d.starts_with(Path::new("home").join("bin").join("current")));
        assert_eq!(d.file_stem().unwrap(), "midnad");
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("midna-install-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(windows)]
    #[test]
    fn a_packaged_build_has_the_marker_and_its_daemon_is_what_bin_current_names() {
        let d = scratch("bundle");
        let exe = d.join("midna-app.exe");
        assert_eq!(bundle_of(&exe), None);
        std::fs::write(d.join(BUNDLE_MARKER), "").unwrap();
        assert_eq!(bundle_of(&exe), Some(d.clone()));
        assert_eq!(login::command(&d), format!("\"{}\" --start-daemon", exe.display()));
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::fs::write(d.join("bin").join("current"), "0.9.0-abcd1234\n").unwrap();
        assert_eq!(current_daemon(&d), d.join("bin").join("0.9.0-abcd1234").join("midnad.exe"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn a_start_menu_shortcut_runs_the_app_from_its_folder() {
        let d = scratch("lnk");
        let (bundle, other) = (d.join("Midna Dev"), d.join("Elsewhere"));
        let lnk = d.join("Midna Dev.lnk");
        assert!(!points_at(&lnk, &bundle.join("midna-app.exe")));
        shortcut(&lnk, &bundle.join("midna-app.exe"), &bundle).unwrap();
        assert!(points_at(&lnk, &bundle.join("midna-app.exe")));
        assert!(!points_at(&lnk, &other.join("midna-app.exe")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn the_apps_entry_names_the_build_and_uninstalls_it_with_no_modify_or_repair() {
        let d = scratch("apps");
        std::fs::write(d.join("midna-app.exe"), vec![0u8; 3000]).unwrap();
        std::fs::create_dir_all(d.join("fonts")).unwrap();
        std::fs::write(d.join("fonts").join("a.ttf"), vec![0u8; 2000]).unwrap();
        assert_eq!(apps_entry::size_kb(&d), 5);
        assert!(apps_entry::key().ends_with(if midna_proto::paths::is_dev() { r"\Uninstall\com.mrgnhnt.midna.dev" } else { r"\Uninstall\com.mrgnhnt.midna" }));
        // Written under a scratch key, never the real entry.
        let key = format!(r"Software\midna-test-apps-{}", std::process::id());
        apps_entry::write(&key, apps_entry::values(&d, 5)).unwrap();
        let exe = d.join("midna-app.exe");
        let text = |v| crate::winreg::get_string(&key, v);
        assert_eq!(text("DisplayName").as_deref(), Some(login::name()));
        assert_eq!(text("DisplayVersion").as_deref(), Some(midna_proto::VERSION));
        assert_eq!(text("UninstallString"), Some(format!("\"{}\" --uninstall", exe.display())));
        assert_eq!(text("DisplayIcon"), Some(format!("{},0", exe.display())));
        assert_eq!(text("InstallLocation"), Some(d.display().to_string()));
        assert_eq!(crate::winreg::get_dword(&key, "NoModify"), Some(1));
        assert_eq!(crate::winreg::get_dword(&key, "NoRepair"), Some(1));
        assert_eq!(crate::winreg::get_dword(&key, "EstimatedSize"), Some(5));
        crate::winreg::delete_key(&key).unwrap();
        assert_eq!(text("DisplayName"), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn a_folder_on_path_matches_without_case_or_a_trailing_slash() {
        let dir = Path::new(r"C:\Users\me\.local\bin");
        assert!(dir_on_path(dir, r"C:\Windows;c:\users\ME\.local\bin\;D:\x"));
        assert!(dir_on_path(dir, "C:/Users/me/.local/bin"));
        assert!(!dir_on_path(dir, r"C:\Windows;C:\Users\me\.local\bin2;;"));
    }

    #[cfg(windows)]
    #[test]
    fn the_cli_is_a_copy_kept_current_and_never_over_someone_elses() {
        let d = scratch("cli");
        let (src, dir) = (d.join("src").join("midna.exe"), d.join("bin"));
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        let id = midna_proto::paths::BUNDLE_ID;
        std::fs::write(&src, format!("v1 {id}")).unwrap();
        let link = dir.join("midna.exe");
        // Not on PATH: left alone unless asked.
        assert_eq!(copy_cli(&src, &dir, false, false), CliLink::NotOnPath { dir: dir.clone() });
        assert!(!link.exists());
        assert_eq!(copy_cli(&src, &dir, false, true), CliLink::NotOnPath { dir: dir.clone() });
        assert_eq!(std::fs::read_to_string(&link).unwrap(), format!("v1 {id}"));
        assert_eq!(copy_cli(&src, &dir, true, false), CliLink::Linked { link: link.clone() });
        // A new build is copied again, and a copy left aside earlier (it was running) goes.
        std::fs::write(dir.join("midna.exe.123.old"), "").unwrap();
        std::fs::write(&src, format!("v2, longer {id}")).unwrap();
        assert_eq!(copy_cli(&src, &dir, true, false), CliLink::Linked { link: link.clone() });
        assert_eq!(std::fs::read_to_string(&link).unwrap(), format!("v2, longer {id}"));
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left, vec![std::ffi::OsString::from("midna.exe")]);
        // Someone else's midna.exe is never replaced.
        std::fs::write(&link, "another program").unwrap();
        assert_eq!(copy_cli(&src, &dir, true, true), CliLink::Conflict { link: link.clone() });
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "another program");
        let _ = std::fs::remove_dir_all(&d);
    }
}
