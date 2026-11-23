//! Actions and keybindings. Bindings come from the `keys.*` settings and are rebuilt on
//! `settings.changed`. Hooks for later screens (palette, needs-you stack, rules, triggers,
//! insights, settings window) are actions too, so a palette can list them.
use gpui_kit::*;
use std::collections::HashMap;

actions!(
    midna,
    [
        /// ⌘K palette (CommandBar-A). Placeholder overlay for now.
        ToggleCommandBar,
        /// Jump to the newest needs-you item on the Mac whose window is in front.
        NextNeedsYou,
        /// Jump to the newest needs-you item on any of my Macs.
        NextNeedsYouAnywhere,
        /// Open the needs-you inbox (`ui/needs_you.rs`).
        OpenNeedsYou,
        NewTerminal,
        NewAgent,
        NewTerminalRoot,
        NewAgentRoot,
        /// Folder picker → `project.add` → a terminal in it.
        OpenProject,
        /// Another main window (same daemon, its own selection).
        NewWindow,
        /// Approve the selected session's pending approval once.
        ApproveOnce,
        Deny,
        OpenSettings,
        OpenRules,
        OpenTriggers,
        OpenInsights,
        /// Back to the terminal pane from Rules/Triggers/Insights, or close an overlay.
        Dismiss,
        SplitRight,
        PopOut,
        RestartSession,
        /// Swap the selected terminal for a fresh one in its place (`session.replace`).
        ReplaceSession,
        /// ⌘W: in the main window, close the selected terminal (the window when none is
        /// selected); in Settings or a pop-out, close that window.
        CloseWindow,
        Quit,
        /// ⌘Q: hold for 2.5 s to quit (see `ui::quit_hold`). The menu item quits at once.
        HoldToQuit,
        /// ⌘H / ⌥⌘H / Show All: the standard macOS app-menu items.
        Hide,
        HideOthers,
        ShowAll,
        // terminal
        TermCopy,
        /// Off macOS, Ctrl+C: copy the selection when there is one, else ^C to the terminal, as
        /// in Windows Terminal (`Terminal::on_copy_or_interrupt`).
        TermCopyOrInterrupt,
        /// ⌘X: copy the selection, and erase it when it's in the input line.
        TermCut,
        TermPaste,
        TermPasteSecret,
        /// Paste the clipboard's image as its path, skipping the image sheet.
        TermPasteImageInline,
        TermSelectAll,
        TermClear,
        /// Agent terminals: scroll to the previous / next prompt you sent (`terminal::prompt_nav`).
        PrevPrompt,
        NextPrompt,
        /// The command bar listing the selected agent terminal's prompts.
        OpenPrompts,
        /// The approval banner's approve options (15 min, 1 h, session, always).
        ApproveOptions,
        /// Select the next / previous terminal in sidebar order.
        NextTerminal,
        PrevTerminal,
        /// Back / forward through the terminals you've shown, like a browser (`tab_history`).
        TerminalBack,
        TerminalForward,
        /// Rename the selected terminal in the header.
        RenameSession,
        /// The header's … menu.
        ToggleTerminalMenu,
        CopySessionId,
        /// Mute / unmute notifications for the selected terminal.
        ToggleMute,
        /// Split: side by side ⇄ stacked.
        ToggleSplitOrientation,
        /// Give the split's terminal its own sidebar row.
        SplitToMain,
        /// Focus the other split pane.
        FocusOtherPane,
        /// Pop-out window: keep on top.
        ToggleKeepOnTop,
        /// The current project's … menu / fold its group in the sidebar.
        ToggleProjectMenu,
        FoldProject,
        /// The current project's Summary of closed terminals.
        ShowProjectSummary,
        /// Collapse the sidebar to a rail / expand it again.
        ToggleSidebar,
        /// ⌃⌘M: the Mac switcher under the sidebar's name plate.
        ToggleMacSwitcher,
        /// Window menu → Pair Another Mac…: Settings › Nearby.
        PairMac,
    ]
);

/// ⌘1–9: select project N (0-based index into the sidebar order).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = midna, no_json)]
pub struct SelectProject(pub usize);

/// ⌃⌘1–9, the Window menu and the Mac switcher: the Nth Mac (0: this one, then my paired Macs
/// by name, see `macs`) — its window opens or comes forward.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = midna, no_json)]
pub struct SwitchMac(pub usize);

/// The Window menu's entry for a Mac that can't be driven now. Nothing handles it, so macOS
/// greys the entry.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = midna, no_json)]
pub struct MacUnavailable(pub usize);

pub const CTX_MAIN: &str = "MidnaMain";
pub const CTX_TERM: &str = "Terminal";
/// Every overlay over the main window has this (escape dismisses it), its [`Kind`] and its
/// id: `MidnaOverlay Modal overlay=annotate`.
pub const CTX_OVERLAY: &str = "MidnaOverlay";

/// What kind of place an overlay is, which decides whether main-window shortcuts reach it.
/// The composer is none of these: it's a field in place over its own terminal (switching
/// terminals puts it away as a draft), so main-window shortcuts keep working there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Takes the window until you finish with it: the image sheet, the needs-you cards. Blocks
    /// main-window shortcuts.
    Modal,
    /// A small panel you dismiss: the command bar, links, queue, subagents, the IDE picker.
    /// Blocks main-window shortcuts.
    Popover,
    /// Stands in for the terminal pane: rules, triggers, insights, notifications, summary.
    /// Main-window shortcuts keep working.
    Screen,
}

impl Kind {
    pub const fn name(self) -> &'static str {
        match self {
            Kind::Modal => "Modal",
            Kind::Popover => "Popover",
            Kind::Screen => "Screen",
        }
    }
}

/// Each overlay's id (`overlay=<id>`), for bindings that work in that overlay alone.
pub mod overlay {
    pub const ANNOTATE: &str = "annotate";
    pub const NEEDS_YOU: &str = "needs_you";
    pub const COMMAND_BAR: &str = "command_bar";
    pub const LINKS: &str = "links";
    pub const QUEUE: &str = "queue";
    pub const SUBAGENTS: &str = "subagents";
    pub const IDE: &str = "ide";
    pub const RULES: &str = "rules";
    pub const TRIGGERS: &str = "triggers";
    pub const INSIGHTS: &str = "insights";
    pub const NOTIFICATIONS: &str = "notifications";
    pub const SUMMARY: &str = "summary";
    pub const MACS: &str = "macs";
    #[cfg(test)]
    pub const ALL: &[&str] = &[ANNOTATE, NEEDS_YOU, COMMAND_BAR, LINKS, QUEUE, SUBAGENTS, IDE, RULES, TRIGGERS, INSIGHTS, NOTIFICATIONS, SUMMARY, MACS];
}

/// An overlay's key context (see [`CTX_OVERLAY`]).
pub fn overlay_context(kind: Kind, id: &'static str) -> KeyContext {
    let mut c = KeyContext::default();
    c.add(CTX_OVERLAY);
    c.add(kind.name());
    c.set("overlay", id);
    c
}

