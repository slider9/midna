//! Crash recovery on Windows: a midnad that starts after another was killed or crashed (what
//! `taskkill /F` does) re-attaches to the keepers it left, so their terminals carry on.
//!
//! It runs in two steps around loading `state.json` and binding the socket:
//!
//! 1. [`find`], before the state loads: the keepers this home's records in `MIDNA_HOME\pty` name
//!    that are still running, each checked to be `midna-pty.exe` serving this home and session (a
//!    pid left by a keeper that was killed may be another program's by now). Their sessions keep
//!    their status and get the shell's pid instead of being marked exited. Nothing is touched yet:
//!    another midnad may still be running them.
//! 2. [`reattach`], once the socket is this midnad's: each keeper whose session is still open in
//!    the state is attached to. Its output pipe is read and dropped (what piled up while nobody
//!    read is already on the console's screen), then its visible screen is read from the console
//!    ([`midna_pty::proto::Snapshot`]: text, the 16 colours, underline, inverse and the cursor;
//!    scrollback is lost) and drawn into the session's new engine, and its reader starts. Output
//!    that arrives between the drain and the snapshot is applied on top of it, once more; the next
//!    change on the screen corrects it. A keeper with no open session is hung up, ending what its
//!    shell started too, and one that can't be attached to is ended, so the session's name is
//!    free for a restart. Records of keepers that are gone are removed.
use super::{Master, keepers};
use crate::daemon::Daemon;
use midna_proto::{Status, StatusState, time};
use midna_pty::client::Client;
use midna_pty::names::{self, Record};
use std::collections::HashMap;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
use windows_sys::Win32::Storage::FileSystem::ReadFile;
use windows_sys::Win32::System::Pipes::PeekNamedPipe;
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject};

/// How long connecting to a keeper may wait (it serves one client at a time; the killed midnad's
/// connection is gone already).
const CONNECT: Duration = Duration::from_secs(3);

/// How long a keeper that's ended gets to end its shell's job and exit before it's terminated.
const END: Duration = Duration::from_secs(3);

/// After a hang-up, how long the keeper gets: its shell has 5 s to exit before it ends the job.
const HANGUP: Duration = Duration::from_secs(8);

/// The most time spent dropping what piled up in a keeper's output pipe: a program printing
/// flat out never lets it empty.
const DRAIN: Duration = Duration::from_millis(500);

/// A keeper an earlier midnad of this home left running.
pub struct Found {
    pub sid: String,
    pub record: Record,
    /// Held from [`find`] on, so its pid stays this keeper's.
    process: OwnedHandle,
}

/// Step 1: the keepers of `home` still running, by their records. Changes nothing.
pub fn find(home: &Path) -> Vec<Found> {
    let records = names::records(home);
    if records.is_empty() {
        return vec![];
    }
    let table = crate::procs::snapshot();
    let home_s = home.to_string_lossy();
    let mut out = vec![];
    for (sid, record) in records {
        // SAFETY: a handle we own from here, so the pid stays this process's while we look.
        let h = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, record.pid) };
        if h.is_null() {
            continue;
        }
        let process = unsafe { OwnedHandle::from_raw_handle(h) };
        let name = names::EXE.trim_end_matches(".exe");
        let ours = table.get(&(record.pid as i32)).is_some_and(|(_, n)| n.eq_ignore_ascii_case(name))
            && crate::procs::command_line(record.pid as i32).is_some_and(|l| l.contains(&*home_s) && l.contains(&format!("--session {sid}")));
        if ours && unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } != WAIT_OBJECT_0 {
            out.push(Found { sid, record, process });
        }
    }
    out
}

/// What [`find`] found, for `Daemon::new_keeping`: each session's shell pid, so it isn't marked
/// exited.
pub fn kept(found: &[Found]) -> HashMap<String, Option<i32>> {
    found.iter().map(|f| (f.sid.clone(), Some(f.record.shell_pid as i32))).collect()
}

/// What [`reattach`] did.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// Sessions attached to their keeper again.
    pub reattached: Vec<String>,
    /// Keepers with no open session, hung up.
    pub hung_up: Vec<String>,
    /// Sessions whose keeper couldn't be attached to, so it was ended.
    pub lost: Vec<String>,
}

