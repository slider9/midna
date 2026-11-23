//! Text that names the platform: macOS's words and keys on macOS, Windows' everywhere else.
//!
//! Text written for macOS says ⌘, ⌥, Mac, Finder and System Settings. Off macOS the shortcuts
//! are other keys (`docs/STATUS-windows.md`, "Shortcuts": ⌘X is Ctrl+Shift+X in the terminal's
//! place, ⇧⌘X Ctrl+Alt+X, ⌥⌘X Alt+Shift+X, and plain Ctrl in overlays and text fields), and the
//! names are Windows' own. [`os`] picks one of two texts when it's compiled, so the macOS text
//! is the literal it always was and works in `const` and `static` tables; the names below are
//! for text put together at run time.

/// `mac` on macOS, `other` everywhere else.
pub const fn os(mac: &'static str, other: &'static str) -> &'static str {
    if cfg!(target_os = "macos") { mac } else { other }
}

/// The computer midna runs on: "Mac" or "PC".
pub const MAC: &str = os("Mac", "PC");
/// The file manager: "Finder" or "File Explorer".
pub const FINDER: &str = os("Finder", "File Explorer");
/// The system's settings app: "System Settings" or "Settings".
pub const SYSTEM_SETTINGS: &str = os("System Settings", "Settings");
/// Where apps that start at login are switched on and off.
pub const LOGIN_ITEMS: &str = os("System Settings ▸ General ▸ Login Items", "Settings ▸ Apps ▸ Startup");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_this_platforms_text() {
        if cfg!(target_os = "macos") {
            assert_eq!((MAC, FINDER, os("⌘K", "Ctrl+Shift+K")), ("Mac", "Finder", "⌘K"));
        } else {
            assert_eq!((MAC, FINDER, os("⌘K", "Ctrl+Shift+K")), ("PC", "File Explorer", "Ctrl+Shift+K"));
            assert_eq!(LOGIN_ITEMS, "Settings ▸ Apps ▸ Startup");
        }
    }
}
