//! Windows: the title row midna draws above a window's content.
//!
//! Every midna window hides the system title bar (`appears_transparent`). On macOS, AppKit
//! still draws the traffic lights and drags the window by its top. On Windows nothing does, so
//! this row stands in: GPUI hands its areas to Windows (`WindowControlArea`). The row is the
//! caption, which gives moving, double-click to maximise, Snap and the system menu. Its three
//! buttons are minimise, maximise/restore and close.
//!
//! GPUI takes the first control area under the pointer in paint order, parent before children,
//! so each button occludes the row behind it.
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;

/// `content` under the title row (Windows); `content` as it is elsewhere.
#[cfg(not(windows))]
pub fn wrap<E: IntoElement>(_t: &Theme, _window: &Window, content: E) -> E {
    content
}

#[cfg(windows)]
pub fn wrap<E: IntoElement>(t: &Theme, window: &Window, content: E) -> Div {
    div().size_full().flex().flex_col().bg(t.bg).child(row(t, window)).child(div().flex_1().min_h_0().child(content))
}

#[cfg(windows)]
const HEIGHT: f32 = 32.;

/// The glyphs Windows' own caption buttons use (Segoe MDL2 Assets, on Windows 10 and 11).
#[cfg(windows)]
const ICONS: &str = "Segoe MDL2 Assets";

#[cfg(windows)]
fn row(t: &Theme, window: &Window) -> Stateful<Div> {
    let (max_glyph, max_id) = if window.is_maximized() { ("\u{E923}", "caption-restore") } else { ("\u{E922}", "caption-max") };
    let fg = if crate::windows::in_use(window) { t.dim } else { t.dim.opacity(0.6) };
    div()
        .id("caption")
        .flex()
        .flex_none()
        .items_center()
        .h(px(HEIGHT))
        .border_b_1()
        .border_color(t.line)
        .bg(t.panel)
        .window_control_area(WindowControlArea::Drag)
        .child(div().flex_1().min_w_0().pl(px(12.)).truncate().font_family(t.ui_font.clone()).text_size(px(12.)).text_color(fg).child(title(window)))
        .child(button(t, fg, "caption-min", "\u{E921}", WindowControlArea::Min))
        .child(button(t, fg, max_id, max_glyph, WindowControlArea::Max))
        .child(button(t, fg, "caption-close", "\u{E8BB}", WindowControlArea::Close))
}

/// The window's title, read from the HWND: GPUI sets it there but its `window_title()` reads
/// nothing back on Windows.
#[cfg(windows)]
fn title(window: &Window) -> String {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowTextW;
    let Ok(handle) = HasWindowHandle::window_handle(window) else { return String::new() };
    let RawWindowHandle::Win32(h) = handle.as_raw() else { return String::new() };
    let mut buf = [0u16; 256];
    // SAFETY: GPUI's own HWND, alive while `window` is; the buffer's length is passed.
    let n = unsafe { GetWindowTextW(h.hwnd.get() as _, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

#[cfg(windows)]
fn button(t: &Theme, fg: Hsla, id: &'static str, glyph: &'static str, area: WindowControlArea) -> Stateful<Div> {
    let close = matches!(area, WindowControlArea::Close);
    let hover_bg: Hsla = if close { rgb(0xc42b1c).into() } else { t.raised };
    let hover_fg = if close { white() } else { t.fg };
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(px(46.))
        .h_full()
        .occlude()
        .window_control_area(area)
        .font_family(ICONS)
        .text_size(px(10.))
        .text_color(fg)
        .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
        .child(glyph)
}
