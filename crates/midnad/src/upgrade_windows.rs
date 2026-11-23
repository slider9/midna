//! Daemon upgrade / restart on Windows: once every agent is idle, a new midnad reopens the
//! terminals.
//!
//! macOS keeps every terminal across an upgrade by `execv`ing the new binary in the same
//! process, so the shells stay its children and the PTY fds stay open (`upgrade.rs`). Windows
//! has no exec, and a pseudoconsole dies with the process that created it, so a new midnad
//! can't take the old one's terminals. Until each terminal has a keeper process that outlives
//! midnad, `daemon.upgrade` / `daemon.restart` restart midnad and reopen what it ran:
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
use crate::daemon::{Config, Daemon};
use crate::engine::{Engine, TermSnap};
use crate::term::EngineMsg;
use midna_proto::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const HANDOFF_VERSION: u32 = 1;
/// The format of `relaunch.json`; a new binary's `--selftest` must list it.
pub const RELAUNCH_VERSION: u32 = 1;
const SELFTEST_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a new midnad waits for the old one to exit.
const OLD_EXIT_WAIT: Duration = Duration::from_secs(30);
/// Written above a reopened shell's prompt, in the old screen.
const NOTE: &str = "\r\n\x1b[0;2m[midna restarted this terminal to update itself; what ran in it was stopped]\x1b[0m\r\n";

pub fn handoff_dir(home: &Path) -> PathBuf {
    home.join("handoff")
}

fn plan_path(home: &Path) -> PathBuf {
    handoff_dir(home).join("relaunch.json")
}

fn log(msg: &str) {
    eprintln!("midnad[{}]: {msg}", std::process::id());
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
    /// A shell with an agent typed into it: the shell, then `line` typed into it.
    Adopted { line: String, resume: bool },
    /// Not reopened (a monitor: its command may not be safe to run again).
    No,
}

/// A handoff to resume: the relaunch plan.
pub struct Resume {
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

/// `midnad --selftest`: the same engine round trip as on macOS. No in-place handoff yet, so
/// `handoff_versions` is empty; `relaunch_versions` are the plans it can reopen.
pub fn selftest() -> Result<Value, String> {
    let mut e = Engine::new(40, 6, None);
    e.feed(b"selftest \x1b[1;32mok\x1b[0m\r\n\x1b]2;midna\x07\x1b[?1049halt\x1b[2;3H\x1b[3", 0);
    let snap = e.snapshot();
    let r = Engine::restored(&snap, None);
    if r.plain_lines() != e.plain_lines() || r.cursor() != e.cursor() || r.title() != "midna" {
        return Err("terminal snapshot round-trip mismatch".into());
    }
    Ok(json!({ "ok": true, "version": midna_proto::VERSION, "pid": std::process::id(), "handoff_versions": [], "relaunch_versions": [RELAUNCH_VERSION] }))
}

/// Run `<bin> --selftest` (bounded) and check it can reopen our plan format. (macOS's lives in
/// `upgrade.rs`; the Windows handover moves the shared parts out of its Unix-only code.)
pub fn run_selftest(bin: &Path) -> Result<Value, String> {
    let mut child = std::process::Command::new(bin)
        .arg("--selftest")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {} --selftest: {e}", bin.display()))?;
    let t0 = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if t0.elapsed() > SELFTEST_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("--selftest did not finish within {}s", SELFTEST_TIMEOUT.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("--selftest: {e}")),
        }
    };
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !status.success() {
        return Err(format!("--selftest failed ({status}){}", if stderr.is_empty() { String::new() } else { format!(": {stderr}") }));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: Value = stdout.lines().rev().find_map(|l| serde_json::from_str(l).ok()).ok_or_else(|| format!("--selftest printed no JSON: {stdout:?}"))?;
    if v["ok"] != json!(true) {
        return Err(format!("--selftest reported failure: {v}"));
    }
    let supported = v["relaunch_versions"].as_array().is_some_and(|a| a.iter().any(|x| x.as_u64() == Some(RELAUNCH_VERSION as u64)));
    if !supported {
        return Err(format!("the new midnad can't reopen terminals from a restart plan of format {RELAUNCH_VERSION}: {v}"));
    }
    Ok(v)
}

