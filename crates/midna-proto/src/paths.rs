//! Where midna lives on disk. Socket: `MIDNA_SOCKET`, else `$MIDNA_HOME/midnad.sock`.
//! Home: `MIDNA_HOME`, else `~/Library/Application Support/com.mrgnhnt.midna` on macOS and
//! `%LOCALAPPDATA%\com.mrgnhnt.midna` on Windows.
//!
//! Midna Dev (`scripts/dev-app.sh`) is built with its home baked in ([`DEV_HOME`]). It ignores
//! `MIDNA_HOME` / `MIDNA_SOCKET`, which every Midna terminal exports pointing at production,
//! and [`guard_write`] refuses production's files.
use std::path::{Component, Path, PathBuf};

pub const BUNDLE_ID: &str = "com.mrgnhnt.midna";
pub const DAEMON_LABEL: &str = "com.mrgnhnt.midna.daemon";
/// System Settings ▸ Notifications opened on midna's own page (not the list of every app).
#[cfg(not(windows))]
pub const NOTIFICATIONS_PANE: &str = "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=com.mrgnhnt.midna";
/// Windows Settings ▸ System ▸ Notifications & actions (it has no page per app).
#[cfg(windows)]
pub const NOTIFICATIONS_PANE: &str = "ms-settings:notifications";

/// Midna Dev's home, set at build time (`packaging/build-app.sh --dev-home DIR`). Some = this
/// build is Midna Dev.
pub const DEV_HOME: Option<&str> = match option_env!("MIDNA_DEV_HOME") {
    Some(h) if !h.is_empty() => Some(h),
    _ => None,
};

/// Is this build Midna Dev? It never touches the installed Midna (see [`guard_write`]).
pub fn is_dev() -> bool {
    DEV_HOME.is_some()
}

#[cfg(not(windows))]
pub fn user_home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// `%USERPROFILE%` (Git Bash's `HOME` can point elsewhere, so it comes second).
#[cfg(windows)]
pub fn user_home() -> PathBuf {
    ["USERPROFILE", "HOME"]
        .iter()
        .find_map(|k| std::env::var_os(k).filter(|v| !v.is_empty()))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Where per-user app data lives: `~/Library/Application Support` on macOS,
/// `%LOCALAPPDATA%` on Windows (local, not roaming: it holds sockets and logs).
#[cfg(not(windows))]
pub fn app_data_dir() -> PathBuf {
    user_home().join("Library/Application Support")
}

#[cfg(windows)]
pub fn app_data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join("AppData").join("Local"))
}

/// The installed Midna's home, whatever this build is.
pub fn production_home() -> PathBuf {
    app_data_dir().join(BUNDLE_ID)
}

/// Where the Windows installer puts Midna: `%LOCALAPPDATA%\Programs\Midna` (per user, no admin).
#[cfg(windows)]
pub fn windows_install_dir() -> PathBuf {
    app_data_dir().join("Programs").join("Midna")
}

pub fn midna_home() -> PathBuf {
    resolve_home(DEV_HOME, std::env::var_os("MIDNA_HOME"))
}

pub fn socket_path() -> PathBuf {
    resolve_socket(DEV_HOME, std::env::var_os("MIDNA_SOCKET"), midna_home())
}

fn resolve_home(dev: Option<&str>, env: Option<std::ffi::OsString>) -> PathBuf {
    if let Some(h) = dev {
        return PathBuf::from(h);
    }
    match env.filter(|h| !h.is_empty()) {
        Some(h) => PathBuf::from(h),
        None => production_home(),
    }
}

fn resolve_socket(dev: Option<&str>, env: Option<std::ffi::OsString>, home: PathBuf) -> PathBuf {
    match env.filter(|s| !s.is_empty()) {
        Some(s) if dev.is_none() => PathBuf::from(s),
        _ => home.join("midnad.sock"),
    }
}