/// Where a binding works: a GPUI context predicate (`&&`, `||`, `!`, `==`, `!=`, `>`), or
/// `None` for anywhere. `!X` is false when any context around the focus has X.
pub type When = Option<&'static str>;

/// The main window (and pop-outs) while no modal or popover is up; screens keep it. The
/// default for main-window shortcuts.
pub const WINDOW: When = Some("MidnaMain && !Modal && !Popover");
/// Anywhere, modals, popovers and Settings included: the app-level actions (settings, new
/// window, quit, hide).
pub const ANYWHERE: When = None;
/// Any window while no modal or popover is up: ⌘W. A modal or popover closes on it instead
/// (see [`OVERLAY_KEYS`]).
pub const UNCOVERED: When = Some("!Modal && !Popover");
/// The focused terminal.
pub const TERM: When = Some("Terminal");
/// Any modal or popover.
pub const COVERED: When = Some("Modal || Popover");

/// The Settings window.
pub const CTX_SETTINGS: &str = "MidnaSettings";

/// What Settings ▸ Shortcuts calls each context and overlay id a when clause can name
/// ([`where_label`]). A name not here shows as written.
pub const PLACES: &[(&str, &str)] = &[
    (CTX_MAIN, "Main window"),
    (CTX_TERM, "Terminal"),
    (CTX_OVERLAY, "Overlays"),
    (Kind::Modal.name(), "Sheets"),
    (Kind::Popover.name(), "Popovers"),
    (Kind::Screen.name(), "Screens"),
    (crate::composer::CTX_COMPOSER, "Composer"),
    (CTX_SETTINGS, "Settings"),
    (crate::ui::text_input::CTX_WRAP, "Notes"),
    (overlay::ANNOTATE, "Image sheet"),
    (overlay::NEEDS_YOU, "Needs-you cards"),
    (overlay::COMMAND_BAR, "Command bar"),
    (overlay::LINKS, "Links"),
    (overlay::QUEUE, "Queue"),
    (overlay::SUBAGENTS, "Subagents"),
    (overlay::IDE, "IDE picker"),
    (overlay::RULES, "Rules"),
    (overlay::TRIGGERS, "Triggers"),
    (overlay::INSIGHTS, "Insights"),
    (overlay::NOTIFICATIONS, "Notifications"),
    (overlay::SUMMARY, "Summary"),
    (overlay::MACS, "Mac switcher"),
];

/// A when clause in words, for Settings ▸ Shortcuts: `None` is "Anywhere",
/// `MidnaMain && !Modal && !Popover` is "Main window, not in sheets or popovers". A clause
/// that doesn't parse shows as written.
pub fn where_label(when: Option<&str>) -> String {
    let Some(w) = when.map(str::trim).filter(|w| !w.is_empty() && *w != "*") else {
        return "Anywhere".into();
    };
    match KeyBindingContextPredicate::parse(w) {
        Ok(p) => capitalize(&describe(&p)),
        Err(_) => w.to_string(),
    }
}

/// A predicate in words: what it needs, then ", not in …" for what it rules out.
fn describe(p: &KeyBindingContextPredicate) -> String {
    use KeyBindingContextPredicate as P;
    let mut all = vec![];
    flatten(p, true, &mut all);
    let (mut yes, mut no) = (vec![], vec![]);
    for p in all {
        match p {
            P::Not(inner) => flatten(inner, false, &mut no),
            P::NotEqual(k, v) if k.as_ref() == "overlay" => no.push(p),
            p => yes.push(p),
        }
    }
    let yes = yes.into_iter().map(term).enumerate().map(|(i, s)| if i == 0 { s } else { lower(&s) }).collect::<Vec<_>>().join(", ");
    let no = lower(&list(&no.into_iter().map(term).collect::<Vec<_>>()));
    match (yes.is_empty(), no.is_empty()) {
        (_, true) => yes,
        (true, false) => format!("not in {no}"),
        (false, false) => format!("{yes}, not in {no}"),
    }
}

/// The parts of a run of `&&` (`and`) or `||`.
fn flatten<'a>(p: &'a KeyBindingContextPredicate, and: bool, out: &mut Vec<&'a KeyBindingContextPredicate>) {
    use KeyBindingContextPredicate as P;
    match p {
        P::And(a, b) | P::Or(a, b) if and == matches!(p, P::And(..)) => {
            flatten(a, and, out);
            flatten(b, and, out);
        }
        p => out.push(p),
    }
}

/// One place: a context, an overlay, or a compound in words.
fn term(p: &KeyBindingContextPredicate) -> String {
    use KeyBindingContextPredicate as P;
    let place = |id: &str| PLACES.iter().find(|(c, _)| *c == id).map_or(id.to_string(), |(_, n)| n.to_string());
    match p {
        P::Identifier(id) => place(id),
        P::Equal(k, v) | P::NotEqual(k, v) if k.as_ref() == "overlay" => place(v),
        P::Or(..) => {
            let mut any = vec![];
            flatten(p, false, &mut any);
            list(&any.into_iter().map(term).collect::<Vec<_>>())
        }
        P::Descendant(outer, inner) => format!("{} in {}", term(inner), lower(&term(outer))),
        P::Not(_) | P::And(..) => describe(p),
        p => p.to_string(),
    }
}

/// "a", "a or b", "a, b or c" (lowercased after the first).
fn list(items: &[String]) -> String {
    let items: Vec<String> = items.iter().enumerate().map(|(i, s)| if i == 0 { s.clone() } else { lower(s) }).collect();
    match items.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
        _ => items.concat(),
    }
}

