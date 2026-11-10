//! Windows toasts for midnad's `notify.posted` events: the API of the macOS code in `notify.rs`,
//! through WinRT's `ToastNotificationManager`.
//!
//! An app that isn't packaged needs an AppUserModelID for its toasts. midna registers one for
//! the current user, `HKCU\Software\Classes\AppUserModelId\com.mrgnhnt.midna` (Midna Dev's and
//! debug builds' end in `.dev`), with the name and icon its toasts show, and posts under it.
//! That needs no installer or Start menu shortcut.
//!
//! A toast's group is its terminal, so `clear` takes a terminal's toasts away; its tag is
//! `notify.send`'s id, so a later one with that id replaces it and `withdraw` finds it.
//!
//! Clicks and buttons: the app id names a COM class (`CustomActivator`) that midna serves while it
//! runs ([`Activator`]). Windows calls it for a click on a toast or one of its buttons, from the
//! banner or from Action Center; the toast's own `Activated` event never fires for a click on its
//! body in Action Center. Each toast carries what a click reports (its terminal, needs-you item,
//! id and URL) as JSON in its arguments. A release build also registers itself as the class's
//! server, so a click after midna quit starts it (`-Embedding`) and then delivers the click.
//! Debug builds don't: a click on one of theirs while they're not running does nothing.
//! Dismissals come from the toast's own `Dismissed` event, while midna runs. All of them reach
//! the app as `Clicked`, on the channel macOS's delegate uses.
//!
//! Every WinRT call runs on one thread of its own (a multithreaded apartment), so the main thread
//! never waits on the notification platform.
//!
//! Sounds: a toast plays only Windows' own sounds, so one with a sound file is posted silent and
//! the file plays here (`PlaySoundW`), unless Windows has midna's toasts off or isn't taking
//! notifications (quiet hours, presentation mode, a full-screen app). One without a file plays
//! Windows' notification sound.
use super::{Clicked, Permission};
use midna_proto::notify::{Posted, Response, ResponseKind};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicI8, AtomicU64, Ordering};
use std::sync::{OnceLock, mpsc};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{
    NotificationSetting, ToastDismissalReason, ToastDismissedEventArgs, ToastFailedEventArgs, ToastNotification, ToastNotificationManager, ToastNotifier,
};
use windows::Win32::Foundation::CLASS_E_NOAGGREGATION;
use windows::Win32::System::Com::{CLSCTX_LOCAL_SERVER, CoRegisterClassObject, IClassFactory, IClassFactory_Impl, REGCLS_MULTIPLEUSE};
use windows::Win32::UI::Notifications::{INotificationActivationCallback, INotificationActivationCallback_Impl, NOTIFICATION_USER_INPUT_DATA};
use windows::core::{BOOL, GUID, HSTRING, IUnknown, Interface, PCWSTR, Ref, implement};

static CLICKS: OnceLock<async_channel::Sender<Clicked>> = OnceLock::new();
/// What Windows says about midna's toasts, as last read: -1 not read yet, 1 off, 2 on.
static STATUS: AtomicI8 = AtomicI8::new(-1);
static WORKER: OnceLock<mpsc::Sender<Cmd>> = OnceLock::new();

/// Toasts held on to, newest last: a toast's `Dismissed` event fires only while something holds it.
const KEEP: usize = 64;
/// Windows' limit on a toast's tag and on its group.
const TAG_MAX: usize = 64;
/// Windows shows at most this many buttons.
const ACTIONS_MAX: usize = 5;
/// The COM class Windows calls when one of midna's toasts is clicked.
const ACTIVATOR: GUID = GUID::from_u128(0x71fc9e00_807f_45b6_a69b_8163f48f5562);
/// Midna Dev's and debug builds' class.
const ACTIVATOR_DEV: GUID = GUID::from_u128(0xb130edb7_f2f7_45b7_8aa1_ff07b076ee68);

/// The AppUserModelID midna's toasts are posted under. Midna Dev and debug builds (run from
/// `target\`) have their own, so a build from source never shows as, or changes, Midna's.
fn app_id() -> &'static str {
    if dev() { "com.mrgnhnt.midna.dev" } else { midna_proto::paths::BUNDLE_ID }
}

fn activator() -> GUID {
    if dev() { ACTIVATOR_DEV } else { ACTIVATOR }
}

fn dev() -> bool {
    midna_proto::paths::is_dev() || cfg!(debug_assertions)
}

enum Cmd {
    Post(Toast),
    Withdraw(String),
    Clear(String),
    ClearAll,
    Refresh,
}

