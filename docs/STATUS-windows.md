# Windows (native) status

A native Windows 10/11 x64 build of the whole workspace, on branch `feat/windows-native`. It's
separate from the WSL2 route (`chore/wslg-spike`), which runs the Linux build inside WSL. Nothing
here changes what macOS does: Windows code is in `cfg(windows)` modules or branches, POSIX code
went from unconditional to `cfg(unix)`, and Apple-only code to `cfg(target_os = "macos")`.

## Build

Needs (installed by hand):

- Rust stable, `x86_64-pc-windows-msvc` (tested with 1.99.1, via rustup-init x64)
- Visual Studio Build Tools: MSVC and a Windows SDK (tested with MSVC 14.51, SDK 10.0.26100)
- Zig **0.15.2** unpacked to `.toolchain/zig/` in the main checkout (libghostty-vt)
- Git for Windows: Git Bash is the build shell, and midna uses its `bash.exe` as the POSIX `sh`
- Python 3 as `python` (tests only: the fake agents in `crates/midnad/tests`)

In Git Bash:

```sh
. ./env.sh                 # puts .toolchain/zig first; GHOSTTY_SOURCE_DIR only if .toolchain/ghostty-src exists
cargo build --workspace    # target/debug/{midnad,midna,midna-app}.exe
scripts/test.sh -p midnad -p midna-cli
```

Without `.toolchain/ghostty-src`, libghostty-vt-sys fetches Ghostty itself.
`scripts/test.sh` falls back to a lock directory (`/tmp/midna-tests.lock.d`) where there's no `lockf`.

`.gitattributes` keeps every text file LF. Git for Windows defaults to `core.autocrlf=true`, and
midna embeds `init.bash`, `shim.sh` and the skill docs with `include_str!`, so a CRLF checkout
would build them in with `\r\n`.

## How each piece maps

| Area | macOS | Windows |
|---|---|---|
| Socket | AF_UNIX (`std::os::unix::net`) | AF_UNIX via `uds_windows` (`midna_proto::ipc`); a socket path is at most 108 bytes |
| Peer identity | `LOCAL_PEERPID` + audit token, codesign | `SIO_AF_UNIX_GETPEERPID`; no token or signature check (GUI matched by exe name) |
| Terminals | openpty + fork, `setsid` | ConPTY per terminal (`pty_windows.rs`), shell in a job object |
| Hang-up / kill | SIGHUP to the foreground group, then SIGKILL | `ClosePseudoConsole` (CTRL_CLOSE_EVENT), then `TerminateJobObject` |
| Signals (`sys::Sig`) | kill(2) | Hup/Term/Kill → TerminateProcess; Stop/Cont → NtSuspendProcess/NtResumeProcess; a "group" is the process plus its descendants |
| Process info (`procs`) | libproc, sysctl | Toolhelp snapshot, GetProcessTimes, NtQueryInformationProcess (command line), the PEB (cwd), GetExtendedTcpTable (listeners) |
| Load / memory guard | `getloadavg`, `vm_stat` | EWMA of GetSystemTimes busy share × CPUs; GlobalMemoryStatusEx |
| Keep awake | IOPMAssertion | PowerCreateRequest / PowerSetRequest(SystemRequired) |
| Secrets (webhooks) | Keychain | Credential Manager (`CredWriteW`, generic, `"{service}/{id}"`) |
| Default shell | `$SHELL -l` | `MIDNA_SHELL`, else `SHELL` if it is a Windows path, else pwsh, else Windows PowerShell (`-NoLogo`) |
| PATH for terminals and agents | the login shell re-reads it | `sys::user_path()`: the system's and the user's from the registry, read fresh for each terminal, then midnad's own. A tool installed while midnad runs is found |
| Canonical paths | `std::fs::canonicalize` | `paths::canonical()` drops the `\\?\` prefix canonicalize adds, so projects and cwds are stored and shown as `D:\x` |
| Daemon start | launchd, `--launchd` | `midnad` detaches with DETACHED_PROCESS (breakaway from the job when allowed); no autostart yet |
| Installed binaries | `bin/current` symlink | `bin/current` is a text file naming the version dir |
| In-place upgrade / restart | exec with the same pid, terminals kept | refused (`upgrade_windows.rs`): no exec on Windows. Stop the daemon and start the new one |
| Codex `notify` | `sh -c` wrapper chains the previous notify | `midna.exe hook codex notify [--global] -- PREV…`; the CLI runs PREV with the payload |
| Title bar | hidden; AppKit draws the traffic lights and drags by the top | hidden; `ui/caption.rs` draws a 32px row above each window: the caption (`WindowControlArea::Drag`: move, Snap, double-click) and minimise / maximise / close |
| App icon | the bundle's `Midna.icns` | `packaging/assets/Midna.ico` (from `make-icon.py`) linked in as icon resource 1 by `midna-app/build.rs`, where GPUI loads the window icon from |

Codex's notify can't go through a sh wrapper on Windows: Git Bash's runtime re-parses a native
command line and mangles JSON (`{"type":"x"}` arrives empty, `a"b` turns into `a\b`).