/// "Image sheet" -> "image sheet", but "IDE picker" stays, and so does a context name that
/// isn't in [`PLACES`] (it shows as written).
fn lower(s: &str) -> String {
    let mut c = s.chars();
    match (c.next(), c.next()) {
        (Some(a), Some(b)) if b.is_lowercase() && PLACES.iter().any(|(_, n)| s.starts_with(n)) => a.to_lowercase().chain(s[a.len_utf8()..].chars()).collect(),
        _ => s.to_string(),
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or(String::new(), |a| a.to_uppercase().chain(c).collect())
}

/// One rebindable shortcut: its `keys.*` setting and the binding it makes (the catalog in
/// `midna_proto::settings` holds the default and the description).
pub struct Shortcut {
    pub setting: &'static str,
    /// Where it works by default (see [`When`]); `keys.when` can change it ([`when_clause`]).
    pub when: When,
    bind: fn(&str, Option<&str>) -> KeyBinding,
}

macro_rules! shortcut {
    ($setting:literal, $action:expr, $when:expr) => {
        Shortcut { setting: $setting, when: $when, bind: |k, when| KeyBinding::new(k, $action, when) }
    };
}

/// Every rebindable shortcut. Each has a `keys.*` row in the settings catalog (a test checks).
/// Main-window shortcuts are blocked under a modal or popover ([`WINDOW`]); only the
/// app-level ones work everywhere.
pub const SHORTCUTS: &[Shortcut] = &[
    shortcut!("keys.command_bar", ToggleCommandBar, WINDOW),
    shortcut!("keys.next_needs_you", NextNeedsYou, WINDOW),
    shortcut!("keys.next_needs_you_anywhere", NextNeedsYouAnywhere, WINDOW),
    shortcut!("keys.needs_you", OpenNeedsYou, WINDOW),
    shortcut!("keys.new_terminal", NewTerminal, WINDOW),
    shortcut!("keys.new_agent", NewAgent, WINDOW),
    shortcut!("keys.new_terminal_root", NewTerminalRoot, WINDOW),
    shortcut!("keys.new_agent_root", NewAgentRoot, WINDOW),
    shortcut!("keys.approve", ApproveOnce, WINDOW),
    shortcut!("keys.approve_options", ApproveOptions, WINDOW),
    shortcut!("keys.deny", Deny, WINDOW),
    shortcut!("keys.settings", OpenSettings, ANYWHERE),
    shortcut!("keys.rules", OpenRules, WINDOW),
    shortcut!("keys.triggers", OpenTriggers, WINDOW),
    shortcut!("keys.insights", OpenInsights, WINDOW),
    shortcut!("keys.open_project", OpenProject, WINDOW),
    shortcut!("keys.new_window", NewWindow, ANYWHERE),
    shortcut!("keys.composer", crate::composer::OpenComposer, WINDOW),
    shortcut!("keys.add_image", crate::annotate::AddImage, WINDOW),
    shortcut!("keys.edit_attachment", crate::annotate::EditAttachment, WINDOW),
    shortcut!("keys.paste_image_inline", TermPasteImageInline, TERM),
    shortcut!("keys.note_newline", crate::ui::text_input::Newline, Some(crate::ui::text_input::CTX_WRAP)),
    shortcut!("keys.links", crate::ui::links::ToggleLinks, WINDOW),
    shortcut!("keys.subagents", crate::ui::subagents::ToggleSubagents, WINDOW),
    shortcut!("keys.queue", crate::ui::queue::ToggleQueue, WINDOW),
    shortcut!("keys.prev_prompt", PrevPrompt, TERM),
    shortcut!("keys.next_prompt", NextPrompt, TERM),
    shortcut!("keys.prompts", OpenPrompts, WINDOW),
    shortcut!("keys.next_terminal", NextTerminal, WINDOW),
    shortcut!("keys.prev_terminal", PrevTerminal, WINDOW),
    shortcut!("keys.terminal_back", TerminalBack, WINDOW),
    shortcut!("keys.terminal_forward", TerminalForward, WINDOW),
    shortcut!("keys.project_menu", ToggleProjectMenu, WINDOW),
    shortcut!("keys.fold_project", FoldProject, WINDOW),
    shortcut!("keys.project_summary", ShowProjectSummary, WINDOW),
    shortcut!("keys.sidebar", ToggleSidebar, WINDOW),
    shortcut!("keys.rename", RenameSession, WINDOW),
    shortcut!("keys.restart", RestartSession, WINDOW),
    shortcut!("keys.replace", ReplaceSession, WINDOW),
    shortcut!("keys.terminal_menu", ToggleTerminalMenu, WINDOW),
    shortcut!("keys.copy_session_id", CopySessionId, WINDOW),
    shortcut!("keys.mute", ToggleMute, WINDOW),
    shortcut!("keys.clear", TermClear, TERM),
    shortcut!("keys.split", SplitRight, WINDOW),
    shortcut!("keys.split_orientation", ToggleSplitOrientation, WINDOW),
    shortcut!("keys.split_to_main", SplitToMain, WINDOW),
    shortcut!("keys.focus_pane", FocusOtherPane, WINDOW),
    shortcut!("keys.pop_out", PopOut, WINDOW),
    shortcut!("keys.open_ide", crate::ide::OpenInIde, WINDOW),
    shortcut!("keys.choose_ide", crate::ide::ChooseIde, WINDOW),
    shortcut!("keys.keep_on_top", ToggleKeepOnTop, WINDOW),
    shortcut!("keys.close", CloseWindow, UNCOVERED),
    shortcut!("keys.quit", HoldToQuit, ANYWHERE),
];

/// What a shortcut's keys do inside one overlay, where the main-window binding is blocked
/// (rebinding the shortcut moves these too). Bound after [`SHORTCUTS`]: two bindings that
/// match at the same depth go to the later one, so the image sheet's ⌘W beats the
/// modal-wide one.
pub const OVERLAY_KEYS: &[Shortcut] = &[
    // ⌘W closes a modal or popover, like escape.
    shortcut!("keys.close", Dismiss, COVERED),
    // A popover's own shortcut closes it again.
    shortcut!("keys.command_bar", ToggleCommandBar, Some("overlay == command_bar")),
    shortcut!("keys.needs_you", OpenNeedsYou, Some("overlay == needs_you")),
    shortcut!("keys.links", crate::ui::links::ToggleLinks, Some("overlay == links")),
    shortcut!("keys.queue", crate::ui::queue::ToggleQueue, Some("overlay == queue")),
    shortcut!("keys.subagents", crate::ui::subagents::ToggleSubagents, Some("overlay == subagents")),
    shortcut!("keys.choose_ide", crate::ide::ChooseIde, Some("overlay == ide")),
    // The needs-you cards answer the card on screen.
    shortcut!("keys.approve", ApproveOnce, Some("overlay == needs_you")),
    shortcut!("keys.deny", Deny, Some("overlay == needs_you")),
    shortcut!("keys.next_needs_you", NextNeedsYou, Some("overlay == needs_you")),
    shortcut!("keys.next_needs_you_anywhere", NextNeedsYouAnywhere, Some("overlay == needs_you")),
    shortcut!("keys.approve", crate::ui::command_bar::RunSelected, Some("overlay == command_bar")),
    shortcut!("keys.deny", crate::ui::command_bar::ClearQuery, Some("overlay == command_bar")),
    shortcut!("keys.approve", crate::annotate::AttachImages, Some("overlay == annotate")),
    shortcut!("keys.close", crate::annotate::RemoveImage, Some("overlay == annotate")),
    // The Summary reopens or removes the record on screen.
    shortcut!("keys.approve", crate::ui::summary::ResumeRecord, Some("overlay == summary")),
    shortcut!("keys.deny", crate::ui::summary::RemoveRecord, Some("overlay == summary")),
];

/// Keys that aren't settings: standard editing keys and the keys a screen handles itself.
/// Listed in Settings ▸ Shortcuts so a key search finds them too.
pub struct Fixed {
    /// Every keystroke this row covers (what a key search matches).
    pub keys: &'static [&'static str],
    /// Shown instead of the keys when they're a range (⌘1–9).
    pub label: Option<&'static str>,
    pub title: &'static str,
    /// Where it works (see [`When`]); Settings shows it through [`where_label`].
    pub when: When,
}

const fn fixed(keys: &'static [&'static str], title: &'static str, when: When) -> Fixed {
    Fixed { keys, label: None, title, when }
}

/// `mac` on macOS, `other` elsewhere: for keys that aren't settings. Off macOS GPUI's `cmd` is
/// the Windows key, and a plain Ctrl+letter in a terminal belongs to the shell and the agents,
/// so the terminal's keys take Ctrl+Shift, as Windows Terminal's do. Places with no terminal
/// in them (overlays, text fields, Settings) use plain Ctrl, as Windows apps do.
pub const fn pick(mac: &'static str, other: &'static str) -> &'static str {
    if cfg!(target_os = "macos") { mac } else { other }
}