/// What Midna Dev must never write: production's home, CLI link, Quick Action and app bundle.
#[cfg(not(windows))]
pub fn production_paths() -> Vec<PathBuf> {
    let home = user_home();
    vec![
        production_home(),
        home.join(".local/bin/midna"),
        home.join("Library/Services/Open in Midna.workflow"),
        home.join("Library/LaunchAgents").join(format!("{DAEMON_LABEL}.plist")),
        PathBuf::from("/Applications/Midna.app"),
    ]
}

/// What Midna Dev must never write on Windows: production's home, install dir and CLI.
#[cfg(windows)]
pub fn production_paths() -> Vec<PathBuf> {
    vec![production_home(), windows_install_dir(), user_home().join(".local").join("bin").join("midna.exe")]
}

/// Midna Dev refuses to write `path` when it is, or is inside, one of production's
/// ([`production_paths`]). Always Ok in every other build.
pub fn guard_write(path: &Path) -> std::io::Result<()> {
    if is_dev() { check_write(path, &production_paths()) } else { Ok(()) }
}

fn check_write(path: &Path, protected: &[PathBuf]) -> std::io::Result<()> {
    let p = resolved(path);
    match protected.iter().find(|q| p.starts_with(resolved(q))) {
        Some(q) => Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("Midna Dev never writes the installed Midna's files: {} is under {}", path.display(), q.display()),
        )),
        None => Ok(()),
    }
}

/// `path` with `.`/`..` folded and symlinks resolved as far as it exists, so a link into
/// production (or `~/x/../Library/…`) is caught too.
fn resolved(path: &Path) -> PathBuf {
    let mut lexical = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                lexical.pop();
            }
            Component::CurDir => {}
            c => lexical.push(c),
        }
    }
    let mut rest = Vec::new();
    let mut base = lexical.as_path();
    loop {
        if let Ok(real) = std::fs::canonicalize(base) {
            return rest.iter().rev().fold(real, |acc: PathBuf, c| acc.join(c));
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                base = parent;
            }
            _ => return lexical,
        }
    }
}

/// [`std::fs::canonicalize`], in the form midna stores and shows paths. On Windows that drops
/// the verbatim prefix canonicalize adds (`\\?\C:\x` is `C:\x`, `\\?\UNC\host\share` is
/// `\\host\share`) when the plain form names the same file: shorter than MAX_PATH, and no
/// component Windows reads differently without the prefix (a trailing dot or space, a device
/// name like `con`).
pub fn canonical(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    let real = std::fs::canonicalize(path)?;
    #[cfg(windows)]
    let real = plain(real);
    Ok(real)
}

#[cfg(windows)]
fn plain(p: PathBuf) -> PathBuf {
    let Some(s) = p.to_str() else { return p };
    let short = match (s.strip_prefix(r"\\?\UNC\"), s.strip_prefix(r"\\?\")) {
        (Some(unc), _) => format!(r"\\{unc}"),
        (None, Some(rest)) if rest.as_bytes().get(1) == Some(&b':') => rest.to_string(),
        _ => return p,
    };
    const DEVICES: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7",
        "lpt8", "lpt9",
    ];
    let odd = short.split('\\').any(|c| {
        let stem = c.split('.').next().unwrap_or(c);
        (c.ends_with('.') && c != "." && c != "..") || c.ends_with(' ') || DEVICES.iter().any(|d| d.eq_ignore_ascii_case(stem))
    });
    if short.len() >= 260 || odd { p } else { PathBuf::from(short) }
}

// ------------------------------------------------------------------ path text
//
// Paths midna keeps as text (projects, cwds, settings) on Windows come as `C:\x` from Windows,
// `C:/x` from git and either from people, and Windows ignores case. These read both; on macOS
// each is what the code had inline (`rsplit('/')`, `starts_with('/')`, …).

/// What separates a path's parts: `/`, and on Windows `\` too.
pub const SEPARATORS: &[char] = if cfg!(windows) { &['/', '\\'] } else { &['/'] };

