//! Daemon upgrade / restart on Windows: a new midnad takes every terminal over from the old one.
//!
//! macOS keeps every terminal across an upgrade by `execv`ing the new binary in the same
//! process, so the shells stay its children and the PTY fds stay open (`upgrade.rs`). Windows
//! has no exec, but each terminal runs in a keeper process that outlives midnad
//! (`pty_windows.rs`), so `daemon.upgrade` / `daemon.restart` hand the keepers to a new midnad:
//!
//! 1. Preflight: `<new> --selftest` must list our `HANDOFF_VERSION`, or nothing changes (a build
//!    from before the keepers lists only `RELAUNCH_VERSION`: see "Reopening" below).
//! 2. The readers pause (a read waiting on a terminal is cancelled, taking nothing), every engine
//!    snapshots its terminal and `MIDNA_HOME\handoff\` gets `handoff.json` (each session's keeper
//!    pipe and pid in place of the Mac's fd) plus one `.vt` file per screen. state.json is saved,
//!    and saved no more by this midnad.
//! 3. It lets go of each keeper's control pipe (a keeper serves one client at a time), stops
//!    serving and closes the socket, deleting its file.
//! 4. It starts the new midnad (`--resume handoff.json`) without a window, inheriting none of its
//!    handles but its output.
//! 5. The new one attaches to every keeper, binds the socket, restores every screen, starts its
//!    readers, writes `handoff\resumed` and emits `daemon.upgraded`.
//! 6. The old one, watching, sees `resumed` and exits; its connections close, and clients
//!    reconnect, as on macOS. midnad's pid changes with every handover.
//!
//! The old midnad is its own watchdog. If the new one exits, or hasn't written `resumed` within
//! the watchdog's time (as on macOS: more on a loaded PC, and while the new one keeps using CPU),
//! the old one ends it, binds the socket again, connects to its keepers again, resumes its
//! readers and emits `daemon.upgrade_failed`. Nothing was lost: its copies of every terminal's
//! pipes and shell stayed open.
//!
//! # Reopening (a new build that can't take the keepers)
//!
//! A new midnad that can't take terminals over (a build from before the keepers) gets them
//! reopened instead, once every agent is idle:
//!
//! 1. Preflight: `<new> --selftest` must list our `RELAUNCH_VERSION`, or nothing changes.
//! 2. The restart waits until no agent would lose anything (`restart::blockers`: not in a turn
//!    or waiting on the human, no background work, an empty input box, quiet for
//!    `agents.restart_idle_secs`), checked with the queued restarts every 2s; `force` goes now.
//! 3. `handoff\relaunch.json` lists each live terminal: a shell with the folder it is in and its
//!    screen (with a line saying it was restarted), an agent with whether it resumes its
//!    conversation, and a `claude` typed into a shell with the line that reopens it there.
//! 4. The new midnad starts (`--resume relaunch.json`) and waits for the old one to exit; the old
//!    one then stops like `daemon.stop`, which closes every terminal.
//! 5. The new one restarts each terminal under its id (`rpc::session::restart_on`), emits
//!    `daemon.upgraded` with what it reopened and leaves a needs-you note saying so.
//!
//! A new midnad that dies before step 5 leaves the plan behind: the next midnad to start runs
//! it. One that exits at once is caught before the old one stops, and nothing changes.
use super::{HANDOFF_VERSION, WATCHDOG_MAX, handoff_dir, hold, lists, log, selftest_of, watchdog_budget};
use crate::daemon::{Config, Daemon};
use crate::engine::{Engine, TermSnap};
use crate::term::{self, EngineMsg};
use midna_proto::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The format of `relaunch.json`; a new binary's `--selftest` must list it.
pub const RELAUNCH_VERSION: u32 = 1;
/// How long a new midnad waits for the old one to exit.
const OLD_EXIT_WAIT: Duration = Duration::from_secs(30);
/// Written above a reopened shell's prompt, in the old screen.
const NOTE: &str = "\r\n\x1b[0;2m[midna restarted this terminal to update itself; what ran in it was stopped]\x1b[0m\r\n";

fn plan_path(home: &Path) -> PathBuf {
    handoff_dir(home).join("relaunch.json")
}

/// What `handoff.json` holds: the terminals the old midnad hands over, each through its keeper.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Handoff {
    pub version: u32,
    /// `upgrade` or `restart`.
    pub reason: String,
    pub created_at: Timestamp,
    pub daemon_pid: i32,
    pub from_version: String,
    pub from_binary: String,
    pub to_binary: String,
    pub home: String,
    pub socket: String,
    #[serde(default)]
    pub app_path: Option<String>,
    pub event_seq: u64,
    pub sessions: Vec<HandoffSession>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandoffSession {
    pub id: Id,
    /// The shell's pid: the keeper must still run it.
    pub pid: i32,
    pub keeper_pid: u32,
    /// The keeper's control pipe.
    pub pipe: String,
    pub term: TermSnap,
    pub primary_file: String,
    #[serde(default)]
    pub alt_file: Option<String>,
    #[serde(default)]
    pub in_turn: bool,
    #[serde(default)]
    pub last_cost: f64,
}

