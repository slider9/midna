//! Portable bits for the CLI tests (the same as midnad's `tests/common`).
#![allow(dead_code)]
use std::path::PathBuf;

/// The folder tests open terminals in and keep their daemon homes in. Unix: `/tmp`, as written,
/// so the paths tests compare stay what they always were. Windows: the user's temp folder,
/// canonical (`paths::canonical`, the form midnad stores cwds in). Either way it's short, so a
/// home under it keeps `midnad.sock` inside the 104/108-byte socket path limit.
pub fn tmp() -> PathBuf {
    #[cfg(unix)]
    return PathBuf::from("/tmp");
    #[cfg(windows)]
    return midna_proto::paths::canonical(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
}

/// [`tmp`] as a string, for arguments and JSON.
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

/// A POSIX tool as a program to start (`echo`): `/bin/{name}` on Unix, Git for Windows'
/// `usr\bin\{name}.exe` on Windows (its 8.3 path, see [`short`]).
pub fn posix_bin(name: &str) -> String {
    #[cfg(unix)]
    return format!("/bin/{name}");
    #[cfg(windows)]
    {
        // posix_sh is <git>\bin\bash.exe.
        let bash = PathBuf::from(midnad::sys::posix_sh());
        match bash.parent().and_then(|b| b.parent()) {
            Some(git) if bash.is_absolute() => short(git.join("usr").join("bin").join(format!("{name}.exe"))).to_string_lossy().into_owned(),
            _ => format!("{name}.exe"),
        }
    }
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
/// already. Windows keeps the environment behind a lock of its own, so setting it while other
/// threads run is sound there.
#[cfg(windows)]
pub fn git_on_path() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
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