fn is_sep(b: u8) -> bool {
    b == b'/' || (cfg!(windows) && b == b'\\')
}

/// A path's last part: `b` for `/a/b` and `/a/b/` (and on Windows `C:\a\b`); "" for a root.
pub fn base_name(p: &str) -> &str {
    p.trim_end_matches(SEPARATORS).rsplit(SEPARATORS).next().unwrap_or("")
}

/// A program as people type it: its [`base_name`], on Windows without `.exe` (or `.cmd`, `.bat`,
/// `.com`), so `C:\Program Files\Git\bin\bash.exe` is `bash`.
pub fn program_name(p: &str) -> &str {
    let b = base_name(p);
    if cfg!(windows) {
        for ext in [".exe", ".cmd", ".bat", ".com"] {
            if let Some(stem) = b.len().checked_sub(ext.len()).filter(|&n| n > 0 && b.is_char_boundary(n)).map(|n| &b[..n])
                && b[stem.len()..].eq_ignore_ascii_case(ext)
            {
                return stem;
            }
        }
    }
    b
}

/// An absolute path: `/…`, and on Windows `C:\…`, `C:/…` or `\\server\share\…` too.
pub fn is_absolute(p: &str) -> bool {
    p.starts_with('/') || (cfg!(windows) && Path::new(p).is_absolute())
}

/// `p` without trailing separators, except that a root keeps its own (`/`, `C:\`).
pub fn trim_end_sep(p: &str) -> &str {
    let t = p.trim_end_matches(SEPARATORS);
    let drive = cfg!(windows) && t.len() == 2 && t.as_bytes()[1] == b':';
    if t.is_empty() || drive { p.get(..t.len() + 1).unwrap_or(p) } else { t }
}

/// What of `path` follows `dir`: "" when they're the same folder, `/x` (or `\x`) when it's
/// inside, None otherwise. On Windows the separators are alike and case doesn't count. A root
/// `dir` covers nothing, as before these were shared.
pub fn after<'a>(path: &'a str, dir: &str) -> Option<&'a str> {
    let dir = dir.trim_end_matches(SEPARATORS).as_bytes();
    let p = path.as_bytes();
    let same = |a: &u8, b: &u8| if cfg!(windows) { a.eq_ignore_ascii_case(b) || (is_sep(*a) && is_sep(*b)) } else { a == b };
    if dir.is_empty() || p.len() < dir.len() || !p.iter().zip(dir).all(|(a, b)| same(a, b)) {
        return None;
    }
    let rest = path.get(dir.len()..)?;
    (rest.is_empty() || rest.starts_with(SEPARATORS)).then_some(rest)
}

/// Whether `path` is the folder `dir` or inside it (both absolute).
pub fn is_under(path: &str, dir: &str) -> bool {
    after(path, dir).is_some()
}

/// `path` from inside the folder `dir` (`src/a.rs` for `/p/src/a.rs` in `/p`), as written in
/// `path`; None when it isn't inside, or is `dir` itself.
pub fn inside<'a>(path: &'a str, dir: &str) -> Option<&'a str> {
    after(path, dir).map(|r| r.trim_start_matches(SEPARATORS)).filter(|r| !r.is_empty())
}

/// Whether `a` and `b` are one path. Off Windows they're equal; on Windows `C:/x`, `C:\x` and
/// `c:\X\` are one path too.
pub fn same_path(a: &str, b: &str) -> bool {
    if cfg!(windows) { after(a, b).is_some_and(|r| r.trim_matches(SEPARATORS).is_empty()) } else { a == b }
}

/// The home folder as text: `$HOME`; on Windows [`user_home`], since a Windows process often
/// has no `HOME`.
pub fn home_var() -> Option<String> {
    if cfg!(windows) { Some(user_home().to_string_lossy().into_owned()) } else { std::env::var("HOME").ok() }
}