/// What `relaunch.json` holds: the terminals the old midnad ran, for the new one to reopen.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    pub version: u32,
    /// `upgrade` or `restart`.
    pub reason: String,
    pub created_at: Timestamp,
    pub daemon_pid: i32,
    pub from_version: String,
    pub to_binary: String,
    pub sessions: Vec<PlanSession>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanSession {
    pub id: Id,
    pub name: String,
    pub reopen: Reopen,
    /// The folder it reopens in (a shell's own, as it was at the restart).
    #[serde(default)]
    pub cwd: Option<String>,
    /// A shell's screen, in its own file in the handoff folder.
    #[serde(default)]
    pub screen_file: Option<String>,
    #[serde(default)]
    pub term: Option<TermSnap>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reopen {
    Shell,
    /// An agent terminal, into its conversation when it has one.
    Agent { resume: bool },
    /// A shell with an agent typed into it: the shell, then `line` typed into it (None: its
    /// arguments can't be typed into that shell as they were, so it isn't).
    Adopted { line: Option<String>, resume: bool },
    /// Not reopened (a monitor: its command may not be safe to run again).
    No,
}

/// A handoff to resume: the keepers to take over, or a plan of terminals to reopen.
pub enum Resume {
    Keepers(Takeover),
    Relaunch(Relaunch),
}

/// `handoff.json` read back, with every keeper it names attached to (or not).
pub struct Takeover {
    handoff: Handoff,
    path: PathBuf,
    /// The terminals attached to, by session, until `adopt_all` takes them.
    attached: Mutex<HashMap<Id, crate::pty::Spawned>>,
    /// Why each of the others couldn't be.
    lost: Vec<(Id, String)>,
}

impl Drop for Takeover {
    /// A midnad that fails to start after attaching (it can't bind the socket, say) lets go of
    /// the keepers rather than hang them up: the old midnad takes them back.
    fn drop(&mut self) {
        for (_, sp) in self.attached.lock().unwrap_or_else(|e| e.into_inner()).drain() {
            sp.master.release_control();
        }
    }
}

/// A relaunch plan read back.
pub struct Relaunch {
    plan: Plan,
    dir: PathBuf,
}

/// A `daemon.upgrade` / `daemon.restart` waiting for the agents to be idle.
struct Pending {
    bin: PathBuf,
    reason: String,
    force: bool,
}

static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

fn pending() -> std::sync::MutexGuard<'static, Option<Pending>> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------- selftest

/// What `--selftest` says only on Windows: the restart plans it can reopen. A debug build run
/// with `MIDNA_DEBUG_NO_HANDOFF` stands in for a build from before the keepers, which can't take
/// terminals over (the tests of reopening use it).
pub(super) fn selftest_formats(v: &mut Value) {
    v["relaunch_versions"] = json!([RELAUNCH_VERSION]);
    if cfg!(debug_assertions) && std::env::var_os("MIDNA_DEBUG_NO_HANDOFF").is_some() {
        v["handoff_versions"] = json!([]);
    }
}

// ---------------------------------------------------------------- the old midnad

/// What each busy agent waits for, as `[{session, name, waiting_for}]` (empty: every agent is idle).
pub fn waiting(d: &Daemon) -> Vec<Value> {
    let agents: Vec<(Id, String)> = {
        let core = d.core();
        core.state.sessions.iter().filter(|s| s.pid.is_some() && s.agent.is_some()).map(|s| (s.id.clone(), s.name.clone())).collect()
    };
    agents
        .into_iter()
        .filter_map(|(sid, name)| {
            let w = crate::restart::blockers(d, &sid);
            (!w.is_empty()).then(|| json!({ "session": sid, "name": name, "waiting_for": w }))
        })
        .collect()
}

fn live_sessions(d: &Daemon) -> u32 {
    d.core().state.sessions.iter().filter(|s| s.pid.is_some()).count() as u32
}

/// `daemon.upgrade` / `daemon.restart`: selftest now (errors go to the caller), then hand the
/// terminals over at once (refused with BUSY on a busy PC, unless `force`). A new midnad that
/// can only reopen them gets them once every agent is idle, or at once with `force`; a second
/// request replaces the first.
pub fn request(d: &Arc<Daemon>, bin: &Path, reason: &str, force: bool) -> Result<Value, RpcError> {
    if !d.cfg.owns_process {
        return Err(RpcError::conflict("this daemon runs inside another process (tests); it can't restart itself"));
    }
    let bin = std::fs::canonicalize(bin).map_err(|e| RpcError::bad_params(format!("{}: {e}", bin.display())))?;
    if d.upgrading.load(Ordering::SeqCst) {
        return Err(RpcError::conflict("midnad is already restarting"));
    }
    let refuse = |e: String| {
        let msg = format!("{reason} aborted, nothing changed: {e}");
        d.emit(kinds::DAEMON_UPGRADE_FAILED, Actor::system(), None, None, json!({ "to": bin, "reason": reason, "error": msg }));
        RpcError::refused(msg)
    };
    let v = selftest_of(&bin).map_err(refuse)?;
    if lists(&v, "handoff_versions", HANDOFF_VERSION) {
        return hand_over_soon(d, &bin, reason, force);
    }
    if !lists(&v, "relaunch_versions", RELAUNCH_VERSION) {
        return Err(refuse(format!(
            "the new midnad can neither take the terminals over (handoff format {HANDOFF_VERSION}) nor reopen them (restart plan format {RELAUNCH_VERSION}): {v}"
        )));
    }
    let waiting = if force { vec![] } else { waiting(d) };
    *pending() = Some(Pending { bin: bin.clone(), reason: reason.into(), force });
    let now = waiting.is_empty();
    if now {
        go(d);
    } else {
        d.emit(kinds::DAEMON_UPGRADE_POSTPONED, Actor::system(), None, None, json!({ "to": bin, "reason": reason, "waiting_for": waiting }));
    }
    let sessions = live_sessions(d);
    Ok(json!({ "ok": true, "binary": bin.to_string_lossy(), "sessions": sessions, "when": if now { "now" } else { "idle" }, "waiting_for": waiting }))
}

/// Hand the terminals over on a thread of its own, shortly after, so the caller's answer is sent
/// first.
fn hand_over_soon(d: &Arc<Daemon>, bin: &Path, reason: &str, force: bool) -> Result<Value, RpcError> {
    super::refuse_if_busy(d, bin, reason, force)?;
    if d.upgrading.swap(true, Ordering::SeqCst) {
        return Err(RpcError::conflict("midnad is already restarting"));
    }
    // A reopening that waited for the agents is replaced.
    *pending() = None;
    let sessions = d.core().rt.len() as u32;
    let (d2, bin2, reason) = (d.clone(), bin.to_path_buf(), reason.to_string());
    std::thread::Builder::new()
        .name("upgrade".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            let e = hand_over(&d2, &bin2, &reason);
            log(&format!("{reason} to {} failed, this midnad keeps running: {e}", bin2.display()));
            d2.emit(kinds::DAEMON_UPGRADE_FAILED, Actor::system(), None, None, json!({ "to": bin2, "reason": reason, "error": e }));
            d2.upgrading.store(false, Ordering::SeqCst);
        })
        .map_err(|e| {
            d.upgrading.store(false, Ordering::SeqCst);
            RpcError::internal(e.to_string())
        })?;
    Ok(json!({ "ok": true, "binary": bin.to_string_lossy(), "sessions": sessions, "when": "now", "waiting_for": [] }))
}

/// What a handover has done so far, for putting it back.
#[derive(Default)]
struct Undo {
    released: Vec<term::RtHandle>,
    socket_closed: bool,
    new: Option<OwnedHandle>,
}

