//! Stable binary location: `MIDNA_HOME/bin/current` -> `MIDNA_HOME/bin/<version>-<hash>/`.
//! `midnad --install-self` copies the running midnad (and the `midna` CLI next to it, if any)
//! into a versioned directory and swaps the `current` symlink atomically. launchd /
//! SMAppService (later phase) should point at `bin/current/midnad`, so `daemon.restart` and
//! a crash restart both run the installed binary.
//!
//! Windows: making a symlink needs a privilege (or Developer Mode), so `bin/current` is a small
//! text file naming the versioned directory instead, swapped the same way (write + rename). The
//! newer ConPTY's files beside midnad.exe go with it (`pty::CONPTY_FILES`).
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// `.exe` on Windows, nothing elsewhere.
const EXE: &str = std::env::consts::EXE_SUFFIX;

static RUNNING: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// The resolved path of the binary this process started from. `current_exe` is the path as
/// launched (often `bin/current/midnad`, a symlink that an install re-points), so resolve it
/// once at startup; `main` calls this first thing.
pub fn running_binary() -> Option<PathBuf> {
    RUNNING.get_or_init(|| std::env::current_exe().ok().map(|p| midna_proto::paths::canonical(&p).unwrap_or(p))).clone()
}

pub fn bin_root(home: &Path) -> PathBuf {
    home.join("bin")
}

#[cfg(unix)]
pub fn current_binary(home: &Path) -> PathBuf {
    bin_root(home).join("current").join("midnad")
}

#[cfg(windows)]
pub fn current_binary(home: &Path) -> PathBuf {
    let root = bin_root(home);
    let name = std::fs::read_to_string(root.join("current")).unwrap_or_default();
    root.join(name.trim()).join(format!("midnad{EXE}"))
}

/// The installed `midna` CLI (`bin/current/midna`; on Windows `midna.exe` in the directory
/// `bin/current` names).
#[cfg(unix)]
pub fn current_cli(home: &Path) -> PathBuf {
    bin_root(home).join("current").join("midna")
}

#[cfg(windows)]
pub fn current_cli(home: &Path) -> PathBuf {
    current_binary(home).with_file_name(format!("midna{EXE}"))
}

/// Windows: the `midna` CLI the agents' global config runs, `bin\midna.exe`. The versioned
/// folder's copy moves with every update; this path doesn't, and each install (and midnad, when
/// it finds this one out of date) makes it a copy of `current_cli` again. Nothing in midna
/// removes it, `--uninstall` included.
#[cfg(windows)]
pub fn stable_cli(home: &Path) -> PathBuf {
    bin_root(home).join(format!("midna{EXE}"))
}

/// Make [`stable_cli`] a copy of [`current_cli`] unless it is one already (a copy keeps the
/// source's time, so the same size and time means current). One that's running (a hook waiting
/// on an approval) can't be replaced, only renamed: it's moved aside and removed on a later call.
/// Returns whether it's current now.
#[cfg(windows)]
pub fn refresh_stable_cli(home: &Path) -> bool {
    let (src, stable) = (current_cli(home), stable_cli(home));
    let Some(want) = std::fs::metadata(&src).ok().filter(|m| m.is_file()) else { return false };
    let root = bin_root(home);
    for e in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with(".midna.") && name.ends_with(".old") {
            let _ = std::fs::remove_file(e.path());
        }
    }
    if std::fs::metadata(&stable).is_ok_and(|m| m.len() == want.len() && m.modified().ok() == want.modified().ok()) {
        return true;
    }
    let tmp = root.join(format!(".midna.{}.tmp", std::process::id()));
    if std::fs::copy(&src, &tmp).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    // Straight over the old one when it isn't running, so there's no moment without it.
    if std::fs::rename(&tmp, &stable).is_ok() {
        return true;
    }
    let aside = root.join(format!(".midna.{}.old", std::process::id()));
    let ok = std::fs::rename(&stable, &aside).is_ok() && std::fs::rename(&tmp, &stable).is_ok();
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(&aside);
    ok
}

/// Install the running executable. Returns the versioned directory.
pub fn install_self(home: &Path) -> std::io::Result<PathBuf> {
    install(&std::env::current_exe()?, home)
}

