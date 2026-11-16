//! Windows: ask a running app to quit (`midna-app.exe --quit`, what `scripts/dev-app-windows.sh`
//! runs before it replaces the app). macOS asks over Apple Events instead. Closing the windows
//! won't do: closing one main window of several asks what to do with its terminals, or closes
//! them. The app waits on a named event and quits when it's set, the same as the Quit action.
//! The name comes from MIDNA_HOME, so only the app using that home quits.
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{CreateEventW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent, WaitForSingleObject};

/// `Local\midna-app-quit-<hash of the home>` (an event name can't hold a `\` of its own).
fn event_name() -> Vec<u16> {
    let home = midna_proto::paths::midna_home().to_string_lossy().to_lowercase();
    // FNV-1a: stable across builds, unlike std's hasher.
    let hash = home.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3));
    format!("Local\\midna-app-quit-{hash:016x}").encode_utf16().chain(std::iter::once(0)).collect()
}

/// Call `quit` (on another thread) when `--quit` asks this app to.
pub fn on_quit_request(quit: impl FnOnce() + Send + 'static) {
    let name = event_name();
    // SAFETY: a NUL-terminated name; auto-reset, not set.
    let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
    if event.is_null() {
        eprintln!("midna-app: quit event: {}", std::io::Error::last_os_error());
        return;
    }
    let event = event as usize;
    std::thread::Builder::new()
        .name("midna-quit-request".into())
        .spawn(move || {
            // SAFETY: the handle stays open for the process's life.
            if unsafe { WaitForSingleObject(event as _, INFINITE) } == 0 {
                quit();
            }
        })
        .expect("spawn quit-request thread");
}

/// Ask the app using this home to quit. False when none is running.
pub fn ask_to_quit() -> bool {
    let name = event_name();
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