/// Step 2, once the socket is ours: attach each keeper [`find`] found to its open session, hang
/// up the rest, and remove the records of keepers that are gone.
pub fn reattach(d: &Arc<Daemon>, found: Vec<Found>) -> Outcome {
    let home = d.cfg.home.clone();
    let mut out = Outcome::default();
    let live: Vec<String> = found.iter().map(|f| f.sid.clone()).collect();
    for f in found {
        let open = d.core().state.session(&f.sid).is_some_and(|s| !s.status.state.is_terminal());
        if !open {
            eprintln!("midnad: hanging up keeper {} of session {}, which is closed, and its shell {}", f.record.pid, f.sid, f.record.shell_pid);
            if let Some(s) = d.core().state.session_mut(&f.sid) {
                s.pid = None;
            }
            out.hung_up.push(f.sid.clone());
            in_background(&home, f, hang_up);
            continue;
        }
        match attach(d, &home, &f) {
            Ok(()) => {
                eprintln!("midnad: re-attached session {} to its keeper {} (shell {})", f.sid, f.record.pid, f.record.shell_pid);
                out.reattached.push(f.sid);
            }
            Err(e) => {
                eprintln!("midnad: can't re-attach session {} to its keeper {}: {e}; ending it", f.sid, f.record.pid);
                if let Some(s) = d.core().state.session_mut(&f.sid) {
                    s.pid = None;
                    s.status = Status { state: StatusState::Exited, reason: Some("daemon restarted (pty lost)".into()), exit_code: None, since: time::now_rfc3339() };
                }
                out.lost.push(f.sid.clone());
                in_background(&home, f, end);
            }
        }
    }
    // Records of keepers that are gone (killed with their shells, or never cleaned up).
    for (sid, _) in names::records(&home) {
        if !live.contains(&sid) {
            let _ = std::fs::remove_file(names::record_path(&home, &sid));
        }
    }
    d.mark_dirty();
    out
}

/// Run `what` (a hang-up or an end, which can take seconds) off the startup path. A terminal
/// started for the session meanwhile waits for the keeper to be gone, as for any earlier keeper
/// of its session: the keeper's pipe is named after it.
fn in_background(home: &Path, f: Found, what: fn(&Path, &Found)) {
    if let Ok(h) = f.process.try_clone() {
        keepers().lock().unwrap_or_else(|e| e.into_inner()).insert(f.record.pipe.clone(), Arc::new(h));
    }
    let home = home.to_path_buf();
    let _ = std::thread::Builder::new().name("pty-recover".into()).spawn(move || what(&home, &f));
}

/// Attach session `f.sid` to its keeper and start its runtime on the keeper's screen.
fn attach(d: &Arc<Daemon>, home: &Path, f: &Found) -> std::io::Result<()> {
    let client = Client::connect(&names::pipe_name(home, &f.sid), CONNECT)?;
    let hello = client.hello()?;
    let a = client.attach()?;
    // A shell that exited while nobody was attached: its keeper waits to tell a client the code,
    // which the session's reaper reads from the process like any exit. The console is closed, so
    // there's no screen to read.
    let snap = if hello.running {
        drain(&a.output);
        match client.snapshot() {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("midnad: no screen for session {}: {e}", f.sid);
                None
            }
        }
    } else {
        None
    };
    let keeper = Arc::new(f.process.try_clone()?);
    keepers().lock().unwrap_or_else(|e| e.into_inner()).insert(f.record.pipe.clone(), keeper.clone());
    let shell_pid = a.shell_pid;
    let master = Master { client, keeper, input: a.input, output: a.output, process: a.process, shell_pid, ended: AtomicBool::new(false) };
    let (cols, rows, vt) = match snap {
        Some(s) => (s.cols, s.rows, s.to_vt().into_bytes()),
        None => (100, 30, vec![]),
    };
    let rt = crate::term::reattach(d, &f.sid, master, shell_pid as i32, cols, rows, vt);
    let mut core = d.core();
    let agent = core.state.session(&f.sid).filter(|s| s.agent.is_some()).cloned();
    core.insert_rt(f.sid.clone(), rt);
    if let Some(s) = core.state.session_mut(&f.sid) {
        s.pid = Some(shell_pid as i32);
    }
    if let Some(k) = agent {
        // An agent that reported a conversation is past its trust dialog.
        core.agents.entry(f.sid.clone()).or_default().hooked = k.agent_info.is_some_and(|i| i.conversation_id.is_some());
    }
    Ok(())
}

/// Read and drop what's waiting in `output`, until it's empty or [`DRAIN`] has passed.
fn drain(output: &OwnedHandle) {
    let h = output.as_raw_handle();
    let t0 = Instant::now();
    let mut buf = vec![0u8; 64 << 10];
    while t0.elapsed() < DRAIN {
        let mut avail = 0u32;
        // SAFETY: a pipe handle we own; no buffer is passed to the peek.
        if unsafe { PeekNamedPipe(h, std::ptr::null_mut(), 0, std::ptr::null_mut(), &mut avail, std::ptr::null_mut()) } == 0 || avail == 0 {
            return;
        }
        let mut n = 0u32;
        let want = (avail as usize).min(buf.len()) as u32;
        // SAFETY: at most `avail` bytes are there, so the read doesn't block.
        if unsafe { ReadFile(h, buf.as_mut_ptr(), want, &mut n, std::ptr::null_mut()) } == 0 || n == 0 {
            return;
        }
    }
}

/// Hang up a keeper nobody will show: its shell gets its console closed, and once it has exited
/// the keeper ends what it started too. Terminated if it hasn't gone by then.
fn hang_up(home: &Path, f: &Found) {
    let asked = Client::connect(&names::pipe_name(home, &f.sid), CONNECT).and_then(|c| c.hangup_ending_job()).is_ok();
    if !asked || !exits_within(f, HANGUP) {
        terminate(f);
    }
    let _ = std::fs::remove_file(names::record_path(home, &f.sid));
}

