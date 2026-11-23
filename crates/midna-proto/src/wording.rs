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
/// Keys written in hints: ↩ ⇧↩ ⇥ ⇧⇥, or Enter, Shift+Enter, Tab, Shift+Tab.
pub const ENTER: &str = os("↩", "Enter");
pub const SHIFT_ENTER: &str = os("⇧↩", "Shift+Enter");
pub const TAB: &str = os("⇥", "Tab");
pub const SHIFT_TAB: &str = os("⇧⇥", "Shift+Tab");
/// Where apps that start at login are switched on and off.
pub const LOGIN_ITEMS: &str = os("System Settings ▸ General ▸ Login Items", "Settings ▸ Apps ▸ Startup");

/// The first word or key in `text` that only macOS has: ⌘ ⌥ ⌃ ⇧ ↩ ⇥, Mac (not MacBook or
/// macro), macOS, or Finder. For tests that check what other platforms show.
pub fn mac_words(text: &str) -> Option<&'static str> {
    const GLYPHS: [&str; 6] = ["⌘", "⌥", "⌃", "⇧", "↩", "⇥"];
    if let Some(g) = GLYPHS.iter().find(|g| text.contains(**g)) {
        return Some(g);
    }
    let words = text.split(|c: char| !c.is_alphanumeric());
    for w in words {
        match w {
            "Mac" | "Macs" => return Some("Mac"),
            "macOS" => return Some("macOS"),
            "Finder" => return Some("Finder"),
            _ => {}
        }
    }
    None
}

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

    #[test]
    fn finds_mac_words() {
        assert_eq!(mac_words("Press ⌘K, then go"), Some("⌘"));
        assert_eq!(mac_words("while the Mac's asleep"), Some("Mac"));
        assert_eq!(mac_words("Reveal in Finder"), Some("Finder"));
        assert_eq!(mac_words("macOS's memory pressure"), Some("macOS"));
        assert_eq!(mac_words("a MacBook, a macro, the machine, Ctrl+Shift+K"), None);
    }
}
