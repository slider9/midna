//! midna's shell startup files on Windows (`adopt.rs`, `src/shell/*`) against the real shells:
//! Windows PowerShell and Git Bash, with startup files that rebuild PATH as people's do. The
//! shims here are stand-ins that print what they were asked to run. Git Bash is skipped where
//! Git for Windows isn't installed.
#![cfg(windows)]
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

mod common;

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Dir {
        // A space and a quote, as a home under `C:\Users\Jo O'Neil` would have.
        let d = common::tmp().join(format!("midna sh '{tag}' {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("shims")).unwrap();
        std::fs::create_dir_all(d.join("home")).unwrap();
        Dir(d)
    }

    fn p(&self, rel: &str) -> String {
        self.0.join(rel).to_string_lossy().into_owned()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(argv: &[String], env: &[(&str, String)], input: &str) -> String {
    let mut c = Command::new(&argv[0]);
    c.args(&argv[1..]);
    for (k, v) in env {
        c.env(k, v);
    }
    let mut child = c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    let out = child.wait_with_output().unwrap();
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

#[test]
fn powershell_runs_midnas_script_after_its_profiles() {
    let d = Dir::new("ps-argv");
    let init = d.0.join("midna.ps1");
    std::fs::write(&init, "[Console]::Out.Write('LOADED ' + $PSVersionTable.PSVersion.Major)").unwrap();
    let plain = |args: &[&str]| std::iter::once("powershell.exe").chain(args.iter().copied()).map(String::from).collect::<Vec<_>>();
    // The flags midna starts PowerShell with get the script; a command of the user's runs as given.
    let argv = midnad::adopt::powershell_argv(&plain(&["-NoLogo"]), &init).expect("PowerShell as midna starts it");
    assert_eq!(&argv[..4], &plain(&["-NoLogo", "-NoExit", "-EncodedCommand"])[..]);
    assert!(midnad::adopt::powershell_argv(&plain(&["-NoLogo", "-Command", "x"]), &init).is_none());
    // Without -NoExit it runs the script and ends: the encoded command reads the file at a path
    // with a space and a quote.
    let once: Vec<String> = argv.into_iter().filter(|a| a != "-NoExit").collect();
    let out = run(&once, &[], "");
    assert!(out.contains("LOADED 5") || out.contains("LOADED 7"), "{out}");
}

#[test]
fn powershell_marks_its_prompt_keeps_the_shims_first_and_follows_its_location() {
    let d = Dir::new("ps-prompt");
    let init = d.0.join("midna.ps1");
    std::fs::write(&init, midnad::adopt::POWERSHELL_INIT).unwrap();
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    // The user's own prompt, and a profile that rebuilt PATH without the shims; then midna's
    // script, a cd and a prompt.
    let script = format!(
        "$ErrorActionPreference = 'Stop'; function prompt {{ 'MINE ' + (Get-Location) + '> ' }}; $env:PATH = '{windir}\\System32'; \
         . ([ScriptBlock]::Create([IO.File]::ReadAllText('{init}'))); \
         Set-Location '{windir}'; $p = prompt; [Console]::Out.Write('RETURNED[' + $p + ']'); \
         [Console]::Out.Write('CWD[' + [Environment]::CurrentDirectory + ']'); [Console]::Out.Write('FIRST[' + ($env:PATH -split ';')[0] + ']'); \
         $null = prompt; [Console]::Out.Write('PATHS[' + @($env:PATH -split ';' | Where-Object {{ $_ -eq $env:MIDNA_SHIMS }}).Count + ']')",
        init = init.to_string_lossy().replace('\'', "''"),
    );
    let argv: Vec<String> = ["powershell.exe", "-NoLogo", "-NoProfile", "-Command", &script].iter().map(|s| s.to_string()).collect();
    let out = run(&argv, &[("MIDNA_SHIMS", d.p("shims"))], "");
    assert!(out.contains(&format!("\x1b]133;A\x07MINE {windir}> ")), "the prompt marked, then the user's prompt in the new folder:\n{out:?}");
    assert!(out.contains("RETURNED[\x1b]133;B\x07]"), "the input's mark comes last, on its own:\n{out:?}");
    assert!(out.contains(&format!("CWD[{windir}]")), "the process's folder follows Set-Location:\n{out:?}");
    assert!(out.contains(&format!("FIRST[{}]", d.p("shims"))), "the shims first again:\n{out:?}");
    assert!(out.contains("PATHS[1]"), "and only once:\n{out:?}");
}

/// A PowerShell terminal, through ConPTY: its prompts arrive marked where they are (ConPTY on
/// Windows 10 hands a mark on before the text written with it, see `midna.ps1`), what's typed
/// is input and the prompt isn't, Enter ends the input, and `cd` moves the folder midna sees.
#[test]
fn a_powershell_terminal_marks_its_prompts_and_follows_cd() {
    use common::*;
    use midna_proto::frame::{F_INPUT, Frame};
    let d = TestDaemon::start();
    let mut c = d.human();
    let s = call(&mut c, "session.open", serde_json::json!({ "kind": "shell", "cwd": tmp(), "command": ["powershell.exe", "-NoLogo", "-NoProfile"] }));
    let id = s["id"].as_str().unwrap().to_string();
    let pid = s["pid"].as_i64().unwrap() as i32;
    let mut st = d.human().attach(&id, 100, 30, 8, 16).unwrap();
    st.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    // Each row: its text, whether a prompt starts on it, and which cells are typed input.
    type Grid = Vec<(String, bool, Vec<bool>)>;
    let mut grid: Grid = vec![Default::default(); 30];
    let mut until = |st: &mut midna_proto::client::AttachStream, what: &str, ok: &dyn Fn(&Grid) -> bool| -> Grid {
        wait_for(20, what, || {
            st.want().unwrap();
            let f: Frame = st.next_frame().unwrap();
            for (y, r) in &f.changed {
                grid[*y as usize] = (r.text(), r.prompt, r.cells.iter().map(|c| c.flags & F_INPUT != 0).collect());
            }
            ok(&grid).then(|| grid.clone())
        })
    };
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let first = until(&mut st, "the first prompt, marked", &|g| g.iter().any(|(t, p, _)| *p && t.starts_with("PS ")));
    let row = first.iter().position(|(t, p, _)| *p && t.starts_with("PS ")).unwrap();
    st.input(format!("cd '{windir}'").as_bytes()).unwrap();
    let typed = until(&mut st, "the typed command, as input", &|g| g[row].0.contains("cd '"));
    let (text, _, input) = &typed[row];
    let at = text[..text.find("cd '").unwrap()].chars().count();
    assert!(input[at] && input[at + 3], "what's typed is input: {typed:?}");
    assert!(!input[..at].iter().any(|&i| i), "the prompt isn't: {typed:?}");
    st.input(b"\r").unwrap();
    let next = format!("PS {windir}>");
    let g = until(&mut st, "the next prompt, in the new folder", &|g| g.iter().any(|(t, p, _)| *p && t.starts_with(&next)));
    assert!(g.iter().filter(|(_, p, _)| *p).count() >= 2, "{g:?}");
    wait_for(5, "the folder midna reads", || (midnad::procs::cwd(pid).as_deref() == Some(windir.as_str())).then_some(()));
    // Enter ended the input: a command's output isn't input.
    st.input(b"Write-Output ('midna' + '-out')\r").unwrap();
    let g = until(&mut st, "the output", &|g| g.iter().any(|(t, _, _)| t.trim_end() == "midna-out"));
    let (_, prompt, input) = g.iter().find(|(t, _, _)| t.trim_end() == "midna-out").unwrap();
    assert!(!prompt && !input.iter().any(|&i| i), "{g:?}");
    call(&mut c, "session.close", serde_json::json!({ "id": id }));
}

#[test]
fn git_bash_runs_the_login_files_and_keeps_the_shims_first() {
    let bash = midnad::sys::posix_sh();
    if !std::path::Path::new(&bash).is_absolute() {
        return eprintln!("no Git Bash: skipped");
    }
    let d = Dir::new("bash");
    let init = d.0.join("init.bash");
    std::fs::write(&init, midnad::adopt::BASH_INIT).unwrap();
    std::fs::write(d.0.join("shims").join("claude"), "#!/bin/sh\necho \"SHIM claude args=$*\"\n").unwrap();
    // A profile that puts its own folders in front, as Git's /etc/profile does too.
    std::fs::write(d.0.join("home").join(".bash_profile"), "export PATH=\"/usr/bin:$PATH\"\nexport FROM_PROFILE=1\n").unwrap();
    // No real claude anywhere on PATH: just Windows and Git's tools.
    let git_usr = std::path::Path::new(&common::posix_bin("sh")).parent().unwrap().to_string_lossy().into_owned();
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let env = [
        ("HOME", d.p("home")),
        ("MIDNA_SHIMS", d.p("shims")),
        ("MIDNA_BASH_LOGIN", "1".into()),
        ("PATH", format!("{};{windir}\\System32;{git_usr}", d.p("shims"))),
    ];
    let argv: Vec<String> = [bash.as_str(), "--init-file", &init.to_string_lossy(), "-i"].iter().map(|s| s.to_string()).collect();
    let out = run(&argv, &env, "claude a\necho \"profile=$FROM_PROFILE first=${PATH%%:*}\"\nexit\n");
    assert!(out.contains("SHIM claude args=a"), "PATH, after the profiles put theirs in front:\n{out}");
    assert!(out.contains("profile=1"), "the login files ran:\n{out}");
    let first = out.split("first=").nth(1).and_then(|r| r.lines().next()).unwrap_or("");
    assert!(first.starts_with('/') && first.ends_with("/shims"), "the shims first, as a Git Bash path:\n{out}");
}
