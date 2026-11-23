//! Windows: one app per MIDNA_HOME, and asking it to quit.
//!
//! - One copy: the app creates a named "show" event at launch. When it's already there,
//!   another copy using this home is running: this launch sets it, which brings that copy's
//!   window to the front, and exits ([`claim`]). macOS does this itself (one app per bundle).
//! - Quitting: `midna-app.exe --quit` (what `scripts/dev-app-windows.sh` runs before it replaces
//!   the app, and `--uninstall` before it stops midnad). macOS asks over Apple Events instead.
//!   Closing the windows won't do: closing one main window of several asks what to do with its
//!   terminals, or closes them. The app waits on a named event and quits when it's set, the same
//!   as the Quit action.
//!
//! The names come from MIDNA_HOME, so Midna and Midna Dev (and a dev build on a temp home)
//! never hand over to, or quit, each other.
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::Threading::{CreateEventW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent, WaitForSingleObject};

/// `Local\midna-app-<what>-<hash of the home>` (an event name can't hold a `\` of its own).
fn event_name(what: &str) -> Vec<u16> {
    let home = midna_proto::paths::midna_home().to_string_lossy().to_lowercase();
    format!("Local\\midna-app-{what}-{:016x}", fnv(&home)).encode_utf16().chain(std::iter::once(0)).collect()
}

/// FNV-1a: stable across builds, unlike std's hasher.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// Create the auto-reset event `name`, not set, or open it when it's already there: its handle,
/// and whether it was already there.
fn make(name: &[u16]) -> std::io::Result<(usize, bool)> {
    // SAFETY: a NUL-terminated name; auto-reset, not set.
    let (event, existed) = unsafe {
        let e = CreateEventW(std::ptr::null(), 0, 0, name.as_ptr());
        (e, GetLastError() == ERROR_ALREADY_EXISTS)
    };
    if event.is_null() { Err(std::io::Error::last_os_error()) } else { Ok((event as usize, existed)) }
}

/// Call `f` on a thread of its own each time `event` is set (just the first time when `once`).
fn wait(event: usize, thread: &str, once: bool, mut f: impl FnMut() + Send + 'static) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(thread.into())
        .spawn(move || {
            // SAFETY: the handle stays open for the process's life.
            while unsafe { WaitForSingleObject(event as _, INFINITE) } == 0 {
                f();
                if once {
                    break;
                }
            }
        })
        .map(drop)
}

/// Set the event `name` if something created it. False when nothing did.
fn set(name: &[u16]) -> bool {
    // SAFETY: a NUL-terminated name; the handle is closed after use.
    unsafe {
        let event = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
        if event.is_null() {
            return false;
        }
        let ok = SetEvent(event) != 0;
        CloseHandle(event);
        ok
    }
}

/// Be this home's one app: true, and `show` is called (on another thread) whenever another
/// launch hands over to this one. False when another copy already runs: it has been asked to
/// come to the front, and this one should exit. An event that can't be made doesn't stop the
/// launch.
pub fn claim(show: impl FnMut() + Send + 'static) -> bool {
    let made = make(&event_name("show")).and_then(|(event, existed)| {
        if !existed {
            return wait(event, "midna-show-request", false, show).map(|()| true);
        }
        // This launch came from a click, so it may put a window in front: let the running
        // copy do it instead (without this its taskbar button only flashes).
        // SAFETY: plain calls; our own handle, closed after.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(windows_sys::Win32::UI::WindowsAndMessaging::ASFW_ANY);
            SetEvent(event as _);
            CloseHandle(event as _);
        }
        Ok(false)
    });
    made.unwrap_or_else(|e| {
        eprintln!("midna-app: show event: {e}");
        true
    })
}

/// Call `quit` (on another thread) when `--quit` asks this app to.
pub fn on_quit_request(quit: impl FnOnce() + Send + 'static) {
    let mut quit = Some(quit);
    let made = make(&event_name("quit")).and_then(|(event, _)| {
        wait(event, "midna-quit-request", true, move || {
            if let Some(q) = quit.take() {
                q();
            }
        })
    });
    if let Err(e) = made {
        eprintln!("midna-app: quit event: {e}");
    }
}

/// Is an app using this home running (one that claimed it)?
pub fn running() -> bool {
    let name = event_name("show");
    // SAFETY: a NUL-terminated name; the handle is closed after use.
    unsafe {
        let event = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
        !event.is_null() && CloseHandle(event) != 0
    }
}

/// Ask the app using this home to quit. False when none is running.
pub fn ask_to_quit() -> bool {
    set(&event_name("quit"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_to_make_an_event_waits_on_it_and_the_next_one_sets_it() {
        let name: Vec<u16> = format!("Local\\midna-app-test-{}", std::process::id()).encode_utf16().chain(std::iter::once(0)).collect();
        let (event, existed) = make(&name).unwrap();
        assert!(!existed);
        let (tx, rx) = std::sync::mpsc::channel();
        wait(event, "test-show", false, move || {
            let _ = tx.send(());
        }).unwrap();
        let (again, existed) = make(&name).unwrap();
        assert!(existed, "already there");
        unsafe { CloseHandle(again as _) };
        for _ in 0..2 {
            assert!(set(&name));
            rx.recv_timeout(std::time::Duration::from_secs(5)).expect("shown");
        }
        let other: Vec<u16> = "Local\\midna-app-test-nothing-made-this\0".encode_utf16().collect();
        assert!(!set(&other));
    }

    #[test]
    fn a_home_hashes_the_same_in_every_build() {
        assert_eq!(fnv(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv("a"), 0xaf63_dc4c_8601_ec8c);
    }
}
