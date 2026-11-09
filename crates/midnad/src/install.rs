//! Stable binary location: `MIDNA_HOME/bin/current` -> `MIDNA_HOME/bin/<version>-<hash>/`.
//! `midnad --install-self` copies the running midnad (and the `midna` CLI next to it, if any)
//! into a versioned directory and swaps the `current` symlink atomically. launchd /
//! SMAppService (later phase) should point at `bin/current/midnad`, so `daemon.restart` and
//! a crash restart both run the installed binary.
//!
//! Windows: making a symlink needs a privilege (or Developer Mode), so `bin/current` is a small
//! text file naming the versioned directory instead, swapped the same way (write + rename).
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

/// Install the running executable. Returns the versioned directory.
pub fn install_self(home: &Path) -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let bytes = std::fs::read(&exe)?;
    let hash: String = Sha256::digest(&bytes).iter().take(4).map(|b| format!("{b:02x}")).collect();
    let name = format!("{}-{hash}", midna_proto::VERSION);
    let root = bin_root(home);
    midna_proto::paths::guard_write(&root)?;
    let dir = root.join(&name);
    std::fs::create_dir_all(&dir)?;
    copy_exec(&exe, &dir.join(format!("midnad{EXE}")))?;
    if let Some(cli) = exe.parent().map(|p| p.join(format!("midna{EXE}"))).filter(|p| p.is_file()) {
        copy_exec(&cli, &dir.join(format!("midna{EXE}")))?;
    }
    // Atomic swap: a new symlink (Windows: pointer file) renamed over the old one.
    let tmp = root.join(format!(".current.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&name, &tmp)?;
    #[cfg(windows)]
    std::fs::write(&tmp, &name)?;
    std::fs::rename(&tmp, root.join("current"))?;
    Ok(dir)
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
}
