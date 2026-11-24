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

#[cfg(not(windows))]
pub fn read_plist(_path: &Path) -> Option<serde_json::Value> {
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

    /// The Run value's name. Midna Dev and a test build have their own, so they never touch the
    /// installed Midna's.
    pub fn name() -> &'static str {
        midna_proto::paths::app_name()
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

/// What goes with midnad: the newer ConPTY's files (midnad's `pty::CONPTY_FILES`) and the
/// terminal keeper (midnad's `install::KEEPER_EXE`).
#[cfg(windows)]
const BESIDE_MIDNAD: [&str; 3] = ["conpty.dll", "OpenConsole.exe", midna_pty::names::EXE];

/// Install the bundle's midnad (and the `midna` CLI, ConPTY files and keeper beside it) into
/// `home\bin\<version>-<hash>` and point `bin\current` at it (`midnad --install-self`), unless
/// that build is already there; then remove old builds ([`prune_old_installs`]).
#[cfg(windows)]
pub fn install_daemon(bundle: &Path, home: &Path) -> Result<PathBuf, String> {
    let src = bundle.join(exe_name("midnad"));
    if !src.is_file() {
        return Err(format!("no midnad in {}", bundle.display()));
    }
    let current = current_daemon(home);
    let beside = |name: &str| same_file(&bundle.join(name), &current.with_file_name(name));
    if same_file(&src, &current) && beside(&exe_name("midna")) && BESIDE_MIDNAD.iter().all(|f| !bundle.join(f).is_file() || beside(f)) {
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
    prune_old_installs(home);
    Ok(current_daemon(home))
}

/// Keep `bin\current`'s build plus the two most recent others (an upgrade may fall back to the
/// previous midnad) and remove the rest, except a build a live terminal keeper runs from (it
/// keeps its build until its terminal closes, and records itself in `MIDNA_HOME\pty`) or one
/// whose midnad is still running (Windows won't delete a running program, and the rest of its
/// folder would go).
#[cfg(windows)]
pub fn prune_old_installs(home: &Path) {
    let canon = |p: &Path| midna_proto::paths::canonical(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().to_lowercase();
    let root = home.join("bin");
    let current = current_daemon(home).parent().map(canon);
    let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter(|e| Some(canon(&e.path())) != current)
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    dirs.sort();
    let n = dirs.len();
    let keepers: Vec<String> = midna_pty::client::live_keeper_dirs(home).iter().map(|d| canon(d)).collect();
    for (_, d) in dirs.into_iter().take(n.saturating_sub(2)) {
        if keepers.contains(&canon(&d)) {
            continue;
        }
        match std::fs::remove_file(d.join(exe_name("midnad"))) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => continue,
            _ => {}
        }
        if let Err(e) = std::fs::remove_dir_all(&d) {
            crate::lifecycle::log(&format!("can't remove the old build {}: {e}", d.display()));
        }
    }
}

/// The Start menu entry: `Midna.lnk` (Midna Dev: `Midna Dev.lnk`, a test build: `Midna Test.lnk`) in the user's Start menu
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
        format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{}", midna_proto::paths::app_id())
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
pub fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

/// Keep this process's standard handles to itself. Rust starts every child with handle
/// inheritance on, so each inheritable handle goes along whatever its stdio is: a midnad a
/// headless command starts (it outlives the command) would hold the pipe the caller reads the
/// command's output from, and a script or package manager reading it would wait for good.
#[cfg(windows)]
pub fn keep_std_handles() {
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation};
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
    // SAFETY: plain calls on this process's own standard handles.
    unsafe {
        for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let h = GetStdHandle(which);
            if !h.is_null() && h != INVALID_HANDLE_VALUE {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

#[cfg(not(windows))]
pub fn detach(cmd: &mut Command) {
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

// ------------------------------------------------------------------ the install folder

/// Where this build installs itself: `%LOCALAPPDATA%\Programs\<its name>` (`Midna`, `Midna Dev`
/// or `Midna Test`, from [`midna_proto::paths::app_name`]). Midna never looks anywhere else.
///
/// Midna Dev and a test build take `MIDNA_INSTALL_DIR` instead when it's set (the tests' temp
/// folders). Without it, they take the folder their Settings ▸ Apps entry names when it holds a
/// build: one installed through `MIDNA_INSTALL_DIR` and started later without it (by its login
/// entry, a toast click, the Start menu) stays where it is instead of moving into Programs.
/// Neither is ever another Midna's folder (`guard_write`): this build would start or replace that.
#[cfg(windows)]
pub fn install_dir() -> PathBuf {
    let ours = |d: PathBuf| (midna_proto::paths::guard_write(&d).is_ok()).then_some(d);
    if midna_proto::paths::is_dev() {
        if let Some(d) = std::env::var_os("MIDNA_INSTALL_DIR").filter(|v| !v.is_empty()).map(PathBuf::from) {
            if let Some(d) = ours(d) {
                return d;
            }
        } else if let Some(d) = registered_install(&apps_entry::key()).and_then(ours) {
            return d;
        }
    }
    programs_dir()
}

/// `%LOCALAPPDATA%\Programs\<this build's name>`: where it installs itself by default.
#[cfg(windows)]
pub fn programs_dir() -> PathBuf {
    midna_proto::paths::app_data_dir().join("Programs").join(midna_proto::paths::app_name())
}

/// The folder the Settings ▸ Apps entry at `key` names, when it holds a build.
#[cfg(windows)]
fn registered_install(key: &str) -> Option<PathBuf> {
    let dir = PathBuf::from(crate::winreg::get_string(key, "InstallLocation")?);
    dir.join(BUNDLE_MARKER).is_file().then_some(dir)
}

/// What a launch of a packaged build does about where it runs from ([`place`]).
#[cfg(windows)]
#[derive(Clone, Debug, PartialEq)]
pub enum Placement {
    /// Run as usual: this is the install folder.
    Here,
    /// Nothing is installed: copy this build in, then start that copy.
    Install,
    /// This version is installed: start that copy.
    StartInstalled,
    /// An older build is installed: replace it the way an update does, then start it.
    Update,
    /// A newer version is installed: refuse.
    Newer(String),
}

/// What to do for a build in `here` whose install folder is `dest`, holding the version
/// `installed` (None: nothing installed there; "": a build whose version can't be read). A build
/// in `programs` ([`programs_dir`], the default install folder) runs there too, whatever `dest`
/// is: a Midna Dev or test build's `dest` can be a folder its Settings ▸ Apps entry still names
/// from an earlier install elsewhere, and `scripts/dev-app-windows.sh` installs into Programs.
#[cfg(windows)]
pub fn placement(here: &Path, dest: &Path, programs: &Path, installed: Option<&str>, version: &str) -> Placement {
    if same_dir(here, dest) || same_dir(here, programs) {
        return Placement::Here;
    }
    match installed {
        None => Placement::Install,
        Some(v) if v == version => Placement::StartInstalled,
        Some(v) if crate::updater::is_newer(v, version) => Placement::Newer(v.to_string()),
        Some(_) => Placement::Update,
    }
}

/// The same folder, however it's spelled (case, `/`, short names, a trailing separator).
#[cfg(windows)]
fn same_dir(a: &Path, b: &Path) -> bool {
    let full = |p: &Path| midna_proto::paths::canonical(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().to_string();
    midna_proto::paths::same_path(&full(a), &full(b))
}

/// A build's version from its [`BUNDLE_MARKER`]'s text (`Midna 1.2.3. With this file…`): the
/// first word of the first line that is a version.
#[cfg(windows)]
pub fn marker_version(text: &str) -> Option<String> {
    let line = text.lines().next()?;
    line.split_whitespace().map(|w| w.trim_end_matches(['.', ','])).find(|w| semver::Version::parse(w).is_ok()).map(str::to_string)
}

/// The installed build's version: its marker's, else what its midnad reports, else "".
#[cfg(windows)]
fn installed_version(dir: &Path) -> String {
    let marked = std::fs::read_to_string(dir.join(BUNDLE_MARKER)).ok().as_deref().and_then(marker_version);
    marked.or_else(|| crate::update_windows::selftest_version(&dir.join(exe_name("midnad"))).ok()).unwrap_or_default()
}

/// Where the launch that installed a build leaves a note for that build's first launch
/// ([`take_install_note`]): `MIDNA_HOME\installed.txt`, the version on the first line and the
/// install folder on the second.
#[cfg(windows)]
fn install_note() -> PathBuf {
    midna_proto::paths::midna_home().join("installed.txt")
}

/// A packaged build run from anywhere but its install folder ([`install_dir`]) goes there, so
/// the Start menu, Settings ▸ Apps and login entries always name the install folder:
///
/// - nothing installed: copy this build in ([`update_windows::swap_in`], the marker last, Mark
///   of the Web dropped) and start that copy;
/// - this version installed: start that copy (which hands over to it if it's running);
/// - an older one: quit it if it's running, and replace it as an update does
///   ([`update_windows::start_apply`]: once this launch has exited, the helper swaps the files
///   with rollback and starts the app);
/// - a newer one: refuse, saying how to uninstall it first.
///
/// Returns the exit code when this launch is done, None when it should run as usual.
///
/// [`update_windows::swap_in`]: crate::update_windows::swap_in
/// [`update_windows::start_apply`]: crate::update_windows::start_apply
#[cfg(windows)]
pub fn place() -> Option<i32> {
    let Mode::Installed { bundle } = mode() else { return None };
    let dest = install_dir();
    let installed = dest.join(BUNDLE_MARKER).is_file().then(|| installed_version(&dest));
    let name = midna_proto::paths::app_name();
    let version = midna_proto::VERSION;
    let step = placement(&bundle, &dest, &programs_dir(), installed.as_deref(), version);
    if step != Placement::Here {
        crate::lifecycle::log(&format!("launch from {}: {step:?} ({} in {})", bundle.display(), installed.as_deref().map_or("nothing", |v| if v.is_empty() { "a build" } else { v }), dest.display()));
    }
    let r = match step {
        Placement::Here => return None,
        Placement::StartInstalled => start_installed(&dest),
        Placement::Newer(v) => Err(newer_installed(&v, &dest)),
        Placement::Install => foreign_files(&bundle)
            .and_then(|()| quit_running())
            .and_then(|()| crate::update_windows::swap_in(&bundle, &dest).map_err(|e| format!("Couldn't install {name} in {}: {e}", dest.display())))
            .and_then(|()| {
                leave_install_note(&dest);
                start_installed(&dest)
            }),
        Placement::Update => foreign_files(&bundle).and_then(|()| quit_running()).and_then(|()| {
            leave_install_note(&dest);
            crate::update_windows::start_apply(&bundle, &dest, true).map_err(|e| format!("Couldn't update {name} in {}: {e}", dest.display()))
        }),
    };
    Some(match r {
        Ok(()) => 0,
        Err(e) => {
            tell(&e);
            1
        }
    })
}

/// Why a build won't go over the newer version `v` installed in `dest`.
#[cfg(windows)]
fn newer_installed(v: &str, dest: &Path) -> String {
    let (name, version) = (midna_proto::paths::app_name(), midna_proto::VERSION);
    format!(
        "{name} {v} is already installed in {}, which is newer than this {version}. To install this one instead, uninstall {name} first (Settings ▸ Apps ▸ {name} ▸ Uninstall), then run this again.",
        dest.display()
    )
}

/// What `--install` did with the build's files ([`copy_in`]).
#[cfg(windows)]
#[derive(Clone, Debug, PartialEq)]
pub enum Copied {
    /// It runs from its install folder: nothing to copy.
    Here,
    /// This version was installed already.
    Already,
    /// Copied into a folder with no build in it.
    Installed,
    /// Put in place of the older build there (its version, "" when it couldn't be read).
    Updated(String),
}

/// `--install`'s copy: the build in `bundle` goes into `dest` by [`place`]'s rules, version
/// `version`. One difference: an older build is replaced from this process ([`swap_in`] renames
/// its running files aside), not by a helper after this exits, because nothing is started
/// afterwards and this process doesn't run from `dest`. `quit` asks a running app to quit
/// first; when it can't, nothing is copied.
///
/// [`swap_in`]: crate::update_windows::swap_in
#[cfg(windows)]
fn copy_in(bundle: &Path, dest: &Path, programs: &Path, version: &str, quit: impl FnOnce() -> Result<(), String>) -> Result<Copied, String> {
    let installed = dest.join(BUNDLE_MARKER).is_file().then(|| installed_version(dest));
    let name = midna_proto::paths::app_name();
    let done = match placement(bundle, dest, programs, installed.as_deref(), version) {
        Placement::Here => return Ok(Copied::Here),
        Placement::StartInstalled => return Ok(Copied::Already),
        Placement::Newer(v) => return Err(newer_installed(&v, dest)),
        Placement::Install => Copied::Installed,
        Placement::Update => Copied::Updated(installed.unwrap_or_default()),
    };
    foreign_files(bundle)?;
    quit()?;
    crate::update_windows::swap_in(bundle, dest).map_err(|e| format!("Couldn't install {name} in {}: {e}", dest.display()))?;
    Ok(done)
}

/// `midna-app.exe --install`, run from a packaged build: install it with no window and exit, for
/// a package manager or a script. It copies the build into its install folder ([`copy_in`]),
/// then does what the app does at each launch from there (`lifecycle`): midnad into
/// `MIDNA_HOME\bin`, the Start menu and Settings ▸ Apps entries, the login entry (which starts
/// midnad if it isn't answering) and the CLI copy. It starts no app; one it asked to quit stays
/// closed, and a running midnad keeps its build until the app next starts and upgrades it.
/// Exit 0 when every step went, 1 when one failed (each is still tried).
#[cfg(windows)]
pub fn install_main(bundle: &Path) -> i32 {
    let (name, version) = (midna_proto::paths::app_name(), midna_proto::VERSION);
    let dest = install_dir();
    let say = |line: String| {
        crate::lifecycle::log_file(&format!("--install: {line}"));
        println!("{line}");
    };
    let mut ok = true;
    let mut fail = |line: String| {
        crate::lifecycle::log_file(&format!("--install: {line}"));
        eprintln!("midna-app --install: {line}");
        ok = false;
    };
    let target = match copy_in(bundle, &dest, &programs_dir(), version, quit_running) {
        Ok(Copied::Here) => {
            say(format!("{name} {version} runs from {}", bundle.display()));
            bundle.to_path_buf()
        }
        Ok(Copied::Already) => {
            say(format!("{name} {version} is already installed in {}", dest.display()));
            dest
        }
        Ok(Copied::Installed) => {
            say(format!("installed {name} {version} in {}", dest.display()));
            dest
        }
        Ok(Copied::Updated(old)) => {
            say(format!("updated {name} in {} from {} to {version}", dest.display(), if old.is_empty() { "an unknown version" } else { &old }));
            dest
        }
        Err(e) => {
            fail(e);
            return 1;
        }
    };
    let home = midna_proto::paths::midna_home();
    let _ = std::fs::create_dir_all(&home);
    match install_daemon(&target, &home) {
        Ok(d) => say(format!("midnad: {}", d.display())),
        Err(e) => fail(format!("midnad: {e}")),
    }
    match add_to_start_menu(&target) {
        Ok(()) => say(format!("Start menu: {}", start_menu_entry().unwrap_or_default().display())),
        Err(e) => fail(format!("Start menu: {e}")),
    }
    match add_to_apps(&target) {
        Ok(()) => say("Settings ▸ Apps: listed".into()),
        Err(e) => fail(format!("Settings ▸ Apps: {e}")),
    }
    match register(&target) {
        LoginItem::Enabled => say("login entry: on".into()),
        LoginItem::RequiresApproval => say("login entry: switched off in Settings ▸ Apps ▸ Startup, so left off".into()),
        LoginItem::Failed(e) => fail(format!("login entry: {e}")),
        other => fail(format!("login entry: {other:?}")),
    }
    let shell_path = if cli_link_dir_pinned() { None } else { login_shell_path() };
    match link_cli(&home, shell_path.as_deref(), false) {
        CliLink::Linked { link } => say(format!("midna CLI: {}", link.display())),
        CliLink::NotOnPath { dir } => say(format!("midna CLI: not copied, {} isn't on PATH", dir.display())),
        CliLink::Conflict { link } => say(format!("midna CLI: not copied, {} isn't Midna's", link.display())),
        CliLink::Failed(e) => fail(format!("midna CLI: {e}")),
        CliLink::Dev => {}
    }
    if ok { 0 } else { 1 }
}

/// A build to install from must hold only the build ([`update_windows::not_the_build`]): its
/// whole folder is copied, so a marker that ended up among someone's files never copies those.
///
/// [`update_windows::not_the_build`]: crate::update_windows::not_the_build
#[cfg(windows)]
fn foreign_files(bundle: &Path) -> Result<(), String> {
    let others = crate::update_windows::not_the_build(bundle)?;
    if others.is_empty() {
        return Ok(());
    }
    let shown: Vec<&str> = others.iter().take(5).map(String::as_str).collect();
    Err(format!(
        "{} holds files that aren't {}'s ({}{}). Unpack the release zip into a folder of its own and run midna-app.exe from there.",
        bundle.display(),
        midna_proto::paths::app_name(),
        shown.join(", "),
        if others.len() > shown.len() { ", …" } else { "" }
    ))
}

/// Ask a running app of this home to quit (its install is about to change), and wait for it.
#[cfg(windows)]
fn quit_running() -> Result<(), String> {
    if !crate::instance::ask_to_quit() {
        return Ok(());
    }
    crate::lifecycle::log("asked the running app to quit");
    let t0 = std::time::Instant::now();
    while crate::instance::running() && t0.elapsed() < std::time::Duration::from_secs(15) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if crate::instance::running() {
        return Err(format!("{} is still running. Quit it, then run this again.", midna_proto::paths::app_name()));
    }
    Ok(())
}

#[cfg(windows)]
fn leave_install_note(dest: &Path) {
    let note = install_note();
    let r = note.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&note, format!("{}\n{}\n", midna_proto::VERSION, dest.display())));
    if let Err(e) = r {
        crate::lifecycle::log(&format!("{}: {e}", note.display()));
    }
}

/// The line the first launch of a newly installed build shows, once: the note the launch that
/// installed it left ([`place`]), when it names this build's version and folder.
#[cfg(windows)]
pub fn take_install_note(bundle: &Path) -> Option<String> {
    let note = install_note();
    let text = std::fs::read_to_string(&note).ok()?;
    let _ = std::fs::remove_file(&note);
    let mut lines = text.lines();
    let (version, dir) = (lines.next()?.trim(), Path::new(lines.next()?.trim()));
    (version == midna_proto::VERSION && same_dir(dir, bundle)).then(|| install_line(midna_proto::paths::app_name(), version, bundle))
}

#[cfg(windows)]
fn install_line(name: &str, version: &str, dir: &Path) -> String {
    format!("{name} {version} is installed in {}. Start it from the Start menu from now on; the folder you ran it from can go.", dir.display())
}

/// Start the app in `dest` and let it come to the front.
#[cfg(windows)]
fn start_installed(dest: &Path) -> Result<(), String> {
    let exe = dest.join(exe_name("midna-app"));
    // SAFETY: a plain call. This launch came from a click; the copy it starts may take the front.
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(windows_sys::Win32::UI::WindowsAndMessaging::ASFW_ANY);
    }
    let mut cmd = Command::new(&exe);
    cmd.current_dir(dest).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    cmd.spawn().map(|_| ()).map_err(|e| format!("Couldn't start {}: {e}", exe.display()))
}

/// Say `msg` to whoever ran this: the console it was run from, else a message box (a release
/// build has no console of its own), except with `MIDNA_NO_ACTIVATE` (a test run in the
/// background), where app.log has it.
#[cfg(windows)]
fn tell(msg: &str) {
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, GetLastError};
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONWARNING, MB_OK, MessageBoxW};
    crate::lifecycle::log(msg);
    // SAFETY: plain calls with NUL-terminated strings we own.
    unsafe {
        // Access denied: it has a console already (a debug build).
        if AttachConsole(ATTACH_PARENT_PROCESS) != 0 || GetLastError() == ERROR_ACCESS_DENIED {
            eprintln!("midna-app: {msg}");
            return;
        }
        if std::env::var_os("MIDNA_NO_ACTIVATE").is_some() {
            return;
        }
        let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        MessageBoxW(std::ptr::null_mut(), wide(msg).as_ptr(), wide(midna_proto::paths::app_name()).as_ptr(), MB_OK | MB_ICONWARNING);
    }
}

// ------------------------------------------------------------------ relaunch

/// Windows relaunches through the update's own helper (`update_windows::apply_main`).
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

    /// Old builds go, but not the current one, the two newest others, one a live keeper runs
    /// from, or one whose midnad is running.
    #[cfg(windows)]
    #[test]
    fn pruning_keeps_builds_that_a_keeper_or_a_midnad_runs_from() {
        use midna_pty::names;
        let home = scratch("prune");
        let bin = home.join("bin");
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let ping = Path::new(&system).join(r"System32\PING.EXE");
        let run = |exe: &Path| Command::new(exe).args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        let record = |session: &str, pid: u32| {
            let r = names::Record { pid, pipe: "p".into(), protocol: 1, build: "b".into(), shell_pid: 1, exe: String::new() };
            std::fs::create_dir_all(names::pty_dir(&home)).unwrap();
            std::fs::write(names::record_path(&home, session), serde_json::to_vec(&r).unwrap()).unwrap();
        };
        let mut running = vec![];
        for v in ["v1", "v2", "v3", "v4", "v5", "v6"] {
            let d = bin.join(v);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("midna.exe"), v).unwrap();
            match v {
                // A live terminal's keeper, recorded in MIDNA_HOME\pty.
                "v1" => {
                    std::fs::write(d.join("midnad.exe"), v).unwrap();
                    std::fs::copy(&ping, d.join(names::EXE)).unwrap();
                    let p = run(&d.join(names::EXE));
                    record("s1", p.id());
                    running.push(p);
                }
                // An old midnad still running.
                "v2" => {
                    std::fs::copy(&ping, d.join("midnad.exe")).unwrap();
                    running.push(run(&d.join("midnad.exe")));
                }
                _ => std::fs::write(d.join("midnad.exe"), v).unwrap(),
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        // A record whose keeper is gone keeps nothing.
        record("s2", u32::MAX - 3);
        std::fs::write(bin.join("current"), "v4").unwrap();
        prune_old_installs(&home);
        let left: Vec<bool> = ["v1", "v2", "v3", "v4", "v5", "v6"].iter().map(|v| bin.join(v).join("midna.exe").exists()).collect();
        for mut p in running {
            let _ = p.kill();
            let _ = p.wait();
        }
        assert_eq!(left, [true, true, false, true, true, true]);
        let _ = std::fs::remove_dir_all(&home);
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
        let id = match (midna_proto::paths::is_dev(), midna_proto::paths::TEST_BUILD) {
            (false, _) => "com.mrgnhnt.midna",
            (true, false) => "com.mrgnhnt.midna.dev",
            (true, true) => "com.mrgnhnt.midna.test",
        };
        assert!(apps_entry::key().ends_with(&format!(r"\Uninstall\{id}")));
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
    fn a_build_run_from_elsewhere_installs_starts_updates_or_refuses() {
        let d = scratch("place");
        let (zip, dest) = (d.join("Downloads").join("Midna-1.2.0-windows-x86_64").join("Midna"), d.join("Programs").join("Midna"));
        let programs = d.join("Local").join("Programs").join("Midna");
        std::fs::create_dir_all(&zip).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        // Run from the install folder, however it's spelled: as usual.
        let upper = PathBuf::from(dest.to_string_lossy().to_uppercase() + "\\");
        assert_eq!(placement(&upper, &dest, &programs, Some("1.0.0"), "1.2.0"), Placement::Here);
        assert_eq!(placement(&dest, &dest, &programs, None, "1.2.0"), Placement::Here);
        // From anywhere else.
        assert_eq!(placement(&zip, &dest, &programs, None, "1.2.0"), Placement::Install);
        assert_eq!(placement(&zip, &dest, &programs, Some("1.2.0"), "1.2.0"), Placement::StartInstalled);
        assert_eq!(placement(&zip, &dest, &programs, Some("1.1.9"), "1.2.0"), Placement::Update);
        assert_eq!(placement(&zip, &dest, &programs, Some("1.2.0-beta.3"), "1.2.0"), Placement::Update);
        assert_eq!(placement(&zip, &dest, &programs, Some(""), "1.2.0"), Placement::Update, "a build whose version can't be read is replaced");
        assert_eq!(placement(&zip, &dest, &programs, Some("1.3.0"), "1.2.0"), Placement::Newer("1.3.0".into()));
        assert_eq!(placement(&zip, &dest, &programs, Some("1.2.0-beta.4"), "1.2.0-beta.3"), Placement::Newer("1.2.0-beta.4".into()));
        // A Dev or test build whose registered folder is elsewhere, run from Programs: it stays.
        assert_eq!(placement(&programs, &zip, &programs, Some("1.0.0"), "1.2.0"), Placement::Here);
        assert_eq!(placement(&programs, &zip, &programs, None, "1.2.0"), Placement::Here);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A build folder as the release zip unpacks it: the app, midnad and the marker.
    #[cfg(windows)]
    fn build(dir: &Path, version: &str, app: &[u8]) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("midna-app.exe"), app).unwrap();
        std::fs::write(dir.join("midnad.exe"), format!("midnad {version}")).unwrap();
        std::fs::write(dir.join(BUNDLE_MARKER), format!("Midna Test {version}. With this file next to it…")).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn install_copies_the_build_in_or_replaces_an_older_one_without_starting_it() {
        let d = scratch("copy-in");
        let (zip, dest, programs) = (d.join("Downloads").join("Midna"), d.join("Programs").join("Midna Test"), d.join("Local").join("Midna Test"));
        let no_quit = || -> Result<(), String> { Ok(()) };
        build(&zip, "1.2.0", b"app 1.2.0");
        // Nothing installed: copied in, and nothing in the zip's folder changes.
        assert_eq!(copy_in(&zip, &dest, &programs, "1.2.0", no_quit), Ok(Copied::Installed));
        assert_eq!(std::fs::read(dest.join("midna-app.exe")).unwrap(), b"app 1.2.0");
        assert_eq!(installed_version(&dest), "1.2.0");
        assert!(!dest.join("midna-update-leftovers.txt").exists(), "nothing was there to set aside");
        // Again: already there. Run from the install folder: nothing to copy.
        assert_eq!(copy_in(&zip, &dest, &programs, "1.2.0", no_quit), Ok(Copied::Already));
        assert_eq!(copy_in(&dest, &dest, &programs, "1.2.0", no_quit), Ok(Copied::Here));
        // An older build, its app running: swapped in here and now, the running one set aside.
        std::fs::remove_dir_all(&dest).unwrap();
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        build(&dest, "1.1.0", &std::fs::read(Path::new(&system).join(r"System32\PING.EXE")).unwrap());
        let mut running = Command::new(dest.join("midna-app.exe")).args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        let mut asked = false;
        let r = copy_in(&zip, &dest, &programs, "1.2.0", || {
            asked = true;
            Ok(())
        });
        let _ = running.kill();
        let _ = running.wait();
        assert_eq!(r, Ok(Copied::Updated("1.1.0".into())));
        assert!(asked, "a running app is asked to quit first");
        assert_eq!(std::fs::read(dest.join("midna-app.exe")).unwrap(), b"app 1.2.0");
        assert_eq!(installed_version(&dest), "1.2.0");
        let leftovers = std::fs::read_to_string(dest.join("midna-update-leftovers.txt")).unwrap();
        assert!(leftovers.contains("midna-app.exe.old"), "{leftovers}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn install_refuses_a_newer_build_a_folder_of_other_files_or_an_app_that_wont_quit() {
        let d = scratch("copy-in-refused");
        let (zip, dest, programs) = (d.join("Downloads").join("Midna"), d.join("Programs").join("Midna Test"), d.join("Local").join("Midna Test"));
        build(&zip, "1.2.0", b"app 1.2.0");
        let untouched = |dest: &Path| std::fs::read_dir(dest).map(|r| r.count()).unwrap_or(0);
        // A newer build installed stays, and so does every file of it.
        build(&dest, "1.3.0", b"app 1.3.0");
        let e = copy_in(&zip, &dest, &programs, "1.2.0", || Ok(())).unwrap_err();
        assert!(e.contains("1.3.0 is already installed") && e.contains("Uninstall"), "{e}");
        assert_eq!(std::fs::read(dest.join("midna-app.exe")).unwrap(), b"app 1.3.0");
        std::fs::remove_dir_all(&dest).unwrap();
        // An app that won't quit: nothing is copied.
        let e = copy_in(&zip, &dest, &programs, "1.2.0", || Err("still running".into())).unwrap_err();
        assert_eq!(e, "still running");
        assert_eq!(untouched(&dest), 0);
        // Someone's files beside the build: never copied, and the app isn't asked to quit.
        std::fs::write(zip.join("taxes.xlsx"), "").unwrap();
        let e = copy_in(&zip, &dest, &programs, "1.2.0", || panic!("asked to quit")).unwrap_err();
        assert!(e.contains("taxes.xlsx"), "{e}");
        assert_eq!(untouched(&dest), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn the_install_folder_is_named_for_the_build() {
        let want = midna_proto::paths::app_data_dir().join("Programs").join(midna_proto::paths::app_name());
        assert_eq!(programs_dir(), want);
        // Midna Dev and test builds may also take MIDNA_INSTALL_DIR or their registered folder.
        if !midna_proto::paths::is_dev() {
            assert_eq!(want, midna_proto::paths::windows_install_dir());
            assert_eq!(install_dir(), want);
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_registered_install_counts_only_while_it_holds_a_build() {
        let d = scratch("registered");
        let key = format!(r"Software\midna-test-registered-{}", std::process::id());
        assert_eq!(registered_install(&key), None);
        crate::winreg::set_string(&key, Some("InstallLocation"), &d.display().to_string()).unwrap();
        assert_eq!(registered_install(&key), None, "no build there");
        std::fs::write(d.join(BUNDLE_MARKER), "Midna Test 1.0.0.").unwrap();
        assert_eq!(registered_install(&key), Some(d.clone()));
        crate::winreg::delete_key(&key).unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn the_marker_names_the_version() {
        let text = "Midna Test 0.9.0-e2e.1. With this file next to it, midna-app.exe runs as an installed Midna: it\ninstalls…";
        assert_eq!(marker_version(text).as_deref(), Some("0.9.0-e2e.1"));
        assert_eq!(marker_version("Midna 1.2.3. With this file").as_deref(), Some("1.2.3"));
        assert_eq!(marker_version("Midna Dev 0.4.0-beta.2.\n").as_deref(), Some("0.4.0-beta.2"));
        assert_eq!(marker_version(""), None);
        assert_eq!(marker_version("a build\n1.2.3"), None, "only the first line");
        assert_eq!(
            install_line("Midna", "1.2.3", Path::new(r"C:\Users\me\AppData\Local\Programs\Midna")),
            r"Midna 1.2.3 is installed in C:\Users\me\AppData\Local\Programs\Midna. Start it from the Start menu from now on; the folder you ran it from can go."
        );
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