/// `p` for matching by name: lowercase on Windows, where case doesn't count; as it is elsewhere.
pub fn case_folded(p: &str) -> std::borrow::Cow<'_, str> {
    if cfg!(windows) { p.to_lowercase().into() } else { p.into() }
}

/// A path git printed, the way this system writes paths: git on Windows prints `C:/Users/x`
/// where Windows and midna have `C:\Users\x`. Only a drive or UNC path changes.
pub fn native(p: &str) -> String {
    if cfg!(windows) && Path::new(p).is_absolute() { p.replace('/', "\\") } else { p.to_string() }
}

/// The file in a midna home's shims dir that says the `claude.exe` and `codex.exe` beside it are
/// midna's own (Windows: they're copies of midna.exe), so `midna shim` never runs one as the
/// real agent.
pub const SHIMS_MARKER: &str = "midna-shims.txt";

pub fn state_path(home: &std::path::Path) -> PathBuf {
    home.join("state.json")
}

pub fn events_path(home: &std::path::Path) -> PathBuf {
    home.join("events.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_is_a_plain_absolute_path() {
        let c = canonical(std::env::temp_dir()).unwrap();
        assert!(c.is_absolute() && c.is_dir(), "{}", c.display());
        assert!(!c.to_string_lossy().starts_with(r"\\?\"), "{}", c.display());
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_prefix_comes_off_only_when_the_plain_path_means_the_same() {
        let p = |s: &str| plain(PathBuf::from(s)).to_string_lossy().into_owned();
        assert_eq!(p(r"\\?\D:\Projects\taskboard"), r"D:\Projects\taskboard");
        assert_eq!(p(r"\\?\C:\"), r"C:\");
        assert_eq!(p(r"\\?\UNC\host\share\x"), r"\\host\share\x");
        // these need the prefix to mean what they say
        assert_eq!(p(r"\\?\C:\x\con"), r"\\?\C:\x\con");
        assert_eq!(p(r"\\?\C:\x\nul.txt"), r"\\?\C:\x\nul.txt");
        assert_eq!(p(r"\\?\C:\x\trailing."), r"\\?\C:\x\trailing.");
        assert_eq!(p(r"\\?\C:\x\trailing "), r"\\?\C:\x\trailing ");
        let long = format!(r"\\?\C:\{}", "a".repeat(300));
        assert_eq!(p(&long), long);
        assert_eq!(p(r"\\?\Volume{1234}\x"), r"\\?\Volume{1234}\x");
    }

    #[test]
    fn dev_build_ignores_the_inherited_home_and_socket() {
        let prod = Some("/Users/me/Library/Application Support/com.mrgnhnt.midna".into());
        let home = resolve_home(Some("/dev/home"), prod);
        assert_eq!(home, PathBuf::from("/dev/home"));
        let sock = resolve_socket(Some("/dev/home"), Some("/prod/midnad.sock".into()), home);
        assert_eq!(sock, PathBuf::from("/dev/home/midnad.sock"));
    }

    #[test]
    fn other_builds_honour_the_env() {
        assert_eq!(resolve_home(None, Some("/tmp/h".into())), PathBuf::from("/tmp/h"));
        assert_eq!(resolve_home(None, Some("".into())), production_home());
        assert_eq!(resolve_socket(None, Some("/tmp/s.sock".into()), "/tmp/h".into()), PathBuf::from("/tmp/s.sock"));
        assert_eq!(resolve_socket(None, None, "/tmp/h".into()), PathBuf::from("/tmp/h/midnad.sock"));
    }

    #[test]
    fn check_write_refuses_production_paths_however_spelled() {
        let tmp = std::env::temp_dir().join(format!("midna-guard-{}", std::process::id()));
        let prod = tmp.join("prod");
        let dev = tmp.join("dev");
        std::fs::create_dir_all(prod.join("bin")).unwrap();
        std::fs::create_dir_all(&dev).unwrap();
        let protected = [prod.clone()];
        assert!(check_write(&prod, &protected).is_err());
        assert!(check_write(&prod.join("bin/current"), &protected).is_err(), "inside, not yet existing");
        assert!(check_write(&dev.join("../prod/bin"), &protected).is_err(), "via ..");
        // Windows needs a privilege (or Developer Mode) to make a symlink.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&prod, dev.join("sneaky")).unwrap();
            assert!(check_write(&dev.join("sneaky/bin/current"), &protected).is_err(), "via a symlink");
        }
        assert!(check_write(&dev.join("bin/current"), &protected).is_ok());
        assert!(check_write(&tmp.join("prod-other"), &protected).is_ok(), "a sibling sharing the prefix");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn path_text_on_slashes() {
        assert_eq!(base_name("/a/b"), "b");
        assert_eq!(base_name("/a/b/"), "b");
        assert_eq!(base_name("/"), "");
        assert_eq!(program_name("/bin/zsh"), "zsh");
        assert!(is_absolute("/x") && !is_absolute("x/y") && !is_absolute("~/x"));
        assert_eq!(trim_end_sep("/a/b/"), "/a/b");
        assert_eq!(trim_end_sep("/"), "/");
        assert!(is_under("/p/src/a.rs", "/p") && is_under("/p", "/p/") && is_under("/p/", "/p"));
        assert!(!is_under("/pq", "/p") && !is_under("/p", "/p/src") && !is_under("/x", "/"));
        assert_eq!(inside("/p/src/a.rs", "/p"), Some("src/a.rs"));
        assert_eq!(inside("/p", "/p"), None);
        assert!(same_path("/a/b", "/a/b") && !same_path("/a/b", "/a/c"));
    }

    #[cfg(not(windows))]
    #[test]
    fn path_text_off_windows_is_as_written() {
        assert_eq!(base_name(r"C:\a\b"), r"C:\a\b");
        assert_eq!(program_name("/usr/bin/tool.exe"), "tool.exe");
        assert!(!is_absolute(r"C:\x"));
        assert!(!is_under("/P/x", "/p"), "case counts");
        assert_eq!(native("C:/a/b"), "C:/a/b");
    }

    #[cfg(windows)]
    #[test]
    fn path_text_on_windows() {
        assert_eq!(base_name(r"C:\a\b"), "b");
        assert_eq!(base_name(r"C:\a\b\"), "b");
        assert_eq!(base_name("C:/a/b"), "b");
        assert_eq!(program_name(r"C:\PROGRA~1\Git\usr\bin\sh.exe"), "sh");
        assert_eq!(program_name(r"C:\npm\claude.CMD"), "claude");
        assert_eq!(program_name(".exe"), ".exe");
        assert!(is_absolute(r"C:\x") && is_absolute("C:/x") && is_absolute(r"\\host\share\x") && is_absolute("/x"));
        assert!(!is_absolute(r"x\y") && !is_absolute("C:x"));
        assert_eq!(trim_end_sep(r"C:\a\"), r"C:\a");
        assert_eq!(trim_end_sep(r"C:\"), r"C:\");
        assert_eq!(trim_end_sep("C:/"), "C:/");
        assert!(is_under(r"C:\Projects\x\src", r"C:\Projects\x"));
        assert!(is_under("C:/projects/X/src", r"C:\Projects\x\"), "git's slashes, other case");
        assert!(!is_under(r"C:\Projects\xy", r"C:\Projects\x"));
        assert_eq!(inside(r"C:\p\src\a.rs", "C:/P"), Some(r"src\a.rs"));
        assert!(same_path(r"C:\a\b", "c:/A/B/") && !same_path(r"C:\a\b", r"C:\a\bc"));
        assert_eq!(native("C:/a/b"), r"C:\a\b");
        assert_eq!(native("//host/share/x"), r"\\host\share\x");
        assert_eq!(native("/r/x"), "/r/x");
    }
}