/// Snapshot, write the handoff, give the keepers and the socket up, start the new midnad and
/// watch it. Exits this process once the new one has resumed; returns only on failure, with
/// everything put back (the new one ended, the socket bound again, the keepers connected again,
/// the readers resumed, the handoff removed).
fn hand_over(d: &Arc<Daemon>, bin: &Path, reason: &str) -> String {
    let t0 = Instant::now();
    let _spawns = crate::pty::spawn_lock();
    let dir = handoff_dir(&d.cfg.home);
    let mut undo = Undo::default();
    let res = (|| -> Result<std::convert::Infallible, String> {
        if !term::pause_readers(Duration::from_secs(2)) {
            return Err("the terminals' readers did not pause".into());
        }
        let h = write_handoff(d, &dir, bin, reason)?;
        let path = dir.join("handoff.json");
        let snap_ms = t0.elapsed().as_millis();
        // The next midnad attaches to each keeper; this one keeps its copies of the pipes.
        for rt in d.core().rt.values() {
            rt.fd.release_control();
            undo.released.push(rt.clone());
        }
        crate::server::close_socket(d)?;
        undo.socket_closed = true;
        let args = [bin.to_string_lossy().into_owned(), "--foreground".into(), "--home".into(), h.home.clone(), "--socket".into(), h.socket.clone(), "--resume".into(), path.to_string_lossy().into_owned()];
        let (new, pid) = start_detached(bin, &args).map_err(|e| format!("could not start {}: {e}", bin.display()))?;
        let new = undo.new.insert(new);
        log(&format!("{reason}: {} session(s) snapshotted in {snap_ms}ms, event seq {}; {} (pid {pid}) takes over", h.sessions.len(), h.event_seq, bin.display()));
        watch(&dir, new, pid, watchdog_budget())?;
        log(&format!("{reason}: pid {pid} resumed {} session(s) in {}ms; this midnad exits", h.sessions.len(), t0.elapsed().as_millis()));
        // Its terminals are the new midnad's now: exit without hanging any up or saving state.
        std::process::exit(0)
    })();
    let Err(e) = res;
    // Put everything back.
    if let Some(new) = undo.new.take() {
        use windows_sys::Win32::System::Threading::{TerminateProcess, WaitForSingleObject};
        unsafe {
            TerminateProcess(new.as_raw_handle(), 1);
            WaitForSingleObject(new.as_raw_handle(), 5000);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    if undo.socket_closed
        && let Err(e) = crate::server::reopen_socket()
    {
        log(&format!("binding the socket again failed: {e}"));
    }
    for rt in &undo.released {
        if let Err(e) = rt.fd.reconnect_control() {
            log(&format!("the keeper {} of shell {} took no connection again: {e}", rt.fd.keeper_pid(), rt.pid));
        }
    }
    term::resume_readers();
    // The calls and events that waited go on, and state is saved again.
    super::release(&d.log);
    d.mark_dirty();
    e
}

/// Snapshot every terminal into `dir` and write `handoff.json` there, after saving the state the
/// next midnad loads (and holding further saves).
fn write_handoff(d: &Arc<Daemon>, dir: &Path, bin: &Path, reason: &str) -> Result<Handoff, String> {
    let rts: Vec<(Id, term::RtHandle)> = d.core().rt.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut sessions = vec![];
    for (sid, rt) in rts {
        // A shell that has exited isn't handed over: its exit is reported here (or, if this
        // midnad exits first, the next one finds no shell in its keeper).
        if !rt.fd.child_alive() {
            continue;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        if rt.tx.send(EngineMsg::Snapshot(tx)).is_err() {
            continue; // engine already stopped (session closing)
        }
        let snap = rx.recv_timeout(Duration::from_secs(5)).map_err(|_| format!("session {sid}: engine did not snapshot"))?;
        let (in_turn, last_cost) = {
            let core = d.core();
            let a = core.agents.get(&sid);
            (a.is_some_and(|a| a.in_turn), a.map(|a| a.last_cost).unwrap_or(0.0))
        };
        let primary_file = format!("{sid}.primary.vt");
        std::fs::write(dir.join(&primary_file), &snap.primary_vt).map_err(|e| e.to_string())?;
        let alt_file = match &snap.alt_vt {
            Some(a) => {
                let f = format!("{sid}.alt.vt");
                std::fs::write(dir.join(&f), a).map_err(|e| e.to_string())?;
                Some(f)
            }
            None => None,
        };
        let (keeper_pid, pipe) = (rt.fd.keeper_pid(), rt.fd.pipe_name().to_string());
        sessions.push(HandoffSession { id: sid, pid: rt.pid, keeper_pid, pipe, term: snap, primary_file, alt_file, in_turn, last_cost });
    }
    d.save_now();
    // From here the next midnad's: nothing saved, served or logged here (`upgrade::hold`).
    hold();
    let h = Handoff {
        version: HANDOFF_VERSION,
        reason: reason.into(),
        created_at: time::now_rfc3339(),
        daemon_pid: std::process::id() as i32,
        from_version: midna_proto::VERSION.into(),
        from_binary: std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        to_binary: bin.to_string_lossy().into_owned(),
        home: d.cfg.home.to_string_lossy().into_owned(),
        socket: d.cfg.socket.to_string_lossy().into_owned(),
        app_path: d.cfg.app_path.clone(),
        event_seq: d.log.seq(),
        sessions,
    };
    let tmp = dir.join("handoff.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&h).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join("handoff.json")).map_err(|e| e.to_string())?;
    Ok(h)
}

/// Wait for the new midnad to write `resumed`. Err when it exits first, or hasn't within `secs`
/// (longer while it keeps using CPU: it's resuming, slowly, up to WATCHDOG_MAX).
fn watch(dir: &Path, new: &OwnedHandle, pid: u32, secs: u64) -> Result<(), String> {
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    let t0 = Instant::now();
    let (mut cpu, mut cpu_at) = (crate::procs::cpu_ns(pid as i32).unwrap_or(0), Instant::now());
    let mut working = true;
    loop {
        if dir.join("resumed").exists() {
            return Ok(());
        }
        if unsafe { WaitForSingleObject(new.as_raw_handle(), 0) } == WAIT_OBJECT_0 {
            let mut code = 0u32;
            unsafe { GetExitCodeProcess(new.as_raw_handle(), &mut code) };
            return Err(format!("the new midnad exited ({code}) before resuming"));
        }
        if cpu_at.elapsed() >= Duration::from_secs(2) {
            let now = crate::procs::cpu_ns(pid as i32).unwrap_or(cpu);
            working = now > cpu;
            (cpu, cpu_at) = (now, Instant::now());
        }
        let late = t0.elapsed() > Duration::from_secs(secs);
        if late && !(working && t0.elapsed() < WATCHDOG_MAX) {
            return Err(format!("the new midnad did not resume within {}s (ended)", t0.elapsed().as_secs()));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Start `bin` with the command line `args` as `midnad` detaching does (a console with no
/// window, its own process group, out of our job when the job allows it), passing it no handle
/// of ours but our output (midnad.log): an inherited connection would stay open after we exit,
/// and its client would never reconnect.
fn start_detached(bin: &Path, args: &[String]) -> std::io::Result<(OwnedHandle, u32)> {
    use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_ACCESS_DENIED, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
    use windows_sys::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
        GetCurrentProcess, InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
        UpdateProcThreadAttribute,
    };
    let wide = |s: &std::ffi::OsStr| -> Vec<u16> { std::os::windows::ffi::OsStrExt::encode_wide(s).chain(Some(0)).collect() };
    // Inheritable copies of our output, the only handles the new midnad gets.
    let mut out: Vec<OwnedHandle> = vec![];
    for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        let h = unsafe { GetStdHandle(which) };
        if h.is_null() || h == INVALID_HANDLE_VALUE {
            out.push(unsafe { OwnedHandle::from_raw_handle(std::ptr::null_mut()) });
            continue;
        }
        let mut dup: HANDLE = std::ptr::null_mut();
        if unsafe { DuplicateHandle(GetCurrentProcess(), h, GetCurrentProcess(), &mut dup, 0, 1, DUPLICATE_SAME_ACCESS) } == 0 {
            dup = std::ptr::null_mut();
        }
        out.push(unsafe { OwnedHandle::from_raw_handle(dup) });
    }
    let list: Vec<HANDLE> = out.iter().map(|h| h.as_raw_handle()).filter(|h| !h.is_null()).collect();
    let app = wide(bin.as_os_str());
    let mut line = wide(std::ffi::OsStr::new(&crate::pty::command_line(args)));
    let mut attrs: Vec<u8> = vec![];
    // SAFETY: the attribute list, its handle list and the strings outlive CreateProcessW; the
    // process and thread handles it returns are ours.
    unsafe {
        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        let mut flags = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
        if !list.is_empty() {
            let mut size = 0usize;
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
            attrs = vec![0u8; size];
            si.lpAttributeList = attrs.as_mut_ptr() as _;
            if InitializeProcThreadAttributeList(si.lpAttributeList, 1, 0, &mut size) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            if UpdateProcThreadAttribute(si.lpAttributeList, 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize, list.as_ptr() as _, list.len() * std::mem::size_of::<HANDLE>(), std::ptr::null_mut(), std::ptr::null())
                == 0
            {
                let e = std::io::Error::last_os_error();
                DeleteProcThreadAttributeList(si.lpAttributeList);
                return Err(e);
            }
            si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            si.StartupInfo.hStdOutput = out[0].as_raw_handle();
            si.StartupInfo.hStdError = out[1].as_raw_handle();
            flags |= EXTENDED_STARTUPINFO_PRESENT;
        }
        let dir = bin.parent().map(|p| wide(p.as_os_str()));
        let mut spawn = |flags: u32| {
            let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
            let ok = CreateProcessW(
                app.as_ptr(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                (!list.is_empty()) as i32,
                flags,
                std::ptr::null(),
                dir.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
                &si.StartupInfo,
                &mut pi,
            );
            if ok == 0 { Err(std::io::Error::last_os_error()) } else { Ok(pi) }
        };
        let r = match spawn(flags | CREATE_BREAKAWAY_FROM_JOB) {
            Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => spawn(flags),
            r => r,
        };
        if !si.lpAttributeList.is_null() {
            DeleteProcThreadAttributeList(si.lpAttributeList);
        }
        drop(attrs);
        // Our copies of the output go (a null one isn't closed).
        for h in out {
            if !h.as_raw_handle().is_null() {
                drop(h);
            } else {
                std::mem::forget(h);
            }
        }
        let pi = r?;
        drop(OwnedHandle::from_raw_handle(pi.hThread));
        Ok((OwnedHandle::from_raw_handle(pi.hProcess), pi.dwProcessId))
    }
}

/// With the queued restarts, every 2s: a waiting restart goes once every agent is idle.
pub fn tick(d: &Arc<Daemon>) {
    if d.upgrading.load(Ordering::SeqCst) || pending().is_none() {
        return;
    }
    if waiting(d).is_empty() {
        go(d);
    }
}

/// Start the restart on its own thread, shortly after, so a caller's answer is sent first.
fn go(d: &Arc<Daemon>) {
    if d.upgrading.swap(true, Ordering::SeqCst) {
        return;
    }
    let d2 = d.clone();
    let spawned = std::thread::Builder::new().name("upgrade".into()).spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        let Some(p) = pending().take() else {
            d2.upgrading.store(false, Ordering::SeqCst);
            return;
        };
        let e = relaunch(&d2, &p);
        let _ = std::fs::remove_dir_all(handoff_dir(&d2.cfg.home));
        d2.upgrading.store(false, Ordering::SeqCst);
        match e {
            // Someone got busy meanwhile: wait again.
            Outcome::Busy => *pending() = Some(p),
            Outcome::Failed(e) => {
                log(&format!("{} to {} failed, this midnad keeps running: {e}", p.reason, p.bin.display()));
                d2.emit(kinds::DAEMON_UPGRADE_FAILED, Actor::system(), None, None, json!({ "to": p.bin, "reason": p.reason, "error": e }));
            }
        }
    });
    if spawned.is_err() {
        d.upgrading.store(false, Ordering::SeqCst);
    }
}

enum Outcome {
    Busy,
    Failed(String),
}

/// Write the plan, start the new midnad and stop. Returns only when nothing has changed.
fn relaunch(d: &Arc<Daemon>, p: &Pending) -> Outcome {
    // No terminal starts while the plan is written.
    let _spawns = crate::pty::spawn_lock();
    if !p.force && !waiting(d).is_empty() {
        return Outcome::Busy;
    }
    let dir = handoff_dir(&d.cfg.home);
    let plan = match write_plan(d, &dir, &p.bin, &p.reason) {
        Ok(plan) => plan,
        Err(e) => return Outcome::Failed(e),
    };
    let path = plan_path(&d.cfg.home);
    let mut cmd = std::process::Command::new(&p.bin);
    cmd.arg("--foreground")
        .arg("--home")
        .arg(&d.cfg.home)
        .arg("--socket")
        .arg(&d.cfg.socket)
        .arg("--resume")
        .arg(&path)
        .stdin(std::process::Stdio::null());
    // The same console-less start as `midnad` detaching (main.rs), writing to the same log.
    let spawned = {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB).spawn().or_else(|_| {
            cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
            cmd.spawn()
        })
    };
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => return Outcome::Failed(format!("could not start {}: {e}", p.bin.display())),
    };
    // It waits for us to exit; one that stops before that never would reopen anything.
    std::thread::sleep(Duration::from_millis(300));
    if let Ok(Some(st)) = child.try_wait() {
        return Outcome::Failed(format!("the new midnad exited at once ({st})"));
    }
    log(&format!("{}: {} terminal(s) to reopen; {} (pid {}) takes over once this one stops", p.reason, plan.sessions.len(), p.bin.display(), child.id()));
    crate::server::stop_daemon(d);
    Outcome::Failed("midnad is already stopping".into())
}

/// Each live terminal and how to reopen it, with the shells' screens, into `dir`.
fn write_plan(d: &Arc<Daemon>, dir: &Path, bin: &Path, reason: &str) -> Result<Plan, String> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let live: Vec<Session> = d.core().state.sessions.iter().filter(|s| s.pid.is_some()).cloned().collect();
    let mut sessions = vec![];
    for s in live {
        let Some(rt) = d.rt(&s.id) else { continue };
        let resume = s.agent_info.as_ref().is_some_and(|i| i.conversation_id.is_some() && crate::agent_work::has_transcript(i));
        let reopen = match (&s.adopted, s.kind) {
            (Some(a), _) => Reopen::Adopted { line: typed_line(&s, a, resume), resume },
            (None, SessionKind::Shell) => Reopen::Shell,
            (None, SessionKind::Agent) => Reopen::Agent { resume },
            (None, SessionKind::Monitor) => Reopen::No,
        };
        let shell = matches!(reopen, Reopen::Shell | Reopen::Adopted { .. });
        let (mut cwd, mut screen_file, mut term) = (None, None, None);
        if shell {
            // PowerShell under midna keeps the process's folder where its location is.
            cwd = crate::procs::cwd(rt.pid).filter(|c| Path::new(c).is_dir());
            let (tx, rx) = std::sync::mpsc::channel();
            if rt.tx.send(EngineMsg::Snapshot(tx)).is_ok()
                && let Ok(snap) = rx.recv_timeout(Duration::from_secs(5))
            {
                let snap = reopened_screen(snap);
                let f = format!("{}.primary.vt", s.id);
                std::fs::write(dir.join(&f), &snap.primary_vt).map_err(|e| e.to_string())?;
                screen_file = Some(f);
                term = Some(snap);
            }
        }
        sessions.push(PlanSession { id: s.id.clone(), name: s.name.clone(), reopen, cwd, screen_file, term });
    }
    d.save_now();
    let plan = Plan {
        version: RELAUNCH_VERSION,
        reason: reason.into(),
        created_at: time::now_rfc3339(),
        daemon_pid: std::process::id() as i32,
        from_version: midna_proto::VERSION.into(),
        to_binary: bin.to_string_lossy().into_owned(),
        sessions,
    };
    let tmp = dir.join("relaunch.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&plan).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join("relaunch.json")).map_err(|e| e.to_string())?;
    Ok(plan)
}

/// The screen a reopened shell starts on, made from a copy of its old one (the running terminal
/// is left as it is, in case the restart fails): the main screen, with the old screen up in the
/// scrollback and the note last. The new pseudoconsole starts on an empty screen with its cursor
/// at the top, and paints by its own rows.
fn reopened_screen(mut snap: TermSnap) -> TermSnap {
    snap.alt_active = false;
    snap.alt_vt = None;
    snap.alt_cursor = None;
    let mut e = Engine::restored(&snap, None);
    let fresh = [NOTE.as_bytes(), &b"\n".repeat(snap.rows as usize), b"\x1b[H"].concat();
    e.feed(&fresh, crate::engine::now_ns());
    let mut out = e.snapshot();
    out.alt_active = false;
    out.alt_vt = None;
    out.alt_cursor = None;
    out
}

/// What to type into the reopened shell to start its agent again: as it was typed, without the
/// first prompt, into its conversation when it has one.
/// None when the shell's quoting isn't known, or a word can't be typed into it as it is.
fn typed_line(s: &Session, a: &AdoptedAgent, resume: bool) -> Option<String> {
    let sh = shell_kind(s.command.first()?)?;
    let spec = crate::adopt::spec_for(a.agent, &a.bin, false);
    let base: Vec<String> = std::iter::once(a.agent.as_str().to_string()).chain(midna_proto::agent_cli::resume_args(a.agent, &spec, &a.args)).collect();
    let argv = match &s.agent_info {
        Some(info) if resume => crate::agent_work::resume_command(a.agent, &base, info).unwrap_or(base),
        _ => base,
    };
    argv.iter().map(|w| quote(w, sh)).collect::<Option<Vec<_>>>().map(|v| v.join(" "))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Sh {
    PowerShell,
    /// sh, bash, zsh (Git Bash, MSYS).
    Posix,
    Cmd,
}

fn shell_kind(program: &str) -> Option<Sh> {
    match midna_proto::paths::program_name(program).to_ascii_lowercase().as_str() {
        "powershell" | "pwsh" => Some(Sh::PowerShell),
        "sh" | "bash" | "zsh" => Some(Sh::Posix),
        "cmd" => Some(Sh::Cmd),
        _ => None,
    }
}

/// `w` as one literal word of `sh`'s command line; None when that can't be written (cmd has no
/// escape for `"` or `%` inside quotes).
fn quote(w: &str, sh: Sh) -> Option<String> {
    // Nothing in these means anything to the three shells at the start of or inside a word.
    let plain = |extra: &str| !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:/=+".contains(c) || extra.contains(c));
    Some(match sh {
        Sh::PowerShell if plain("\\") => w.to_string(),
        // Single quotes take everything literally; a quote inside is doubled.
        Sh::PowerShell => format!("'{}'", w.replace('\'', "''")),
        Sh::Posix if plain("") => w.to_string(),
        Sh::Posix => format!("'{}'", w.replace('\'', "'\\''")),
        Sh::Cmd if plain("\\") => w.to_string(),
        Sh::Cmd if w.contains(['"', '%']) => return None,
        Sh::Cmd => format!("\"{w}\""),
    })
}

/// SIGTERM's graceful restart is Unix only.
pub fn restart_now(_d: &Arc<Daemon>, _reason: &str) -> String {
    "midnad can't restart in place on Windows".into()
}

// ---------------------------------------------------------------- the new midnad

fn read_plan(path: &Path) -> Result<Relaunch, String> {
    let raw = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut plan: Plan = serde_json::from_slice(&raw).map_err(|e| format!("{}: {e}", path.display()))?;
    if plan.version != RELAUNCH_VERSION {
        return Err(format!("restart plan format {} (this midnad reopens {RELAUNCH_VERSION})", plan.version));
    }
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    for s in &mut plan.sessions {
        if let (Some(t), Some(f)) = (s.term.as_mut(), &s.screen_file) {
            t.primary_vt = std::fs::read(dir.join(f)).unwrap_or_default();
        }
    }
    Ok(Relaunch { plan, dir })
}

/// `midnad --resume <file>`: `handoff.json` attaches to every keeper it names (the old midnad
/// waits, watching); `relaunch.json` waits for the old midnad to exit, then reopens.
pub fn load(path: &Path, _fallback: bool) -> Result<Resume, String> {
    if path.file_name().is_some_and(|n| n == "handoff.json") {
        return take_over(path).map(Resume::Keepers);
    }
    let r = read_plan(path)?;
    let t0 = Instant::now();
    while r.plan.daemon_pid > 0 && crate::sys::alive(r.plan.daemon_pid) {
        if t0.elapsed() > OLD_EXIT_WAIT {
            return Err(format!("midnad {} did not exit within {}s", r.plan.daemon_pid, OLD_EXIT_WAIT.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(Resume::Relaunch(r))
}

/// Read `handoff.json` and its screens, and attach to each session's keeper.
fn take_over(path: &Path) -> Result<Takeover, String> {
    let raw = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut handoff: Handoff = serde_json::from_slice(&raw).map_err(|e| format!("{}: {e}", path.display()))?;
    if handoff.version != HANDOFF_VERSION {
        return Err(format!("handoff format {} (this midnad takes {HANDOFF_VERSION})", handoff.version));
    }
    // A test stands in for a new build that dies during the handover: the copy of midnad in the
    // folder this names.
    if cfg!(debug_assertions)
        && let Some(folder) = std::env::var_os("MIDNA_TEST_RESUME_EXIT")
        && std::env::current_exe().ok().is_some_and(|p| p.parent().and_then(Path::file_name) == Some(folder.as_os_str()))
    {
        log("MIDNA_TEST_RESUME_EXIT: exiting during the handover");
        std::process::exit(7);
    }
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let home = PathBuf::from(&handoff.home);
    let (mut attached, mut lost) = (HashMap::new(), vec![]);
    // Every screen first: a handover that fails after attaching must let the keepers go.
    for s in &mut handoff.sessions {
        s.term.primary_vt = std::fs::read(dir.join(&s.primary_file)).map_err(|e| format!("{}: {e}", s.primary_file))?;
        if let Some(f) = &s.alt_file {
            s.term.alt_vt = Some(std::fs::read(dir.join(f)).map_err(|e| format!("{f}: {e}"))?);
        }
    }
    for s in &handoff.sessions {
        let owner = crate::pty::Owner { home: &home, sid: &s.id, tied: false };
        match crate::pty::attach_to(&owner, s.pid as u32) {
            Ok(sp) => {
                attached.insert(s.id.clone(), sp);
            }
            // A keeper that has exited took its shell with it (it ends its job): that terminal
            // is lost. One that runs on but won't take this midnad would be left running with
            // nobody to show or end it, so the handover fails: the old midnad, still its client,
            // takes every keeper back.
            Err(e) if crate::sys::alive(s.keeper_pid as i32) => {
                for (_, sp) in attached.drain() {
                    sp.master.release_control();
                }
                return Err(format!("session {}: its keeper (pid {}) runs but couldn't be taken over: {e}", s.id, s.keeper_pid));
            }
            Err(e) => lost.push((s.id.clone(), e.to_string())),
        }
    }
    Ok(Takeover { handoff, path: path.to_path_buf(), attached: Mutex::new(attached), lost })
}

/// A plan a new midnad didn't get to (it died first): the next one to start reopens it.
pub fn leftover(home: &Path) -> Option<Resume> {
    let path = plan_path(home);
    if !path.is_file() {
        return None;
    }
    match read_plan(&path) {
        Ok(r) => {
            log(&format!("reopening the terminals of an unfinished {} ({})", r.plan.reason, path.display()));
            Some(Resume::Relaunch(r))
        }
        Err(e) => {
            log(&format!("ignoring {e}"));
            None
        }
    }
}

/// Apply the handoff's paths to the config.
pub fn apply_config(r: &Resume, cfg: &mut Config) {
    let Resume::Keepers(t) = r else { return };
    cfg.home = PathBuf::from(&t.handoff.home);
    cfg.socket = PathBuf::from(&t.handoff.socket);
    // The dev-only GUI override, as on macOS: only a debug build takes it from the handoff.
    if cfg!(debug_assertions) && cfg.app_path.is_none() {
        cfg.app_path = t.handoff.app_path.clone();
    }
}

/// The sessions whose keeper was attached to -> their shell's pid. A reopening keeps none.
pub fn kept_sessions(r: &Resume) -> HashMap<Id, Option<i32>> {
    let Resume::Keepers(t) = r else { return HashMap::new() };
    let attached = t.attached.lock().unwrap_or_else(|e| e.into_inner());
    t.handoff.sessions.iter().filter(|s| attached.contains_key(&s.id)).map(|s| (s.id.clone(), Some(s.pid))).collect()
}

/// Nothing is inherited on Windows: the old midnad closed the socket for this one to bind.
pub fn inherited_listener(_r: &Resume) -> Option<midna_proto::ipc::UnixListener> {
    None
}

/// Take the handed-over terminals (or reopen the plan's), then say so.
pub fn adopt_all(d: &Arc<Daemon>, r: &Resume) {
    match r {
        Resume::Keepers(t) => adopt_keepers(d, t),
        Resume::Relaunch(r) => reopen_all(d, r),
    }
}

/// Every attached keeper's terminal runs here now, on its handed-over screen. Then `resumed`
/// tells the old midnad to exit.
fn adopt_keepers(d: &Arc<Daemon>, t: &Takeover) {
    let mut attached = std::mem::take(&mut *t.attached.lock().unwrap_or_else(|e| e.into_inner()));
    let mut kept = 0u32;
    let mut lost: Vec<Id> = vec![];
    for (sid, why) in &t.lost {
        log(&format!("session {sid}: its keeper was not taken over: {why}"));
        lost.push(sid.clone());
    }
    for s in &t.handoff.sessions {
        let Some(sp) = attached.remove(&s.id) else { continue };
        let Some(known) = d.core().state.session(&s.id).cloned() else {
            // Not in the state this midnad loaded (closed as the handover began): it ends.
            drop(sp);
            lost.push(s.id.clone());
            continue;
        };
        let rt = term::adopt(d, &s.id, sp.master, sp.pid, &s.term);
        let mut core = d.core();
        core.insert_rt(s.id.clone(), rt);
        if known.agent.is_some() {
            let a = core.agents.entry(s.id.clone()).or_default();
            a.in_turn = s.in_turn;
            a.last_cost = s.last_cost;
            // Not handed over: an agent that reported a conversation is past its trust dialog.
            a.hooked = known.agent_info.is_some_and(|i| i.conversation_id.is_some());
        }
        kept += 1;
    }
    for sid in &lost {
        let mut core = d.core();
        if let Some(sess) = core.state.session_mut(sid) {
            sess.pid = None;
            sess.status = Status { state: StatusState::Exited, reason: Some("daemon restarted (keeper lost)".into()), exit_code: None, since: time::now_rfc3339() };
        }
    }
    d.mark_dirty();
    let dir = t.path.parent().map(Path::to_path_buf).unwrap_or_default();
    let _ = std::fs::write(dir.join("resumed"), std::process::id().to_string());
    let me = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    d.emit(
        kinds::DAEMON_UPGRADED,
        Actor::system(),
        None,
        None,
        json!({
            "from": { "version": t.handoff.from_version, "binary": t.handoff.from_binary, "pid": t.handoff.daemon_pid },
            "to": { "version": midna_proto::VERSION, "binary": me, "pid": std::process::id() },
            "sessions_kept": kept,
            "sessions_lost": lost,
            "reason": t.handoff.reason,
            "fallback": false,
        }),
    );
    log(&format!("resumed {kept} session(s) from {} ({})", t.handoff.from_binary, t.handoff.reason));
    // The old midnad sees `resumed` and exits; then the handoff goes.
    let old = t.handoff.daemon_pid;
    let _ = std::thread::Builder::new().name("handoff-cleanup".into()).spawn(move || {
        let t0 = Instant::now();
        while old > 0 && old != std::process::id() as i32 && crate::sys::alive(old) && t0.elapsed() < OLD_EXIT_WAIT {
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// Reopen the plan's terminals (on a thread: the socket answers meanwhile), then say so.
fn reopen_all(d: &Arc<Daemon>, r: &Relaunch) {
    let (plan, dir) = (r.plan.clone(), r.dir.clone());
    let _ = std::fs::remove_file(dir.join("relaunch.json"));
    let d = d.clone();
    let _ = std::thread::Builder::new().name("relaunch".into()).spawn(move || {
        let t0 = Instant::now();
        let done: Vec<Value> = plan.sessions.iter().filter_map(|s| reopen(&d, &plan, s)).collect();
        let _ = std::fs::remove_dir_all(&dir);
        let reopened = done.iter().filter(|v| v["reopened"] == json!(true)).count();
        log(&format!("{}: {reopened} of {} terminal(s) reopened in {}ms", plan.reason, done.len(), t0.elapsed().as_millis()));
        d.emit(
            kinds::DAEMON_UPGRADED,
            Actor::system(),
            None,
            None,
            json!({
                "reason": plan.reason, "from_version": plan.from_version, "version": midna_proto::VERSION, "from_pid": plan.daemon_pid,
                "relaunched": true, "sessions": done,
            }),
        );
        if !done.is_empty() {
            let title = format!("midna restarted to update itself and reopened {reopened} of {} terminal{}", done.len(), if done.len() == 1 { "" } else { "s" });
            let mut item = d.new_needs_you(NeedsYouKind::Note, title, Actor::system(), None);
            item.detail = done.iter().filter_map(|v| v["said"].as_str()).collect::<Vec<_>>().join("\n");
            d.raise_needs_you(item);
        }
    });
}

/// Reopen one terminal; what happened, as `{session, name, reopened, said}`. None when the
/// terminal is gone (closed, or reopened already by another midnad).
fn reopen(d: &Arc<Daemon>, plan: &Plan, p: &PlanSession) -> Option<Value> {
    let s = d.core().state.session(&p.id).cloned()?;
    if s.pid.is_some() {
        return None;
    }
    let reason = format!("midna {}", plan.reason);
    let (resume, on, line) = match &p.reopen {
        Reopen::No => return Some(json!({ "session": p.id, "name": p.name, "reopened": false, "said": format!("{}: not reopened (a monitor's command may not be safe to run again)", p.name) })),
        // Without its conversation an agent starts again without its first prompt, so an idle
        // agent doesn't redo its task.
        Reopen::Agent { resume } => (*resume, crate::rpc::session::RestartOn { no_prompt: true, ..Default::default() }, None),
        Reopen::Shell => (false, crate::rpc::session::RestartOn { snap: p.term.clone(), cwd: p.cwd.clone(), no_prompt: false }, None),
        Reopen::Adopted { line, resume } => {
            // The shell comes back as a shell; the agent is typed into it once it's up.
            {
                let mut core = d.core();
                if let Some(x) = core.state.session_mut(&p.id) {
                    x.adopted = None;
                    x.kind = SessionKind::Shell;
                    x.agent = None;
                    x.agent_info = None;
                }
                core.agents.remove(&p.id);
            }
            d.mark_dirty();
            (*resume, crate::rpc::session::RestartOn { snap: p.term.clone(), cwd: p.cwd.clone(), no_prompt: false }, line.clone())
        }
    };
    let r = crate::rpc::session::restart_on(d, &p.id, resume, &reason, Actor::system(), on);
    let said = match (&r, &p.reopen) {
        (Err(e), _) => format!("{}: not reopened: {}", p.name, e.message),
        // restart_on starts fresh when the conversation was never saved.
        (Ok(_), Reopen::Agent { resume }) if *resume && s.agent_info.as_ref().is_none_or(crate::agent_work::has_transcript) => {
            format!("{}: the agent reopened in its conversation", p.name)
        }
        (Ok(_), Reopen::Agent { .. }) => format!("{}: the agent started again without its first prompt (it had no conversation to reopen)", p.name),
        (Ok(_), Reopen::Adopted { line: Some(_), resume }) => {
            format!("{}: the shell reopened{}, and {} was typed into it{}", p.name, folder(p), s.agent.map_or("the agent", |a| a.as_str()), if *resume { " to reopen its conversation" } else { "" })
        }
        (Ok(_), Reopen::Adopted { line: None, resume }) => {
            let agent = s.agent.map_or("the agent", |a| a.as_str());
            let conv = s.agent_info.as_ref().and_then(|i| i.conversation_id.clone()).filter(|_| *resume);
            format!(
                "{}: the shell reopened{}; start {agent} again yourself{} (its arguments can't be typed into this shell as they were)",
                p.name,
                folder(p),
                conv.map(|c| format!(" with --resume {c}")).unwrap_or_default()
            )
        }
        (Ok(_), _) => format!("{}: the shell reopened{}; what ran in it was stopped", p.name, folder(p)),
    };
    if r.is_ok()
        && let Some(line) = line
    {
        type_when_ready(d, &p.id, line);
    }
    Some(json!({ "session": p.id, "name": p.name, "reopened": r.is_ok(), "said": said }))
}

fn folder(p: &PlanSession) -> String {
    p.cwd.as_ref().map(|c| format!(" in {c}")).unwrap_or_default()
}

/// Type `line` into a reopened shell once it has drawn its prompt: its screen changed from the
/// one it started on, then stayed the same for a second (at most 20s).
fn type_when_ready(d: &Arc<Daemon>, sid: &str, line: String) {
    let (d, sid) = (d.clone(), sid.to_string());
    let _ = std::thread::Builder::new().name("relaunch-type".into()).spawn(move || {
        let screen = |d: &Daemon| d.rt(&sid).and_then(|rt| rt.read(true)).map(|(s, _, _)| s);
        let first = screen(&d);
        let (t0, mut last, mut since) = (Instant::now(), first.clone(), Instant::now());
        while t0.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(100));
            let now = screen(&d);
            if now.is_none() {
                return;
            }
            if now != last {
                (last, since) = (now, Instant::now());
            } else if last != first && since.elapsed() >= Duration::from_secs(1) {
                break;
            }
        }
        if let Some(rt) = d.rt(&sid) {
            rt.write(format!("{line}\r").as_bytes());
        }
    });
}

/// A normal start: remove a leftover handoff folder (one copied over from a Mac home, or the
/// handoff of a midnad killed while handing over: its keepers are ended as leftovers). A restart
/// plan in it was picked up first (`leftover`).
pub fn set_aside_stale(home: &Path) {
    let _ = std::fs::remove_dir_all(handoff_dir(home));
}

pub fn watchdog_main(_path: &Path) -> ! {
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_is_typed_literally_into_each_shell() {
        let ps = |w| quote(w, Sh::PowerShell);
        assert_eq!(ps("--resume").as_deref(), Some("--resume"));
        assert_eq!(ps(r"C:\work\a.txt").as_deref(), Some(r"C:\work\a.txt"));
        // PowerShell expands $ in double quotes, splats @x and makes a,b an array.
        assert_eq!(ps("$HOME").as_deref(), Some("'$HOME'"));
        assert_eq!(ps("@args").as_deref(), Some("'@args'"));
        assert_eq!(ps("a,b").as_deref(), Some("'a,b'"));
        assert_eq!(ps("it's \"x\"").as_deref(), Some("'it''s \"x\"'"));
        assert_eq!(ps("").as_deref(), Some("''"));
        let sh = |w| quote(w, Sh::Posix);
        assert_eq!(sh("$HOME `x`").as_deref(), Some("'$HOME `x`'"));
        assert_eq!(sh(r"C:\x").as_deref(), Some(r"'C:\x'"));
        assert_eq!(sh("it's").as_deref(), Some(r"'it'\''s'"));
        let cmd = |w| quote(w, Sh::Cmd);
        assert_eq!(cmd("a & b").as_deref(), Some("\"a & b\""));
        assert_eq!(cmd("say \"hi\""), None);
        assert_eq!(cmd("%PATH%"), None);
    }

    #[test]
    fn the_shell_is_known_by_its_program() {
        assert_eq!(shell_kind(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"), Some(Sh::PowerShell));
        assert_eq!(shell_kind("pwsh"), Some(Sh::PowerShell));
        assert_eq!(shell_kind(r"C:\Program Files\Git\bin\bash.exe"), Some(Sh::Posix));
        assert_eq!(shell_kind("cmd.exe"), Some(Sh::Cmd));
        assert_eq!(shell_kind("nu.exe"), None);
    }

    #[test]
    fn the_reopened_screen_is_a_copy_and_the_old_one_is_left_alone() {
        let mut e = Engine::new(40, 6, None);
        e.feed(b"PS C:\\> echo hi\r\nhi\r\nPS C:\\> ", 0);
        let before = e.plain_lines();
        let snap = e.snapshot();
        let out = reopened_screen(snap);
        // The live screen didn't change.
        assert_eq!(e.plain_lines(), before);
        // The copy has the old screen, then the note, then an empty screen with the cursor at the top.
        let mut r = Engine::restored(&out, None);
        assert_eq!(r.cursor(), (0, 0));
        // What the new shell paints at its own top row lands below the note, over nothing.
        r.feed(b"PS C:\\> \x1b[1;1HPS D:\\> ", 0);
        let lines = r.plain_lines();
        let note = lines.iter().position(|l| l.contains("midna restarted this terminal")).expect("note");
        assert!(lines[..note].iter().any(|l| l == "hi"), "{lines:?}");
        let prompt = lines.iter().position(|l| l == "PS D:\\>").expect("prompt");
        assert!(prompt > note, "{lines:?}");
        assert_eq!(lines.iter().filter(|l| l.starts_with("PS")).count(), 3, "{lines:?}");
    }
}