/// End a keeper and everything in its shell's job: asked to, else terminated (its job ends with it).
fn end(home: &Path, f: &Found) {
    let asked = Client::connect(&names::pipe_name(home, &f.sid), Duration::from_secs(1)).and_then(|c| c.kill()).is_ok();
    if !asked || !exits_within(f, END) {
        terminate(f);
    }
    let _ = std::fs::remove_file(names::record_path(home, &f.sid));
}

fn exits_within(f: &Found, d: Duration) -> bool {
    unsafe { WaitForSingleObject(f.process.as_raw_handle(), d.as_millis() as u32) == WAIT_OBJECT_0 }
}

fn terminate(f: &Found) {
    unsafe {
        TerminateProcess(f.process.as_raw_handle(), 1);
        WaitForSingleObject(f.process.as_raw_handle(), END.as_millis() as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{environment, keeper_exe, tie_job};
    use super::*;
    use midna_pty::client::{self, Spawn};

    fn home(name: &str) -> std::path::PathBuf {
        let h = std::env::temp_dir().join(format!("midnad-recover-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        std::fs::create_dir_all(&h).unwrap();
        h
    }

    /// A keeper started as a killed midnad would have left it: running a shell, nobody attached.
    /// In the tie job all the same, so a failing test leaves nothing behind.
    fn left_keeper(h: &Path, sid: &str, argv: &[&str]) -> OwnedHandle {
        let shell = Spawn { argv: argv.iter().map(|s| s.to_string()).collect(), cwd: ".".into(), env: environment(&[]), cols: 80, rows: 24 };
        let started = client::start_in(&keeper_exe().unwrap(), h, sid, shell, tie_job()).expect("start");
        drop(started.client);
        for _ in 0..50 {
            if names::records(h).iter().any(|(s, _)| s == sid) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        started.keeper.process
    }

    #[test]
    fn find_names_the_running_keepers_and_leaves_them_alone() {
        let h = home("find");
        let keeper = left_keeper(&h, "s1", &["cmd", "/d"]);
        let found = find(&h);
        assert_eq!(found.iter().map(|f| f.sid.as_str()).collect::<Vec<_>>(), ["s1"]);
        assert_eq!(kept(&found).get("s1"), Some(&Some(found[0].record.shell_pid as i32)));
        assert!(unsafe { WaitForSingleObject(keeper.as_raw_handle(), 0) } != WAIT_OBJECT_0, "find ended the keeper");
        hang_up(&h, &found[0]);
        assert!(unsafe { WaitForSingleObject(keeper.as_raw_handle(), 0) } == WAIT_OBJECT_0, "the hang-up left the keeper running");
        assert!(names::records(&h).is_empty());
        let _ = std::fs::remove_dir_all(&h);
    }

    /// A record whose pid is gone, or another program's now, isn't a keeper.
    #[test]
    fn a_stale_record_names_no_keeper() {
        let h = home("stale");
        let me = std::process::id();
        let record = Record { pid: me, pipe: names::pipe_name(&h, "stale"), protocol: 1, build: "test".into(), shell_pid: me, exe: "midna-pty.exe".into() };
        std::fs::create_dir_all(names::pty_dir(&h)).unwrap();
        std::fs::write(names::record_path(&h, "stale"), serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(find(&h).is_empty());
        let _ = std::fs::remove_dir_all(&h);
    }

    /// What piled up in a keeper's output pipe is dropped before the screen is read.
    #[test]
    fn drain_empties_the_output_pipe_without_blocking() {
        let h = home("drain");
        let _keeper = left_keeper(&h, "d1", &["cmd", "/d", "/k", "echo drained-output"]);
        let c = Client::connect(&names::pipe_name(&h, "d1"), CONNECT).unwrap();
        let a = c.attach().unwrap();
        let shows = |c: &Client| c.snapshot().is_ok_and(|s| (0..s.rows as usize).any(|y| s.row_text(y).contains("drained-output")));
        // The console is attachable once cmd has started; then its output piles up unread.
        let t0 = Instant::now();
        while !shows(&c) {
            assert!(t0.elapsed() < Duration::from_secs(10), "cmd never showed its output");
            std::thread::sleep(Duration::from_millis(100));
        }
        let peek = || {
            let mut avail = 0u32;
            unsafe { PeekNamedPipe(a.output.as_raw_handle(), std::ptr::null_mut(), 0, std::ptr::null_mut(), &mut avail, std::ptr::null_mut()) };
            avail
        };
        std::thread::sleep(Duration::from_millis(300));
        assert!(peek() > 0, "nothing piled up in the output pipe");
        let t0 = Instant::now();
        drain(&a.output);
        assert!(t0.elapsed() < DRAIN + Duration::from_millis(200));
        assert_eq!(peek(), 0);
        assert!(shows(&c), "the screen went with the drained output");
        let _ = c.kill();
        let _ = std::fs::remove_dir_all(&h);
    }
}