/// [`pick`] for a list of keys.
pub const fn pick_all(mac: &'static [&'static str], other: &'static [&'static str]) -> &'static [&'static str] {
    if cfg!(target_os = "macos") { mac } else { other }
}

/// The terminal's own keys that aren't settings, as bound on this platform (labels in menus
/// and hints come from these too).
pub mod term_keys {
    use super::pick;
    pub const COPY: &str = pick("cmd-c", "ctrl-shift-c");
    pub const CUT: &str = pick("cmd-x", "ctrl-shift-x");
    pub const PASTE: &str = pick("cmd-v", "ctrl-v");
    pub const PASTE_SECRET: &str = pick("cmd-alt-v", "alt-shift-v");
    pub const SELECT_ALL: &str = pick("cmd-a", "ctrl-shift-a");
    pub const TO_BOTTOM: &str = pick("cmd-down", "ctrl-shift-end");
}

pub const FIXED: &[Fixed] = &[
    Fixed {
        keys: pick_all(
            &["cmd-1", "cmd-2", "cmd-3", "cmd-4", "cmd-5", "cmd-6", "cmd-7", "cmd-8", "cmd-9"],
            &["ctrl-1", "ctrl-2", "ctrl-3", "ctrl-4", "ctrl-5", "ctrl-6", "ctrl-7", "ctrl-8", "ctrl-9"],
        ),
        label: Some(pick("⌘1–9", "Ctrl+1–9")),
        title: "Jump to project 1–9",
        when: WINDOW,
    },
    fixed(&["escape"], "Close the overlay, or go back to the terminal", Some(CTX_OVERLAY)),
    #[cfg(target_os = "macos")]
    fixed(&["cmd-h"], "Hide midna", ANYWHERE),
    #[cfg(target_os = "macos")]
    fixed(&["cmd-alt-h"], "Hide other apps", ANYWHERE),
    fixed(pick_all(&["cmd-c"], &["ctrl-shift-c"]), "Copy the selection", TERM),
    #[cfg(not(target_os = "macos"))]
    fixed(&["ctrl-c"], "Copy the selection when there is one (again, or with none: ^C to the terminal)", TERM),
    fixed(pick_all(&["cmd-v"], &["ctrl-v", "ctrl-shift-v"]), "Paste (an image on the clipboard opens the image sheet, or pastes its path with terminal.image_paste = inline; a secret pasted into an agent asks to store it)", TERM),
    fixed(pick_all(&["cmd-alt-v"], &["alt-shift-v"]), pick("Paste as Secret: store the clipboard in the Keychain, paste [secret:NAME]", "Paste as Secret: store the clipboard in Credential Manager, paste [secret:NAME]"), TERM),
    fixed(pick_all(&["cmd-a"], &["ctrl-shift-a"]), "Select all", TERM),
    fixed(pick_all(&["cmd-z", "cmd-shift-z", "cmd-y"], &["ctrl-z", "ctrl-shift-z"]), "Undo / redo in Claude Code's input", TERM),
    fixed(pick_all(&["cmd-f"], &["ctrl-shift-f"]), "Find in the terminal", TERM),
    fixed(pick_all(&["cmd-g", "enter"], &["f3", "enter"]), "Find: older match", TERM),
    fixed(pick_all(&["cmd-shift-g", "shift-enter"], &["shift-f3", "shift-enter"]), "Find: newer match", TERM),
    fixed(pick_all(&["cmd-up", "cmd-home"], &["ctrl-shift-home"]), "Scroll to the top", TERM),
    fixed(pick_all(&["cmd-down", "cmd-end"], &["ctrl-shift-end"]), "Scroll to the bottom", TERM),
    fixed(pick_all(&["cmd-pageup", "cmd-pagedown"], &["ctrl-shift-pageup", "ctrl-shift-pagedown"]), "Scroll a page", TERM),
    fixed(&["up", "down"], "Move the selection", Some("Popover")),
    fixed(&["enter"], "Run the selected command", Some("overlay == command_bar")),
    fixed(&["shift-enter"], "Ask an agent instead", Some("overlay == command_bar")),
    fixed(&["tab"], "Switch agent (Claude / Codex)", Some("overlay == command_bar")),
    fixed(&["shift-tab"], "Switch between project and root", Some("overlay == command_bar")),
    fixed(&["shift-enter"], "Pin or unpin the selected link", Some("overlay == links")),
    fixed(&["alt-enter"], "Jump to where the link came up", Some("overlay == links")),
    fixed(&["tab", "shift-tab"], "Next / previous tab", Some("overlay == links")),
    fixed(&["enter"], "Queue the message, or edit the selected one", Some("overlay == queue")),
    fixed(&["shift-enter"], "Send the selected message now", Some("overlay == queue")),
    fixed(&["alt-up", "alt-down"], "Move the selected message", Some("overlay == queue")),
    fixed(&["alt-backspace"], "Remove the selected message", Some("overlay == queue")),
    fixed(&["tab"], "Change when it goes in", Some("overlay == queue")),
    fixed(pick_all(&["cmd-o"], &["ctrl-o"]), "Open the card's terminal", Some("overlay == needs_you")),
    fixed(pick_all(&["cmd-shift-enter"], &["ctrl-shift-enter"]), "Approve all", Some("overlay == needs_you")),
    fixed(&["enter"], "Edit the selected note", Some("overlay == annotate")),
    fixed(&["backspace"], "Remove the selected note", Some("overlay == annotate")),
    fixed(pick_all(&["cmd-z", "cmd-shift-z"], &["ctrl-z", "ctrl-shift-z"]), "Undo / redo in the note being edited", Some("overlay == annotate")),
    fixed(pick_all(&["cmd-z"], &["ctrl-z"]), "Put back the last removed note or image", Some("overlay == annotate")),
    fixed(pick_all(&["cmd-up", "cmd-down"], &["ctrl-up", "ctrl-down"]), "Previous / next image", Some("overlay == annotate")),
    fixed(pick_all(&["cmd-w"], &["ctrl-shift-w"]), "Remove the image, or close the sheet when there's none", Some("overlay == annotate")),
    fixed(pick_all(&["cmd-=", "cmd--", "cmd-0"], &["ctrl-=", "ctrl--", "ctrl-0"]), "Zoom in / out / fit", Some("overlay == annotate")),
    Fixed { keys: pick_all(&["cmd"], &["ctrl"]), label: Some(pick("⌘ scroll · pinch", "Ctrl scroll · pinch")), title: "Zoom about the pointer", when: Some("overlay == annotate") },
    Fixed { keys: &["alt"], label: Some(pick("⌥ drag · scroll", "Alt drag · scroll")), title: "Pan the image (a middle-drag too)", when: Some("overlay == annotate") },
    fixed(pick_all(&["cmd-v"], &["ctrl-v"]), "Add the image on the clipboard", Some("overlay == annotate")),
    #[cfg(target_os = "macos")]
    fixed(&["enter"], "Send to the terminal", Some(crate::composer::CTX_COMPOSER)),
    #[cfg(target_os = "macos")]
    fixed(&["cmd-z", "cmd-shift-z"], "Undo / redo", Some(crate::composer::CTX_COMPOSER)),
    fixed(pick_all(&["cmd-k", "cmd-f"], &["ctrl-k", "ctrl-f"]), midna_proto::wording::os("Search settings (↩ asks an agent instead)", "Search settings (Enter asks an agent instead)"), Some(CTX_SETTINGS)),
    fixed(pick_all(&["cmd-[", "cmd-]"], &["ctrl-[", "ctrl-]"]), "Previous / next section", Some(CTX_SETTINGS)),
    fixed(pick_all(&["cmd-alt-k"], &["ctrl-alt-k"]), "Record keys: find a shortcut by pressing it", Some(CTX_SETTINGS)),
];