struct Toast {
    xml: String,
    tag: String,
    group: String,
    /// A sound file to play with it, and how loud (0–100).
    play: Option<(String, u8)>,
    about: About,
}

/// What a click on a toast reports.
#[derive(Clone, Debug, Default, PartialEq)]
struct About {
    session: String,
    needs_you: Option<String>,
    id: Option<String>,
    open: Option<String>,
}

/// Whether Windows shows midna's toasts (Settings ▸ System ▸ Notifications & actions). There's
/// nothing to ask for: they're on until the human turns them off.
pub fn permission() -> Permission {
    match STATUS.load(Ordering::Relaxed) {
        -1 => Permission::Unknown,
        1 => Permission::Denied,
        _ => Permission::Allowed,
    }
}

/// Register midna's app id, serve its clicks, and read whether its toasts are on. Call once, at
/// launch.
pub fn start(clicks: async_channel::Sender<Clicked>) {
    let _ = CLICKS.set(clicks);
    send(Cmd::Refresh);
}

/// Re-read whether midna's toasts are on (the human may have changed it in Settings).
pub fn refresh_permission() {
    send(Cmd::Refresh);
}

/// Windows never asks: read the setting again.
pub fn request_permission() {
    send(Cmd::Refresh);
}

/// Show one toast about `session` (None: about midna in general).
pub fn post(p: &Posted, session: Option<&str>) {
    static N: AtomicU64 = AtomicU64::new(0);
    let tag = match p.id.as_deref() {
        Some(id) => tag_for(id),
        None => format!("{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)),
    };
    let file = p.sound_file.clone().filter(|_| p.sound);
    let body = midna_proto::notify::plain_body(&p.body);
    let about = About { session: session.unwrap_or_default().to_string(), needs_you: p.needs_you_id.clone(), id: p.id.clone(), open: p.open.clone() };
    send(Cmd::Post(Toast {
        xml: toast_xml(&p.title, &body, p.image.as_deref(), &p.actions, p.sound && file.is_none(), &about),
        tag,
        group: group_for(session),
        play: file.map(|f| (f, p.volume.unwrap_or(100))),
        about,
    }));
}

/// Take a `notify.send` toast away, on screen or in Action Center (`notify.withdraw`).
pub fn withdraw(id: &str) {
    send(Cmd::Withdraw(tag_for(id)));
}

/// Take every midna toast away (`notify.clear`).
pub fn clear_all() {
    send(Cmd::ClearAll);
}

/// Take `session`'s toasts away: you're looking at its terminal now.
pub fn clear(session: &str) {
    send(Cmd::Clear(group_for(Some(session))));
}

pub fn open_url(url: &str) {
    crate::opener::open(url);
}

/// Play a sound file: `PlaySoundW`, which plays one sound at a time (a new one cuts off the
/// last) and has no volume of its own, so `volume` only silences at 0.
pub fn play(file: &str, volume: u8) {
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};
    if volume == 0 {
        return;
    }
    let wide = wide(file);
    // SAFETY: a NUL-terminated path; SND_ASYNC copies what it needs before returning.
    unsafe { PlaySoundW(wide.as_ptr(), std::ptr::null_mut(), SND_FILENAME | SND_ASYNC | SND_NODEFAULT) };
}

fn send(cmd: Cmd) {
    let tx = WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        if let Err(e) = std::thread::Builder::new().name("notify-toasts".into()).spawn(move || worker(rx)) {
            eprintln!("notify: no toast thread: {e}");
        }
        tx
    });
    let _ = tx.send(cmd);
}

fn worker(rx: mpsc::Receiver<Cmd>) {
    use windows_sys::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
    // SAFETY: this thread joins the multithreaded apartment once and stays in it.
    unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) };
    register();
    let factory: IClassFactory = Factory.into();
    // SAFETY: a class object COM holds (and calls from its own threads) until midna exits.
    if let Err(e) = unsafe { CoRegisterClassObject(&activator(), &factory, CLSCTX_LOCAL_SERVER, REGCLS_MULTIPLEUSE) } {
        eprintln!("notify: clicks on toasts won't arrive: {e}");
    }
    let id = HSTRING::from(app_id());
    let notifier = match ToastNotificationManager::CreateToastNotifierWithId(&id) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("notify: no toast notifier: {e}");
            STATUS.store(1, Ordering::Relaxed);
            return;
        }
    };
    let mut kept: VecDeque<(String, ToastNotification)> = VecDeque::new();
    for cmd in rx {
        let done = match cmd {
            Cmd::Refresh => {
                refresh(&notifier);
                Ok(())
            }
            Cmd::Post(t) => show(&notifier, &id, t, &mut kept),
            Cmd::Withdraw(tag) => remove_tag(&notifier, &id, &tag, &mut kept),
            Cmd::Clear(group) => ToastNotificationManager::History().and_then(|h| h.RemoveGroupWithId(&HSTRING::from(group), &id)),
            Cmd::ClearAll => ToastNotificationManager::History().and_then(|h| h.ClearWithId(&id)),
        };
        if let Err(e) = done {
            eprintln!("notify: {e}");
        }
    }
}

