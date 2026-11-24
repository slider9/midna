//! midnad's own toasts on Windows: what `notify::post` shows when no app is connected (`via:
//! system`), and the sounds `notify::play` plays then. The macOS daemon uses osascript and
//! afplay for these.
//!
//! They're posted under the app's AppUserModelID (`midna_proto::toast::app_id`; Midna Dev's and
//! debug builds' is `com.mrgnhnt.midna.dev`), with the app's XML, tag and group, so they show as
//! Midna's, the app's `clear` takes them away once you look at their terminal, and a click carries
//! what the app's activator reads. The app registers that id, and the class that takes clicks, at
//! every launch; a release build also registers itself as the class's server, so a click on one
//! of these after the app quit starts it (`-Embedding`) and then delivers the click. midnad only
//! registers the id's name and icon when the app never has, so a toast shows at all.
//!
//! Every WinRT call runs on one thread of its own (a multithreaded apartment).
//!
//! Sounds: a toast plays only Windows' own sounds, so one with a WAV file is posted silent and the
//! file plays here (`PlaySoundW`) at its volume (`notify_media::rendered` scales it), unless
//! Windows has midna's toasts off or isn't taking notifications (quiet hours, presentation mode,
//! a full-screen app). Any other file gets Windows' notification sound instead.
use midna_proto::notify::Posted;
use midna_proto::toast::{self, About};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, mpsc};
use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{NotificationSetting, ToastNotification, ToastNotificationManager, ToastNotifier};
use windows::core::HSTRING;

static WORKER: OnceLock<mpsc::Sender<Cmd>> = OnceLock::new();

enum Cmd {
    Toast(Toast),
    /// A sound with no toast: the file and its volume (1–100).
    Play(PathBuf, String, u8),
}

struct Toast {
    home: PathBuf,
    xml: String,
    tag: String,
    group: String,
    /// Take away the toasts with this tag first (a `notify.send` id sent again).
    replace: bool,
    /// A WAV file to play with it, and how loud (1–100).
    play: Option<(String, u8)>,
}

/// The app's rule: Midna Dev and debug builds use Midna Dev's id (a test build its own).
fn dev() -> bool {
    midna_proto::paths::is_dev() || cfg!(debug_assertions)
}

/// Show `p` as a toast about `session` (None: about midna in general), and play its sound.
pub fn show(home: &Path, p: &Posted, session: Option<&str>) {
    static N: AtomicU64 = AtomicU64::new(0);
    let tag = match p.id.as_deref() {
        Some(id) => toast::tag_for(id),
        None => format!("d{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)),
    };
    let file = p.sound_file.clone().filter(|f| p.sound && is_wav(Path::new(f)));
    let about = About { session: session.unwrap_or_default().to_string(), needs_you: p.needs_you_id.clone(), id: p.id.clone(), open: p.open.clone() };
    let body = midna_proto::notify::plain_body(&p.body);
    send(Cmd::Toast(Toast {
        home: home.to_path_buf(),
        xml: toast::xml(&p.title, &body, p.image.as_deref(), &p.actions, p.sound && file.is_none(), &about),
        tag,
        group: toast::group_for(session),
        replace: p.id.is_some(),
        play: file.map(|f| (f, p.volume.unwrap_or(100))),
    }));
}

/// Play `file` at `volume` (1–100), with no toast. Only a WAV plays.
pub fn play(home: &Path, file: &str, volume: u8) {
    send(Cmd::Play(home.to_path_buf(), file.to_string(), volume));
}

fn send(cmd: Cmd) {
    let tx = WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        if let Err(e) = std::thread::Builder::new().name("notify-toasts".into()).spawn(move || worker(rx)) {
            eprintln!("midnad: no toast thread: {e}");
        }
        tx
    });
    let _ = tx.send(cmd);
}

fn worker(rx: mpsc::Receiver<Cmd>) {
    use windows_sys::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
    // SAFETY: this thread joins the multithreaded apartment once and stays in it.
    unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) };
    let id = HSTRING::from(toast::app_id(dev()));
    let mut notifier: Option<ToastNotifier> = None;
    for cmd in rx {
        match cmd {
            Cmd::Play(home, file, volume) => play_now(&home, &file, volume),
            Cmd::Toast(t) => {
                if notifier.is_none() {
                    register(&t.home);
                    notifier = ToastNotificationManager::CreateToastNotifierWithId(&id).map_err(|e| eprintln!("midnad: no toast notifier: {e}")).ok();
                }
                let Some(n) = &notifier else { continue };
                if let Err(e) = show_now(n, &id, &t) {
                    eprintln!("midnad: toast: {e}");
                }
            }
        }
    }
}