/// Install the midnad at `exe` and what's beside it.
fn install(exe: &Path, home: &Path) -> std::io::Result<PathBuf> {
    let bytes = std::fs::read(exe)?;
    let hash: String = Sha256::digest(&bytes).iter().take(4).map(|b| format!("{b:02x}")).collect();
    let name = format!("{}-{hash}", midna_proto::VERSION);
    let root = bin_root(home);
    midna_proto::paths::guard_write(&root)?;
    let dir = root.join(&name);
    std::fs::create_dir_all(&dir)?;
    copy_exec(exe, &dir.join(format!("midnad{EXE}")))?;
    if let Some(cli) = exe.parent().map(|p| p.join(format!("midna{EXE}"))).filter(|p| p.is_file()) {
        copy_exec(&cli, &dir.join(format!("midna{EXE}")))?;
    }
    // Skipped when the copy there is the same: a daemon running from `dir` has them in use, and
    // Windows won't replace a file that's in use.
    #[cfg(windows)]
    for name in crate::pty::CONPTY_FILES {
        let (from, to) = (exe.with_file_name(name), dir.join(name));
        if from.is_file() && !same_bytes(&from, &to) {
            copy_exec(&from, &to)?;
        }
    }
    // Atomic swap: a new symlink (Windows: pointer file) renamed over the old one.
    let tmp = root.join(format!(".current.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&name, &tmp)?;
    #[cfg(windows)]
    std::fs::write(&tmp, &name)?;
    std::fs::rename(&tmp, root.join("current"))?;
    // midnad does it later if this can't (one still running that can't be moved either).
    #[cfg(windows)]
    refresh_stable_cli(home);
    Ok(dir)
}

#[cfg(windows)]
fn same_bytes(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) if x.len() == y.len() => std::fs::read(a).ok().is_some_and(|a| std::fs::read(b).ok().is_some_and(|b| a == b)),
        _ => false,
    }
}

fn copy_exec(from: &Path, to: &Path) -> std::io::Result<()> {
    let tmp = to.with_extension("tmp");
    std::fs::copy(from, &tmp)?;
    #[cfg(unix)]
    std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    std::fs::rename(&tmp, to)
}

#[cfg(test)]
mod tests {
    use super::EXE;

    #[test]
    fn install_self_versions_and_points_current() {
        let home = std::env::temp_dir().join(format!("midna-inst-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let dir = super::install_self(&home).unwrap();
        let bin = dir.join(format!("midnad{EXE}"));
        assert!(bin.is_file());
        let cur = super::current_binary(&home);
        assert!(cur.is_file());
        assert_eq!(std::fs::canonicalize(&cur).unwrap(), std::fs::canonicalize(&bin).unwrap());
        // Re-installing the same binary is idempotent.
        assert_eq!(super::install_self(&home).unwrap(), dir);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The agents' global config names `bin\midna.exe`: an update leaves the path alone and makes
    /// it the new CLI, even while a hook is running the old one.
    #[cfg(windows)]
    #[test]
    fn the_stable_cli_follows_updates_even_while_running() {
        let home = std::env::temp_dir().join(format!("midna-inst-stable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let bin = super::bin_root(&home);
        let point = |v: &str, body: &[u8]| {
            std::fs::create_dir_all(bin.join(v)).unwrap();
            std::fs::write(bin.join(v).join("midna.exe"), body).unwrap();
            std::fs::write(bin.join("current"), v).unwrap();
        };
        let stable = super::stable_cli(&home);
        assert!(!super::refresh_stable_cli(&home), "nothing installed, nothing to copy");
        assert!(!stable.exists());

        point("v1", b"one");
        assert!(super::refresh_stable_cli(&home));
        assert_eq!(std::fs::read(&stable).unwrap(), b"one");
        assert!(super::refresh_stable_cli(&home), "already current");

        // A hook running it: a real program, since only a running image refuses to be replaced.
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        std::fs::copy(std::path::Path::new(&system).join(r"System32\PING.EXE"), &stable).unwrap();
        let mut running = std::process::Command::new(&stable).args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        point("v2", b"two, a new version");
        let refreshed = super::refresh_stable_cli(&home);
        let _ = running.kill();
        let _ = running.wait();
        assert!(refreshed);
        assert_eq!(std::fs::read(&stable).unwrap(), b"two, a new version");
        // The running one was moved aside; the next call clears it away.
        let left = || -> Vec<String> { std::fs::read_dir(&bin).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with(".midna.")).collect() };
        assert_eq!(left().len(), 1, "{:?}", left());
        assert!(super::refresh_stable_cli(&home));
        assert!(left().is_empty(), "{:?}", left());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(windows)]
    #[test]
    fn the_newer_conpty_goes_with_midnad_and_one_in_use_stays() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = std::env::temp_dir().join(format!("midna-inst-conpty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (bundle, home) = (root.join("bundle"), root.join("home"));
        std::fs::create_dir_all(&bundle).unwrap();
        let exe = bundle.join("midnad.exe");
        std::fs::write(&exe, "midnad").unwrap();
        let dir = super::install(&exe, &home).unwrap();
        assert!(!dir.join("conpty.dll").exists(), "none beside it, none installed");

        for name in crate::pty::CONPTY_FILES {
            std::fs::write(bundle.join(name), name).unwrap();
        }
        assert_eq!(super::install(&exe, &home).unwrap(), dir);
        for name in crate::pty::CONPTY_FILES {
            assert_eq!(std::fs::read_to_string(dir.join(name)).unwrap(), name);
        }
        // A daemon running from there has them open the way a loaded DLL is (no sharing for
        // delete): installing the same files again leaves them be.
        let held: Vec<_> = crate::pty::CONPTY_FILES
            .iter()
            .map(|name| std::fs::OpenOptions::new().read(true).share_mode(1 /* FILE_SHARE_READ */).open(dir.join(name)).unwrap())
            .collect();
        assert_eq!(super::install(&exe, &home).unwrap(), dir);
        drop(held);
        let _ = std::fs::remove_dir_all(&root);
    }
}