/// Is `NotificationSetting` on? A notifier that can't say (an id Windows doesn't know yet)
/// counts as on: `show` finds out.
fn refresh(n: &ToastNotifier) -> bool {
    let on = n.Setting().map_or(true, |s| s == NotificationSetting::Enabled);
    STATUS.store(if on { 2 } else { 1 }, Ordering::Relaxed);
    on
}

fn show(n: &ToastNotifier, app: &HSTRING, t: Toast, kept: &mut VecDeque<(String, ToastNotification)>) -> windows::core::Result<()> {
    if t.about.id.is_some() {
        // The same id under another terminal is another toast to Windows: replace it too.
        let _ = remove_tag(n, app, &t.tag, kept);
    }
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(&t.xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    toast.SetTag(&HSTRING::from(&t.tag))?;
    toast.SetGroup(&HSTRING::from(&t.group))?;
    let about = t.about.clone();
    toast.Dismissed(&TypedEventHandler::<ToastNotification, ToastDismissedEventArgs>::new(move |_, args: Ref<ToastDismissedEventArgs>| {
        // Only the human's dismissal: not one that timed out into Action Center or that midna took away.
        if about.id.is_some() && args.as_ref().and_then(|a| a.Reason().ok()) == Some(ToastDismissalReason::UserCanceled) {
            report(&about, Response { kind: ResponseKind::Dismissed, action: None });
        }
        Ok(())
    }))?;
    toast.Failed(&TypedEventHandler::<ToastNotification, ToastFailedEventArgs>::new(|_, args: Ref<ToastFailedEventArgs>| {
        if let Some(e) = args.as_ref().and_then(|a| a.ErrorCode().ok()) {
            eprintln!("notify: toast failed: {}", windows::core::Error::from_hresult(e));
        }
        Ok(())
    }))?;
    n.Show(&toast)?;
    if let Some((file, volume)) = t.play
        && refresh(n)
        && taking_notifications()
    {
        play(&file, volume);
    }
    kept.retain(|(tag, _)| *tag != t.tag);
    kept.push_back((t.tag, toast));
    while kept.len() > KEEP {
        kept.pop_front();
    }
    Ok(())
}

/// Take the toasts tagged `tag` off the screen and out of Action Center, whichever group.
fn remove_tag(n: &ToastNotifier, app: &HSTRING, tag: &str, kept: &mut VecDeque<(String, ToastNotification)>) -> windows::core::Result<()> {
    for (_, t) in kept.iter().filter(|(k, _)| k == tag) {
        let _ = n.Hide(t);
    }
    kept.retain(|(k, _)| k != tag);
    let history = ToastNotificationManager::History()?;
    let tag = HSTRING::from(tag);
    for t in history.GetHistoryWithId(app)? {
        if t.Tag()? == tag {
            history.RemoveGroupedTagWithId(&tag, &t.Group()?, app)?;
        }
    }
    Ok(())
}

fn report(about: &About, response: Response) {
    if let Some(tx) = CLICKS.get() {
        // midna's own toasts have nothing to report, only a terminal to show.
        let response = about.id.is_some().then_some(response);
        let _ = tx.try_send(Clicked { session: about.session.clone(), needs_you: about.needs_you.clone(), id: about.id.clone(), open: about.open.clone(), response });
    }
}

/// Windows calls this for a click on one of midna's toasts or its buttons (`CustomActivator`).
#[implement(INotificationActivationCallback)]
struct Activator;

impl INotificationActivationCallback_Impl for Activator_Impl {
    fn Activate(&self, _app: &PCWSTR, args: &PCWSTR, _data: *const NOTIFICATION_USER_INPUT_DATA, _count: u32) -> windows::core::Result<()> {
        // SAFETY: Windows passes a NUL-terminated string, or null.
        let args = if args.is_null() { String::new() } else { unsafe { args.to_string() }.unwrap_or_default() };
        // No arguments: midna's own name in Action Center was clicked, not a toast.
        if let Some((about, action)) = parse_args(&args) {
            let response = match action {
                Some(label) => Response { kind: ResponseKind::Action, action: Some(label) },
                None => Response { kind: ResponseKind::Clicked, action: None },
            };
            report(&about, response);
        }
        Ok(())
    }
}

/// Makes [`Activator`]s for COM.
#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(&self, outer: Ref<IUnknown>, iid: *const GUID, object: *mut *mut core::ffi::c_void) -> windows::core::Result<()> {
        if outer.as_ref().is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let callback: INotificationActivationCallback = Activator.into();
        // SAFETY: COM's out-parameter, filled with the interface it asked for.
        unsafe { callback.query(iid, object) }.ok()
    }

    fn LockServer(&self, _lock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

/// A toast's (or a button's) arguments: what a click reports, and the button's label.
fn click_args(about: &About, action: Option<&str>) -> String {
    serde_json::json!({ "s": about.session, "n": about.needs_you, "i": about.id, "o": about.open, "a": action }).to_string()
}

fn parse_args(args: &str) -> Option<(About, Option<String>)> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    Some((About { session: s("s").unwrap_or_default(), needs_you: s("n"), id: s("i"), open: s("o") }, s("a")))
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

/// Register midna's app id for this user: the name and icon its toasts show (the icon kept in
/// midna's home) and the class that takes their clicks. A release build also registers this exe
/// as that class's server. Rewritten at every launch, so they follow the build and home that ran
/// last.
fn register() {
    let (name, png): (&str, &[u8]) = if dev() {
        ("Midna Dev", include_bytes!("../../../packaging/assets/MidnaDev.png"))
    } else {
        ("Midna", include_bytes!("../../../packaging/assets/Midna.png"))
    };
    let icon = midna_proto::paths::midna_home().join("toast-icon.png");
    if std::fs::read(&icon).ok().as_deref() != Some(png) {
        let written = midna_proto::paths::guard_write(&icon)
            .and_then(|_| std::fs::create_dir_all(icon.parent().unwrap_or(&icon)))
            .and_then(|_| std::fs::write(&icon, png));
        if let Err(e) = written {
            eprintln!("notify: toast icon {}: {e}", icon.display());
        }
    }
    let clsid = braced(&activator());
    let app = format!("Software\\Classes\\AppUserModelId\\{}", app_id());
    set_value(&app, Some("DisplayName"), name);
    set_value(&app, Some("IconUri"), &icon.to_string_lossy());
    set_value(&app, Some("CustomActivator"), &clsid);
    // A debug build started from a toast would run on the default home, not the one it was
    // tried on.
    if !dev()
        && let Ok(exe) = std::env::current_exe()
    {
        set_value(&format!("Software\\Classes\\CLSID\\{clsid}\\LocalServer32"), None, &format!("\"{}\"", exe.display()));
    }
}

/// Set a string value under `HKEY_CURRENT_USER` (None: the key's default value), creating the key.
fn set_value(key: &str, value: Option<&str>, data: &str) {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
    let (k, v, d) = (wide(key), value.map(wide), wide(data));
    let v = v.as_ref().map_or(std::ptr::null(), |v| v.as_ptr());
    // SAFETY: NUL-terminated UTF-16 strings; REG_SZ's size counts the NUL, in bytes.
    let err = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), v, REG_SZ, d.as_ptr().cast(), (d.len() * 2) as u32) };
    if err != 0 {
        eprintln!("notify: registry {key}: {}", std::io::Error::from_raw_os_error(err as i32));
    }
}

