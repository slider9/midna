//! The composer off macOS. `src/composer.rs` is a native `NSTextView` laid over the terminal
//! (it has to be, for Kass's Accessibility checks); there's no such view here yet, so the
//! composer never opens and `keys.composer` does nothing. Same API as `src/composer.rs`, so the
//! main window doesn't care which one it has.
//!
//! A GPUI text field could stand in for it later; until then a long prompt is typed into the
//! terminal itself.
use crate::app::MainWindow;
use crate::theme::Theme;
use gpui_kit::*;

actions!(
    midna,
    [
        /// Open the composer by hand (`keys.composer`). Not available off macOS yet.
        OpenComposer,
    ]
);

pub const CTX_COMPOSER: &str = "MidnaComposer";

/// Never open off macOS.
pub struct Composer;

impl Composer {
    pub fn new(_window: &mut Window, _cx: &mut Context<MainWindow>) -> Self {
        Composer
    }
}

/// Esc in the terminal never closes a composer: there isn't one.
pub fn escape_in_terminal(_m: &mut MainWindow, _ks: &Keystroke, _window: &mut Window, _cx: &mut Context<MainWindow>) -> bool {
    false
}

/// Nothing to give focus back to.
pub fn refocus(_m: &mut MainWindow, _window: &mut Window, _cx: &mut Context<MainWindow>) -> bool {
    false
}

/// Text as the terminal should receive it (the same as `src/composer.rs`'s `encode`, which
/// `terminal/secret_paste.rs` also uses). Trailing newlines are dropped. Multi-line text is one
/// bracketed paste when the terminal enabled it (mode 2004); newlines are sent as CR, like a
/// terminal paste.
pub fn encode(text: &str, bracketed_paste: bool) -> String {
    let t = text.replace("\r\n", "\n");
    let t = t.trim_end_matches(['\n', '\r']);
    if !t.contains('\n') {
        return t.to_string();
    }
    let t = t.replace('\n', "\r");
    if bracketed_paste { format!("\x1b[200~{}\x1b[201~", t.replace("\x1b[201~", "")) } else { t }
}

pub fn sync(m: &mut MainWindow, _window: &mut Window, _cx: &mut Context<MainWindow>) {
    let Composer = m.composer;
}

pub fn render(_m: &MainWindow, _t: &Theme, _cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    None
}

/// `keys.composer` is accepted and ignored, so the binding still resolves.
pub fn register<E: InteractiveElement>(el: E, cx: &mut Context<MainWindow>) -> E {
    el.on_action(cx.listener(|_m, _: &OpenComposer, _w, _cx| {}))
}

pub fn bindings() -> Vec<KeyBinding> {
    vec![]
}

/// For logs: there is no native first responder to report.
pub fn first_responder(_m: &MainWindow) -> String {
    "gpui".to_string()
}

#[cfg(test)]
mod tests {
    use super::encode;
    use ::core::prelude::v1::test;

    #[test]
    fn encode_single_and_multi_line() {
        assert_eq!(encode("fix the tests", true), "fix the tests");
        assert_eq!(encode("fix the tests\n\n", true), "fix the tests");
        assert_eq!(encode("a\nb", true), "\x1b[200~a\rb\x1b[201~");
        assert_eq!(encode("a\r\nb", false), "a\rb");
    }
}

/// What the secret sheet tells macOS's composer field. Off macOS there's no field to tell.
#[allow(dead_code)]
pub enum ComposerEvent {
    Insert(String),
    Refocus,
}

pub fn emit(_e: ComposerEvent) {}
