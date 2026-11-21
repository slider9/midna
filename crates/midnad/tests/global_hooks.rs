//! hooks.status / preview / install / uninstall, and the either/or with per-launch hooks.
mod common;
use common::*;
use serde_json::{Value, json};

/// A fake agent that prints whether midna added its own hooks, then waits.
fn fake_agent(home: &std::path::Path) -> String {
    script(home.with_extension("agent.sh"), "echo \"ARGS:$*\"\necho \"INJECTED:${MIDNA_HOOKS_INJECTED:-no}\"\nsleep 30\n")
}

fn launch(d: &TestDaemon, agent: &str) -> String {
    let mut h = d.human();
    let s = call(&mut h, "session.open", json!({ "kind": "agent", "agent": agent, "cwd": tmp(), "cols": 1000 }));
    let sid = s["id"].as_str().unwrap().to_string();
    wait_for(10, "fake agent output", || {
        let r = call(&mut h, "session.read", json!({ "id": sid }));
        let text = r["text"].as_str().unwrap_or_default().to_string();
        text.contains("INJECTED:").then_some(text)
    })
}

#[test]
fn install_flips_per_launch_hooks_and_uninstall_restores() {
    let pre = tmp().join(format!("midna-gh-{}", std::process::id()));
    let bin = fake_agent(&pre);
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(bin));
    let claude_dir = d.home.join("agent-config/claude");
    let codex_dir = d.home.join("agent-config/codex");
    let mut h = d.human();

    // Neither agent set up on this "Mac".
    let st = call(&mut h, "hooks.status", json!({}));
    assert_eq!(st["claude"]["state"], "unavailable");
    assert_eq!(st["claude"]["per_session"], true);

    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::create_dir_all(&codex_dir).unwrap();
    let user = "{\n  \"model\": \"opus\",\n  \"hooks\": {\n    \"Stop\": [\n      { \"hooks\": [{ \"type\": \"command\", \"command\": \"/u/stop\" }] }\n    ]\n  }\n}\n";
    std::fs::write(claude_dir.join("settings.json"), user).unwrap();
    std::fs::write(codex_dir.join("config.toml"), "notify = [\"/apps/Other\", \"turn-ended\"]\nmodel = \"x\"\n").unwrap();
    assert_eq!(call(&mut h, "hooks.status", json!({}))["claude"]["state"], "not_installed");

    // Before installing, agents midna starts carry midna's own hooks.
    let out = launch(&d, "claude");
    assert!(out.contains("claude-settings.json") && out.contains("INJECTED:1"), "{out}");

    // Preview writes nothing.
    let p = call(&mut h, "hooks.preview", json!({}));
    assert_eq!(p["files"].as_array().unwrap().len(), 2);
    assert!(p["files"][0]["lines"].as_array().unwrap().iter().any(|l| l["op"] == "+"));
    assert_eq!(std::fs::read_to_string(claude_dir.join("settings.json")).unwrap(), user);

    // An agent may not install them.
    let e = call_err(&mut d.agent(None), "hooks.install", json!({}));
    assert_eq!(e.code, midna_proto::error::HUMAN_ONLY, "{e:?}");

    let st = call(&mut h, "hooks.install", json!({}));
    assert_eq!(st["claude"]["state"], "current", "{st}");
    assert_eq!(st["codex"]["state"], "current", "{st}");
    assert_eq!(st["claude"]["per_session"], false);
    let written: Value = serde_json::from_str(&std::fs::read_to_string(claude_dir.join("settings.json")).unwrap()).unwrap();
    assert_eq!(written["hooks"]["Stop"][0]["hooks"][0]["command"], "/u/stop", "the user's hook stays first");
    assert!(claude_dir.join("settings.json.before-midna").exists());
    let toml = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
    // midna's notify: a sh wrapper named midna-notify; on Windows `midna hook codex notify --global -- PREV`.
    let ours = |toml: &str| if cfg!(windows) { toml.contains("\"--global\"") } else { toml.contains("midna-notify") };
    assert!(ours(&toml) && toml.contains("/apps/Other"), "{toml}");

    // Now midna-started agents rely on the global hooks: no second set.
    let out = launch(&d, "claude");
    assert!(out.contains("claude-settings-base.json") && out.contains("INJECTED:no"), "{out}");
    let out = launch(&d, "codex");
    assert!(!out.contains("notify=") && out.contains("INJECTED:no"), "{out}");

    // Someone edits the file: stale, and midna injects again.
    let mut v = written.clone();
    v["hooks"].as_object_mut().unwrap().remove("PreToolUse");
    std::fs::write(claude_dir.join("settings.json"), serde_json::to_string_pretty(&v).unwrap()).unwrap();
    let st = call(&mut h, "hooks.status", json!({}));
    assert_eq!(st["claude"]["state"], "stale");
    assert!(st["claude"]["detail"].as_str().unwrap().contains("PreToolUse"));
    let out = launch(&d, "claude");
    assert!(out.contains("INJECTED:1"), "{out}");

    // Uninstall gives the user's files back.
    let st = call(&mut h, "hooks.uninstall", json!({}));
    assert_eq!(st["claude"]["state"], "not_installed");
    let back: Value = serde_json::from_str(&std::fs::read_to_string(claude_dir.join("settings.json")).unwrap()).unwrap();
    assert_eq!(back, serde_json::from_str::<Value>(user).unwrap());
    let toml = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
    assert!(toml.contains("\"/apps/Other\"") && !ours(&toml), "{toml}");
    remove_script(pre.with_extension("agent.sh"));
}

