//! Daemon upgrade / restart on Windows: not in place yet.
//!
//! macOS keeps every terminal across an upgrade by `execv`ing the new binary in the same
//! process, so the shells stay its children and the PTY fds stay open (`upgrade.rs`). Windows
//! has no exec, and a pseudoconsole dies with the process that created it, so a new daemon
//! can't take the old one's terminals. Keeping them will need the pseudoconsoles to live in a
//! separate host process that outlives the daemon (the way VS Code's pty host does). Until
//! then `daemon.upgrade` / `daemon.restart` are refused, with what to do instead, and a restart
//! means stopping the daemon (which closes its terminals) and starting the new one.
use crate::daemon::{Config, Daemon};
use crate::engine::Engine;
use midna_proto::*;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const HANDOFF_VERSION: u32 = 1;

/// A handoff to resume. Windows never writes one, so there is never one to resume.
pub enum Resume {}

pub fn handoff_dir(home: &Path) -> PathBuf {
    home.join("handoff")
}

const NOT_YET: &str = "midnad can't upgrade or restart in place on Windows yet (its terminals can't outlive the process that \
    runs them): stop the daemon (`midna daemon stop`, which closes every terminal) and start the new one";

/// `midnad --selftest`: the same engine round trip as on macOS.
pub fn selftest() -> Result<Value, String> {
    let mut e = Engine::new(40, 6, None);
    e.feed(b"selftest \x1b[1;32mok\x1b[0m\r\n\x1b]2;midna\x07\x1b[?1049halt\x1b[2;3H\x1b[3", 0);
    let snap = e.snapshot();
    let r = Engine::restored(&snap, None);
    if r.plain_lines() != e.plain_lines() || r.cursor() != e.cursor() || r.title() != "midna" {
        return Err("terminal snapshot round-trip mismatch".into());
    }
    Ok(json!({ "ok": true, "version": midna_proto::VERSION, "pid": std::process::id(), "handoff_versions": [] }))
}

pub fn restart_binary(_cfg: &Config) -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("midnad.exe"))
}

pub fn request(_d: &Arc<Daemon>, _bin: &Path, _reason: &str, _force: bool) -> Result<Value, RpcError> {
    Err(RpcError::refused(NOT_YET))
}

pub fn restart_now(_d: &Arc<Daemon>, _reason: &str) -> String {
    NOT_YET.into()
}

pub fn load(_path: &Path, _fallback: bool) -> Result<Resume, String> {
    Err(NOT_YET.into())
}

pub fn apply_config(r: &Resume, _cfg: &mut Config) {
    match *r {}
}

pub fn kept_sessions(r: &Resume) -> HashMap<Id, Option<i32>> {
    match *r {}
}

pub fn inherited_listener(r: &Resume) -> Option<midna_proto::ipc::UnixListener> {
    match *r {}
}

pub fn adopt_all(_d: &Arc<Daemon>, r: &Resume) {
    match *r {}
}

/// Remove a leftover handoff folder (one copied over from a Mac home, say).
pub fn set_aside_stale(home: &Path) {
    let _ = std::fs::remove_dir_all(handoff_dir(home));
}

pub fn watchdog_main(_path: &Path) -> ! {
    std::process::exit(0)
}