/// The keys bound to each `keys.*` setting right now (normalized), for tooltips and labels.
#[derive(Default)]
pub struct BoundKeys(HashMap<&'static str, String>);
impl Global for BoundKeys {}

/// The catalog default for a `keys.*` setting ("" when unbound or unknown).
pub fn default_key(setting: &str) -> &'static str {
    match midna_proto::settings::setting(setting).map(|s| s.default) {
        Some(midna_proto::settings::DefaultValue::Str(s)) => s,
        _ => "",
    }
}

/// Pretty keys for a `keys.*` setting as bound now ("" when unbound).
pub fn label(cx: &App, setting: &str) -> String {
    let keys = cx.try_global::<BoundKeys>().and_then(|b| b.0.get(setting).cloned()).unwrap_or_else(|| default_key(setting).to_string());
    if keys.is_empty() { String::new() } else { pretty(&keys) }
}

/// Each `keys.*` setting's when clause as bound now (`keys.when` applied; `None` = anywhere).
#[derive(Default)]
pub struct BoundWhen(HashMap<&'static str, Option<String>>);
impl Global for BoundWhen {}

/// Where a `keys.*` shortcut works as bound now: its when clause, or `None` for anywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))] // the composer's (macOS only)
pub fn when_clause(cx: &App, setting: &str) -> Option<String> {
    match cx.try_global::<BoundWhen>().and_then(|b| b.0.get(setting)) {
        Some(w) => w.clone(),
        None => SHORTCUTS.iter().find(|s| s.setting == setting).and_then(|s| s.when).map(str::to_string),
    }
}

/// A shortcut's when clause with its `keys.when` rule applied (`<setting> = <clause>`,
/// `*` = anywhere). midnad checks rules before saving them; one GPUI still can't parse is
/// logged and the built-in clause kept.
fn effective_when(s: &Shortcut, rules: &[String]) -> Option<String> {
    let Some(w) = rules.iter().find_map(|r| r.split_once('=').filter(|(k, _)| k.trim() == s.setting).map(|(_, w)| w.trim())) else {
        return s.when.map(str::to_string);
    };
    if w == midna_proto::settings::WHEN_ANYWHERE {
        None
    } else if KeyBindingContextPredicate::parse(w).is_ok() {
        Some(w.to_string())
    } else {
        eprintln!("midna-app: ignoring keys.when for {}: {w:?} doesn't parse", s.setting);
        s.when.map(str::to_string)
    }
}

fn valid(keys: &str) -> bool {
    !keys.trim().is_empty() && keys.split_whitespace().all(|k| Keystroke::parse(k).is_ok())
}

/// Rebuild all keybindings from the current settings (`get` returns a `keys.*` value, `when`
/// is `keys.when`'s rules).
pub fn bind_keys(cx: &mut App, get: impl Fn(&str) -> Option<String>, when: &[String]) {
    cx.clear_key_bindings();
    let mut bound = BoundKeys::default();
    let mut wheres = BoundWhen::default();
    let mut b: Vec<KeyBinding> = Vec::new();
    for s in SHORTCUTS {
        let w = effective_when(s, when);
        let k = normalize_keys(&get(s.setting).unwrap_or_else(|| default_key(s.setting).to_string()));
        if valid(&k) {
            b.push((s.bind)(&k, w.as_deref()));
            bound.0.insert(s.setting, k);
        } else {
            if !k.is_empty() {
                eprintln!("midna-app: ignoring invalid keybinding {k:?}");
            }
            bound.0.insert(s.setting, String::new());
        }
        wheres.0.insert(s.setting, w);
    }
    b.extend(crate::composer::bindings());
    for i in 1..=9 {
        b.push(KeyBinding::new(&format!("{}-{i}", pick("cmd", "ctrl")), SelectProject(i - 1), WINDOW));
        // The Window menu shows these beside its Macs (it finds them by the action).
        #[cfg(target_os = "macos")]
        b.push(KeyBinding::new(&format!("ctrl-cmd-{i}"), SwitchMac(i - 1), ANYWHERE));
    }
    b.push(KeyBinding::new("ctrl-cmd-m", ToggleMacSwitcher, Some("MidnaMain && !Modal")));
    b.push(KeyBinding::new("escape", Dismiss, Some(CTX_OVERLAY)));
    b.push(KeyBinding::new(term_keys::COPY, TermCopy, TERM));
    b.push(KeyBinding::new(term_keys::CUT, TermCut, TERM));
    b.push(KeyBinding::new(term_keys::PASTE, TermPaste, TERM));
    b.push(KeyBinding::new(term_keys::PASTE_SECRET, TermPasteSecret, TERM));
    b.push(KeyBinding::new(term_keys::SELECT_ALL, TermSelectAll, TERM));
    #[cfg(target_os = "macos")]
    {
        b.push(KeyBinding::new("cmd-h", Hide, ANYWHERE));
        b.push(KeyBinding::new("cmd-alt-h", HideOthers, ANYWHERE));
    }
    #[cfg(not(target_os = "macos"))]
    {
        b.push(KeyBinding::new("ctrl-c", TermCopyOrInterrupt, TERM));
        b.push(KeyBinding::new("ctrl-shift-v", TermPaste, TERM));
    }
    for s in OVERLAY_KEYS {
        if let Some(k) = bound.0.get(s.setting).filter(|k| !k.is_empty()) {
            b.push((s.bind)(k, s.when));
        }
    }
    cx.bind_keys(b);
    cx.set_global(bound);
    cx.set_global(wheres);
}

/// Whether a binding (`keys`, any format) is exactly the recorded keystrokes (normalized).
pub fn keys_match(keys: &str, recorded: &[String]) -> bool {
    let strokes: Vec<String> = normalize_keys(keys).split_whitespace().map(canonical).collect();
    !recorded.is_empty() && strokes.len() >= recorded.len() && strokes.iter().zip(recorded).all(|(a, b)| *a == canonical(b))
}

/// "alt-cmd-k" and "cmd-alt-k" are the same keystroke: sort the modifiers.
fn canonical(stroke: &str) -> String {
    match Keystroke::parse(stroke) {
        Ok(k) => keystroke_text(&k),
        Err(_) => stroke.to_string(),
    }
}

/// A keystroke as the settings catalog writes it ("cmd-alt-shift-t", "ctrl-shift-tab").
pub fn setting_keys(k: &Keystroke) -> String {
    let m = &k.modifiers;
    let mut out = String::new();
    for (on, name) in [(m.platform, "cmd-"), (m.control, "ctrl-"), (m.alt, "alt-"), (m.shift, "shift-")] {
        if on {
            out.push_str(name);
        }
    }
    out.push_str(&k.key);
    out
}

/// A keystroke in one fixed modifier order (ctrl-alt-shift-cmd), for comparing.
pub fn keystroke_text(k: &Keystroke) -> String {
    let m = &k.modifiers;
    let mut out = String::new();
    for (on, name) in [(m.control, "ctrl-"), (m.alt, "alt-"), (m.shift, "shift-"), (m.platform, "cmd-")] {
        if on {
            out.push_str(name);
        }
    }
    out.push_str(&k.key);
    out
}

/// Named punctuation keys (the daemon's catalog says `cmd-comma`) -> the characters GPUI
/// matches (`cmd-,`).
pub fn normalize_keys(keys: &str) -> String {
    keys.split_whitespace()
        .map(|stroke| match stroke.rsplit_once('-') {
            Some((mods, key)) => {
                let k = match key {
                    "comma" => ",",
                    "period" => ".",
                    "slash" => "/",
                    "semicolon" => ";",
                    "quote" => "'",
                    "backslash" => "\\",
                    "backtick" => "`",
                    "bracketleft" => "[",
                    "bracketright" => "]",
                    "equal" => "=",
                    k => k,
                };
                format!("{mods}-{k}")
            }
            None => stroke.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// "cmd-shift-t" -> "⇧⌘T" for display; off macOS "ctrl-shift-t" -> "Ctrl+Shift+T".
pub fn pretty(keys: &str) -> String {
    if keys.trim().is_empty() {
        return String::new();
    }
    let keys = &normalize_keys(keys);
    let mut out = String::new();
    for (i, stroke) in keys.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        // the key itself can be "-" ("cmd--")
        let (mods, key): (Vec<&str>, &str) = match stroke.strip_suffix("--") {
            Some(m) => (m.split('-').collect(), "-"),
            None if stroke == "-" => (vec![], "-"),
            None => match stroke.rsplit_once('-') {
                Some((m, k)) => (m.split('-').collect(), k),
                None => (vec![], stroke),
            },
        };
        if cfg!(target_os = "macos") {
            pretty_mac(&mods, key, &mut out);
        } else {
            pretty_words(&mods, key, &mut out);
        }
    }
    out
}

/// One keystroke as macOS writes it: ⌃⌥⇧⌘ then the key.
fn pretty_mac(mods: &[&str], key: &str, out: &mut String) {
    let has = |m: &str| mods.contains(&m);
    if has("ctrl") {
        out.push('⌃');
    }
    if has("alt") {
        out.push('⌥');
    }
    if has("shift") {
        out.push('⇧');
    }
    if has("cmd") {
        out.push('⌘');
    }
    out.push_str(&match key {
        "enter" => "↩".to_string(),
        "backspace" => "⌫".to_string(),
        "escape" => "⎋".to_string(),
        "space" => "Space".to_string(),
        "tab" => "⇥".to_string(),
        "up" => "↑".into(),
        "down" => "↓".into(),
        "left" => "←".into(),
        "right" => "→".into(),
        "pageup" => "PgUp".into(),
        "pagedown" => "PgDn".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        "delete" => "⌦".into(),
        k => k.to_uppercase(),
    });
}

/// A key written in macOS glyphs ("⇧⌘C", "⌥↩") as this platform shows it: as written on
/// macOS; elsewhere in words with ⌘ read as Ctrl ("Ctrl+Shift+C", "Alt+Enter"). For the keys of
/// places where macOS's ⌘ is a plain Ctrl off macOS: menus, overlays, text fields (in the
/// terminal it's Ctrl+Shift instead: [`term_keys`]).
pub fn glyph_keys(s: &str) -> String {
    if cfg!(target_os = "macos") || s.is_empty() {
        return s.to_string();
    }
    let mut mods: Vec<&str> = vec![];
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        let m = match c {
            '⌃' | '⌘' => "ctrl",
            '⌥' => "alt",
            '⇧' => "shift",
            _ => break,
        };
        if !mods.contains(&m) {
            mods.push(m);
        }
        rest = &rest[c.len_utf8()..];
    }
    let key = match rest {
        "↩" => "enter",
        "⌫" => "backspace",
        "⎋" => "escape",
        "⇥" => "tab",
        "⌦" => "delete",
        k => k,
    };
    let mut out = String::new();
    pretty_words(&mods, &key.to_lowercase(), &mut out);
    out
}

/// One keystroke as Windows writes it: Ctrl+Alt+Shift+Win+key.
fn pretty_words(mods: &[&str], key: &str, out: &mut String) {
    for (m, name) in [("ctrl", "Ctrl"), ("alt", "Alt"), ("shift", "Shift"), ("cmd", "Win")] {
        if mods.contains(&m) {
            out.push_str(name);
            out.push('+');
        }
    }
    out.push_str(&match key {
        "enter" => "Enter".to_string(),
        "backspace" => "Backspace".to_string(),
        "escape" => "Esc".to_string(),
        "space" => "Space".to_string(),
        "tab" => "Tab".to_string(),
        "up" => "↑".into(),
        "down" => "↓".into(),
        "left" => "←".into(),
        "right" => "→".into(),
        "pageup" => "PgUp".into(),
        "pagedown" => "PgDn".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        "delete" => "Del".into(),
        k => k.to_uppercase(),
    });
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    #[test]
    fn pretty_keys() {
        assert_eq!(super::pretty("cmd-enter"), "⌘↩");
        assert_eq!(super::pretty("cmd-shift-t"), "⇧⌘T");
        assert_eq!(super::pretty("cmd-backspace"), "⌘⌫");
        assert_eq!(super::pretty("cmd-,"), "⌘,");
        assert_eq!(super::pretty("cmd-comma"), "⌘,");
        assert_eq!(super::normalize_keys("cmd-shift-comma"), "cmd-shift-,");
        let ks = gpui_kit::Keystroke::parse(&super::normalize_keys("cmd-comma")).unwrap();
        assert_eq!((ks.key.as_str(), ks.modifiers.platform), (",", true));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn pretty_keys_in_words() {
        assert_eq!(super::pretty("ctrl-enter"), "Ctrl+Enter");
        assert_eq!(super::pretty("ctrl-alt-shift-t"), "Ctrl+Alt+Shift+T");
        assert_eq!(super::pretty("alt-shift-left"), "Alt+Shift+←");
        assert_eq!(super::pretty("ctrl-comma"), "Ctrl+,");
        assert_eq!(super::pretty("ctrl--"), "Ctrl+-");
        assert_eq!(super::pretty("ctrl-k ctrl-s"), "Ctrl+K Ctrl+S");
        assert_eq!(super::pretty("shift-f3"), "Shift+F3");
        assert_eq!(super::pretty("cmd-t"), "Win+T");
        assert_eq!(super::glyph_keys("⇧⌘C"), "Ctrl+Shift+C");
        assert_eq!(super::glyph_keys("⌥↩"), "Alt+Enter");
        assert_eq!(super::glyph_keys("⌘⇧↩"), "Ctrl+Shift+Enter");
        assert_eq!(super::glyph_keys("⌥↑↓"), "Alt+↑↓");
        assert_eq!(super::glyph_keys("↩"), "Enter");
    }

    /// No two shortcuts share keys: a setting's default against another's, and (off macOS,
    /// where the scheme is new) a shortcut that reaches the terminal against the terminal's
    /// own keys.
    #[test]
    fn default_shortcuts_dont_collide() {
        let mut seen: std::collections::HashMap<String, &str> = std::collections::HashMap::new();
        for s in super::SHORTCUTS {
            let k = super::normalize_keys(super::default_key(s.setting));
            if k.is_empty() {
                continue;
            }
            let k = super::canonical(&k);
            if let Some(other) = seen.insert(k.clone(), s.setting) {
                panic!("{} and {other} both default to {k}", s.setting);
            }
        }
        if cfg!(target_os = "macos") {
            return;
        }
        let reaches_terminal = [super::WINDOW, super::TERM, super::ANYWHERE, super::UNCOVERED];
        seen.retain(|_, s| super::SHORTCUTS.iter().any(|x| x.setting == *s && reaches_terminal.contains(&x.when)));
        for f in super::FIXED.iter().filter(|f| f.when == super::TERM) {
            for k in f.keys {
                if let Some(s) = seen.get(&super::canonical(k)) {
                    panic!("{s} defaults to {k}, which the terminal keeps for {:?}", f.title);
                }
            }
        }
    }

    #[test]
    fn when_clauses_parse() {
        for s in super::SHORTCUTS.iter().chain(super::OVERLAY_KEYS) {
            if let Some(w) = s.when {
                assert!(gpui_kit::KeyBindingContextPredicate::parse(w).is_ok(), "{}: {w:?}", s.setting);
            }
        }
    }

    #[test]
    fn where_labels() {
        use super::where_label as w;
        assert_eq!(w(super::ANYWHERE), "Anywhere");
        assert_eq!(w(Some("*")), "Anywhere");
        assert_eq!(w(super::WINDOW), "Main window, not in sheets or popovers");
        assert_eq!(w(super::UNCOVERED), "Not in sheets or popovers");
        assert_eq!(w(super::COVERED), "Sheets or popovers");
        assert_eq!(w(super::TERM), "Terminal");
        assert_eq!(w(Some("overlay == annotate")), "Image sheet");
        assert_eq!(w(Some("overlay == links || overlay == queue || overlay == ide")), "Links, queue or IDE picker");
        assert_eq!(w(Some("MidnaMain && !(Modal || overlay == command_bar)")), "Main window, not in sheets or command bar");
        assert_eq!(w(Some("MidnaMain && overlay != annotate")), "Main window, not in image sheet");
        assert_eq!(w(Some("MidnaMain > Terminal")), "Terminal in main window");
        // names it doesn't know show as written
        assert_eq!(w(Some("Editor && !Vim")), "Editor, not in Vim");
        assert_eq!(w(Some("Terminal && !(Popover || Vim)")), "Terminal, not in popovers or Vim");
        assert_eq!(w(Some("MidnaMain &&")), "MidnaMain &&");
    }

    /// Every context and overlay a binding names has a human name.
    #[test]
    fn every_when_names_a_known_place() {
        let fixed = super::FIXED.iter().map(|f| (f.title, f.when));
        let bound = super::SHORTCUTS.iter().chain(super::OVERLAY_KEYS).map(|s| (s.setting, s.when));
        for (what, when) in bound.chain(fixed) {
            let Some(w) = when else { continue };
            assert!(gpui_kit::KeyBindingContextPredicate::parse(w).is_ok(), "{what}: {w:?}");
            for word in w.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|s| !s.is_empty() && *s != "overlay") {
                assert!(super::PLACES.iter().any(|(c, _)| *c == word), "{what}: no name for {word:?} in actions::PLACES");
            }
        }
    }

    #[test]
    fn daemon_when_check_agrees_with_gpui() {
        let cases = [
            "MidnaMain", "MidnaMain && !Modal && !Popover", "a || b && !(c || d)", "Workspace > Terminal", "overlay == annotate",
            "a != b && c", "  x-y_z  ", "a == (b)", "", "&& a", "a &&", "(a", "a)", "a b", "!(a) == b", "a == (b && c)",
            "a == b == c", "a $ b",
        ];
        for w in cases.into_iter().chain(super::SHORTCUTS.iter().chain(super::OVERLAY_KEYS).filter_map(|s| s.when)) {
            let gpui = gpui_kit::KeyBindingContextPredicate::parse(w).is_ok();
            assert_eq!(midna_proto::settings::check_when(w).is_ok(), gpui, "{w:?} (GPUI ok: {gpui})");
        }
    }

    #[test]
    fn keys_when_overrides_a_shortcuts_clause() {
        let s = |k: &str| super::SHORTCUTS.iter().find(|s| s.setting == k).unwrap();
        let rules = ["keys.new_agent = MidnaMain".to_string(), "keys.split = *".into(), "keys.rules = MidnaMain &&".into()];
        assert_eq!(super::effective_when(s("keys.new_agent"), &rules).as_deref(), Some("MidnaMain"));
        assert_eq!(super::effective_when(s("keys.split"), &rules), None);
        // One GPUI can't parse keeps the built-in clause, as does a shortcut without a rule.
        assert_eq!(super::effective_when(s("keys.rules"), &rules).as_deref(), super::WINDOW);
        assert_eq!(super::effective_when(s("keys.deny"), &rules).as_deref(), super::WINDOW);
        assert_eq!(super::effective_when(s("keys.quit"), &[]), None);
    }

    #[test]
    fn overlay_keys_name_real_overlays() {
        for s in super::OVERLAY_KEYS {
            let Some(id) = s.when.and_then(|w| w.strip_prefix("overlay == ")) else { continue };
            assert!(super::overlay::ALL.contains(&id), "{}: no overlay {id:?}", s.setting);
        }
    }
}

/// The default bindings in a window shaped like the main one: `MidnaMain` with an overlay in
/// it that has focus. Each test presses keys and reads which actions ran.
#[cfg(test)]
mod dispatch {
    // Not `super::*`: GPUI's test macro would come in with it and shadow `#[test]`.
    use super::{
        ApproveOnce, CTX_MAIN, CloseWindow, Deny, Dismiss, HoldToQuit, Kind, NewAgent, OpenSettings, SelectProject, SplitRight,
        bind_keys, overlay, overlay_context,
    };
    use crate::annotate::{AttachImages, RemoveImage};
    use crate::ui::summary::{RemoveRecord, ResumeRecord};
    use crate::ui::links::ToggleLinks;
    use gpui_kit::prelude::*;
    use gpui_kit::{Action, App, Context, FocusHandle, TestAppContext, Window, div};
    use std::cell::RefCell;
    use std::rc::Rc;

    type Log = Rc<RefCell<Vec<&'static str>>>;

    struct Probe {
        kind: Kind,
        id: &'static str,
        focus: FocusHandle,
        log: Log,
    }

    fn record<A: Action>(log: &Log, name: &'static str) -> impl Fn(&A, &mut Window, &mut App) + 'static {
        let log = log.clone();
        move |_, _, _| log.borrow_mut().push(name)
    }

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let l = &self.log;
            div()
                .key_context(CTX_MAIN)
                .size_full()
                .on_action(record::<NewAgent>(l, "new_agent"))
                .on_action(record::<SplitRight>(l, "split"))
                .on_action(record::<SelectProject>(l, "select_project"))
                .on_action(record::<ApproveOnce>(l, "approve"))
                .on_action(record::<Deny>(l, "deny"))
                .on_action(record::<CloseWindow>(l, "close_window"))
                .on_action(record::<OpenSettings>(l, "settings"))
                .on_action(record::<HoldToQuit>(l, "quit"))
                .child(
                    div()
                        .key_context(overlay_context(self.kind, self.id))
                        .track_focus(&self.focus)
                        .size_full()
                        .on_action(record::<RemoveImage>(l, "remove_image"))
                        .on_action(record::<AttachImages>(l, "attach"))
                        .on_action(record::<Dismiss>(l, "dismiss"))
                        .on_action(record::<ToggleLinks>(l, "toggle_links"))
                        .on_action(record::<ResumeRecord>(l, "resume_record"))
                        .on_action(record::<RemoveRecord>(l, "remove_record")),
                )
        }
    }

    /// Opens the window with the overlay focused, then presses each key and collects what ran.
    fn press(kind: Kind, id: &'static str, keys: &[&str], cx: &mut TestAppContext) -> Vec<(String, Vec<&'static str>)> {
        press_with(&[], kind, id, keys, cx)
    }

    /// [`press`] with `keys.when` rules applied.
    fn press_with(when: &[&str], kind: Kind, id: &'static str, keys: &[&str], cx: &mut TestAppContext) -> Vec<(String, Vec<&'static str>)> {
        let when: Vec<String> = when.iter().map(|w| w.to_string()).collect();
        cx.update(|cx| bind_keys(cx, |_| None, &when));
        let log = Log::default();
        let l = log.clone();
        let window = cx.add_window(|window, cx| {
            let focus = cx.focus_handle();
            window.focus(&focus, cx);
            Probe { kind, id, focus, log: l }
        });
        cx.run_until_parked();
        keys.iter()
            .map(|k| {
                cx.simulate_keystrokes(window.into(), k);
                (k.to_string(), std::mem::take(&mut *log.borrow_mut()))
            })
            .collect()
    }

    fn ran(k: &str, actions: &[&'static str]) -> (String, Vec<&'static str>) {
        (k.to_string(), actions.to_vec())
    }

    /// A shortcut's default keys on this platform.
    fn key(setting: &str) -> String {
        super::normalize_keys(super::default_key(setting))
    }

    /// Project 1's keys on this platform.
    fn project_1() -> String {
        super::pick("cmd-1", "ctrl-1").to_string()
    }

    #[gpui_kit::test]
    fn image_sheet_blocks_main_shortcuts(cx: &mut TestAppContext) {
        let [t, d, w, enter, q, comma] = ["keys.new_agent", "keys.split", "keys.close", "keys.approve", "keys.quit", "keys.settings"].map(key);
        let p1 = project_1();
        let keys: [&str; 8] = [&t, &d, &p1, &w, &enter, &q, &comma, "escape"];
        let got = press(Kind::Modal, overlay::ANNOTATE, &keys, cx);
        assert_eq!(
            got,
            vec![
                ran(&t, &[]),
                ran(&d, &[]),
                ran(&p1, &[]),
                ran(&w, &["remove_image"]),
                ran(&enter, &["attach"]),
                ran(&q, &["quit"]),
                ran(&comma, &["settings"]),
                ran("escape", &["dismiss"]),
            ]
        );
    }

    #[gpui_kit::test]
    fn popover_blocks_main_shortcuts(cx: &mut TestAppContext) {
        let [t, enter, w, l, comma] = ["keys.new_agent", "keys.approve", "keys.close", "keys.links", "keys.settings"].map(key);
        let got = press(Kind::Popover, overlay::LINKS, &[&t, &enter, &w, &l, &comma].map(|s| s.as_str()), cx);
        assert_eq!(
            got,
            vec![
                ran(&t, &[]),
                ran(&enter, &[]),
                ran(&w, &["dismiss"]),
                ran(&l, &["toggle_links"]),
                ran(&comma, &["settings"]),
            ]
        );
    }

    #[gpui_kit::test]
    fn screens_keep_main_shortcuts(cx: &mut TestAppContext) {
        let (t, p1, w) = (key("keys.new_agent"), project_1(), key("keys.close"));
        let got = press(Kind::Screen, overlay::RULES, &[&t, &p1, &w].map(|s| s.as_str()), cx);
        assert_eq!(got, vec![ran(&t, &["new_agent"]), ran(&p1, &["select_project"]), ran(&w, &["close_window"])]);
    }

    #[gpui_kit::test]
    fn summary_takes_approve_and_deny_for_its_record(cx: &mut TestAppContext) {
        // ⌘↩, ⌘⌫ and ⌘T on macOS.
        let (yes, no, t) = (key("keys.approve"), key("keys.deny"), key("keys.new_agent"));
        let got = press(Kind::Screen, overlay::SUMMARY, &[&yes, &no, &t].map(|s| s.as_str()), cx);
        assert_eq!(got, vec![ran(&yes, &["resume_record"]), ran(&no, &["remove_record"]), ran(&t, &["new_agent"])]);
        // Any other screen leaves them to the main window.
        let got = press(Kind::Screen, overlay::RULES, &[&yes, &no].map(|s| s.as_str()), cx);
        assert_eq!(got, vec![ran(&yes, &["approve"]), ran(&no, &["deny"])]);
    }

    #[gpui_kit::test]
    fn keys_when_moves_a_shortcut(cx: &mut TestAppContext) {
        // New agent made to work over the image sheet, split anywhere; project 1 isn't a
        // setting and stays put.
        let when = ["keys.new_agent = MidnaMain", "keys.split = *"];
        let (t, d, p1) = (key("keys.new_agent"), key("keys.split"), project_1());
        let got = press_with(&when, Kind::Modal, overlay::ANNOTATE, &[&t, &d, &p1].map(|s| s.as_str()), cx);
        assert_eq!(got, vec![ran(&t, &["new_agent"]), ran(&d, &["split"]), ran(&p1, &[])]);
        cx.update(|cx| {
            assert_eq!(super::when_clause(cx, "keys.new_agent").as_deref(), Some("MidnaMain"));
            assert_eq!(super::when_clause(cx, "keys.split"), None);
            assert_eq!(super::when_clause(cx, "keys.deny").as_deref(), super::WINDOW);
        });
    }
}
