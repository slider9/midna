//! The keeper end to end, with real processes. A keeper only serves midnad from its home's `bin`,
//! so this test copies itself into a temp `MIDNA_HOME\bin\test\midnad.exe`, beside a copy of
//! `midna-pty.exe`, and runs that copy in roles (`--role <name>`): one starts a keeper and exits,
//! another attaches to it later, and some run inside the shell to draw on its screen.
//!
//! `keeper.exe --many 20` (not part of the run) starts 20 keepers and reports what they cost.

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() {
    win::main()
}

#[cfg(windows)]
mod win {
    use midna_pty::client::{self, Client, Spawn};
    use midna_pty::names;
    use serde_json::{Value, json};
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};

    fn arg(args: &[String], name: &str) -> Option<String> {
        args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
    }

    pub fn main() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.first().map(String::as_str) == Some("--role") {
            let out = role(&args[1], &args[2..]);
            println!("{out}");
            return;
        }
        let home = std::env::temp_dir().join(format!("midna-pty-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let t = Test::new(&home);
        if let Some(n) = arg(&args, "--many") {
            println!("{}", serde_json::to_string_pretty(&many(&t, n.parse().unwrap())).unwrap());
            let _ = std::fs::remove_dir_all(&home);
            return;
        }
        let cases: [(&str, fn(&Test)); 7] = [
            ("a keeper outlives the client that started it and serves the next", handover_cmd),
            ("ctrl-c reaches the shell's programs, after a snapshot too", ctrl_c),
            ("powershell as the shell", handover_powershell),
            ("snapshots of wide characters and the alternate screen", screens),
            ("hangup ends the shell and the keeper", hangup),
            ("kill ends the shell and the keeper", kill),
            ("a client that isn't this home's midnad is refused", refused),
        ];
        let filter = args.iter().find(|a| !a.starts_with('-')).cloned();
        let mut failed = vec![];
        for (name, case) in cases {
            if filter.as_ref().is_some_and(|f| !name.contains(f.as_str())) {
                continue;
            }
            let t0 = Instant::now();
            match std::panic::catch_unwind(|| case(&t)) {
                Ok(()) => println!("test {name} ... ok ({:.1}s)", t0.elapsed().as_secs_f64()),
                Err(_) => {
                    println!("test {name} ... FAILED");
                    failed.push(name);
                }
            }
        }
        if failed.is_empty() {
            let _ = std::fs::remove_dir_all(&home);
            return;
        }
        // A failed case can leave its keeper and shell running: end them (by the pid they
        // recorded), and keep the home for its log.
        for (_, r) in names::records(&home) {
            unsafe {
                use windows_sys::Win32::System::Threading::{PROCESS_TERMINATE, TerminateProcess};
                let h = OpenProcess(PROCESS_TERMINATE, 0, r.pid);
                if !h.is_null() {
                    TerminateProcess(h, 1);
                    windows_sys::Win32::Foundation::CloseHandle(h);
                }
            }
        }
        println!("\nfailed: {failed:?}\nkeeper log: {}", names::pty_dir(&home).join("keeper.log").display());
        std::process::exit(1);
    }

    struct Test {
        home: PathBuf,
        /// This test, as `MIDNA_HOME\bin\test\midnad.exe`.
        midnad: PathBuf,
    }

    impl Test {
        fn new(home: &Path) -> Test {
            let dir = home.join("bin").join("test");
            std::fs::create_dir_all(&dir).unwrap();
            let midnad = dir.join("midnad.exe");
            std::fs::copy(std::env::current_exe().unwrap(), &midnad).unwrap();
            std::fs::copy(env!("CARGO_BIN_EXE_midna-pty"), dir.join(names::EXE)).unwrap();
            Test { home: home.to_path_buf(), midnad }
        }

        /// Run this test as midnad in `role`: what it printed.
        fn role(&self, role: &str, session: &str, extra: &[&str]) -> Value {
            let out = Command::new(&self.midnad)
                .args(["--role", role, "--home"])
                .arg(&self.home)
                .args(["--session", session])
                .args(extra)
                .output()
                .unwrap();
            let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| json!({"error": String::from_utf8_lossy(&out.stdout), "stderr": String::from_utf8_lossy(&out.stderr)}));
            assert!(out.status.success() && v.get("error").is_none(), "{role} {session}: {v}");
            v
        }

        fn start(&self, session: &str, shell: &str) -> (u32, u32) {
            let t0 = Instant::now();
            let v = self.role("start", session, &["--shell", shell]);
            // The role's output ended when it exited: the keeper didn't inherit its stdout.
            assert!(t0.elapsed() < Duration::from_secs(20), "{v}");
            let (keeper, shell_pid) = (v["keeper_pid"].as_u64().unwrap() as u32, v["shell_pid"].as_u64().unwrap() as u32);
            assert!(alive(keeper) && alive(shell_pid), "{v}");
            let rec = names::records(&self.home).into_iter().find(|(s, _)| s == session).map(|(_, r)| r).expect("a record");
            assert_eq!((rec.pid, rec.shell_pid, rec.protocol), (keeper, shell_pid, 1));
            assert_eq!(rec.pipe, names::pipe_name(&self.home, session));
            assert!(Path::new(&rec.exe).ends_with(Path::new("test").join(names::EXE)), "{}", rec.exe);
            (keeper, shell_pid)
        }

        fn gone(&self, session: &str, keeper: u32, shell_pid: u32) {
            let deadline = Instant::now() + Duration::from_secs(15);
            while (alive(keeper) || alive(shell_pid)) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            assert!(!alive(shell_pid), "the shell is still running");
            assert!(!alive(keeper), "the keeper is still running");
            assert!(!names::record_path(&self.home, session).exists(), "the record is still there");
        }
    }

    fn alive(pid: u32) -> bool {
        unsafe {
            let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
            if h.is_null() {
                return false;
            }
            let r = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
            windows_sys::Win32::Foundation::CloseHandle(h);
            r
        }
    }

    fn handover_cmd(t: &Test) {
        let (keeper, shell) = t.start("s1", "cmd");
        let v = t.role("drive", "s1", &["--shell", "cmd"]);
        assert_eq!(v["hello"]["shell_pid"], shell, "{v}");
        assert_eq!(v["hello"]["running"], true, "{v}");
        assert_eq!(v["hello"]["pid"], keeper, "{v}");
        assert_eq!(v["seen"], true, "{v}");
        assert_eq!(v["snapshot_has_marker"], true, "{v}");
        assert_eq!(v["exited"], 7, "{v}");
        assert_eq!(v["process_code"], 7, "{v}");
        t.gone("s1", keeper, shell);
    }

    fn ctrl_c(t: &Test) {
        let (keeper, shell) = t.start("s7", "cmd");
        let v = t.role("ctrlc", "s7", &[]);
        assert_eq!(v["pinging"], true, "{v}");
        assert_eq!(v["interrupted"], true, "ping ignored Ctrl+C: {v}");
        assert_eq!(v["exited"], 7, "{v}");
        t.gone("s7", keeper, shell);
    }

    fn handover_powershell(t: &Test) {
        let (keeper, shell) = t.start("s2", "powershell");
        let v = t.role("drive", "s2", &["--shell", "powershell"]);
        println!("  powershell: {}", json!({"took_ms": v["took_ms"], "snapshot_ms": v["snapshot_ms"]}));
        assert_eq!(v["seen"], true, "{v}");
        assert_eq!(v["snapshot_has_marker"], true, "{v}");
        assert_eq!(v["exited"], 7, "{v}");
        assert_eq!(v["process_code"], 7, "{v}");
        t.gone("s2", keeper, shell);
    }

    fn screens(t: &Test) {
        let (keeper, shell) = t.start("s3", "cmd");
        let v = t.role("screens", "s3", &["--me", &t.midnad.display().to_string()]);
        println!("  wide row: {}\n  wide cells: {}\n  alt screen: {}", v["wide_row"], v["wide_cells"], v["alt_rows"]);
        assert_eq!(v["wide_seen"], true, "{v}");
        assert!(v["wide_row"].as_str().unwrap().contains("W1 漢字 W2"), "{v}");
        assert_eq!(v["alt_has_marker"], true, "{v}");
        assert_eq!(v["alt_has_main_screen"], false, "{v}");
        assert_eq!(v["back_has_main_screen"], true, "{v}");
        assert_eq!(v["exited"], 7, "{v}");
        t.gone("s3", keeper, shell);
    }

    fn hangup(t: &Test) {
        let (keeper, shell) = t.start("s4", "cmd");
        let v = t.role("end", "s4", &["--how", "hangup"]);
        assert!(v["exited"].is_u64(), "{v}");
        t.gone("s4", keeper, shell);
    }

    fn kill(t: &Test) {
        let (keeper, shell) = t.start("s5", "cmd");
        let v = t.role("end", "s5", &["--how", "kill"]);
        assert_eq!(v["exited"], 1, "{v}");
        t.gone("s5", keeper, shell);
    }

    fn refused(t: &Test) {
        let (keeper, shell) = t.start("s6", "cmd");
        let pipe = names::pipe_name(&t.home, "s6");
        // This test's own executable isn't midnad.exe.
        let c = Client::connect(&pipe, Duration::from_secs(5)).unwrap();
        assert!(c.hello().is_err(), "served a client that isn't midnad");
        drop(c);
        // A midnad.exe from somewhere else isn't this home's.
        let elsewhere = t.home.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::copy(&t.midnad, elsewhere.join("midnad.exe")).unwrap();
        let out = Command::new(elsewhere.join("midnad.exe")).args(["--role", "hello", "--home"]).arg(&t.home).args(["--session", "s6"]).output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(v["error"].is_string(), "{v}");
        // And the keeper still serves its own.
        assert_eq!(t.role("hello", "s6", &[])["shell_pid"], shell);
        assert!(alive(keeper));
        t.role("end", "s6", &["--how", "kill"]);
        t.gone("s6", keeper, shell);
    }

    /// N keepers at once: what the system pays for them, idle and with every output pipe full.
    fn many(t: &Test, n: usize) -> Value {
        let before = perf();
        let t0 = Instant::now();
        let started: Vec<(String, u32, u32)> = (0..n).map(|i| {
            let s = format!("m{i}");
            let (k, sh) = t.start(&s, "cmd");
            (s, k, sh)
        }).collect();
        let start_ms = t0.elapsed().as_millis();
        std::thread::sleep(Duration::from_secs(2));
        let idle = perf();
        // Every shell prints 7 MB that nobody reads: it all waits in the 8 MB output pipes.
        for (s, _, _) in &started {
            let cmd = format!("\"{}\" --role flood --mb 7", t.midnad.display());
            t.role("type", s, &["--text", &cmd]);
        }
        std::thread::sleep(Duration::from_secs(25));
        let full = perf();
        for (s, k, sh) in &started {
            t.role("end", s, &["--how", "kill"]);
            t.gone(s, *k, *sh);
        }
        std::thread::sleep(Duration::from_secs(2));
        let after = perf();
        let mb = |a: &Value, b: &Value, k: &str| (b[k].as_f64().unwrap() - a[k].as_f64().unwrap()) / (1 << 20) as f64;
        json!({
            "keepers": n,
            "start_all_ms": start_ms,
            "idle_mb": {"commit": mb(&before, &idle, "commit"), "nonpaged": mb(&before, &idle, "nonpaged"), "paged": mb(&before, &idle, "paged"), "processes": idle["processes"].as_f64().unwrap() - before["processes"].as_f64().unwrap()},
            "pipes_full_mb": {"commit": mb(&before, &full, "commit"), "nonpaged": mb(&before, &full, "nonpaged"), "paged": mb(&before, &full, "paged")},
            "after_mb": {"commit": mb(&before, &after, "commit"), "nonpaged": mb(&before, &after, "nonpaged")},
        })
    }

    fn perf() -> Value {
        use windows_sys::Win32::System::ProcessStatus::{K32GetPerformanceInfo, PERFORMANCE_INFORMATION};
        unsafe {
            let mut p: PERFORMANCE_INFORMATION = std::mem::zeroed();
            K32GetPerformanceInfo(&mut p, std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32);
            let page = p.PageSize as f64;
            json!({"commit": p.CommitTotal as f64 * page, "nonpaged": p.KernelNonpaged as f64 * page, "paged": p.KernelPaged as f64 * page, "processes": p.ProcessCount})
        }
    }

    // ------------------------------------------------------------------------------- roles

    fn role(name: &str, args: &[String]) -> Value {
        let home = PathBuf::from(arg(args, "--home").unwrap_or_default());
        let session = arg(args, "--session").unwrap_or_default();
        let pipe = names::pipe_name(&home, &session);
        let r = match name {
            "start" => start_role(&home, &session, &arg(args, "--shell").unwrap()),
            "hello" => Client::connect(&pipe, Duration::from_secs(10))
                .and_then(|c| c.hello())
                .map(|h| json!({"shell_pid": h.shell_pid, "running": h.running}))
                .map_err(|e| e.to_string()),
            "drive" => drive(&pipe, &arg(args, "--shell").unwrap()),
            "screens" => screens_role(&pipe, &arg(args, "--me").unwrap()),
            "end" => end(&pipe, &arg(args, "--how").unwrap()),
            "ctrlc" => ctrlc_role(&pipe),
            "type" => type_role(&pipe, &arg(args, "--text").unwrap()),
            "paint-wide" => Ok(paint_wide()),
            "paint-alt" => Ok(paint_alt()),
            "flood" => Ok(flood(arg(args, "--mb").unwrap().parse().unwrap())),
            _ => Err(format!("no role {name}")),
        };
        r.unwrap_or_else(|e| json!({"error": e}))
    }

    fn shell_argv(shell: &str) -> Vec<String> {
        match shell {
            "powershell" => vec!["powershell.exe".into(), "-NoLogo".into(), "-NoProfile".into()],
            _ => vec!["cmd.exe".into(), "/d".into()],
        }
    }

    fn start_role(home: &Path, session: &str, shell: &str) -> Result<Value, String> {
        let exe = std::env::current_exe().unwrap().with_file_name(names::EXE);
        let s = Spawn { argv: shell_argv(shell), cwd: home.display().to_string(), env: std::env::vars().collect(), cols: 100, rows: 30 };
        let st = client::start(&exe, home, session, s).map_err(|e| e.to_string())?;
        // Exit without a word to the keeper: it stands on its own now.
        Ok(json!({"keeper_pid": st.keeper.pid, "shell_pid": st.shell_pid, "breakaway": st.keeper.breakaway}))
    }

    /// The output pipe, read on a thread into a buffer.
    struct Output(Arc<Mutex<Vec<u8>>>);

    impl Output {
        fn read(h: HANDLE) -> Output {
            let buf: Arc<Mutex<Vec<u8>>> = Arc::default();
            let (b, h) = (buf.clone(), h as usize);
            std::thread::spawn(move || {
                let mut chunk = vec![0u8; 65536];
                loop {
                    let mut n = 0u32;
                    if unsafe { ReadFile(h as HANDLE, chunk.as_mut_ptr(), chunk.len() as u32, &mut n, std::ptr::null_mut()) } == 0 || n == 0 {
                        break;
                    }
                    b.lock().unwrap().extend_from_slice(&chunk[..n as usize]);
                }
            });
            Output(buf)
        }

        fn wait_for(&self, text: &str, secs: u64) -> bool {
            let deadline = Instant::now() + Duration::from_secs(secs);
            while Instant::now() < deadline {
                if strip(&self.0.lock().unwrap()).contains(text) {
                    // A moment for the console host to finish the line.
                    std::thread::sleep(Duration::from_millis(200));
                    return true;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            false
        }
    }

    fn type_text(h: HANDLE, s: &str) {
        let mut b = s.as_bytes();
        while !b.is_empty() {
            let mut n = 0u32;
            assert!(unsafe { WriteFile(h, b.as_ptr(), b.len() as u32, &mut n, std::ptr::null_mut()) } != 0);
            b = &b[n as usize..];
        }
    }

    fn drive(pipe: &str, shell: &str) -> Result<Value, String> {
        let t0 = Instant::now();
        let c = Client::connect(pipe, Duration::from_secs(10)).map_err(|e| e.to_string())?;
        let hello = c.hello().map_err(|e| e.to_string())?;
        let a = c.attach().map_err(|e| e.to_string())?;
        let out = Output::read(a.output.as_raw_handle());
        let (cmd, marker) = match shell {
            "powershell" => ("Write-Output (\"PS-\" + (1+1) + \"-Z\")\r", "PS-2-Z"),
            _ => ("echo MARK-%ERRORLEVEL%-Z\r", "MARK-0-Z"),
        };
        type_text(a.input.as_raw_handle(), cmd);
        let seen = out.wait_for(marker, 30);
        let s0 = Instant::now();
        let snap = c.snapshot().map_err(|e| e.to_string())?;
        let snapshot_ms = s0.elapsed().as_secs_f64() * 1000.0;
        let rows: Vec<String> = (0..snap.rows as usize).map(|y| snap.row_text(y)).collect();
        let has = rows.iter().any(|r| r.trim_end() == marker);
        type_text(a.input.as_raw_handle(), "exit 7\r");
        let exited = c.wait_exited(Duration::from_secs(15));
        let mut code = 0u32;
        let waited = unsafe { WaitForSingleObject(a.process.as_raw_handle(), 5000) } == WAIT_OBJECT_0;
        unsafe { GetExitCodeProcess(a.process.as_raw_handle(), &mut code) };
        Ok(json!({
            "hello": {"shell_pid": hello.shell_pid, "running": hello.running, "pid": hello.pid, "protocol": hello.protocol},
            "seen": seen, "snapshot_has_marker": has, "rows": if has { json!(null) } else { json!(rows) },
            "exited": exited, "process_code": if waited { json!(code) } else { json!(null) },
            "took_ms": t0.elapsed().as_millis(), "snapshot_ms": snapshot_ms,
        }))
    }

    fn screens_role(pipe: &str, me: &str) -> Result<Value, String> {
        let c = Client::connect(pipe, Duration::from_secs(10)).map_err(|e| e.to_string())?;
        c.hello().map_err(|e| e.to_string())?;
        let a = c.attach().map_err(|e| e.to_string())?;
        let out = Output::read(a.output.as_raw_handle());
        let input = a.input.as_raw_handle();
        let rows = |c: &Client| -> Result<(Vec<String>, midna_pty::proto::Snapshot), String> {
            let s = c.snapshot().map_err(|e| e.to_string())?;
            Ok(((0..s.rows as usize).map(|y| s.row_text(y)).collect(), s))
        };
        type_text(input, &format!("\"{me}\" --role paint-wide\r"));
        let wide_seen = out.wait_for("WIDE-END", 20);
        let (main, snap) = rows(&c)?;
        let y = main.iter().position(|r| r.contains("W1 ")).unwrap_or(0);
        let x = snap.chars[y].iter().position(|&u| u == 'W' as u16).unwrap_or(0);
        let wide_cells: Vec<String> = (x..(x + 14).min(snap.cols as usize)).map(|i| format!("{:04x}/{:04x}", snap.chars[y][i], snap.attrs[y][i])).collect();
        type_text(input, &format!("\"{me}\" --role paint-alt\r"));
        let alt_seen = out.wait_for("ALT-MARK", 20);
        let (alt, _) = rows(&c)?;
        out.wait_for("ALT-DONE", 20);
        let (back, _) = rows(&c)?;
        type_text(input, "exit 7\r");
        let exited = c.wait_exited(Duration::from_secs(15));
        let has = |rows: &[String], t: &str| rows.iter().any(|r| r.contains(t));
        Ok(json!({
            "wide_seen": wide_seen, "wide_row": main[y].trim_end(), "wide_cells": wide_cells,
            "alt_seen": alt_seen, "alt_has_marker": has(&alt, "ALT-MARK"), "alt_has_main_screen": has(&alt, "WIDE-END"),
            "alt_rows": alt.iter().map(|r| r.trim_end()).filter(|r| !r.is_empty()).collect::<Vec<_>>(),
            "back_has_main_screen": has(&back, "WIDE-END"),
            "exited": exited,
        }))
    }

    fn ctrlc_role(pipe: &str) -> Result<Value, String> {
        let c = Client::connect(pipe, Duration::from_secs(10)).map_err(|e| e.to_string())?;
        let a = c.attach().map_err(|e| e.to_string())?;
        let out = Output::read(a.output.as_raw_handle());
        let input = a.input.as_raw_handle();
        c.snapshot().map_err(|e| e.to_string())?;
        type_text(input, "ping -n 30 127.0.0.1\r");
        let pinging = out.wait_for("Pinging 127.0.0.1", 10);
        std::thread::sleep(Duration::from_millis(1500));
        type_text(input, "\x03");
        type_text(input, "echo AFTER-%ERRORLEVEL%-Z\r");
        // Ping would run on for half a minute if Ctrl+C didn't reach it.
        let interrupted = out.wait_for("AFTER-", 8);
        type_text(input, "exit 7\r");
        let exited = c.wait_exited(Duration::from_secs(15));
        Ok(json!({"pinging": pinging, "interrupted": interrupted, "exited": exited}))
    }

    fn end(pipe: &str, how: &str) -> Result<Value, String> {
        let c = Client::connect(pipe, Duration::from_secs(10)).map_err(|e| e.to_string())?;
        c.hello().map_err(|e| e.to_string())?;
        let t0 = Instant::now();
        match how {
            "hangup" => c.hangup(),
            _ => c.kill(),
        }
        .map_err(|e| e.to_string())?;
        let exited = c.wait_exited(Duration::from_secs(15));
        Ok(json!({"exited": exited, "ms": t0.elapsed().as_millis()}))
    }

    fn type_role(pipe: &str, text: &str) -> Result<Value, String> {
        let c = Client::connect(pipe, Duration::from_secs(10)).map_err(|e| e.to_string())?;
        let a = c.attach().map_err(|e| e.to_string())?;
        type_text(a.input.as_raw_handle(), &format!("{text}\r"));
        std::thread::sleep(Duration::from_millis(300));
        Ok(json!({}))
    }

    fn console_write(s: &str) {
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE, WriteConsoleW};
        let w: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let mut n = 0;
            WriteConsoleW(GetStdHandle(STD_OUTPUT_HANDLE), w.as_ptr(), w.len() as u32, &mut n, std::ptr::null());
        }
    }

    fn paint_wide() -> Value {
        console_write("W1 漢字 W2 😀 W3\r\nWIDE-END\r\n");
        json!({})
    }

    fn paint_alt() -> Value {
        use windows_sys::Win32::System::Console::{ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_OUTPUT_HANDLE, SetConsoleMode};
        unsafe {
            let h = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut m = 0;
            GetConsoleMode(h, &mut m);
            SetConsoleMode(h, m | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
        console_write("\x1b[?1049h\x1b[2J\x1b[3;5HALT-MARK\r\n");
        std::thread::sleep(Duration::from_secs(3));
        console_write("\x1b[?1049lALT-DONE\r\n");
        json!({})
    }

    fn flood(mb: usize) -> Value {
        use std::io::Write;
        let line = format!("{}\n", "y".repeat(99));
        let mut o = std::io::stdout().lock();
        for _ in 0..(mb << 20) / line.len() {
            let _ = o.write_all(line.as_bytes());
        }
        json!({})
    }

    /// Text with escape sequences taken out, for finding a marker.
    fn strip(b: &[u8]) -> String {
        let s = String::from_utf8_lossy(b);
        let mut out = String::new();
        let mut it = s.chars();
        while let Some(c) = it.next() {
            if c == '\x1b' {
                match it.next() {
                    Some('[') => {
                        for c in it.by_ref() {
                            if ('@'..='~').contains(&c) {
                                break;
                            }
                        }
                    }
                    Some(']') => {
                        while let Some(c) = it.next() {
                            if c == '\x07' {
                                break;
                            }
                            if c == '\x1b' {
                                it.next();
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            } else if c >= ' ' {
                out.push(c);
            }
        }
        out
    }
}