Windows production paths (`midna_proto::paths::production_paths`): `%LOCALAPPDATA%\com.mrgnhnt.midna`,
`%LOCALAPPDATA%\Programs\Midna`, `~\.local\bin\midna.exe`. Nothing is installed there yet.

## What works (verified on Windows 10 19045)

- `cargo build --workspace`, with no warnings.
- Tests:
  - `midna-proto`: 103/103.
  - `midnad --lib`: 224 pass, 1 ignored. This includes ConPTY (`a_console_app_runs_in_the_pseudoconsole`), MSVC argument quoting, `which` with PATHEXT, the process table, a TCP listener lookup and the socket peer pid.
  - `midna-cli`: all 51 unit tests and every integration test pass: `cli` 13, `mcp` 4, `secrets_cli` 6 (including `secret exec` masking), `system_wait` 1, `triggers_cli` 3. `adopt.rs` is Unix only.
  - midnad integration tests (`crates/midnad/tests/*.rs`): 170 of 200 pass (4 ignored: three helpers the tests run as programs, and one ignored on every platform). What fails and why is item 2 under "Not done yet".
  - `midna-app`: 349 unit tests.
- End to end, with `midnad.exe --foreground` on a temp `MIDNA_HOME` and the real `midna.exe`:
  - Terminals open and run commands for Windows PowerShell 5.1 (the default shell), cmd and Git Bash. `send` and `read --screen` work, `list` shows the console titles, and cwd is honoured.
  - A terminal's `PATH` is midna's shims dir followed by the inherited entries. `MIDNA_SESSION` and `TERM_PROGRAM=midna` are set.
  - `midna` run inside a terminal claims its terminal, and midnad checks the claim against the caller's pid and its ancestry.
  - `close` ends the terminal. Killing the daemon by PID leaves none of its descendants alive (conhost, the shells).
- The app builds and draws through DirectX, and it has started a dev midnad and connected to it. Its windows move, snap, minimise, maximise and close through the title row, which was checked with `WM_NCHITTEST`. The exe carries the icon, so the taskbar shows it.

## Not done yet

In rough order of how much they matter:

1. **Keyboard shortcuts.** GPUI's `cmd` is the Windows key, so the app's 114 `cmd-…` bindings and the `keys.*` defaults in `midna-proto/src/settings.rs` need `ctrl` variants (with care for terminal Ctrl keys). There are also about 57 `modifiers.platform` checks.
2. **Integration tests: the product gaps they show.** The tests run on Windows through the helpers in `crates/midnad/tests/common/mod.rs` (and midna-cli's): `tmp()`, `sh()` and `posix_bin()` (Git for Windows' `usr\bin` tools, as 8.3 paths), `script()` (a `.sh` with a `.cmd` that runs it), and a copy of the test binary standing in for `nc -U`. Each test binary sets `MIDNA_SHELL` to Git's `sh`, so one-line commands get a POSIX shell as on macOS. What's Unix only is `cfg(unix)` with its reason: `shell_integration.rs`, in-place upgrade and restart (`upgrade.rs` checks the refusal instead), file modes, `/` as the root, and `adopt.rs` until the shims below exist. The tests that still fail do so on the product:
   - **Paths split and checked on `/` only.** Basenames use `rsplit('/')` (`rpc/session.rs:300`, `auto_name.rs`, `rpc/project.rs:50`, `git.rs:74`, `procs.rs:285`, `midna-proto/src/system.rs`), so a terminal is named `C:\PROGRA~1\Git\usr\bin\sh.exe` and a project by its whole path. `agents.trust_folders`, `ui.status.items` and `ui.header.buttons` (`midna-proto/src/settings.rs:132,191`) and `rpc/script.rs:124` want a leading `/`. `rpc/project.rs:66` matches a project with `"{}/"`, so a subfolder becomes a project of its own, and worktree paths git prints as `C:/…` aren't canonicalised. 17 tests across approvals, auto_name, daemon, local_triggers, notify and system fail on these.
   - **`.cmd` agents get broken arguments.** A `.cmd` runs as `cmd /d /s /c "…"`, and cmd drops every argument after a newline (Claude's system hint) and acts on `|`, `<`, `>` and `&` inside them (Codex's hint). npm's `claude.cmd` and `codex.cmd` are affected the same way (agent_args, global_hooks).
   - **Agent calls are slow:** about 20 ms an agent RPC and 130 ms an `agent.hook` alone (a human call takes under 1 ms), 0.5–1 s a hook with a few terminals running. Resume's usage-limit tests run out their few-second windows in parallel and pass with `--test-threads=1`.
   - **ConPTY on Windows 10** drops faint text (SGR 2), so Claude's dim placeholder reads as typed text and a queued restart waits forever (restart_queue). It also clears a new terminal's screen, so an exited agent's last screen isn't kept above its shell (shell_on_exit). It keeps an app's kitty keyboard push, and probably its mouse mode, to itself (prompt jumps in a full-screen view fail; the tests now expect legacy keys). Its first title is the program's path.
   - **One handle leaks** per terminal open and close (`resources.rs`: 146 → 346 over 200 cycles).
   - **Intermittent: typed input gets lost.** Bytes sent to a fresh Git Bash terminal sometimes go missing: the first (`stream_attach_frames_roundtrip` once read `cho streamed-…`), a `\0` typed in a command, or the final Enter (`stream_keys_scroll_and_selection` fails about two runs in three). `steps_for_a_refused_prompt_clear_it_from_the_input_box` and `apps_register_their_own_link_patterns` fail now and then too.
3. **Typed agents in PowerShell and cmd.** The adopt shims are sh scripts without an extension, which only Git Bash runs. PowerShell and cmd need `claude.cmd` / `codex.cmd` (`midna.exe shim claude %*`; `is_shim` already recognises them).
4. **Shell integration.** `init.bash`, zsh and fish are wired up by shell name (`/`-separated, no `.exe`), so Git Bash terminals get none yet. PowerShell has no integration (prompt marks, cwd reports) at all.
5. **Daemon lifecycle.** There's no autostart (a Run key or Task Scheduler entry), no installer or updater, and no in-place upgrade. Upgrading keeping terminals alive would need a separate pty-host process that owns the ConPTYs.
6. **App stubs.** Notifications play a sound only (no toast). Badge, twilight, Kass composer, haptics and app link previews are missing. Commands that only exist on macOS (`sips`, `plutil`, `mdfind`, `xed`, `open -a`, `defaults`) do nothing, and menus and text still use macOS wording.
7. **Codex notify when midna.exe is gone.** The global entry calls midna.exe directly, so uninstalling midna without `midna hooks uninstall` also stops the previous notify command. The sh wrapper on macOS checks `-x` first.
8. **Console windows.**
   - midnad runs with a hidden console (`CREATE_NO_WINDOW`) so its git and other console children share it and no windows flash. With `DETACHED_PROCESS` each one opened a window.
   - `midna-app` is still a console-subsystem exe, so started from Explorer it would open a console window. A release build needs `windows_subsystem = "windows"`, and then the app's own child processes need `CREATE_NO_WINDOW`.
   - A daemon killed with `taskkill /F` can leave a headless `conhost.exe` (a terminal's ConPTY host) behind; a normal stop closes them.
9. **Socket path length.** A `MIDNA_HOME` deep enough to push `midnad.sock` past 108 bytes fails with "path must be shorter than SUN_LEN". The default home is short; a deep temp dir isn't.

## macOS side not compiled from here

This machine only has the Windows target, so the macOS build of the changed code hasn't been
compiled. To type-check it from Windows:

```sh
rustup target add aarch64-apple-darwin
cargo check --target aarch64-apple-darwin -p midna-proto -p midnad -p midna-cli
```

midna-app most likely needs a Mac, because gpui's macOS crates run bindgen against the SDK.
Before merging, run `scripts/test.sh` on a Mac.
