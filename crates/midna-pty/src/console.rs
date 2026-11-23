//! Reading the shell's screen from its console host: the keeper attaches to the shell's console
//! for a moment, reads the visible window of the active screen buffer and detaches. T164's spike
//! found this gives the exact text and cursor on both ConPTYs in about 5 ms, where a resize nudge
//! redraws nothing on the newer one.
use crate::proto::Snapshot;
use crate::shell::Shell;
use crate::win::{owned, raw, wide};
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows_sys::Win32::System::Console::{
    AttachConsole, CHAR_INFO, CONSOLE_CURSOR_INFO, CONSOLE_SCREEN_BUFFER_INFO, COORD, CTRL_BREAK_EVENT, CTRL_C_EVENT, FreeConsole, GetConsoleCursorInfo,
    GetConsoleScreenBufferInfo, ReadConsoleOutputW, SMALL_RECT, SetConsoleCtrlHandler,
};

/// Held while attached to the shell's console, and while closing it: closing sends every process
/// attached to it CTRL_CLOSE_EVENT, which ends a process whatever its handler says, so the keeper
/// is never attached then.
static ATTACH: Mutex<()> = Mutex::new(());

/// Close the shell's pseudoconsole (see [`Shell::close_console`]), never while a snapshot is
/// being read.
pub fn close(shell: &Shell) {
    let _one = ATTACH.lock().unwrap_or_else(|e| e.into_inner());
    shell.close_console();
}

unsafe extern "system" fn ignore_interrupts(kind: u32) -> windows_sys::core::BOOL {
    (kind == CTRL_C_EVENT || kind == CTRL_BREAK_EVENT) as i32
}

/// The keeper never acts on a console's Ctrl+C or Ctrl+Break: while it's attached for a
/// snapshot, one typed in the terminal reaches it too, and must not end it. Called once, at
/// start.
///
/// It also turns Ctrl+C handling back on for the keeper itself. A process ignores Ctrl+C when it
/// was started in a new process group or called `SetConsoleCtrlHandler(NULL, TRUE)`, and the
/// programs it starts inherit that: the shell would then ignore every Ctrl+C typed in the
/// terminal. So the keeper only sets it while attached ([`snapshot`]), never while it starts
/// the shell.
pub fn ignore_console_interrupts() {
    unsafe {
        SetConsoleCtrlHandler(None, 0);
        SetConsoleCtrlHandler(Some(ignore_interrupts), 1);
    }
}

/// The visible screen of the shell's console.
pub fn snapshot(shell: &Shell) -> Result<Snapshot, String> {
    // A close that's under way (Windows' own ConPTY can take seconds) answers at once instead.
    let Ok(_one) = ATTACH.try_lock() else { return Err("the console is closing".into()) };
    if !shell.console_open() {
        return Err("the console is closed".into());
    }
    // SAFETY: console calls on handles we own; each buffer is sized for the region it receives.
    unsafe {
        SetConsoleCtrlHandler(None, 1);
        FreeConsole();
        let r = if AttachConsole(shell.pid) == 0 {
            Err(format!("can't attach to the shell's console: {}", std::io::Error::last_os_error()))
        } else {
            let r = read_screen();
            FreeConsole();
            r
        };
        SetConsoleCtrlHandler(None, 0);
        r
    }
}

unsafe fn read_screen() -> Result<Snapshot, String> {
    unsafe {
        let name = wide("CONOUT$");
        let out = owned(CreateFileW(name.as_ptr(), GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut()))
            .map_err(|e| format!("CONOUT$: {e}"))?;
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(raw(&out), &mut info) == 0 {
            return Err(format!("GetConsoleScreenBufferInfo: {}", std::io::Error::last_os_error()));
        }
        let w = info.srWindow;
        let (cols, rows) = ((w.Right - w.Left + 1).max(0) as usize, (w.Bottom - w.Top + 1).max(0) as usize);
        let mut chars = Vec::with_capacity(rows);
        let mut attrs = Vec::with_capacity(rows);
        // A row at a time: one call for a large window can be refused for its size.
        let mut row: Vec<CHAR_INFO> = vec![std::mem::zeroed(); cols];
        for y in 0..rows {
            let mut region = SMALL_RECT { Left: w.Left, Top: w.Top + y as i16, Right: w.Right, Bottom: w.Top + y as i16 };
            if ReadConsoleOutputW(raw(&out), row.as_mut_ptr(), COORD { X: cols as i16, Y: 1 }, COORD { X: 0, Y: 0 }, &mut region) == 0 {
                return Err(format!("ReadConsoleOutputW: {}", std::io::Error::last_os_error()));
            }
            chars.push(row.iter().map(|c| c.Char.UnicodeChar).collect());
            attrs.push(row.iter().map(|c| c.Attributes).collect());
        }
        let mut cursor: CONSOLE_CURSOR_INFO = std::mem::zeroed();
        let visible = GetConsoleCursorInfo(raw(&out), &mut cursor) == 0 || cursor.bVisible != 0;
        let c = info.dwCursorPosition;
        Ok(Snapshot {
            cols: cols as u16,
            rows: rows as u16,
            cursor_col: (c.X - w.Left).clamp(0, (cols as i16 - 1).max(0)) as u16,
            cursor_row: (c.Y - w.Top).clamp(0, (rows as i16 - 1).max(0)) as u16,
            cursor_visible: visible,
            chars,
            attrs,
        })
    }
}