fn show_now(n: &ToastNotifier, app: &HSTRING, t: &Toast) -> windows::core::Result<()> {
    if t.replace {
        // The same id under another terminal is another toast to Windows: take it away too.
        let history = ToastNotificationManager::History()?;
        let tag = HSTRING::from(&t.tag);
        for old in history.GetHistoryWithId(app)? {
            if old.Tag()? == tag {
                history.RemoveGroupedTagWithId(&tag, &old.Group()?, app)?;
            }
        }
    }
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(&t.xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    toast.SetTag(&HSTRING::from(&t.tag))?;
    toast.SetGroup(&HSTRING::from(&t.group))?;
    n.Show(&toast)?;
    let on = n.Setting().map_or(true, |s| s == NotificationSetting::Enabled);
    if let Some((file, volume)) = &t.play
        && on
        && taking_notifications()
    {
        play_now(&t.home, file, *volume);
    }
    Ok(())
}

/// `PlaySoundW`, which plays one sound at a time (a new one cuts off the last) and has no volume
/// of its own: below 100 it plays the file scaled to `volume` (`notify_media::rendered`).
fn play_now(home: &Path, file: &str, volume: u8) {
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};
    if volume == 0 || !is_wav(Path::new(file)) {
        return;
    }
    let path = if volume < 100 { crate::notify_media::rendered(home, file, volume).unwrap_or_else(|| file.into()) } else { file.into() };
    let wide = wide(&path.to_string_lossy());
    // SAFETY: a NUL-terminated path; SND_ASYNC copies what it needs before returning.
    if unsafe { PlaySoundW(wide.as_ptr(), std::ptr::null_mut(), SND_FILENAME | SND_ASYNC | SND_NODEFAULT) } == 0 {
        eprintln!("midnad: couldn't play {}", path.display());
    }
}

/// Is `file` a WAV (the only kind `PlaySoundW` plays)?
fn is_wav(file: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 12];
    std::fs::File::open(file).and_then(|mut f| f.read_exact(&mut head)).is_ok() && &head[..4] == b"RIFF" && &head[8..] == b"WAVE"
}

/// Is Windows taking notifications now (not quiet hours, presentation mode or a full-screen
/// app)? Yes when it can't say.
fn taking_notifications() -> bool {
    use windows_sys::Win32::UI::Shell::{QUNS_ACCEPTS_NOTIFICATIONS, QUNS_APP, SHQueryUserNotificationState};
    let mut state = QUNS_ACCEPTS_NOTIFICATIONS;
    // SAFETY: writes one QUERY_USER_NOTIFICATION_STATE.
    let ok = unsafe { SHQueryUserNotificationState(&mut state) } >= 0;
    !ok || state == QUNS_ACCEPTS_NOTIFICATIONS || state == QUNS_APP
}

/// Windows shows no toast for an id it has no name for. The app registers its id at every launch
/// (name, icon and click handler); when it never has, midnad gives it the name and icon (kept in
/// the home, where the app keeps it too) and leaves the rest to the app.
fn register(home: &Path) {
    let key = format!("Software\\Classes\\AppUserModelId\\{}", toast::app_id(dev()));
    if reg_has(&key, "DisplayName") {
        return;
    }
    let (name, png): (&str, &[u8]) = if dev() {
        (if midna_proto::paths::TEST_BUILD { "Midna Test" } else { "Midna Dev" }, include_bytes!("../../../packaging/assets/MidnaDev.png"))
    } else {
        ("Midna", include_bytes!("../../../packaging/assets/Midna.png"))
    };
    let icon = home.join("toast-icon.png");
    let written = midna_proto::paths::guard_write(&icon).and_then(|_| std::fs::create_dir_all(home)).and_then(|_| std::fs::write(&icon, png));
    if let Err(e) = written {
        eprintln!("midnad: toast icon {}: {e}", icon.display());
    }
    for (value, data) in [("DisplayName", name.to_string()), ("IconUri", icon.to_string_lossy().into_owned())] {
        if let Err(e) = reg_set(&key, value, &data) {
            eprintln!("midnad: registry {key}\\{value}: {e}");
        }
    }
}

fn reg_has(key: &str, value: &str) -> bool {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
    let (key, value) = (wide(key), wide(value));
    // SAFETY: NUL-terminated strings; with no buffer it only says whether the value is there.
    unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) == 0 }
}

/// Set a string value under HKCU, making the key if it isn't there.
fn reg_set(key: &str, value: &str, data: &str) -> std::io::Result<()> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
    let (key, value, data) = (wide(key), wide(value), wide(data));
    // SAFETY: NUL-terminated strings; `data`'s size in bytes includes its NUL.
    let r = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32) };
    if r == 0 { Ok(()) } else { Err(std::io::Error::from_raw_os_error(r as i32)) }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_wav_plays() {
        let dir = std::env::temp_dir().join(format!("midnad-toast-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (wav, aiff) = (dir.join("a.wav"), dir.join("a.aiff"));
        std::fs::write(&wav, b"RIFF\0\0\0\0WAVEfmt ").unwrap();
        std::fs::write(&aiff, b"FORM\0\0\0\0AIFF").unwrap();
        assert!(is_wav(&wav));
        assert!(!is_wav(&aiff));
        assert!(!is_wav(&dir.join("missing.wav")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
