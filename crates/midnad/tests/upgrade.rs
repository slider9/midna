//! In-place upgrade / restart / stop against the real `midnad` binary (an upgrade execs, so it
//! can't run in-process). Ports `spikes/daemon-reexec/test.sh` and `test_flood.sh`.
//!
//! Every test runs its own daemon on a temp MIDNA_HOME; this test binary is the "GUI" (human).
//!
//! Windows has no exec: a new midnad takes every terminal over from its keeper instead, so the
//! daemon's pid changes (`src/upgrade_windows.rs`). Its stopgap for a new build that can't, a
//! restart once every agent is idle that reopens each terminal, has tests of its own.
mod common;
use midna_proto::{Client, ClientError};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static N: AtomicU32 = AtomicU32::new(0);
/// The copies of midnad the daemon runs from. A Windows keeper takes only a `midnad.exe` from
/// its home's `bin` (or beside it) as its client, so there each version is a folder there, with
/// its keeper beside it.
const V1: &str = if cfg!(windows) { "bin/v1/midnad.exe" } else { "v1" };
const V2: &str = if cfg!(windows) { "bin/v2/midnad.exe" } else { "v2" };
/// Windows: a copy that passes its selftest and exits as it takes the terminals over
/// (`MIDNA_TEST_RESUME_EXIT`).
#[cfg(windows)]
const DIES: &str = "bin/dies/midnad.exe";
#[cfg(windows)]
const KEEPER: &str = "midna-pty.exe";

/// This test process's own copy of the built midnad, run once before any test waits on it:
/// macOS scans a new executable the first time it runs, which takes a second on a calm Mac
/// and can outlast a test's wait for the socket on a loaded one. Tests hard-link it, so their
/// v1/v2 are that same scanned file, at paths of their own that `cargo build` can't swap.
fn built() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        // Left by earlier runs whose process is gone.
        for e in std::fs::read_dir(common::tmp()).into_iter().flatten().flatten() {
            let pid = e.file_name().to_str().and_then(|n| n.strip_prefix("midna-u-bin-")).and_then(|p| p.parse::<i32>().ok());
            if pid.is_some_and(|p| !midnad::sys::alive(p)) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
        let dir = common::tmp().join(format!("midna-u-bin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join(if cfg!(windows) { "midnad.exe" } else { "midnad" });
        std::fs::copy(env!("CARGO_BIN_EXE_midnad"), &bin).unwrap();
        // Windows terminals run in the keeper beside midnad (scripts/test.sh builds it).
        #[cfg(windows)]
        std::fs::copy(Path::new(env!("CARGO_BIN_EXE_midnad")).with_file_name(KEEPER), dir.join(KEEPER)).expect("midna-pty.exe beside midnad.exe: cargo build -p midna-pty");
        assert!(Command::new(&bin).arg("--version").stdout(Stdio::null()).status().unwrap().success());
        bin
    })
}

struct Bin {
    home: PathBuf,
    child: Child,
}

impl Bin {
    /// The built midnad as v1/v2 (links to `built()`, so `cargo build` can't swap them mid-test
    /// and the watchdog has a stable fallback path), started from v1, on a calm Mac
    /// (MIDNA_DEBUG_LOAD=0: `cargo test` itself loads the real one).
    fn start() -> Bin {
        Bin::start_with_load(0.)
    }

    /// The daemon reports this load average.
    fn start_with_load(load1: f64) -> Bin {
        Bin::start_with(load1, &[])
    }

