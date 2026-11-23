//! Protocol 1 between midnad and a keeper, over the keeper's control pipe.
//!
//! Each message is a frame: its length as 4 bytes, little-endian, then that many bytes of one
//! JSON object whose `type` names the message. The protocol is frozen: a later build may add
//! messages and fields, but never changes or drops one, so any midnad can drive any keeper that
//! is still running. Unknown fields are ignored on both sides, and a request the keeper doesn't
//! know gets an [`Reply::Error`] naming it, so a newer midnad can tell what an older keeper lacks.
//!
//! Every request gets exactly one reply, in order, with the request's own `type` (or `error`).
//! The keeper also sends one message nobody asked for, [`Reply::Exited`], once per connection
//! when the shell has exited (right after its first reply if the shell had already exited).
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

pub const PROTOCOL: u32 = 1;

/// The largest frame either side accepts. A snapshot of a very large window is about 2 MB.
pub const MAX_FRAME: usize = 16 << 20;

/// Client to keeper.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// The client's protocol and build. Replied to with [`Reply::Hello`].
    Hello { protocol: u32, build: String },
    /// Start the shell (once per keeper): `argv` found on the `PATH` in `env` (a batch-file shim
    /// is run as what it runs), in `cwd` (the user's home if that isn't a folder), with exactly
    /// the environment `env`, in a pseudoconsole of `cols` by `rows`.
    Spawn { argv: Vec<String>, cwd: String, env: Vec<(String, String)>, cols: u16, rows: u16 },
    /// Copy the shell's input and output pipes and its process handle into the process `pid`,
    /// which must be the connected client.
    Attach { pid: u32 },
    Resize { cols: u16, rows: u16 },
    /// The visible screen as the console host holds it.
    Snapshot,
    /// Close the pseudoconsole, which gives the shell a console hang-up; its job is ended if it
    /// hasn't exited a few seconds later. The keeper exits once the shell has.
    Hangup,
    /// End every process in the shell's job (the force close). The keeper exits once the shell has.
    Kill,
}

impl Request {
    pub fn kind(&self) -> &'static str {
        match self {
            Request::Hello { .. } => "hello",
            Request::Spawn { .. } => "spawn",
            Request::Attach { .. } => "attach",
            Request::Resize { .. } => "resize",
            Request::Snapshot => "snapshot",
            Request::Hangup => "hangup",
            Request::Kill => "kill",
        }
    }
}

/// Keeper to client.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    Hello {
        protocol: u32,
        build: String,
        /// The keeper's own pid.
        pid: u32,
        /// None until a client has sent `spawn`.
        shell_pid: Option<u32>,
        /// The shell is still running.
        running: bool,
        exit_code: Option<u32>,
    },
    Spawn { shell_pid: u32 },
    /// Handle values valid in the client's process, which now owns them. `process` has
    /// SYNCHRONIZE and PROCESS_QUERY_LIMITED_INFORMATION: enough to wait for the shell and read
    /// its exit code.
    Attach { input: u64, output: u64, process: u64, shell_pid: u32 },
    Resize {},
    Snapshot(Snapshot),
    Hangup {},
    Kill {},
    /// The request named `request` failed, or this keeper doesn't know it.
    Error { request: String, message: String },
    /// The shell exited with `code`. Not a reply: sent once per connection.
    Exited { code: u32 },
}

/// The visible part of the console's active screen buffer (the alternate screen, when an app
/// is using one), read with the console API. Styles are the console's legacy attributes: the 16
/// colours, underline and inverse; 256 and RGB colours, italics and hyperlinks aren't kept, nor is
/// anything scrolled off the window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub cols: u16,
    pub rows: u16,
    /// The cursor, from the window's top left (0-based).
    pub cursor_col: u16,
    pub cursor_row: u16,
    pub cursor_visible: bool,
    /// Each row's cells, one UTF-16 code unit per cell, as the console holds them: a wide
    /// character fills two cells, flagged by [`LEADING`] and [`TRAILING`] in `attrs`.
    pub chars: Vec<Vec<u16>>,
    /// Each cell's attributes (`CHAR_INFO.Attributes`).
    pub attrs: Vec<Vec<u16>>,
}