// ---------------------------------------------------------------- the old midnad

/// The binary `daemon.restart` starts: the installed `bin\<current>\midnad.exe` when we run from
/// the installed location, else our own executable (which may have been rebuilt).
pub fn restart_binary(cfg: &Config) -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("midnad.exe"));
    let current = crate::install::current_binary(&cfg.home);
    let installed_root = std::fs::canonicalize(cfg.home.join("bin")).unwrap_or_else(|_| cfg.home.join("bin"));
    let exe_canon = std::fs::canonicalize(&exe).unwrap_or_else(|_| exe.clone());
    if current.is_file() && exe_canon.starts_with(&installed_root) {
        return current;
    }
    exe
}

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

/// `daemon.upgrade` / `daemon.restart`: selftest now (errors go to the caller), then restart
/// once every agent is idle, or at once with `force`. A second request replaces the first.
pub fn request(d: &Arc<Daemon>, bin: &Path, reason: &str, force: bool) -> Result<Value, RpcError> {
    if !d.cfg.owns_process {
        return Err(RpcError::conflict("this daemon runs inside another process (tests); it can't restart itself"));
    }
    let bin = std::fs::canonicalize(bin).map_err(|e| RpcError::bad_params(format!("{}: {e}", bin.display())))?;
    if d.upgrading.load(Ordering::SeqCst) {
        return Err(RpcError::conflict("midnad is already restarting"));
    }
    if let Err(e) = run_selftest(&bin) {
        let msg = format!("{reason} aborted, nothing changed: {e}");
        d.emit(kinds::DAEMON_UPGRADE_FAILED, Actor::system(), None, None, json!({ "to": bin, "reason": reason, "error": msg }));
        return Err(RpcError::refused(msg));
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
            Relaunch::Busy => *pending() = Some(p),
            Relaunch::Failed(e) => {
                log(&format!("{} to {} failed, this midnad keeps running: {e}", p.reason, p.bin.display()));
                d2.emit(kinds::DAEMON_UPGRADE_FAILED, Actor::system(), None, None, json!({ "to": p.bin, "reason": p.reason, "error": e }));
            }
        }
    });
    if spawned.is_err() {
        d.upgrading.store(false, Ordering::SeqCst);
    }
}

enum Relaunch {
    Busy,
    Failed(String),
}