#[test]
fn unparseable_settings_are_never_rewritten() {
    let d = TestDaemon::start();
    let claude_dir = d.home.join("agent-config/claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let jsonc = "{\n  // my comment\n  \"model\": \"opus\"\n}\n";
    std::fs::write(claude_dir.join("settings.json"), jsonc).unwrap();
    let mut h = d.human();
    assert_eq!(call(&mut h, "hooks.status", json!({}))["claude"]["state"], "error");
    let e = call_err(&mut h, "hooks.install", json!({ "agents": ["claude"] }));
    assert!(e.message.contains("won't rewrite"), "{e:?}");
    assert_eq!(std::fs::read_to_string(claude_dir.join("settings.json")).unwrap(), jsonc);
}

/// Windows sets no `HOME` (Git Bash does), and midnad started at login has none. Claude Code's
/// and Codex's folders are then in the user's profile, not a relative `.claude` beside wherever
/// midnad works, which read as "Claude Code not set up". The real binary, since the in-process
/// daemons in these tests have their own agent folders.
#[cfg(windows)]
#[test]
fn without_home_the_agent_folders_are_in_the_profile() {
    let root = tmp().join(format!("midna-gh-p-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (home, profile) = (root.join("h"), root.join("p"));
    std::fs::create_dir_all(profile.join(".claude")).unwrap();
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_midnad"))
        .args(["--foreground", "--home"])
        .arg(&home)
        .current_dir(&root)
        .env_remove("HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("MIDNA_SOCKET")
        .env_remove("MIDNA_SESSION")
        .env("USERPROFILE", &profile)
        .env("MIDNA_NO_GH", "1")
        .env("MIDNA_NOTIFY_SYSTEM", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // Stopped by its pid however the test ends.
    struct Kill(std::process::Child, std::path::PathBuf);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
            let _ = std::fs::remove_dir_all(&self.1);
        }
    }
    let _daemon = Kill(child, root.clone());
    let mut h = wait_for(10, "daemon socket", || {
        let mut c = midna_proto::Client::connect(home.join("midnad.sock")).ok()?;
        c.set_caller(None);
        Some(c)
    });
    let st = call(&mut h, "hooks.status", json!({}));
    assert_eq!(st["claude"]["path"], json!(profile.join(".claude").join("settings.json")), "{st}");
    assert_eq!(st["claude"]["state"], "not_installed", "{st}");
    assert_eq!(st["codex"]["path"], json!(profile.join(".codex").join("config.toml")), "{st}");
    assert_eq!(st["codex"]["state"], "unavailable", "{st}");
}
