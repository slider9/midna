//! `midna admit-wait GO-FILE MESSAGE -- ARGV…`: a held agent terminal's process on Windows
//! (midnad `admit.rs`, `guard.hold_agents_at`). It prints MESSAGE, waits until GO-FILE appears
//! or Ctrl-C is pressed, removes GO-FILE and runs ARGV as its child, exiting with its code.
//! Unix runs a `/bin/sh` script for this. Windows used Git Bash, which a PC without Git for
//! Windows doesn't have.
//!
//! midnad tells a terminal still waiting from one whose agent runs by whether this process has a
//! child yet, so nothing else may start one before the agent.
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Set by Ctrl-C (or Ctrl-Break): start now.
static GO: AtomicBool = AtomicBool::new(false);

/// Ctrl-C and Ctrl-Break start the agent while waiting, and are the agent's once it runs (it's
/// on the same console). A handler routine, not the "ignore" flag, which children would inherit.
fn catch_ctrl_c() {
    unsafe extern "system" fn on(kind: u32) -> windows_sys::core::BOOL {
        use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT};
        let ours = kind == CTRL_C_EVENT || kind == CTRL_BREAK_EVENT;
        if ours {
            GO.store(true, Ordering::Relaxed);
        }
        ours as windows_sys::core::BOOL
    }
    unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(on), 1) };
}

/// Wait until `go` exists or `started()` says so, then remove `go`.
pub fn wait_for(go: &Path, started: impl Fn() -> bool, tick: Duration) {
    while !started() && !go.exists() {
        std::thread::sleep(tick);
    }
    let _ = std::fs::remove_file(go);
}

pub fn run(raw: &[String]) -> ! {
    let (go, msg, argv) = match raw {
        [go, msg, dash, argv @ ..] if dash == "--" && !argv.is_empty() => (Path::new(go), msg, argv),
        _ => {
            eprintln!("usage: midna admit-wait GO-FILE MESSAGE -- COMMAND [ARGS…]");
            std::process::exit(2)
        }
    };
    catch_ctrl_c();
    println!("{msg}");
    wait_for(go, || GO.load(Ordering::Relaxed), Duration::from_millis(250));
    // Found as midnad's terminals find a program (with PATHEXT), and a batch-file shim (npm's
    // `claude.cmd`) swapped for what it runs: through cmd.exe an argument with a newline (the
    // agent's system hint) would be cut short.
    let path = std::env::var("PATH").unwrap_or_default();
    let Some(argv) = midna_proto::cmd_shim::resolve(argv, &path) else {
        eprintln!("{}: not found on PATH", argv[0]);
        std::process::exit(127)
    };
    match Command::new(&argv[0]).args(&argv[1..]).status() {
        Ok(s) => std::process::exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("{}: {e}", argv[0]);
            std::process::exit(126)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_for_the_go_file_or_a_start_then_removes_it() {
        let dir = std::env::temp_dir().join(format!("midna-admit-wait-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let go = dir.join("go");
        let g = go.clone();
        let t = std::thread::spawn(move || wait_for(&g, || false, Duration::from_millis(10)));
        std::thread::sleep(Duration::from_millis(200));
        assert!(!t.is_finished(), "it waits");
        std::fs::write(&go, b"").unwrap();
        t.join().unwrap();
        assert!(!go.exists(), "the go file is gone");
        // Ctrl-C: no go file needed.
        wait_for(&go, || true, Duration::from_millis(10));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
