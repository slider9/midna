//! Windows crash recovery against the real `midnad.exe`: a midnad killed the way `taskkill /F`
//! does it (TerminateProcess) leaves its terminals' keepers running, and the next one re-attaches
//! to each, shows its visible screen and cursor, and types into it (`src/pty_recover_windows.rs`).
//!
//! Each test runs its own daemon on a temp MIDNA_HOME, from a copy of midnad.exe with the keeper,
//! midna-pty.exe, beside it: a keeper serves only midnad.exe from beside it or from the home's
//! `bin`, so the second midnad must be that same file.
#![cfg(windows)]
mod common;
use common::{call, wait_for};
use midna_proto::frame::Frame;
use midna_proto::{AttachStream, Client};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static N: AtomicU32 = AtomicU32::new(0);
const SIZE: (u16, u16) = (100, 30);

struct Daemon {
    home: PathBuf,
    child: Child,
}

impl Daemon {
    fn start() -> Daemon {
        let home = common::tmp().join(format!("midna-c-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&home);
        let bin = home.join("b");
        std::fs::create_dir_all(&bin).unwrap();
        let built = Path::new(env!("CARGO_BIN_EXE_midnad"));
        std::fs::copy(built, bin.join("midnad.exe")).unwrap();
        std::fs::copy(built.with_file_name("midna-pty.exe"), bin.join("midna-pty.exe")).expect("midna-pty.exe beside midnad.exe: cargo build -p midna-pty");
        let child = Daemon::launch(&home);
        let d = Daemon { home, child };
        d.human();
        d
    }

    /// midnad, logging to daemon.log (appended to, for the next one).
    fn launch(home: &Path) -> Child {
        let log = std::fs::OpenOptions::new().create(true).append(true).open(home.join("daemon.log")).unwrap();
        Command::new(home.join("b").join("midnad.exe"))
            .args(["--foreground", "--home"])
            .arg(home)
            .env_remove("MIDNA_SOCKET")
            .env_remove("MIDNA_SESSION")
            .env("MIDNA_APP_PATH", std::env::current_exe().unwrap())
            .env("MIDNA_NO_GH", "1")
            .env("MIDNA_NOTIFY_SYSTEM", "0")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap()
    }

    fn socket(&self) -> PathBuf {
        self.home.join("midnad.sock")
    }

    fn human(&self) -> Client {
        wait_for(15, "daemon connection", || {
            let mut c = Client::connect(self.socket()).ok()?;
            c.set_caller(None);
            c.call_value("daemon.info", json!({})).ok()?;
            Some(c)
        })
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.home.join("daemon.log")).unwrap_or_default()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // A stop hangs up every terminal; then make sure no keeper is left either way.
        if let Ok(mut c) = Client::connect(self.socket()) {
            c.set_caller(None);
            let _ = c.call_value("daemon.stop", json!({}));
        }
        let t0 = Instant::now();
        while self.socket().exists() && t0.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let keepers = |home: &Path| -> Vec<i32> {
            let table = midnad::procs::snapshot();
            let home = home.to_string_lossy().into_owned();
            table
                .into_iter()
                .filter(|(p, (_, n))| n.eq_ignore_ascii_case("midna-pty") && midnad::procs::command_line(*p).is_some_and(|l| l.contains(&home)))
                .map(|(p, _)| p)
                .collect()
        };
        let t0 = Instant::now();
        while !keepers(&self.home).is_empty() && t0.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(100));
        }
        for p in keepers(&self.home) {
            eprintln!("crash_windows: keeper {p} outlived the stop; killing it");
            midnad::sys::signal(p, midnad::sys::Sig::Kill);
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// The visible screen as a stream client sees it on attach: each row's text (trailing spaces
/// trimmed) and the cursor.
#[derive(Debug, PartialEq)]
struct Screen {
    rows: Vec<String>,
    cursor: Option<(u16, u16)>,
}

impl Screen {
    fn text(&self) -> String {
        self.rows.join("\n")
    }
}

fn attach(d: &Daemon, id: &str) -> AttachStream {
    let st = d.human().attach(id, SIZE.0, SIZE.1, 8, 16).unwrap();
    st.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    st
}

fn apply(grid: &mut Vec<String>, cursor: &mut Option<(u16, u16)>, f: &Frame) {
    grid.resize(f.rows as usize, String::new());
    for (y, r) in &f.changed {
        if let Some(row) = grid.get_mut(*y as usize) {
            *row = r.text().trim_end().to_string();
        }
    }
    *cursor = f.cursor;
}

/// The screen once it holds still: frames are pulled until none comes for a while.
fn screen(d: &Daemon, id: &str) -> Screen {
    let mut st = attach(d, id);
    let f = st.next_frame().unwrap();
    assert!(f.full);
    let (mut rows, mut cursor) = (vec![], None);
    apply(&mut rows, &mut cursor, &f);
    st.set_read_timeout(Some(Duration::from_millis(700))).unwrap();
    loop {
        if st.want().is_err() {
            break;
        }
        match st.next_frame() {
            Ok(f) => apply(&mut rows, &mut cursor, &f),
            Err(_) => break,
        }
    }
    Screen { rows, cursor }
}

/// Wait until `id`'s screen shows `what`.
fn shows(d: &Daemon, id: &str, what: &str) -> Screen {
    let t0 = Instant::now();
    loop {
        let s = screen(d, id);
        if s.text().contains(what) {
            return s;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "{id} never showed {what:?}:\n{}", s.text());
    }
}

fn open(c: &mut Client, command: Vec<String>) -> String {
    let s = call(c, "session.open", json!({ "kind": "shell", "cwd": common::tmp(), "command": command, "cols": SIZE.0, "rows": SIZE.1 }));
    s["id"].as_str().unwrap().to_string()
}

/// Wait until state.json, which midnad saves shortly after a change, holds every one of `ids`.
fn saved(d: &Daemon, ids: &[String]) {
    wait_for(10, "state.json with every session", || {
        let state: Value = serde_json::from_slice(&std::fs::read(d.home.join("state.json")).ok()?).ok()?;
        let saved = state["sessions"].as_array()?;
        ids.iter().all(|id| saved.iter().any(|s| s["id"] == id.as_str())).then_some(())
    });
}

fn session(c: &mut Client, id: &str) -> Value {
    call(c, "session.get", json!({ "id": id }))
}

/// Three terminals, one of them a full-screen app on the alternate screen, survive midnad being
/// terminated: the next midnad re-attaches each to its keeper with the same shell, shows the
/// screen and cursor it had, and types into it.
#[test]
fn a_new_midnad_reattaches_every_terminal_a_killed_one_left() {
    let mut d = Daemon::start();
    let mut c = d.human();
    let me = std::env::current_exe().unwrap().to_string_lossy().into_owned();
    let ids = [
        open(&mut c, vec!["cmd.exe".into()]),
        open(&mut c, vec!["powershell.exe".into(), "-NoLogo".into(), "-NoProfile".into()]),
        open(&mut c, vec![me, "--exact".into(), "helper_full_screen_app".into(), "--ignored".into(), "--nocapture".into()]),
    ];
    call(&mut c, "session.input", json!({ "id": ids[0], "text": "echo alpha-before-crash", "enter": true }));
    call(&mut c, "session.input", json!({ "id": ids[1], "text": "Write-Output beta-before-crash", "enter": true }));
    shows(&d, &ids[2], "FULLSCREEN-APP");
    call(&mut c, "session.input", json!({ "id": ids[2], "text": "ab", "enter": false }));
    let before = [shows(&d, &ids[0], "\nalpha-before-crash"), shows(&d, &ids[1], "\nbeta-before-crash"), shows(&d, &ids[2], "keys:ab")];
    let records = wait_for(10, "a keeper record per terminal", || {
        let r = midna_pty::names::records(&d.home);
        (r.len() == ids.len()).then_some(r)
    });
    // Each session's shell and state (idle at a prompt, running the app).
    let kept = |c: &mut Client| -> Vec<(Value, Value)> {
        ids.iter().map(|id| session(c, id)).map(|s| (s["pid"].clone(), s["status"]["state"].clone())).collect()
    };
    let shells = kept(&mut c);
    saved(&d, &ids);

    // What taskkill /F does.
    drop(c);
    d.child.kill().unwrap();
    d.child.wait().unwrap();
    d.child = Daemon::launch(&d.home);
    let mut c = d.human();

    let started = call(&mut c, "events.list", json!({ "since_seq": 0, "limit": 1000, "filter": { "kinds": ["daemon.started"] } }));
    let last = started.as_array().and_then(|e| e.last()).cloned().unwrap_or_default();
    let mut reattached: Vec<String> = last["data"]["sessions_reattached"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    reattached.sort();
    let mut want = ids.to_vec();
    want.sort();
    assert_eq!(reattached, want, "{started}\n{}", d.log());
    assert_eq!(midna_pty::names::records(&d.home), records, "the keepers changed");
    assert_eq!(kept(&mut c), shells, "{}", d.log());
    for (i, id) in ids.iter().enumerate() {
        let after = screen(&d, id);
        assert_eq!(after, before[i], "{id}'s screen after the crash:\n{}\n\nbefore it:\n{}", after.text(), before[i].text());
    }

    call(&mut c, "session.input", json!({ "id": ids[0], "text": "echo alpha-after-crash", "enter": true }));
    call(&mut c, "session.input", json!({ "id": ids[1], "text": "Write-Output beta-after-crash", "enter": true }));
    call(&mut c, "session.input", json!({ "id": ids[2], "text": "cd", "enter": false }));
    shows(&d, &ids[0], "\nalpha-after-crash");
    shows(&d, &ids[1], "\nbeta-after-crash");
    shows(&d, &ids[2], "keys:abcd");
}

/// A shell that exits while no midnad runs is reported with its exit code by the next one, and
/// a keeper whose session was closed is hung up.
#[test]
fn a_shell_that_exited_meanwhile_is_reported_and_a_closed_sessions_keeper_hung_up() {
    let mut d = Daemon::start();
    let mut c = d.human();
    // The first exits 7 a few seconds from now, by when no midnad runs.
    let exits = vec!["cmd.exe".into(), "/d".into(), "/c".into(), "ping -n 8 127.0.0.1 >nul & exit 7".into()];
    let ids = [open(&mut c, exits), open(&mut c, vec!["cmd.exe".into()])];
    let records = wait_for(10, "a keeper record per terminal", || {
        let r = midna_pty::names::records(&d.home);
        (r.len() == 2).then_some(r)
    });
    saved(&d, &ids);
    let record = |id: &str| records.iter().find(|(s, _)| s == id).unwrap().1.clone();
    let (first, second) = (record(&ids[0]), record(&ids[1]));
    drop(c);
    d.child.kill().unwrap();
    d.child.wait().unwrap();

    // While no midnad runs: the first shell exits (its keeper waits to tell a midnad its code),
    // and the second session is closed in the state the next midnad loads.
    wait_for(15, "the first shell to exit", || (!midnad::sys::alive(first.shell_pid as i32)).then_some(()));
    assert!(midnad::sys::alive(first.pid as i32), "the keeper went without telling anyone the exit code");
    let state_path = d.home.join("state.json");
    let listing: Vec<String> = std::fs::read_dir(&d.home).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    let mut state: Value = serde_json::from_slice(&std::fs::read(&state_path).unwrap_or_else(|e| panic!("{}: {e}; {listing:?}", state_path.display()))).unwrap();
    for s in state["sessions"].as_array_mut().unwrap() {
        if s["id"] == ids[1] {
            s["status"] = json!({ "state": "exited", "reason": "closed", "exit_code": null, "since": "2026-01-01T00:00:00Z" });
        }
    }
    std::fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();

    d.child = Daemon::launch(&d.home);
    let mut c = d.human();
    let t0 = Instant::now();
    let s = loop {
        let s = session(&mut c, &ids[0]);
        if s["status"]["exit_code"].is_number() {
            break s;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "the first session never exited: {s}\n{}", d.log());
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(s["status"]["exit_code"], 7, "{s}\n{}", d.log());
    assert_eq!(s["status"]["state"], "failed", "{s}");
    wait_for(15, "the closed session's keeper and shell to go", || {
        (!midnad::sys::alive(second.pid as i32) && !midnad::sys::alive(second.shell_pid as i32)).then_some(())
    });
    assert!(d.log().contains(&format!("of session {}, which is closed", ids[1])), "{}", d.log());
    let t0 = Instant::now();
    while !midna_pty::names::records(&d.home).is_empty() {
        let keepers = std::fs::read_to_string(d.home.join("pty").join("keeper.log")).unwrap_or_default();
        assert!(t0.elapsed() < Duration::from_secs(10), "records left: {:?}\n{keepers}\n{}", midna_pty::names::records(&d.home), d.log());
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Run in a terminal by the first test: a full-screen app on the alternate screen that echoes
/// each key it reads after `keys:`.
#[test]
#[ignore]
fn helper_full_screen_app() {
    use std::io::{Read, Write};
    use windows_sys::Win32::System::Console::{
        ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE, SetConsoleMode,
    };
    unsafe {
        let (i, o) = (GetStdHandle(STD_INPUT_HANDLE), GetStdHandle(STD_OUTPUT_HANDLE));
        let mut m = 0;
        GetConsoleMode(o, &mut m);
        SetConsoleMode(o, m | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        GetConsoleMode(i, &mut m);
        SetConsoleMode(i, m & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT));
    }
    let mut out = std::io::stdout();
    write!(out, "\x1b[?1049h\x1b[2J\x1b[3;5HFULLSCREEN-APP\x1b[6;5Hkeys:").unwrap();
    out.flush().unwrap();
    let mut b = [0u8; 1];
    while std::io::stdin().read(&mut b).is_ok_and(|n| n == 1) {
        out.write_all(&b).unwrap();
        out.flush().unwrap();
    }
}
