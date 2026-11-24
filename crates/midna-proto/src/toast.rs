//! A Windows toast's XML, tag and group, and what a click on it hands back. Shared by the app
//! (`midna-app/src/notify_windows.rs`) and midnad (`midnad/src/notify_windows.rs`, the toasts it
//! posts when no app is connected): both post under the same app id, and the app's activator
//! reads the arguments of either one's toasts, so they must be written the same way.
//!
//! A toast's group is its terminal, so the app's `clear` takes a terminal's toasts away; its tag
//! is `notify.send`'s id, so a later one with that id replaces it and `withdraw` finds it.

/// Windows' limit on a toast's tag and on its group.
pub const TAG_MAX: usize = 64;
/// Windows shows at most this many buttons.
pub const ACTIONS_MAX: usize = 5;

/// The AppUserModelID midna's toasts are posted under: Midna Dev's and debug builds' (`dev`) end
/// in `.dev` (a test build's in `.test`), so a build from source never shows as, or changes,
/// Midna's.
pub fn app_id(dev: bool) -> &'static str {
    match (dev, crate::paths::TEST_BUILD) {
        (false, _) => crate::paths::BUNDLE_ID,
        (true, false) => crate::paths::DEV_APP_ID,
        (true, true) => crate::paths::TEST_APP_ID,
    }
}

/// What a click on a toast reports.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct About {
    pub session: String,
    pub needs_you: Option<String>,
    pub id: Option<String>,
    pub open: Option<String>,
}

/// A toast's (or a button's) arguments: what a click reports, and the button's label.
pub fn click_args(about: &About, action: Option<&str>) -> String {
    serde_json::json!({ "s": about.session, "n": about.needs_you, "i": about.id, "o": about.open, "a": action }).to_string()
}

/// What a click reported, and the button's label. None: not a toast's arguments (midna's own
/// name clicked in Action Center hands back none).
pub fn parse_args(args: &str) -> Option<(About, Option<String>)> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    Some((About { session: s("s").unwrap_or_default(), needs_you: s("n"), id: s("i"), open: s("o") }, s("a")))
}

/// The tag for a `notify.send` id.
pub fn tag_for(id: &str) -> String {
    short(&format!("id:{id}"))
}

/// The group for a terminal's toasts (`midna`: none in particular).
pub fn group_for(session: Option<&str>) -> String {
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
pub fn xml(title: &str, body: &str, image: Option<&str>, actions: &[String], sound: bool, about: &About) -> String {
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

    #[test]
    fn a_toast_escapes_what_it_shows() {
        let x = xml("Approve <rm -rf>?", "a & b \"c\"\u{1b}[0m", None, &[], true, &About::default());
        assert!(x.contains("<text>Approve &lt;rm -rf&gt;?</text><text>a &amp; b &quot;c&quot;[0m</text>"), "{x}");
        assert!(!x.contains("<audio"), "{x}");
    }

    #[test]
    fn buttons_image_and_silence() {
        let actions: Vec<String> = ["Yes", "No", "a\"b", "4", "5", "6"].map(String::from).to_vec();
        let x = xml("t", "", Some(r"C:\Users\me\img 1.png"), &actions, false, &About::default());
        assert!(x.contains("<image src=\"file:///C:/Users/me/img 1.png\"/>"), "{x}");
        assert_eq!(x.matches("<action ").count(), ACTIONS_MAX);
        assert_eq!(x.matches("<text>").count(), 1, "no body, no second line: {x}");
        assert!(x.ends_with("<audio silent=\"true\"/></toast>"), "{x}");
    }

    #[test]
    fn args_round_trip() {
        let about = About { session: "595bd00a".into(), needs_you: Some("n-1".into()), id: Some("deploy \"1\"".into()), open: Some("https://x.test/?a=1&b=2".into()) };
        assert_eq!(parse_args(&click_args(&about, Some("Skip"))), Some((about.clone(), Some("Skip".into()))));
        let none = About::default();
        assert_eq!(parse_args(&click_args(&none, None)), Some((none, None)));
        assert_eq!(parse_args(""), None, "midna's own name clicked in Action Center");
    }

    #[test]
    fn tags_groups_and_app_ids() {
        assert_eq!(tag_for("build-1"), "id:build-1");
        assert_eq!(group_for(None), "midna");
        assert_eq!(group_for(Some("s-1")), "s-1");
        let long = "x".repeat(100);
        assert!(tag_for(&long).len() <= TAG_MAX);
        assert_eq!(tag_for(&long), tag_for(&long));
        assert_ne!(tag_for(&long), tag_for(&"y".repeat(100)));
        assert_eq!((app_id(false), app_id(true)), ("com.mrgnhnt.midna", "com.mrgnhnt.midna.dev"));
    }
}
