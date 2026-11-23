//! Windows: a `claude` typed into a PowerShell or cmd terminal runs under midna through
//! midnad's `claude.exe` shim, a copy of midna.exe (`adopt.rs` covers the sh shims on Unix).
//! The "real" claude is a copy of this test binary, which plays a fake agent when started under
//! that name: it records its argv, answers `--help` and `--version`, and exits 3 once a `quit`
//! file appears (`mcp …` exits at once). An .exe, not a .cmd: cmd would cut midna's multi-line
//! system hint short (`a_typed_claude_from_npm_gets_every_argument` checks a `.cmd`). No libtest (`harness = false`), since it would refuse Claude's arguments.

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() {
    if win::fake_agent() {
        return;
    }
    win::run_all();
}

#[cfg(windows)]
mod win {
    use serde_json::{Value, json};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    static N: AtomicU32 = AtomicU32::new(0);

    /// The fake agent, when this binary was started as `claude.exe` or `codex.exe`.
    pub fn fake_agent() -> bool {
        let exe = std::env::current_exe().unwrap();
        let agent = exe.file_stem().unwrap().to_string_lossy().to_ascii_lowercase();
        if agent != "claude" && agent != "codex" {
            return false;
        }
        let dir = exe.parent().unwrap();
        let args: Vec<String> = std::env::args().skip(1).collect();
        match args.first().map(String::as_str) {
            Some("--help") => {
                print!("{}", if agent == "claude" { midna_proto::agent_cli::BUILTIN_CLAUDE_HELP } else { midna_proto::agent_cli::BUILTIN_CODEX_HELP });
                std::process::exit(0)
            }
            Some("--version") => {
                println!("FAKE {agent} 1.0");
                std::process::exit(0)
            }
            _ => {}
        }
        let line = args.join(" ").replace('\n', "\\n");
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("{agent}-argv"))).unwrap();
        std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes()).unwrap();
        if matches!(args.first().map(String::as_str), Some("mcp" | "exec")) {
            std::process::exit(0)
        }
        println!("FAKE-UP");
        while !dir.join("quit").exists() {
            std::thread::sleep(Duration::from_millis(100));
        }
        std::process::exit(3)
    }

    pub fn run_all() {
        let tests: [(&str, fn()); 4] = [
            ("the_shims_are_copies_of_midna", the_shims_are_copies_of_midna),
            ("typed_claude_in_powershell_runs_under_midna_and_restarts_in_the_same_shell", typed_claude_in_powershell_runs_under_midna_and_restarts_in_the_same_shell),
            ("anything_but_an_interactive_session_runs_as_typed_in_cmd", anything_but_an_interactive_session_runs_as_typed_in_cmd),
            ("a_typed_claude_from_npm_gets_every_argument", a_typed_claude_from_npm_gets_every_argument),
        ];
        let filter: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
        let mut failed = vec![];
        for (name, f) in tests.iter().filter(|(n, _)| filter.is_empty() || filter.iter().any(|x| n.contains(x.as_str()))) {
            let ok = std::panic::catch_unwind(f).is_ok();
            println!("test {name} ... {}", if ok { "ok" } else { "FAILED" });
            if !ok {
                failed.push(*name);
            }
        }
        println!("\ntest result: {}. {} failed", if failed.is_empty() { "ok" } else { "FAILED" }, failed.len());
        if !failed.is_empty() {
            std::process::exit(101);
        }
    }

    struct D {
        handle: Option<midnad::Handle>,
        home: PathBuf,
        fake: PathBuf,
    }

    impl D {
        fn start() -> D {
            let n = format!("{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
            // Short: the socket path is at most 108 bytes.
            let home = std::env::temp_dir().join(format!("mw-{n}"));
            let fake = std::env::temp_dir().join(format!("mw-{n}-bin"));
            for d in [&home, &fake] {
                let _ = std::fs::remove_dir_all(d);
            }
            std::fs::create_dir_all(&fake).unwrap();
            let me = std::env::current_exe().unwrap();
            for agent in ["claude", "codex"] {
                std::fs::copy(&me, fake.join(format!("{agent}.exe"))).unwrap();
            }
            let mut cfg = midnad::Config::for_home(home.clone());
            cfg.app_path = Some(me.to_string_lossy().into_owned());
            cfg.cli_path = env!("CARGO_BIN_EXE_midna").into();
            D { handle: Some(midnad::start(cfg).unwrap()), home, fake }
        }

        fn human(&self) -> midna_proto::Client {
            let mut c = midna_proto::Client::connect(self.home.join("midnad.sock")).unwrap();
            c.set_caller(None);
            c
        }

        fn launches(&self) -> Vec<String> {
            std::fs::read_to_string(self.fake.join("claude-argv")).unwrap_or_default().lines().map(str::to_string).collect()
        }

        /// A shell terminal (`command` with the script last) running `line` with the fake
        /// claude on PATH behind midna's shims.
        fn open(&self, h: &mut midna_proto::Client, command: Vec<String>) -> String {
            let s = call(h, "session.open", json!({ "kind": "shell", "command": command, "cwd": std::env::temp_dir() }));
            s["id"].as_str().unwrap().to_string()
        }

        fn powershell(&self, h: &mut midna_proto::Client, line: &str) -> String {
            let script = format!("$env:Path = \"$env:MIDNA_SHIMS;{};$env:Path\"; {line}; echo \"SHELL-BACK $LASTEXITCODE\"; Start-Sleep 30", self.fake.display());
            self.open(h, ["powershell.exe", "-NoLogo", "-NoProfile", "-Command", &script].map(String::from).to_vec())
        }

        fn cmd(&self, h: &mut midna_proto::Client, line: &str) -> String {
            // No quotes: the argument reaches cmd with them escaped as \", which cmd doesn't read.
            let script = format!("set PATH=%MIDNA_SHIMS%;{};%PATH%& {line} & echo SHELL-BACK & ping -n 30 127.0.0.1 >nul", self.fake.display());
            self.open(h, ["cmd.exe", "/d", "/c", &script].map(String::from).to_vec())
        }
    }

    impl Drop for D {
        fn drop(&mut self) {
            if let Some(h) = self.handle.take() {
                h.shutdown();
            }
            let _ = std::fs::remove_dir_all(&self.home);
            let _ = std::fs::remove_dir_all(&self.fake);
        }
    }

    fn call(c: &mut midna_proto::Client, m: &str, p: Value) -> Value {
        c.call_value(m, p).unwrap_or_else(|e| panic!("{m} failed: {e}"))
    }

    fn wait_for<T>(secs: u64, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
        let t0 = Instant::now();
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(t0.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn screen(h: &mut midna_proto::Client, sid: &str) -> String {
        call(h, "session.read", json!({ "id": sid, "screen": true })).to_string()
    }

    fn the_shims_are_copies_of_midna() {
        let d = D::start();
        let shims = d.home.join("shims");
        let cli = std::fs::metadata(env!("CARGO_BIN_EXE_midna")).unwrap();
        for agent in ["claude", "codex"] {
            let m = std::fs::metadata(shims.join(format!("{agent}.exe"))).unwrap();
            assert_eq!((m.len(), m.modified().unwrap()), (cli.len(), cli.modified().unwrap()), "{agent}.exe");
            // Git Bash still runs the sh one.
            assert!(shims.join(agent).is_file());
        }
        assert!(shims.join(midna_proto::paths::SHIMS_MARKER).is_file());
        // A changed midna.exe replaces them; a running one is moved aside.
        let other = d.fake.join("midna.exe");
        std::fs::copy(env!("CARGO_BIN_EXE_midna"), &other).unwrap();
        std::fs::write(&other, b"not really midna").unwrap();
        midnad::adopt::exe_shims(&shims, &other);
        assert_eq!(std::fs::read(shims.join("claude.exe")).unwrap(), b"not really midna");
        assert!(!std::fs::read_dir(&shims).unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(".old")), "nothing ran them, so nothing is left aside");
    }

    fn typed_claude_in_powershell_runs_under_midna_and_restarts_in_the_same_shell() {
        let d = D::start();
        let mut h = d.human();
        let sid = d.powershell(&mut h, "claude --model opus 'hello there'");
        wait_for(15, "fake claude started", || (d.launches().len() == 1).then_some(()));

        // midna's flags first, then the arguments as typed; the terminal is an agent terminal now.
        let first = &d.launches()[0];
        assert!(first.contains("--settings ") && first.ends_with("--model opus hello there"), "{first}");
        let s = wait_for(5, "adopted", || Some(call(&mut h, "session.get", json!({ "id": sid }))).filter(|s| !s["adopted"].is_null()));
        assert_eq!((s["kind"].as_str(), s["agent"].as_str()), (Some("agent"), Some("claude")), "{s}");
        assert!(s["adopted"]["bin"].as_str().unwrap().starts_with(&*d.fake.to_string_lossy()), "the real one, not the shim: {s}");

        // Claude reports its conversation through midna's hooks (sent here by hand).
        let mut a = midna_proto::Client::connect(d.home.join("midnad.sock")).unwrap().as_agent(Some(sid.clone()));
        call(&mut a, "agent.hook", json!({ "agent": "claude", "event": "SessionStart", "payload": { "session_id": "conv-9", "source": "startup" } }));

        // Restart: the same conversation, inside the same shell, without the first prompt.
        let r = call(&mut h, "session.restart", json!({ "id": sid }));
        assert_eq!(r["status"]["reason"], "restarted (resumed)", "{r}");
        wait_for(15, "relaunch", || (d.launches().len() == 2).then_some(()));
        let second = &d.launches()[1];
        assert!(second.ends_with("--model opus --resume conv-9") && !second.contains("hello there"), "{second}");
        assert!(!screen(&mut h, &sid).contains("SHELL-BACK"), "the shell never got control back");

        // Claude exits on its own: its exit code reaches the shell, and the terminal is a shell again.
        std::fs::write(d.fake.join("quit"), "").unwrap();
        wait_for(15, "back at the shell", || screen(&mut h, &sid).contains("SHELL-BACK 3").then_some(()));
        let s = wait_for(5, "released", || Some(call(&mut h, "session.get", json!({ "id": sid }))).filter(|s| s["adopted"].is_null()));
        assert!(s["agent"].is_null() && s["agent_info"].is_null() && s["kind"] == "shell", "{s}");
    }

    /// npm installs `claude.cmd`, a shim that runs node on the package's script. The shim and
    /// midna start what it runs, not cmd, which would end the system prompt at its first newline
    /// and act on the `|` and `&`.
    fn a_typed_claude_from_npm_gets_every_argument() {
        let d = D::start();
        let npm = d.fake.join("node_modules");
        std::fs::create_dir_all(&npm).unwrap();
        std::fs::rename(d.fake.join("claude.exe"), npm.join("claude.exe")).unwrap();
        // What npm's cmd-shim writes for a package whose bin is an .exe (for a .js one it runs
        // node.exe on it; `midna_proto::cmd_shim` has that one).
        let shim = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\"%dp0%\\node_modules\\claude.exe\"   %*\r\n";
        std::fs::write(d.fake.join("claude.cmd"), shim).unwrap();
        let launches = || std::fs::read_to_string(npm.join("claude-argv")).unwrap_or_default().lines().map(str::to_string).collect::<Vec<_>>();
        let mut h = d.human();
        let sid = d.powershell(&mut h, "claude --append-system-prompt 'You work card 12.' 'a|b&c' 'hello there'");
        wait_for(15, "fake claude started", || (launches().len() == 1).then_some(()));
        let first = &launches()[0];
        // The fake logs a newline as \n: the caller's prompt, then midna's hint after it.
        assert!(first.contains("--append-system-prompt You work card 12.\\n") && first.ends_with("a|b&c hello there"), "{first}");
        let s = call(&mut h, "session.get", json!({ "id": sid }));
        assert!(s["adopted"]["bin"].as_str().unwrap().ends_with("claude.cmd"), "{s}");
        std::fs::write(npm.join("quit"), "").unwrap();
        wait_for(15, "back at the shell", || screen(&mut h, &sid).contains("SHELL-BACK 3").then_some(()));
    }

    fn anything_but_an_interactive_session_runs_as_typed_in_cmd() {
        let d = D::start();
        let mut h = d.human();
        let sid = d.cmd(&mut h, "claude --version & claude mcp list");
        wait_for(15, "back at the shell", || screen(&mut h, &sid).contains("SHELL-BACK").then_some(()));
        assert_eq!(d.launches(), ["mcp list"], "a subcommand gets exactly what was typed: {}", screen(&mut h, &sid));
        assert!(screen(&mut h, &sid).contains("FAKE claude 1.0"));
        assert!(call(&mut h, "session.get", json!({ "id": sid }))["adopted"].is_null());

        // An interactive one is adopted in cmd too.
        let sid = d.cmd(&mut h, "claude hi");
        wait_for(15, "adopted", || call(&mut h, "session.get", json!({ "id": sid }))["adopted"]["pid"].as_i64());
        // The shim reports the adoption before the fake claude logs its launch.
        wait_for(15, "launched", || (d.launches().len() == 2).then_some(()));
        assert!(d.launches()[1].ends_with(" hi"), "{:?}", d.launches());
        std::fs::write(d.fake.join("quit"), "").unwrap();
        wait_for(15, "released", || call(&mut h, "session.get", json!({ "id": sid }))["adopted"].is_null().then_some(()));

        // Off: even an interactive session runs as typed.
        let _ = std::fs::remove_file(d.fake.join("quit"));
        call(&mut h, "settings.set", json!({ "key": "agents.adopt_typed", "value": false }));
        let sid = d.cmd(&mut h, "claude hi");
        wait_for(15, "ran", || (d.launches().len() == 3).then_some(()));
        assert_eq!(d.launches()[2], "hi");
        assert!(call(&mut h, "session.get", json!({ "id": sid }))["adopted"].is_null());
    }
}