/// `COMMON_LVB_LEADING_BYTE` and `COMMON_LVB_TRAILING_BYTE`: the two halves of a wide character.
pub const LEADING: u16 = 0x0100;
pub const TRAILING: u16 = 0x0200;
const REVERSE: u16 = 0x4000;
const UNDERSCORE: u16 = 0x8000;

impl Snapshot {
    /// The screen as VT that draws it on a blank terminal of the same size: each row's text in
    /// its colours, then the cursor where it was.
    pub fn to_vt(&self) -> String {
        // Console colour bits are BGR (blue 1, green 2, red 4); ANSI's are RGB.
        fn ansi(c: u16) -> u16 {
            (c & 4) >> 2 | (c & 2) | (c & 1) << 2
        }
        let mut vt = String::from("\x1b[0m\x1b[H\x1b[2J");
        for (y, (chars, attrs)) in self.chars.iter().zip(&self.attrs).enumerate() {
            vt.push_str(&format!("\x1b[{};1H", y + 1));
            let mut prev = None;
            let mut units = vec![];
            let flush = |vt: &mut String, units: &mut Vec<u16>| {
                vt.extend(char::decode_utf16(units.drain(..)).map(|c| c.unwrap_or('\u{fffd}')));
            };
            for (&ch, &a) in chars.iter().zip(attrs) {
                if a & TRAILING != 0 {
                    continue;
                }
                let style = a & !(LEADING | TRAILING);
                if prev != Some(style) {
                    flush(&mut vt, &mut units);
                    let (fg, bg) = (style & 0xf, (style >> 4) & 0xf);
                    // White on black is the console's default, and what an app that never set a
                    // colour has; it's drawn in the terminal's own colours.
                    let fg = if fg == 7 { 39 } else if fg & 8 != 0 { 90 + ansi(fg & 7) } else { 30 + ansi(fg) };
                    let bg = if bg == 0 { 49 } else if bg & 8 != 0 { 100 + ansi(bg & 7) } else { 40 + ansi(bg) };
                    vt.push_str(&format!("\x1b[0;{fg};{bg}"));
                    if style & UNDERSCORE != 0 {
                        vt.push_str(";4");
                    }
                    if style & REVERSE != 0 {
                        vt.push_str(";7");
                    }
                    vt.push('m');
                    prev = Some(style);
                }
                units.push(if ch == 0 { b' ' as u16 } else { ch });
            }
            flush(&mut vt, &mut units);
        }
        vt.push_str(&format!("\x1b[0m\x1b[{};{}H", self.cursor_row + 1, self.cursor_col + 1));
        vt.push_str(if self.cursor_visible { "\x1b[?25h" } else { "\x1b[?25l" });
        vt
    }

    /// Row `y`'s text, a wide character once.
    pub fn row_text(&self, y: usize) -> String {
        let (Some(chars), Some(attrs)) = (self.chars.get(y), self.attrs.get(y)) else { return String::new() };
        let units = chars.iter().zip(attrs).filter(|(_, a)| *a & TRAILING == 0).map(|(&c, _)| if c == 0 { b' ' as u16 } else { c });
        char::decode_utf16(units).map(|c| c.unwrap_or('\u{fffd}')).collect()
    }
}

pub fn write_frame(w: &mut impl Write, msg: &impl Serialize) -> io::Result<()> {
    let body = serde_json::to_vec(msg).map_err(io::Error::other)?;
    if body.len() > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("frame of {} bytes is over the limit", body.len())));
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    w.write_all(&frame)?;
    w.flush()
}

/// One frame's JSON object. `UnexpectedEof` when the other side closed between frames.
pub fn read_frame(r: &mut impl Read) -> io::Result<serde_json::Value> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("frame of {len} bytes is over the limit")));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if !v.is_object() || !v["type"].is_string() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "a frame is a JSON object with a type"));
    }
    Ok(v)
}