    /// The daemon reports this load average, and gets `env` too.
    fn start_with(load1: f64, env: &[(&str, String)]) -> Bin {
        let home = common::tmp().join(format!("midna-u-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        #[cfg(unix)]
        let copies = [V1, V2];
        #[cfg(windows)]
        let copies = [V1, V2, DIES];
        for v in copies {
            let to = home.join(v);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::hard_link(built(), &to).unwrap();
            #[cfg(windows)]
            std::fs::hard_link(built().with_file_name(KEEPER), to.with_file_name(KEEPER)).unwrap();
        }
        let child = Bin::launch(&home, load1, env);
        let b = Bin { home, child };
        wait_for(10, "daemon socket", || Client::connect(b.socket()).ok());
        b
    }

    /// Start v1 in `home`, logging to daemon.log (appended to, for a second midnad).
    fn launch(home: &Path, load1: f64, env: &[(&str, String)]) -> Child {
        Bin::launch_exe(home, &home.join(V1), load1, env)
    }

    /// Start the midnad at `exe` in `home`.
    fn launch_exe(home: &Path, exe: &Path, load1: f64, env: &[(&str, String)]) -> Child {
        let log = std::fs::OpenOptions::new().create(true).append(true).open(home.join("daemon.log")).unwrap();
        Command::new(exe)
            .args(["--foreground", "--home"])
            .arg(&home)
            .env_remove("MIDNA_SOCKET")
            .env_remove("MIDNA_SESSION")
            // Inherited from a midna terminal, it would make SIGTERM stop instead of restart.
            .env_remove("MIDNA_SIGTERM")
            .env("MIDNA_APP_PATH", std::env::current_exe().unwrap())
            .env("MIDNA_NO_GH", "1")
            .env("MIDNA_NOTIFY_SYSTEM", "0")
            .env("MIDNA_UPGRADE_WATCHDOG_SECS", "5")
            .env("MIDNA_DEBUG_LOAD", load1.to_string())
            .env_remove("MIDNA_DEBUG_NO_HANDOFF")
            .env_remove("MIDNA_TEST_RESUME_EXIT")
            .envs(env.iter().map(|(k, v)| (*k, v)))
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap()
    }

    fn socket(&self) -> PathBuf {
        self.home.join("midnad.sock")
    }

    fn path(&self, name: &str) -> String {
        self.home.join(name).to_string_lossy().into_owned()
    }

    /// The pid of the midnad answering now.
    fn pid(&self) -> i64 {
        self.info()["pid"].as_i64().unwrap()
    }

    /// A fresh human connection, retried while the daemon is between images.
    fn human(&self) -> Client {
        wait_for(15, "daemon connection", || {
            let mut c = Client::connect(self.socket()).ok()?;
            c.set_caller(None);
            c.call_value("daemon.info", json!({})).ok()?;
            Some(c)
        })
    }

    fn info(&self) -> Value {
        call(&mut self.human(), "daemon.info", json!({}))
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.home.join("daemon.log")).unwrap_or_default()
    }

    /// Wait for the n-th `daemon.upgraded` event and return its data.
    fn wait_upgraded(&self, n: usize) -> Value {
        wait_for(20, "daemon.upgraded", || {
            let mut c = Client::connect(self.socket()).ok()?;
            c.set_caller(None);
            let v = c.call_value("events.list", json!({ "since_seq": 0, "limit": 1000, "filter": { "kinds": ["daemon.upgraded"] } })).ok()?;
            let evs = v.as_array()?.clone();
            (evs.len() >= n).then(|| evs[n - 1]["data"].clone())
        })
    }
}

impl Drop for Bin {
    fn drop(&mut self) {
        // An explicit stop hangs up every terminal; then make sure nothing is left.
        let pid = Client::connect(self.socket()).ok().and_then(|mut c| {
            c.set_caller(None);
            let pid = c.call_value("daemon.info", json!({})).ok()?["pid"].as_i64();
            let _ = c.call_value("daemon.stop", json!({}));
            pid
        });
        let t0 = Instant::now();
        while self.socket().exists() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
        if let Some(p) = pid {
            midnad::sys::signal(p as i32, midnad::sys::Sig::Kill);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn call(c: &mut Client, m: &str, p: Value) -> Value {
    c.call_value(m, p).unwrap_or_else(|e| panic!("{m} failed: {e}"))
}

fn wait_for<T>(secs: u64, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let t0 = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        if t0.elapsed() > Duration::from_secs(secs) {
            panic!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

// The helpers from here serve the tests that keep terminals through an upgrade.

/// Wait for the counter in `id` to pass `n`, for as long as it keeps counting: fails only when
/// it hasn't moved for 20s. A loaded Mac (or one that backgrounded this test's processes) can
/// take seconds per tick; a counter that was hung up or killed stops for good.
fn counts_past(c: &mut Client, id: &str, lines: u32, n: u64, what: &str) {
    let (mut last, mut moved) = (None, Instant::now());
    loop {
        let now = counts(&read(c, id, lines)).last().copied();
        if now.is_some_and(|v| v > n) {
            return;
        }
        if now != last {
            (last, moved) = (now, Instant::now());
        }
        assert!(moved.elapsed() < Duration::from_secs(20), "{what}: the counter stopped at {last:?}, waiting for {n}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Wait for `done` at the end of a session's output for as long as output keeps arriving:
/// fails only when nothing has come for 30s.
fn wait_flowing(c: &mut Client, id: &str, done: &str) {
    loop {
        let tail = read(c, id, 5);
        if tail.contains(done) {
            return;
        }
        let last = session(c, id)["last_activity_at"].as_str().and_then(midna_proto::time::parse_rfc3339).unwrap_or(0);
        let idle = midna_proto::time::now_unix() - last;
        assert!(idle < 30, "no output for {idle}s and no {done}: {tail:?}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn open(c: &mut Client, kind: &str, argv: &[&str]) -> (String, i64) {
    let s = call(c, "session.open", json!({ "kind": kind, "cwd": common::tmp(), "command": argv, "cols": 100, "rows": 30 }));
    (s["id"].as_str().unwrap().to_string(), s["pid"].as_i64().unwrap())
}

fn read(c: &mut Client, id: &str, lines: u32) -> String {
    call(c, "session.read", json!({ "id": id, "lines": lines }))["text"].as_str().unwrap().to_string()
}

fn screen(c: &mut Client, id: &str) -> String {
    call(c, "session.read", json!({ "id": id, "screen": true }))["text"].as_str().unwrap().to_string()
}

fn alive(pid: i64) -> bool {
    midnad::sys::alive(pid as i32)
}

/// A shell that runs `cmd`: sh, Git's on Windows.
fn sh_c(cmd: &str) -> Vec<String> {
    vec![common::sh(), "-c".into(), cmd.into()]
}

/// An interactive shell and the text its prompt ends with.
fn interactive() -> (Vec<String>, char) {
    if cfg!(windows) { (vec![common::sh(), "-i".into()], '$') } else { (vec!["/bin/zsh".into(), "-f".into()], '%') }
}

fn strs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

/// The path a binary was run from ends with `name` (`v2`, `bin/v2/midnad.exe`).
fn ends_with(path: &str, name: &str) -> bool {
    path.replace('\\', "/").ends_with(&format!("/{name}"))
}

/// A loop printing `C <n>` every 50ms; its HUP trap records any hangup in `hup_file`.
fn counter_cmd(hup_file: &str) -> String {
    let hup_file = common::sh_word(hup_file);
    format!("trap 'echo HUP >> {hup_file}' HUP; i=0; while :; do echo C $i; i=$((i+1)); sleep 0.05; done")
}

/// The `C <n>` numbers in a session's text; panics on any gap.
fn counts(text: &str) -> Vec<u64> {
    let v: Vec<u64> = text.lines().filter_map(|l| l.strip_prefix("C ")?.trim().parse().ok()).collect();
    for w in v.windows(2) {
        assert_eq!(w[1], w[0] + 1, "gap in the counter output: {} -> {}", w[0], w[1]);
    }
    v
}

fn session(c: &mut Client, id: &str) -> Value {
    call(c, "session.get", json!({ "id": id }))
}

#[test]
fn upgrade_keeps_shells_ids_screens_and_counting() {
    let d = Bin::start();
    let mut c = d.human();
    let pid0 = d.pid();
    let hup = d.path("hup");
    let (loop_id, loop_pid) = open(&mut c, "monitor", &strs(&sh_c(&counter_cmd(&hup))));
    let (shell, prompt) = interactive();
    let (sh_id, sh_pid) = open(&mut c, "shell", &strs(&shell));
    wait_for(10, "prompt", || screen(&mut c, &sh_id).contains(prompt).then_some(()));
    call(&mut c, "session.input", json!({ "id": sh_id, "text": "echo still-$((40+2))-here", "enter": false }));
    wait_for(10, "typed text", || screen(&mut c, &sh_id).contains("echo still-").then_some(()));
    wait_for(10, "counting", || (counts(&read(&mut c, &loop_id, 500)).len() > 10).then_some(()));
    std::thread::sleep(Duration::from_millis(300));
    let before = screen(&mut c, &sh_id);
    let ids_before: Vec<Value> = call(&mut c, "session.list", json!({})).as_array().unwrap().iter().map(|s| s["id"].clone()).collect();

    let r = call(&mut c, "daemon.upgrade", json!({ "binary_path": d.path(V2) }));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["sessions"], json!(2));
    let up = d.wait_upgraded(1);
    assert_eq!(up["sessions_kept"], json!(2), "{up}");
    assert_eq!(up["fallback"], json!(false));
    assert!(ends_with(up["to"]["binary"].as_str().unwrap(), V2), "{up}");

    let mut c = d.human();
    if cfg!(unix) {
        assert_eq!(d.pid(), pid0, "same pid after execv");
    } else {
        assert_ne!(d.pid(), pid0, "a new midnad took the terminals over");
    }
    let ids_after: Vec<Value> = call(&mut c, "session.list", json!({})).as_array().unwrap().iter().map(|s| s["id"].clone()).collect();
    assert_eq!(ids_after, ids_before, "same session ids");
    for (id, pid) in [(&loop_id, loop_pid), (&sh_id, sh_pid)] {
        let s = session(&mut c, id);
        assert_eq!(s["pid"].as_i64(), Some(pid), "{s}");
        assert!(!matches!(s["status"]["state"].as_str(), Some("exited" | "failed")), "{s}");
        assert!(alive(pid));
    }
    // Screen contents preserved exactly, typed-but-unsubmitted input included.
    assert_eq!(screen(&mut c, &sh_id), before);

    // The loop kept counting with no gap and no SIGHUP.
    let n0 = *counts(&read(&mut c, &loop_id, 2000)).last().unwrap();
    wait_for(10, "more counting", || (*counts(&read(&mut c, &loop_id, 2000)).last().unwrap() > n0 + 10).then_some(()));
    let all = counts(&read(&mut c, &loop_id, 5000));
    assert_eq!(all[0], 0, "the whole history is still there");
    assert!(!Path::new(&hup).exists(), "the counter got SIGHUP");

    // Still interactive: submit the line typed before the upgrade.
    call(&mut c, "session.input", json!({ "id": sh_id, "text": "", "enter": true }));
    wait_for(10, "command output", || screen(&mut c, &sh_id).lines().any(|l| l.trim() == "still-42-here").then_some(()));

    // And again, back to v1 (a second handoff from a resumed image).
    call(&mut c, "daemon.upgrade", json!({ "binary_path": d.path(V1) }));
    let up = d.wait_upgraded(2);
    assert_eq!(up["sessions_kept"], json!(2));
    let mut c = d.human();
    counts(&read(&mut c, &loop_id, 5000));
    call(&mut c, "session.input", json!({ "id": sh_id, "text": "echo second-$((1+1))", "enter": true }));
    wait_for(10, "second output", || screen(&mut c, &sh_id).lines().any(|l| l.trim() == "second-2").then_some(()));
    assert!(!Path::new(&hup).exists());
    assert!(d.log().contains("resumed 2 session(s)"));
}

#[test]
fn upgrade_during_flood_loses_nothing() {
    let d = Bin::start();
    let mut c = d.human();
    // A pseudoconsole is slower than a pty by far: Windows floods 5 MB.
    let flood_cmd = format!("yes | head -c {}; echo; echo FLOOD-DONE", if cfg!(windows) { 5_000_000 } else { 50_000_000 });
    let (flood, _) = open(&mut c, "monitor", &strs(&sh_c(&flood_cmd)));
    // Numbered lines, paced to span both upgrades. Few enough to all stay in the engine's
    // scrollback (libghostty's max_scrollback is a byte budget: ~800 rows at 100 columns).
    let (nums, _) = open(&mut c, "monitor", &strs(&sh_c("i=1; while [ $i -le 500 ]; do echo N $i; [ $((i % 10)) = 0 ] && sleep 0.05; i=$((i+1)); done; echo NUMS-DONE")));
    std::thread::sleep(Duration::from_millis(300));
    call(&mut c, "daemon.upgrade", json!({ "binary_path": d.path(V2) }));
    d.wait_upgraded(1);
    let mut c = d.human();
    call(&mut c, "daemon.upgrade", json!({ "binary_path": d.path(V1) }));
    d.wait_upgraded(2);
    let mut c = d.human();
    // 50 MB takes minutes on a loaded Mac: wait as long as it keeps coming, so only a stall fails.
    wait_flowing(&mut c, &flood, "FLOOD-DONE");
    wait_for(60, "numbers to finish", || read(&mut c, &nums, 5).contains("NUMS-DONE").then_some(()));
    let text = read(&mut c, &nums, 9000);
    let v: Vec<u64> = text.lines().filter_map(|l| l.strip_prefix("N ")?.trim().parse().ok()).collect();
    assert_eq!(v.len(), 500, "every numbered line arrived exactly once");
    assert!(v.iter().enumerate().all(|(i, n)| *n == i as u64 + 1), "lines out of order or duplicated");
    // The flood's tail is intact `y` lines.
    let tail = read(&mut c, &flood, 30);
    assert!(tail.lines().filter(|l| !l.is_empty() && *l != "FLOOD-DONE").all(|l| l == "y"), "{tail}");
}

#[test]
fn selftest_failure_aborts_the_upgrade() {
    let d = Bin::start();
    let mut c = d.human();
    let pid0 = d.pid();
    let hup = d.path("hup");
    let (loop_id, _) = open(&mut c, "monitor", &strs(&sh_c(&counter_cmd(&hup))));
    #[cfg(unix)]
    let broken = {
        let broken = d.path("broken");
        std::fs::write(&broken, "#!/bin/sh\necho 'v2 cannot run: simulated' >&2\nexit 3\n").unwrap();
        std::fs::set_permissions(&broken, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        broken
    };
    #[cfg(windows)]
    let broken = {
        let broken = d.path("broken.cmd");
        std::fs::write(&broken, "@echo v2 cannot run: simulated 1>&2\r\n@exit /b 3\r\n").unwrap();
        broken
    };
    let err = match c.call_value("daemon.upgrade", json!({ "binary_path": broken })) {
        Err(ClientError::Rpc(e)) => e,
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert_eq!(err.code, midna_proto::error::REFUSED);
    assert!(err.message.contains("selftest") && err.message.contains("simulated"), "{}", err.message);
    // Nothing changed: same connection still works, same pid, no handoff left, still counting.
    assert_eq!(call(&mut c, "daemon.info", json!({}))["pid"].as_i64().unwrap(), pid0);
    assert!(!d.home.join("handoff").exists());
    let n0 = *counts(&read(&mut c, &loop_id, 2000)).last().unwrap_or(&0);
    counts_past(&mut c, &loop_id, 2000, n0 + 5, "counting after abort");
    let failed = call(&mut c, "events.list", json!({ "since_seq": 0, "filter": { "kinds": ["daemon.upgrade_failed"] } }));
    assert_eq!(failed.as_array().unwrap().len(), 1);
    // A binary that isn't there is a bad-params error, also without side effects.
    assert!(c.call_value("daemon.upgrade", json!({ "binary_path": d.path("nope") })).is_err());
    assert!(!Path::new(&hup).exists());
}

#[test]
fn agents_get_a_needs_you_for_upgrade() {
    let d = Bin::start();
    let mut agent = Client::connect(d.socket()).unwrap().as_agent(None);
    let err = match agent.call_value("daemon.upgrade", json!({ "binary_path": d.path(V2) })) {
        Err(ClientError::Rpc(e)) => e,
        other => panic!("expected human-only, got {other:?}"),
    };
    assert_eq!(err.code, midna_proto::error::HUMAN_ONLY);
    let mut c = d.human();
    let needs = call(&mut c, "needs_you.list", json!({}));
    assert!(needs.as_array().unwrap().iter().any(|n| n["detail"].as_str().unwrap_or("").contains("daemon.upgrade")), "{needs}");
}

#[cfg(unix)]
#[test]
fn watchdog_falls_back_when_the_new_binary_dies() {
    let d = Bin::start();
    let mut c = d.human();
    let pid0 = d.info()["pid"].as_i64().unwrap();
    let hup = d.path("hup");
    let (loop_id, loop_pid) = open(&mut c, "monitor", &["/bin/sh", "-c", &counter_cmd(&hup)]);
    let (sh_id, _) = open(&mut c, "shell", &["/bin/zsh", "-f"]);
    wait_for(10, "prompt", || screen(&mut c, &sh_id).contains('%').then_some(()));
    // Passes the selftest (delegates to the real binary), then dies instead of resuming.
    let dies = d.path("dies");
    std::fs::write(&dies, format!("#!/bin/sh\n[ \"$1\" = --selftest ] && exec {} --selftest\necho 'v2 crashed: simulated' >&2\nexit 7\n", d.path("v1"))).unwrap();
    std::fs::set_permissions(&dies, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    call(&mut c, "daemon.upgrade", json!({ "binary_path": dies }));
    let up = d.wait_upgraded(1);
    assert_eq!(up["fallback"], json!(true), "{up}");
    assert_eq!(up["sessions_kept"], json!(2), "{up}");
    let mut c = d.human();
    let pid1 = d.info()["pid"].as_i64().unwrap();
    assert_ne!(pid1, pid0, "the watchdog process became the daemon");
    assert!(alive(loop_pid));
    let n0 = *counts(&read(&mut c, &loop_id, 3000)).last().unwrap();
    counts_past(&mut c, &loop_id, 3000, n0 + 5, "counting after fallback");
    assert!(!Path::new(&hup).exists());
    // The shells aren't our children any more: exits are still noticed (kqueue), without a code.
    call(&mut c, "session.input", json!({ "id": sh_id, "text": "exit 3", "enter": true }));
    let s = wait_for(10, "exit noticed", || {
        let s = session(&mut c, &sh_id);
        matches!(s["status"]["state"].as_str(), Some("exited" | "failed")).then_some(s)
    });
    assert!(s["pid"].is_null(), "{s}");
    assert!(d.log().contains("falling back"), "{}", d.log());
}

/// The old midnad watches the new one take over. One that exits before it has (its keepers
/// attached, its socket bound) is caught: the old midnad binds the socket again, takes its
/// keepers back and carries on, its terminals none the worse, their exits still reported.
#[cfg(windows)]
#[test]
fn a_new_midnad_that_dies_taking_over_falls_back_to_the_old_one() {
    // The copy in `bin\dies` exits as it takes over.
    let d = Bin::start_with(0., &[("MIDNA_TEST_RESUME_EXIT", "dies".into())]);
    let dies = d.path(DIES);
    let mut c = d.human();
    let pid0 = d.pid();
    let hup = d.path("hup");
    let (loop_id, loop_pid) = open(&mut c, "monitor", &strs(&sh_c(&counter_cmd(&hup))));
    let (shell, prompt) = interactive();
    let (sh_id, _) = open(&mut c, "shell", &strs(&shell));
    wait_for(10, "prompt", || screen(&mut c, &sh_id).contains(prompt).then_some(()));
    wait_for(10, "counting", || (counts(&read(&mut c, &loop_id, 500)).len() > 3).then_some(()));
    let before = screen(&mut c, &sh_id);
    call(&mut c, "daemon.upgrade", json!({ "binary_path": dies }));
    let failed = wait_for(30, "daemon.upgrade_failed", || {
        let mut c = Client::connect(d.socket()).ok()?;
        c.set_caller(None);
        let v = c.call_value("events.list", json!({ "since_seq": 0, "limit": 100, "filter": { "kinds": ["daemon.upgrade_failed"] } })).ok()?;
        v.as_array()?.first().map(|e| e["data"].clone())
    });
    assert!(failed["error"].as_str().unwrap().contains("exited (7)"), "{failed}");
    let mut c = d.human();
    assert_eq!(d.pid(), pid0, "the old midnad carries on");
    assert!(!d.home.join("handoff").exists());
    assert!(alive(loop_pid));
    let n0 = *counts(&read(&mut c, &loop_id, 3000)).last().unwrap();
    counts_past(&mut c, &loop_id, 3000, n0 + 5, "counting after the fallback");
    assert!(!Path::new(&hup).exists());
    assert_eq!(screen(&mut c, &sh_id), before);
    // Still its terminals: typing works, and an exit's code is reported.
    call(&mut c, "session.input", json!({ "id": sh_id, "text": "exit 3", "enter": true }));
    let st = wait_for(10, "exit code", || Some(session(&mut c, &sh_id)["status"].clone()).filter(|s| s["state"] == "failed"));
    assert_eq!(st["exit_code"], 3, "{st}");
    assert!(d.log().contains("this midnad keeps running"), "{}", d.log());
    // And it can still hand over to a build that works.
    call(&mut c, "daemon.upgrade", json!({ "binary_path": d.path(V2) }));
    let up = d.wait_upgraded(1);
    assert_eq!(up["sessions_kept"], json!(1), "{up}");
}

/// A keeper takes only a midnad from its home's `bin` (or beside it) as its client. A new
/// midnad one refuses doesn't take over and leave it running unseen: the handover fails, and the
/// old midnad keeps every terminal and keeper.
#[cfg(windows)]
#[test]
fn a_keeper_that_refuses_the_new_midnad_fails_the_handover() {
    let d = Bin::start();
    let mut c = d.human();
    let pid0 = d.pid();
    let hup = d.path("hup");
    let (loop_id, loop_pid) = open(&mut c, "monitor", &strs(&sh_c(&counter_cmd(&hup))));
    wait_for(10, "counting", || (counts(&read(&mut c, &loop_id, 500)).len() > 3).then_some(()));
    let keepers = midna_pty::names::records(&d.home);
    assert_eq!(keepers.len(), 1, "{keepers:?}");
    // Outside `bin`, with no keeper beside it: it passes its selftest, but the keeper won't have it.
    let stray = d.home.join("elsewhere").join("midnad.exe");
    std::fs::create_dir_all(stray.parent().unwrap()).unwrap();
    std::fs::hard_link(built(), &stray).unwrap();
    call(&mut c, "daemon.upgrade", json!({ "binary_path": stray }));
    let failed = wait_for(30, "daemon.upgrade_failed", || {
        let mut c = Client::connect(d.socket()).ok()?;
        c.set_caller(None);
        let v = c.call_value("events.list", json!({ "since_seq": 0, "limit": 100, "filter": { "kinds": ["daemon.upgrade_failed"] } })).ok()?;
        v.as_array()?.first().map(|e| e["data"].clone())
    });
    assert!(failed["error"].as_str().unwrap().contains("exited"), "{failed}");
    assert!(d.log().contains("couldn't be taken over"), "{}", d.log());
    let mut c = d.human();
    assert_eq!(d.pid(), pid0);
    assert!(alive(loop_pid));
    assert_eq!(midna_pty::names::records(&d.home).iter().map(|(_, r)| r.pid).collect::<Vec<_>>(), keepers.iter().map(|(_, r)| r.pid).collect::<Vec<_>>());
    let n0 = *counts(&read(&mut c, &loop_id, 3000)).last().unwrap();
    counts_past(&mut c, &loop_id, 3000, n0 + 5, "counting after the refused handover");
    // Its events kept their order: seqs rise one at a time.
    let evs = call(&mut c, "events.list", json!({ "since_seq": 0, "limit": 1000 }));
    let seqs: Vec<u64> = evs.as_array().unwrap().iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
}

// Windows has no SIGTERM to restart on.
#[cfg(unix)]
#[test]
fn sigterm_restarts_in_place_and_sigint_stops_and_hangs_up() {
    let d = Bin::start();
    let mut c = d.human();
    let pid0 = d.info()["pid"].as_i64().unwrap();
    let hup = d.path("hup");
    let (loop_id, loop_pid) = open(&mut c, "monitor", &["/bin/sh", "-c", &counter_cmd(&hup)]);
    wait_for(10, "counting", || (counts(&read(&mut c, &loop_id, 500)).len() > 3).then_some(()));
    unsafe { libc::kill(pid0 as i32, libc::SIGTERM) };
    let up = d.wait_upgraded(1);
    assert_eq!(up["reason"], json!("sigterm"));
    assert_eq!(up["sessions_kept"], json!(1));
    let mut c = d.human();
    assert_eq!(d.info()["pid"].as_i64().unwrap(), pid0);
    let n0 = *counts(&read(&mut c, &loop_id, 2000)).last().unwrap();
    counts_past(&mut c, &loop_id, 2000, n0 + 5, "counting after restart");
    assert!(!Path::new(&hup).exists());

    // A monitor whose job-controlled child ignores SIGHUP (the reported orphan) and a
    // background job in another process group: an explicit stop must take both down.
    let (_, mon_pid) = open(&mut c, "monitor", &["/bin/sh", "-c", "set -m; (trap '' HUP; exec sleep 300) & echo BG $!; sleep 300 & wait"]);
    let bg: i64 = wait_for(10, "bg pid", || {
        let s = call(&mut c, "session.list", json!({}));
        let id = s.as_array()?.iter().find(|x| x["pid"].as_i64() == Some(mon_pid))?["id"].as_str()?.to_string();
        read(&mut c, &id, 10).lines().find_map(|l| l.strip_prefix("BG ")?.trim().parse().ok())
    });
    assert!(alive(bg));
    unsafe { libc::kill(pid0 as i32, libc::SIGINT) };
    // The daemon is this test's child: reap it rather than probing (a zombie still "exists").
    wait_for(10, "daemon exit", || (!d.socket().exists() && unsafe { libc::waitpid(pid0 as i32, std::ptr::null_mut(), libc::WNOHANG) } == pid0 as i32).then_some(()));
    wait_for(5, "sessions hung up", || (!alive(loop_pid) && !alive(mon_pid) && !alive(bg)).then_some(()));
    assert!(Path::new(&hup).exists(), "an explicit stop sends SIGHUP");
}

#[test]
fn restart_rpc_is_a_graceful_handoff() {
    let d = Bin::start();
    let mut c = d.human();
    let (shell, prompt) = interactive();
    let (sh_id, sh_pid) = open(&mut c, "shell", &strs(&shell));
    wait_for(10, "prompt", || screen(&mut c, &sh_id).contains(prompt).then_some(()));
    // Agents may restart (nothing is lost); the CLI is an agent.
    let mut agent = Client::connect(d.socket()).unwrap().as_agent(None);
    let r = call(&mut agent, "daemon.restart", json!({}));
    assert!(ends_with(r["binary"].as_str().unwrap(), V1), "{r}");
    let up = d.wait_upgraded(1);
    assert_eq!(up["reason"], json!("restart"));
    let mut c = d.human();
    assert_eq!(session(&mut c, &sh_id)["pid"].as_i64(), Some(sh_pid));
    call(&mut c, "session.input", json!({ "id": sh_id, "text": "echo after-$((2+3))", "enter": true }));
    wait_for(10, "output", || screen(&mut c, &sh_id).lines().any(|l| l.trim() == "after-5").then_some(()));
    // Same pid across execv: the shell is still our child, so waitpid still reports its code.
    // (Windows: the keeper handed the new midnad the shell's process, so it can.)
    call(&mut c, "session.input", json!({ "id": sh_id, "text": "exit 4", "enter": true }));
    let st = wait_for(10, "exit code", || Some(session(&mut c, &sh_id)["status"].clone()).filter(|s| s["state"] == "failed"));
    assert_eq!(st["exit_code"], 4, "{st}");
}

#[test]
fn a_busy_mac_postpones_the_upgrade_unless_forced() {
    let cpus = std::thread::available_parallelism().unwrap().get() as f64;
    // Two and a half times as much work as cores: 250% against the default system.busy_load of 200%.
    let d = Bin::start_with_load(cpus * 2.5);
    let mut c = d.human();
    let pid0 = d.pid();
    for (m, p) in [("daemon.upgrade", json!({ "binary_path": d.path(V2) })), ("daemon.restart", json!({}))] {
        let e = match c.call_value(m, p) {
            Err(ClientError::Rpc(e)) => e,
            other => panic!("{m}: expected busy, got {other:?}"),
        };
        assert_eq!(e.code, midna_proto::error::BUSY, "{m}: {e:?}");
        assert!(e.message.contains("busy"), "{e:?}");
        assert_eq!(e.data.as_ref().unwrap()["load_percent"], 250, "{e:?}");
    }
    let ev = call(&mut c, "events.list", json!({ "since_seq": 0, "limit": 100, "filter": { "kinds": ["daemon.upgrade_postponed"] } }));
    assert_eq!(ev.as_array().unwrap().len(), 2, "{ev}");
    assert_eq!(ev[0]["data"]["reason"], "upgrade");

    // Raising the bar makes it calm enough; force goes ahead regardless.
    call(&mut c, "settings.set", json!({ "key": "system.busy_load", "value": 300 }));
    assert_eq!(call(&mut c, "system.load", json!({}))["busy"], false);
    call(&mut c, "settings.set", json!({ "key": "system.busy_load", "value": 200 }));
    let r = call(&mut c, "daemon.upgrade", json!({ "binary_path": d.path(V2), "force": true }));
    assert_eq!(r["ok"], json!(true));
    let up = d.wait_upgraded(1);
    assert_eq!(up["fallback"], json!(false), "{up}");
    if cfg!(unix) {
        assert_eq!(d.pid(), pid0);
    }
}

/// Windows: a stand-in for `claude` that records its argv and draws Claude's input box with a
/// faint placeholder, so midnad reads the box as empty (on the newer ConPTY; Windows 10's own
/// drops faint text, and the agent never looks idle).
#[cfg(windows)]
struct FakeClaude {
    dir: PathBuf,
}

#[cfg(windows)]
impl FakeClaude {
    fn new(tag: &str) -> FakeClaude {
        let dir = common::tmp().join(format!("midna-uf-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rule = "─".repeat(40);
        let body = format!(
            "case \"$1\" in --version) echo '2.1.289 (Claude Code)'; exit 0;; esac\n\
             echo \"$@\" >> '{d}/argv'\n\
             printf '\\033[38;2;136;136;136m%s\\033[39m\\n\\342\\235\\257\\302\\240\\033[2mTry \"fix lint errors\"\\033[22m\\n\\033[38;2;136;136;136m%s\\033[39m\\n' '{rule}' '{rule}'\n\
             trap 'exit 0' HUP TERM\n\
             sleep 300 & wait $!\n",
            d = dir.display()
        );
        common::script(dir.join("claude"), &body);
        FakeClaude { dir }
    }

    fn bin(&self) -> String {
        self.dir.join("claude.cmd").to_string_lossy().into_owned()
    }

    fn launches(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("argv")).unwrap_or_default().lines().map(str::to_string).collect()
    }
}

#[cfg(windows)]
impl Drop for FakeClaude {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(windows)]
fn hook(d: &Bin, sid: &str, event: &str, payload: Value) {
    call(&mut d.human(), "agent.hook", json!({ "agent": "claude", "event": event, "payload": payload, "session": sid }));
}

/// A session's last 200 lines.
#[cfg(windows)]
fn recent(c: &mut Client, id: &str) -> String {
    call(c, "session.read", json!({ "id": id, "lines": 200 }))["text"].as_str().unwrap_or_default().to_string()
}

/// A daemon with the fake agent and no quiet time before a restart, an sh terminal in `sub`
/// that printed a marker, and an agent terminal in a turn of conversation conv-1. Its new
/// builds stand in for ones from before the keepers (`MIDNA_DEBUG_NO_HANDOFF`): they reopen the
/// terminals rather than take them over.
#[cfg(windows)]
fn busy_daemon(fake: &FakeClaude) -> (Bin, String, String, PathBuf) {
    let sh = common::sh(); // and MIDNA_SHELL, PATH for the daemon
    let d = Bin::start_with(0., &[("MIDNA_AGENT_BIN", fake.bin()), ("MIDNA_DEBUG_NO_HANDOFF", "1".into())]);
    let mut c = d.human();
    call(&mut c, "settings.set", json!({ "key": "agents.restart_idle_secs", "value": 0 }));
    let sub = d.home.join("work");
    std::fs::create_dir_all(&sub).unwrap();
    let shell = call(&mut c, "session.open", json!({ "kind": "shell", "cwd": common::tmp(), "command": [sh] }))["id"].as_str().unwrap().to_string();
    call(&mut c, "session.input", json!({ "id": shell, "text": format!("cd {}; echo before-the-restart", common::sh_word(&sub)), "enter": true }));
    wait_for(10, "the shell's marker", || recent(&mut c, &shell).lines().any(|l| l.trim() == "before-the-restart").then_some(()));
    let agent = call(&mut c, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": common::tmp(), "prompt": "do the thing" }))["id"].as_str().unwrap().to_string();
    wait_for(10, "fake claude started", || (!fake.launches().is_empty()).then_some(()));
    hook(&d, &agent, "SessionStart", json!({ "session_id": "conv-1", "source": "startup" }));
    hook(&d, &agent, "UserPromptSubmit", json!({ "session_id": "conv-1", "prompt": "do the thing" }));
    (d, shell, agent, sub)
}

/// A new build that can't take the terminals over from their keepers (one from before them)
/// gets them reopened: `daemon.upgrade` waits until every agent is idle, then the new midnad
/// reopens each terminal under its id: the shell in the folder it had moved
/// to, below its old screen, and the agent in its conversation. It says what it reopened.
#[cfg(windows)]
#[test]
fn upgrade_waits_for_idle_agents_then_reopens_terminals_on_windows() {
    let fake = FakeClaude::new("u");
    let (d, shell, agent, sub) = busy_daemon(&fake);
    let mut c = d.human();
    let pid0 = d.info()["pid"].as_i64().unwrap();
    // A connection held across the restart, as the app's is.
    let mut held = d.human();

    let r = call(&mut c, "daemon.upgrade", json!({ "binary_path": d.path(V2) }));
    assert_eq!((r["ok"].as_bool(), r["when"].as_str()), (Some(true), Some("idle")), "{r}");
    let w = &r["waiting_for"][0];
    assert_eq!(w["session"], json!(agent), "{r}");
    assert!(w["waiting_for"].as_array().unwrap().iter().any(|x| x == "the agent is working"), "{r}");
    let ev = call(&mut c, "events.list", json!({ "since_seq": 0, "limit": 100, "filter": { "kinds": ["daemon.upgrade_postponed"] } }));
    assert_eq!(ev[0]["data"]["waiting_for"][0]["session"], json!(agent), "{ev}");
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(d.info()["pid"].as_i64(), Some(pid0), "must wait while the agent works");

    // The turn ends: the restart goes.
    hook(&d, &agent, "Stop", json!({ "session_id": "conv-1", "hook_event_name": "Stop", "stop_hook_active": false, "background_tasks": [], "session_crons": [] }));
    let up = d.wait_upgraded(1);
    assert_ne!(d.info()["pid"].as_i64(), Some(pid0), "a new midnad: {}", d.log());
    // Gone with the old midnad (the new one inherits none of its handles), so the app reconnects.
    assert!(held.call_value("daemon.info", json!({})).is_err(), "the old connection still answers");
    assert_eq!(up["relaunched"], json!(true), "{up}");
    let said: Vec<&str> = up["sessions"].as_array().unwrap().iter().map(|s| s["said"].as_str().unwrap()).collect();
    assert_eq!(said.len(), 2, "{up}");
    assert!(up["sessions"].as_array().unwrap().iter().all(|s| s["reopened"] == json!(true)), "{up}");

    let mut c = d.human();
    wait_for(15, "the agent's second launch", || (fake.launches().len() >= 2).then_some(()));
    let argv = fake.launches()[1].clone();
    assert!(argv.ends_with("--resume conv-1"), "into its conversation: {argv}");
    assert!(!argv.contains("do the thing"), "the first prompt isn't sent again: {argv}");

    // The shell: the same id, running again, below its old screen, in the folder it was in.
    let s = call(&mut c, "session.get", json!({ "id": shell }));
    assert!(s["pid"].as_i64().is_some(), "{s}");
    let text = recent(&mut c, &shell);
    assert!(text.contains("before-the-restart") && text.contains("midna restarted this terminal to update itself"), "{text}");
    call(&mut c, "session.input", json!({ "id": shell, "text": "pwd -W", "enter": true }));
    let want = sub.to_string_lossy().replace('\\', "/").to_lowercase();
    wait_for(10, "pwd in the reopened shell", || recent(&mut c, &shell).to_lowercase().contains(&want).then_some(()));

    let notes = call(&mut c, "needs_you.list", json!({}));
    let note = notes.as_array().unwrap().iter().find(|n| n["kind"] == "note").unwrap_or_else(|| panic!("{notes}"));
    assert!(note["title"].as_str().unwrap().contains("reopened 2 of 2 terminals"), "{note}");
    assert!(note["detail"].as_str().unwrap().contains("reopened in its conversation"), "{note}");
}

/// `force` doesn't wait for the agents; a binary that fails its selftest changes nothing. An
/// agent with no conversation to reopen starts again without its first prompt.
#[cfg(windows)]
#[test]
fn forced_restart_goes_at_once_and_a_bad_binary_is_refused_on_windows() {
    let fake = FakeClaude::new("f");
    let (d, shell, agent, _) = busy_daemon(&fake);
    let mut c = d.human();
    let pid0 = d.info()["pid"].as_i64().unwrap();
    let fresh = call(&mut c, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": common::tmp(), "prompt": "second task" }))["id"].as_str().unwrap().to_string();
    wait_for(10, "the second agent started", || (fake.launches().len() >= 2).then_some(()));

    let bad = d.home.join("bad.cmd");
    std::fs::write(&bad, "@exit /b 3\r\n").unwrap();
    let e = match c.call_value("daemon.upgrade", json!({ "binary_path": bad })) {
        Err(ClientError::Rpc(e)) => e,
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert_eq!(e.code, midna_proto::error::REFUSED, "{e:?}");
    assert!(e.message.contains("nothing changed"), "{e:?}");
    assert_eq!(d.info()["pid"].as_i64(), Some(pid0));

    let r = call(&mut c, "daemon.restart", json!({ "force": true }));
    assert_eq!(r["when"], "now", "{r}");
    let up = d.wait_upgraded(1);
    assert_eq!(up["reason"], "restart", "{up}");
    assert_ne!(d.info()["pid"].as_i64(), Some(pid0));
    let said = up["sessions"].as_array().unwrap().iter().find(|s| s["session"] == json!(fresh)).map(|s| s["said"].to_string());
    assert!(said.as_deref().is_some_and(|s| s.contains("without its first prompt")), "{up}");
    let mut c = d.human();
    wait_for(15, "both agents launched again", || (fake.launches().len() >= 4).then_some(()));
    let launches = fake.launches();
    assert_eq!(launches.iter().filter(|l| l.contains("second task")).count(), 1, "the first prompt isn't sent again: {launches:?}");
    for id in [&shell, &agent, &fresh] {
        let s = wait_for(10, "reopened", || {
            let s = call(&mut c, "session.get", json!({ "id": id }));
            s["pid"].as_i64().map(|_| s)
        });
        assert_ne!(s["status"]["state"], "exited", "{s}");
    }
}

/// Each Windows terminal runs in a keeper (midna-pty.exe) that outlives midnad on purpose, so a
/// stop has to end them: `daemon.stop` hangs up every terminal and returns once the keepers,
/// their console hosts and their shells are gone.
#[cfg(windows)]
#[test]
fn a_stopped_midnad_leaves_no_keeper_console_host_or_shell_on_windows() {
    let d = Bin::start();
    let mut c = d.human();
    let mut ids = vec![];
    // cmd for the first: Git's sh starts Windows programs out of the terminal's job when the job
    // allows it, which a terminal's does (pty_windows.rs), so only the console's hang-up timing
    // out ends those.
    for shell in ["cmd.exe".to_string(), common::sh(), common::sh()] {
        let s = call(&mut c, "session.open", json!({ "kind": "shell", "cwd": common::tmp(), "command": [shell] }));
        ids.push(s["id"].as_str().unwrap().to_string());
    }
    // A program in the terminal's job with a console of its own, which the hang-up never
    // reaches: only ending the job stops it, even once the shell has exited.
    let ping = "powershell -NoProfile -Command \"Start-Process -WindowStyle Hidden ping -ArgumentList '-n','377','127.0.0.1'\"";
    call(&mut c, "session.input", json!({ "id": ids[0], "text": ping, "enter": true }));
    let pinging = || {
        midnad::procs::snapshot()
            .into_iter()
            .filter(|(p, (_, n))| n.eq_ignore_ascii_case("ping") && midnad::procs::command_line(*p).is_some_and(|l| l.contains("377")))
            .map(|(p, _)| p)
            .collect::<Vec<i32>>()
    };
    let hidden = wait_for(15, "the hidden ping", || Some(pinging()).filter(|p| !p.is_empty()));
    // And one runs a program that outlasts the console's hang-up as long as Windows lets it.
    let me = std::env::current_exe().unwrap();
    let line = format!("\"{}\" --exact helper_outlast_a_hangup --ignored --nocapture", me.display());
    call(&mut c, "session.input", json!({ "id": ids[0], "text": line, "enter": true }));
    wait_for(10, "the helper to be ready", || common::read(&mut c, &ids[0]).contains("outlasting-hangups").then_some(()));
    let records = wait_for(10, "a keeper record per terminal", || {
        let r = midna_pty::names::records(&d.home);
        (r.len() == ids.len()).then_some(r)
    });
    for (sid, _) in &records {
        assert!(ids.contains(sid), "{sid} isn't one of {ids:?}");
    }
    std::thread::sleep(Duration::from_millis(500));
    // What runs now: the keepers, the console hosts they started, the shells and their children.
    let table = midnad::procs::snapshot();
    let keepers: Vec<i32> = records.iter().map(|(_, r)| r.pid as i32).collect();
    let mut ours: Vec<i32> = keepers.clone();
    let mut grew = true;
    while grew {
        grew = false;
        for (&pid, (ppid, _)) in &table {
            // Windows keeps a parent's pid after it exits: a process started before its "parent"
            // is a stranger whose parent had that pid once.
            let after = |p: i32| midnad::procs::start_time(pid).zip(midnad::procs::start_time(p)).is_some_and(|(c, p)| c >= p);
            if ours.contains(ppid) && !ours.contains(&pid) && after(*ppid) {
                ours.push(pid);
                grew = true;
            }
        }
    }
    let names: Vec<String> = ours.iter().filter_map(|p| table.get(p).map(|(_, n)| n.to_lowercase())).collect();
    assert!(names.iter().any(|n| n == "midna-pty"), "no keeper among {names:?}");
    let helper = me.file_stem().unwrap().to_string_lossy().to_lowercase();
    assert!(names.contains(&helper), "no {helper} among {names:?}");
    assert!(names.iter().any(|n| n == "openconsole" || n == "conhost"), "no console host among {names:?}");

    ours.extend(&hidden);
    let t0 = Instant::now();
    let _ = c.call_value("daemon.stop", json!({}));
    wait_for(15, "midnad to exit", || (!d.socket().exists()).then_some(()));
    // A pid still running under the same name is one of them (a reused pid has another).
    let now = midnad::procs::snapshot();
    let left: Vec<String> = ours.iter().filter(|p| now.get(p).map(|(_, n)| n) == table.get(p).map(|(_, n)| n)).map(|p| format!("{p} {}", table.get(p).map_or("?", |(_, n)| n.as_str()))).collect();
    assert!(left.is_empty(), "still running {:.1}s after daemon.stop: {left:?}", t0.elapsed().as_secs_f64());
    assert!(midna_pty::names::records(&d.home).is_empty(), "keeper records left: {:?}", midna_pty::names::records(&d.home));
}

/// A midnad that's killed leaves its keepers running. A next one the keepers won't serve (here
/// a copy of midnad.exe in a folder of its own: a keeper serves only `midnad.exe`, from the home's
/// `bin` or beside it) can't
/// re-attach to them, so it ends them, with what they started, and the sessions they held can be
/// restarted. Re-attaching is `tests/crash_windows.rs`.
#[cfg(windows)]
#[test]
fn a_killed_midnads_keepers_the_next_cant_attach_to_are_ended_and_its_terminals_restart_on_windows() {
    let mut d = Bin::start();
    let mut c = d.human();
    let id = call(&mut c, "session.open", json!({ "kind": "shell", "cwd": common::tmp(), "command": ["cmd.exe"] }))["id"].as_str().unwrap().to_string();
    let ping = "powershell -NoProfile -Command \"Start-Process -WindowStyle Hidden ping -ArgumentList '-n','378','127.0.0.1'\"";
    call(&mut c, "session.input", json!({ "id": id, "text": ping, "enter": true }));
    let hidden = wait_for(15, "the hidden ping", || {
        let p: Vec<i32> = midnad::procs::snapshot()
            .into_iter()
            .filter(|(p, (_, n))| n.eq_ignore_ascii_case("ping") && midnad::procs::command_line(*p).is_some_and(|l| l.contains("378")))
            .map(|(p, _)| p)
            .collect();
        (!p.is_empty()).then_some(p)
    });
    let records = midna_pty::names::records(&d.home);
    assert_eq!(records.len(), 1, "{records:?}");
    // The next midnad hangs up a keeper whose session isn't in the state it loads.
    wait_for(10, "state.json with the session", || {
        let state: Value = serde_json::from_slice(&std::fs::read(d.home.join("state.json")).ok()?).ok()?;
        state["sessions"].as_array()?.iter().any(|s| s["id"] == id.as_str()).then_some(())
    });
    let (keeper, shell) = (records[0].1.pid as i32, records[0].1.shell_pid as i32);
    let table = midnad::procs::snapshot();
    let theirs: Vec<i32> = [keeper, shell].into_iter().chain(hidden).collect();
    let running = |pids: &[i32]| -> Vec<String> {
        let now = midnad::procs::snapshot();
        pids.iter().filter(|p| now.get(p).map(|(_, n)| n) == table.get(p).map(|(_, n)| n)).map(|p| format!("{p} {}", table.get(p).map_or("?", |(_, n)| n.as_str()))).collect()
    };

    drop(c);
    d.child.kill().unwrap();
    d.child.wait().unwrap();
    assert_eq!(running(&theirs).len(), theirs.len(), "the keeper is meant to outlive midnad");

    let stranger = d.home.join("stranger");
    std::fs::create_dir_all(&stranger).unwrap();
    for f in std::fs::read_dir(d.home.join("bin/v1")).unwrap().flatten() {
        std::fs::copy(f.path(), stranger.join(f.file_name())).unwrap();
    }
    d.child = Bin::launch_exe(&d.home, &stranger.join("midnad.exe"), 0., &[]);
    let mut c = d.human();
    wait_for(10, "the keeper, its shell and the ping to end", || running(&theirs).is_empty().then_some(()));
    wait_for(5, "its record to go", || midna_pty::names::records(&d.home).is_empty().then_some(()));
    assert!(d.log().contains(&format!("can't re-attach session {id}")), "{}", d.log());
    let s = call(&mut c, "session.restart", json!({ "id": id }));
    assert_eq!(s["status"]["reason"], "restarted", "{s}");
    call(&mut c, "session.input", json!({ "id": id, "text": "echo back-again", "enter": true }));
    wait_for(10, "the restarted terminal", || common::read(&mut c, &id).contains("back-again").then_some(()));
}

/// Run in a terminal by `a_stopped_midnad_leaves_no_keeper_console_host_or_shell_on_windows`: stalls the console's hang-up (CTRL_CLOSE_EVENT) for as
/// long as Windows waits for it (about 5 s), so only ending the terminal's job stops it at once.
#[cfg(windows)]
#[test]
#[ignore]
fn helper_outlast_a_hangup() {
    unsafe extern "system" fn stall(_: u32) -> windows_sys::core::BOOL {
        std::thread::sleep(Duration::from_secs(60));
        1
    }
    unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(stall), 1) };
    println!("outlasting-hangups");
    std::thread::sleep(Duration::from_secs(300));
}
