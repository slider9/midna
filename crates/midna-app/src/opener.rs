//! Hand a URL, file or folder to whatever the system opens it with, or show a file in the file
//! manager. macOS call sites run `/usr/bin/open` themselves; this is what the other platforms
//! use instead (Windows: `ShellExecuteW` and Explorer; elsewhere `xdg-open`).
#![cfg_attr(target_os = "macos", allow(dead_code))]
#[cfg(all(unix, not(target_os = "macos")))]
use std::path::Path;

/// Open `target` (a URL or a path) with its default handler. Off the calling thread.
#[cfg(windows)]
pub fn open(target: &str) {
    let target = target.to_string();
    std::thread::spawn(move || shell_execute(&target));
}

#[cfg(windows)]
fn shell_execute(target: &str) {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (verb, file) = (wide("open"), wide(target));
    // SAFETY: NUL-terminated UTF-16 strings that outlive the call; no parent window.
    let r = unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
    // ShellExecuteW returns a value > 32 on success.
    if r as usize <= 32 {
        eprintln!("midna-app: couldn't open {target} (ShellExecute {})", r as usize);
    }
}

/// Show `path` selected in Explorer.
#[cfg(windows)]
pub fn reveal(path: &str) {
    let path = path.replace('/', "\\");
    let _ = std::process::Command::new("explorer.exe").arg(format!("/select,{path}")).spawn();
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn open(target: &str) {
    let _ = std::process::Command::new("xdg-open").arg(target).stdin(std::process::Stdio::null()).spawn();
}

/// No portable "select in file manager" on Linux: open the folder it's in.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn reveal(path: &str) {
    let dir = Path::new(path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string());
    open(&dir);
}

#[cfg(target_os = "macos")]
pub fn open(target: &str) {
    let _ = std::process::Command::new("/usr/bin/open").arg(target).stdin(std::process::Stdio::null()).spawn();
}

#[cfg(target_os = "macos")]
pub fn reveal(path: &str) {
    let _ = std::process::Command::new("/usr/bin/open").arg("-R").arg(path).spawn();
}