/// A request as the keeper reads it: Err(the reply) for one it can't take, naming the request.
pub fn parse_request(v: serde_json::Value) -> Result<Request, Reply> {
    let kind = v["type"].as_str().unwrap_or_default().to_string();
    serde_json::from_value(v).map_err(|e| {
        let message = if KNOWN.contains(&kind.as_str()) { format!("bad {kind}: {e}") } else { format!("unknown message {kind:?}") };
        Reply::Error { request: kind, message }
    })
}

const KNOWN: [&str; 7] = ["hello", "spawn", "attach", "resize", "snapshot", "hangup", "kill"];

/// A message as a client reads it: None for one this build doesn't know (from a newer keeper).
pub fn parse_reply(v: serde_json::Value) -> Option<Reply> {
    serde_json::from_value(v).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn roundtrip(msg: &impl Serialize) -> serde_json::Value {
        let mut buf = vec![];
        write_frame(&mut buf, msg).unwrap();
        assert_eq!(u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize, buf.len() - 4);
        read_frame(&mut &buf[..]).unwrap()
    }

    #[test]
    fn every_request_round_trips_with_its_type() {
        let all = [
            Request::Hello { protocol: 1, build: "0.1.0".into() },
            Request::Spawn { argv: vec!["cmd".into(), "/d".into()], cwd: r"C:\work".into(), env: vec![("PATH".into(), r"C:\bin".into())], cols: 80, rows: 24 },
            Request::Attach { pid: 42 },
            Request::Resize { cols: 100, rows: 30 },
            Request::Snapshot,
            Request::Hangup,
            Request::Kill,
        ];
        for r in all {
            let v = roundtrip(&r);
            assert_eq!(v["type"], r.kind());
            assert_eq!(parse_request(v).unwrap(), r);
        }
    }

    #[test]
    fn the_wire_format_is_the_frozen_one() {
        let spawn = Request::Spawn { argv: vec!["cmd".into()], cwd: "C:\\".into(), env: vec![("A".into(), "1".into())], cols: 80, rows: 24 };
        assert_eq!(serde_json::to_value(&spawn).unwrap(), json!({"type": "spawn", "argv": ["cmd"], "cwd": "C:\\", "env": [["A", "1"]], "cols": 80, "rows": 24}));
        assert_eq!(serde_json::to_value(Request::Snapshot).unwrap(), json!({"type": "snapshot"}));
        assert_eq!(serde_json::to_value(Reply::Exited { code: 7 }).unwrap(), json!({"type": "exited", "code": 7}));
        assert_eq!(serde_json::to_value(Reply::Kill {}).unwrap(), json!({"type": "kill"}));
        let mut buf = vec![];
        write_frame(&mut buf, &Request::Kill).unwrap();
        assert_eq!(buf, b"\x0f\x00\x00\x00{\"type\":\"kill\"}");
    }

    #[test]
    fn replies_round_trip() {
        let snap = Snapshot { cols: 2, rows: 1, cursor_col: 1, cursor_row: 0, cursor_visible: true, chars: vec![vec![65, 66]], attrs: vec![vec![7, 7]] };
        for r in [
            Reply::Hello { protocol: 1, build: "b".into(), pid: 1, shell_pid: Some(2), running: true, exit_code: None },
            Reply::Spawn { shell_pid: 9 },
            Reply::Attach { input: 1, output: 2, process: 3, shell_pid: 4 },
            Reply::Resize {},
            Reply::Snapshot(snap),
            Reply::Hangup {},
            Reply::Error { request: "x".into(), message: "y".into() },
            Reply::Exited { code: 7 },
        ] {
            assert_eq!(parse_reply(roundtrip(&r)).unwrap(), r);
        }
    }

    #[test]
    fn unknown_messages_and_fields() {
        // A newer midnad's message: an error that names it, so the keeper stays usable.
        let Err(Reply::Error { request, message }) = parse_request(json!({"type": "teleport", "to": 3})) else { panic!() };
        assert_eq!(request, "teleport");
        assert!(message.contains("unknown"), "{message}");
        let Err(Reply::Error { message, .. }) = parse_request(json!({"type": "resize", "cols": "wide"})) else { panic!() };
        assert!(message.starts_with("bad resize"), "{message}");
        // Fields added later are ignored.
        assert_eq!(parse_request(json!({"type": "attach", "pid": 5, "since": 2})).unwrap(), Request::Attach { pid: 5 });
        assert_eq!(parse_reply(json!({"type": "exited", "code": 1, "at": 3})).unwrap(), Reply::Exited { code: 1 });
        // A newer keeper's message: skipped by an older client.
        assert_eq!(parse_reply(json!({"type": "title", "text": "x"})), None);
    }

    #[test]
    fn frames_that_are_cut_short_too_big_or_not_objects() {
        let mut buf = vec![];
        write_frame(&mut buf, &Request::Hangup).unwrap();
        let e = read_frame(&mut &buf[..buf.len() - 1]).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(read_frame(&mut &b""[..]).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        let huge = ((MAX_FRAME + 1) as u32).to_le_bytes();
        assert_eq!(read_frame(&mut &huge[..]).unwrap_err().kind(), io::ErrorKind::InvalidData);
        let mut arr = 2u32.to_le_bytes().to_vec();
        arr.extend_from_slice(b"[]");
        assert_eq!(read_frame(&mut &arr[..]).unwrap_err().kind(), io::ErrorKind::InvalidData);
        // Two frames back to back read one at a time.
        let mut two = vec![];
        write_frame(&mut two, &Request::Snapshot).unwrap();
        write_frame(&mut two, &Request::Kill).unwrap();
        let mut r = &two[..];
        assert_eq!(read_frame(&mut r).unwrap()["type"], "snapshot");
        assert_eq!(read_frame(&mut r).unwrap()["type"], "kill");
    }

    #[test]
    fn a_snapshot_draws_as_vt_with_wide_characters_once() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        // "a漢b": the wide character fills two cells, flagged leading and trailing.
        let han = w("漢")[0];
        let snap = Snapshot {
            cols: 4,
            rows: 2,
            cursor_col: 3,
            cursor_row: 1,
            cursor_visible: false,
            chars: vec![vec![w("a")[0], han, han, w("b")[0]], vec![w("x")[0], 0, 0, 0]],
            // Row 0: default, then bright red on blue for the wide character, then underlined.
            // Row 1: inverse.
            attrs: vec![vec![0x07, 0x1c | LEADING, 0x1c | TRAILING, 0x8007], vec![0x4007, 0x07, 0x07, 0x07]],
        };
        assert_eq!(snap.row_text(0), "a漢b");
        assert_eq!(snap.row_text(1), "x   ");
        let vt = snap.to_vt();
        assert_eq!(
            vt,
            "\x1b[0m\x1b[H\x1b[2J\x1b[1;1H\x1b[0;39;49ma\x1b[0;91;44m漢\x1b[0;39;49;4mb\x1b[2;1H\x1b[0;39;49;7mx\x1b[0;39;49m   \x1b[0m\x1b[2;4H\x1b[?25l"
        );
    }

    #[test]
    fn a_character_outside_the_bmp_comes_back_as_one_replacement_character() {
        // What both ConPTYs' console hosts return for 😀 (tests/keeper.rs): U+FFFD in each half.
        let snap = Snapshot {
            cols: 3,
            rows: 1,
            cursor_col: 0,
            cursor_row: 0,
            cursor_visible: true,
            chars: vec![vec![0xfffd, 0xfffd, b'x' as u16]],
            attrs: vec![vec![0x07 | LEADING, 0x07 | TRAILING, 0x07]],
        };
        assert_eq!(snap.row_text(0), "\u{fffd}x");
        // A lone surrogate (a host that splits a pair across the cells) doesn't break the row.
        let split = Snapshot { chars: vec![vec![0xd83d, 0xde00, b'x' as u16]], attrs: vec![vec![0x07 | LEADING, 0x07 | TRAILING, 0x07]], ..snap };
        assert_eq!(split.row_text(0), "\u{fffd}x");
    }
}