/// Write the plan, start the new midnad and stop. Returns only when nothing has changed.
fn relaunch(d: &Arc<Daemon>, p: &Pending) -> Relaunch {
    // No terminal starts while the plan is written.
    let _spawns = crate::pty::spawn_lock();
    if !p.force && !waiting(d).is_empty() {
        return Relaunch::Busy;
    }
    let dir = handoff_dir(&d.cfg.home);
    let plan = match write_plan(d, &dir, &p.bin, &p.reason) {
        Ok(plan) => plan,
        Err(e) => return Relaunch::Failed(e),
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
        Err(e) => return Relaunch::Failed(format!("could not start {}: {e}", p.bin.display())),
    };
    // It waits for us to exit; one that stops before that never would reopen anything.
    std::thread::sleep(Duration::from_millis(300));
    if let Ok(Some(st)) = child.try_wait() {
        return Relaunch::Failed(format!("the new midnad exited at once ({st})"));
    }
    log(&format!("{}: {} terminal(s) to reopen; {} (pid {}) takes over once this one stops", p.reason, plan.sessions.len(), p.bin.display(), child.id()));
    crate::server::stop_daemon(d);
    Relaunch::Failed("midnad is already stopping".into())
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
            // The new pseudoconsole starts on an empty screen with its cursor at the top, and
            // paints by its own rows: the old screen goes up into the scrollback, the note last.
            let rows = rt.read(true).map(|(_, _, r)| r).unwrap_or(30) as usize;
            let fresh = [NOTE.as_bytes(), &b"\n".repeat(rows), b"\x1b[H"].concat();
            let _ = rt.with(move |e| e.feed(&fresh, crate::engine::now_ns()));
            let (tx, rx) = std::sync::mpsc::channel();
            if rt.tx.send(EngineMsg::Snapshot(tx)).is_ok()
                && let Ok(mut snap) = rx.recv_timeout(Duration::from_secs(5))
            {
                // The new shell starts on the main screen.
                snap.alt_active = false;
                snap.alt_vt = None;
                snap.alt_cursor = None;
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

/// What to type into the reopened shell to start its agent again: as it was typed, without the
/// first prompt, into its conversation when it has one.
fn typed_line(s: &Session, a: &AdoptedAgent, resume: bool) -> String {
    let spec = crate::adopt::spec_for(a.agent, &a.bin, false);
    let base: Vec<String> = std::iter::once(a.agent.as_str().to_string()).chain(midna_proto::agent_cli::resume_args(a.agent, &spec, &a.args)).collect();
    let argv = match &s.agent_info {
        Some(info) if resume => crate::agent_work::resume_command(a.agent, &base, info).unwrap_or(base),
        _ => base,
    };
    argv.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ")
}

/// A word as PowerShell and cmd both read it.
fn quote(w: &str) -> String {
    let plain = !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:/\\=@+,".contains(c));
    if plain { w.to_string() } else { format!("\"{}\"", w.replace('"', "")) }
}

/// SIGTERM's graceful restart is Unix only.
pub fn restart_now(_d: &Arc<Daemon>, _reason: &str) -> String {
    "midnad can't restart in place on Windows".into()
}

// ---------------------------------------------------------------- the new midnad

fn read_plan(path: &Path) -> Result<Resume, String> {
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
    Ok(Resume { plan, dir })
}

/// `midnad --resume relaunch.json`: read the plan and wait for the old midnad to exit.
pub fn load(path: &Path, _fallback: bool) -> Result<Resume, String> {
    let r = read_plan(path)?;
    let t0 = Instant::now();
    while r.plan.daemon_pid > 0 && crate::sys::alive(r.plan.daemon_pid) {
        if t0.elapsed() > OLD_EXIT_WAIT {
            return Err(format!("midnad {} did not exit within {}s", r.plan.daemon_pid, OLD_EXIT_WAIT.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(r)
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
            Some(r)
        }
        Err(e) => {
            log(&format!("ignoring {e}"));
            None
        }
    }
}

pub fn apply_config(_r: &Resume, _cfg: &mut Config) {}

/// Nothing is kept: every terminal is reopened.
pub fn kept_sessions(_r: &Resume) -> HashMap<Id, Option<i32>> {
    HashMap::new()
}

pub fn inherited_listener(_r: &Resume) -> Option<midna_proto::ipc::UnixListener> {
    None
}

/// Reopen the plan's terminals (on a thread: the socket answers meanwhile), then say so.
pub fn adopt_all(d: &Arc<Daemon>, r: &Resume) {
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
        Reopen::Agent { resume } => (*resume, crate::rpc::session::RestartOn::default(), None),
        Reopen::Shell => (false, crate::rpc::session::RestartOn { snap: p.term.clone(), cwd: p.cwd.clone() }, None),
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
            (*resume, crate::rpc::session::RestartOn { snap: p.term.clone(), cwd: p.cwd.clone() }, Some(line.clone()))
        }
    };
    let r = crate::rpc::session::restart_on(d, &p.id, resume, &reason, Actor::system(), on);
    let said = match (&r, &p.reopen) {
        (Err(e), _) => format!("{}: not reopened: {}", p.name, e.message),
        (Ok(_), Reopen::Agent { resume: true }) => format!("{}: the agent reopened in its conversation", p.name),
        (Ok(_), Reopen::Agent { resume: false }) => format!("{}: the agent started again (it had no conversation to reopen)", p.name),
        (Ok(_), Reopen::Adopted { resume, .. }) => {
            format!("{}: the shell reopened{}, and {} was typed into it{}", p.name, folder(p), s.agent.map_or("the agent", |a| a.as_str()), if *resume { " to reopen its conversation" } else { "" })
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

/// A normal start: remove a leftover handoff folder (one copied over from a Mac home, say). A
/// restart plan in it was picked up first (`leftover`).
pub fn set_aside_stale(home: &Path) {
    let _ = std::fs::remove_dir_all(handoff_dir(home));
}

pub fn watchdog_main(_path: &Path) -> ! {
    std::process::exit(0)
}