/// A CLSID as the registry writes it: `{71FC9E00-807F-45B6-A69B-8163F48F5562}`.
fn braced(g: &GUID) -> String {
    let d = g.data4;
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1, g.data2, g.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
    )
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The tag for a `notify.send` id.
fn tag_for(id: &str) -> String {
    short(&format!("id:{id}"))
}

/// The group for a terminal's toasts (`midna`: none in particular).
fn group_for(session: Option<&str>) -> String {
    short(session.unwrap_or("midna"))
}

/// `s`, or a hash of it when it's longer than a tag may be.
fn short(s: &str) -> String {
    use std::hash::{Hash, Hasher};
    if s.encode_utf16().count() <= TAG_MAX {
        return s.to_string();
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("h:{:016x}", h.finish())
}

/// A toast's XML: a title, the body, an image below them, buttons, and Windows' sound or none.
/// A click on it, or on a button, hands Windows `about` (and the button's label) to pass back.
fn toast_xml(title: &str, body: &str, image: Option<&str>, actions: &[String], sound: bool, about: &About) -> String {
    let mut x = format!("<toast launch=\"{}\"><visual><binding template=\"ToastGeneric\"><text>{}</text>", esc(&click_args(about, None)), esc(title));
    if !body.is_empty() {
        x += &format!("<text>{}</text>", esc(body));
    }
    if let Some(path) = image {
        x += &format!("<image src=\"{}\"/>", esc(&format!("file:///{}", path.replace('\\', "/"))));
    }
    x += "</binding></visual>";
    if !actions.is_empty() {
        x += "<actions>";
        for a in actions.iter().take(ACTIONS_MAX) {
            x += &format!("<action content=\"{}\" arguments=\"{}\"/>", esc(a), esc(&click_args(about, Some(a))));
        }
        x += "</actions>";
    }
    if !sound {
        x += "<audio silent=\"true\"/>";
    }
    x + "</toast>"
}

/// Text for XML: markup escaped, and control characters XML can't hold dropped.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out += "&amp;",
            '<' => out += "&lt;",
            '>' => out += "&gt;",
            '"' => out += "&quot;",
            '\'' => out += "&apos;",
            '\t' | '\n' | '\r' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The arguments Windows hands back for a click on `x`'s body, or on its button `label`.
    fn args_in(x: &str, label: Option<&str>) -> String {
        let doc = XmlDocument::new().unwrap();
        doc.LoadXml(&HSTRING::from(x)).unwrap();
        let (tag, attr) = if label.is_some() { ("action", "arguments") } else { ("toast", "launch") };
        let nodes = doc.GetElementsByTagName(&HSTRING::from(tag)).unwrap();
        (0..nodes.Length().unwrap())
            .map(|i| nodes.Item(i).unwrap())
            .map(|n| n.cast::<windows::Data::Xml::Dom::XmlElement>().unwrap())
            .find(|e| label.is_none_or(|l| e.GetAttribute(&HSTRING::from("content")).unwrap() == l))
            .map(|e| e.GetAttribute(&HSTRING::from(attr)).unwrap().to_string())
            .unwrap()
    }

    #[test]
    fn a_toast_escapes_what_it_shows() {
        let x = toast_xml("Approve <rm -rf>?", "a & b \"c\"\u{1b}[0m", None, &[], true, &About::default());
        assert!(x.contains("<text>Approve &lt;rm -rf&gt;?</text><text>a &amp; b &quot;c&quot;[0m</text>"), "{x}");
        assert!(!x.contains("<audio"), "{x}");
        // Every toast midna makes is XML Windows takes.
        args_in(&x, None);
    }

    #[test]
    fn buttons_image_and_silence() {
        let actions: Vec<String> = ["Yes", "No", "a\"b", "4", "5", "6"].map(String::from).to_vec();
        let x = toast_xml("t", "", Some(r"C:\Users\me\img 1.png"), &actions, false, &About::default());
        assert!(x.contains("<image src=\"file:///C:/Users/me/img 1.png\"/>"), "{x}");
        assert_eq!(x.matches("<action ").count(), ACTIONS_MAX);
        assert_eq!(x.matches("<text>").count(), 1, "no body, no second line: {x}");
        assert!(x.ends_with("<audio silent=\"true\"/></toast>"), "{x}");
    }

    #[test]
    fn a_click_hands_back_what_it_reports() {
        let about = About { session: "595bd00a".into(), needs_you: Some("n-1".into()), id: Some("deploy \"1\" & <2>".into()), open: Some("https://x.test/?a=1&b=2".into()) };
        let x = toast_xml("t", "b", None, &["Deploy".into(), "Skip: \"later\"".into()], true, &about);
        assert_eq!(parse_args(&args_in(&x, None)), Some((about.clone(), None)));
        assert_eq!(parse_args(&args_in(&x, Some("Skip: \"later\""))), Some((about.clone(), Some("Skip: \"later\"".into()))));
        let none = About::default();
        assert_eq!(parse_args(&click_args(&none, None)), Some((none, None)));
        assert_eq!(parse_args(""), None, "midna's own name clicked in Action Center");
    }

    #[test]
    fn tags_groups_and_class_ids() {
        assert_eq!(tag_for("build-1"), "id:build-1");
        assert_eq!(group_for(None), "midna");
        assert_eq!(group_for(Some("s-1")), "s-1");
        let long = "x".repeat(100);
        assert!(tag_for(&long).len() <= TAG_MAX);
        assert_eq!(tag_for(&long), tag_for(&long));
        assert_ne!(tag_for(&long), tag_for(&"y".repeat(100)));
        assert_eq!(braced(&ACTIVATOR), "{71FC9E00-807F-45B6-A69B-8163F48F5562}");
        assert_eq!(braced(&ACTIVATOR_DEV), "{B130EDB7-F2F7-45B7-8AA1-FF07B076EE68}");
    }
}
