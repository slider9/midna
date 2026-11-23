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
- Microsoft's newer ConPTY in `.toolchain/conpty/`: `scripts/setup-conpty.sh` downloads the
  pinned `Microsoft.Windows.Console.ConPTY` from nuget.org and checks it, or takes a `.nupkg` you
  downloaded (`scripts/setup-conpty.sh FILE.nupkg`). Packages need it. Without it, dev builds and
  the tests use Windows' own (see Terminals below).

In Git Bash:

```sh
. ./env.sh                 # puts .toolchain/zig first; GHOSTTY_SOURCE_DIR only if .toolchain/ghostty-src exists
cargo build --workspace    # target/debug/{midnad,midna,midna-app}.exe
scripts/test.sh -p midnad -p midna-cli
```

Without `.toolchain/ghostty-src`, libghostty-vt-sys fetches Ghostty itself. With
`.toolchain/conpty`, `env.sh` sets `MIDNA_CONPTY` to its `conpty.dll`, so a cargo build's midnad
and the tests' daemons use it.
`scripts/test.sh` falls back to a lock directory (`/tmp/midna-tests.lock.d`) where there's no `lockf`.

### Terminals: which ConPTY

Windows 10's own ConPTY (kernel32) is old. It drops dim text (SGR 2), so Claude's dim
placeholder reads as typed text and a prompt queued for when Claude is idle waits forever. It
clears a new terminal's screen. It also keeps an app's kitty keyboard push to itself, and
probably its mouse mode. midnad uses Microsoft's current ConPTY instead, the one Windows Terminal
runs, when `conpty.dll` and `OpenConsole.exe` sit beside `midnad.exe` (`pty_windows.rs`
`conpty()`). Each terminal is then hosted by that `OpenConsole.exe`. VS Code and WezTerm ship it
the same way. `MIDNA_CONPTY=<path to a conpty.dll>` picks one, and `MIDNA_CONPTY=system` uses
Windows' own regardless. midnad's log says which one its terminals run in.

Once, on the newer ConPTY, a queued multi-line message vanished on its way into Claude Code
2.1.329. Taskboard's code-review findings for T58 went in at 23:05:52 on 2026-11-20 as one paste
and an Enter. Nothing reached the input box, the scrollback or the conversation, and no
`UserPromptSubmit` came, but the queue called it `sent`. Six other multi-line messages that week
arrived, and 20 trial pastes into Claude 2.1.330 all showed, so the cause is unknown. Into
Claude's input box the queue now checks delivery (`queue::deliver`, for every platform). The
text has to show in the box, or it's typed once more. After Enter, Claude has to start on it
(status, turn or title), or Enter is pressed once more while the text is still in the box.
Otherwise the message fails with a needs-you item instead of counting as sent. In the same
terminal the screen once lagged: a ctrl-u that cleared the box showed only with the next key
pressed.

### Installing

`packaging/build-windows.sh` builds release binaries and assembles a folder,
`dist/<version>-windows/Midna`, holding `midna-app.exe`, `midnad.exe`, `midna.exe`, the newer
ConPTY (`conpty.dll`, `OpenConsole.exe`), the fonts' and ConPTY's licenses and
`midna-bundle.txt`. It won't build without the ConPTY (`--conpty FILE` names another copy). With
that marker next to it, `midna-app.exe` runs as an installed Midna (`nonmac/install.rs`). Copy
the folder to `%LOCALAPPDATA%\Programs\Midna` and run `midna-app.exe`. On each launch it:

- installs midnad and the ConPTY files beside it (`midnad --install-self`) into
  `MIDNA_HOME\bin\<version>-<hash>`, unless that build is already there;
- adds itself to the Start menu (`%APPDATA%\Microsoft\Windows\Start Menu\Programs\Midna.lnk`);
- adds itself to Settings ▸ Apps (and Programs and Features), or brings that entry's version and
  folder up to date: a key under `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall`
  named for the bundle id (`com.mrgnhnt.midna`, Midna Dev's `com.mrgnhnt.midna.dev`), with its
  name, version, publisher, icon, folder and size. Its Uninstall button runs `midna-app.exe
  --uninstall`; there's no Modify or Repair;
- registers the login entry and starts midnad if it isn't answering;
- copies the `midna` CLI into `~\.local\bin` when that folder is on PATH.

There's no installer or updater yet. `midna-app.exe --uninstall` (also what Settings ▸ Apps'
Uninstall runs) quits the running app (it would start midnad again), stops midnad (closing its
terminals) and removes the login entry, the CLI copy, the Start menu entry and the Settings ▸
Apps entry, but leaves the folder and the home.

One app runs per `MIDNA_HOME`. At launch the app creates a named event,
`Local\midna-app-show-<hash of MIDNA_HOME>` (`instance.rs`). When it's already there, another
copy is running: the new launch lets it take the foreground (`AllowSetForegroundWindow`), sets
the event and exits, and the running copy brings the main window you used last to the front
(restoring it if minimized), or opens one when only Settings or a pop-out is open. Midna and
Midna Dev have different homes, so they never hand over to each other. Runs with the fake
backend, under Slate and for snapshots stand apart and always start.

To try a change in a real app, `scripts/dev-app-windows.sh` (the Windows `dev-app.sh`) builds
**Midna Dev** and installs it to `%LOCALAPPDATA%\Programs\Midna Dev`. Midna Dev has:

- its home built in (`%LOCALAPPDATA%\com.mrgnhnt.midna.dev`);
- its own login entry ("Midna Dev"), Start menu entry, toasts and CLI copy (in its home's `cli`
  folder);
- an inverted icon (`packaging/assets/MidnaDev.ico`) and the Gruvbox theme.

It quits the running copy with `midna-app.exe --quit` and starts the new one through Explorer.
midnad can't upgrade in place on Windows, so a running daemon stays on its old build (the app
says so); `--restart-daemon` stops it first, which closes its terminals.

Windows won't rename or delete a folder that a running process works in. midnad used to keep the
working folder it was started in, which was the app's folder when the app started it, so the
folder couldn't be replaced while the daemon ran. midnad now works in its home, and so do the
terminals' console hosts it starts. The dev script replaces the folder's files rather than the
folder, so a daemon from an older build doesn't block it either. Programs the app opens, such as
an editor, still start in the app's folder and would block replacing it the same way.

**From inside the Claude desktop app.** Its terminals, and so its agents' commands, run inside
its MSIX package. There Windows redirects new files under AppData into the package's own folder
(`%LOCALAPPDATA%\Packages\Claude_…\LocalCache`). Microsoft documents the same for HKCU registry
writes, which go to a per-package copy (not checked here). A midnad started there gets a home
only that package sees, and its AF_UNIX socket there refuses every connection (WSAEINVAL). A test home under `%TEMP%` isn't redirected, and the
integration tests use one. Programs started through Explorer or WMI (`Win32_Process.Create`)
run outside the package. That's why `dev-app-windows.sh` starts Midna Dev through Explorer.
`--uninstall` has no such way round it, so run it from a shell outside the app.

`.gitattributes` keeps every text file LF. Git for Windows defaults to `core.autocrlf=true`, and
midna embeds `init.bash`, `shim.sh` and the skill docs with `include_str!`, so a CRLF checkout
would build them in with `\r\n`.

## How each piece maps

| Area | macOS | Windows |
|---|---|---|
| Socket | AF_UNIX (`std::os::unix::net`) | AF_UNIX via `uds_windows` (`midna_proto::ipc`); a socket path is at most 108 bytes |
| Peer identity | `LOCAL_PEERPID` + audit token, codesign | `SIO_AF_UNIX_GETPEERPID`; no token or signature check (GUI matched by exe name) |
| Terminals | openpty + fork, `setsid` | ConPTY per terminal (`pty_windows.rs`): Microsoft's newer one beside midnad.exe, else Windows' own (see Terminals: which ConPTY); shell in a job object |
| Hang-up / kill | SIGHUP to the foreground group, then SIGKILL | `ClosePseudoConsole` (CTRL_CLOSE_EVENT), then `TerminateJobObject` |
| Signals (`sys::Sig`) | kill(2) | Hup/Term/Kill → TerminateProcess; Stop/Cont → NtSuspendProcess/NtResumeProcess; a "group" is the process plus its descendants |
| Process info (`procs`) | libproc, sysctl | Toolhelp snapshot for every process; one process (its parent, name, start; `bsd_info`, `parent_of`, `peer::ancestry`) asked of itself through one handle, with a snapshot only for one that can't be opened. GetProcessTimes, NtQueryInformationProcess (parent, command line), the PEB (cwd), GetExtendedTcpTable (listeners) |
| Load / memory guard | `getloadavg`, `vm_stat` | EWMA of GetSystemTimes busy share × CPUs; GlobalMemoryStatusEx |
| Keep awake | IOPMAssertion | PowerCreateRequest / PowerSetRequest(SystemRequired) |
| Secrets (webhooks) | Keychain | Credential Manager (`CredWriteW`, generic, `"{service}/{id}"`) |
| Default shell | `$SHELL -l` | `MIDNA_SHELL`, else `SHELL` if it is a Windows path, else pwsh, else Windows PowerShell (`-NoLogo`) |
| PATH for terminals and agents | the login shell re-reads it | `sys::user_path()`: the system's and the user's from the registry, read fresh for each terminal, then midnad's own. A tool installed while midnad runs is found |
| Canonical paths | `std::fs::canonicalize` | `paths::canonical()` drops the `\\?\` prefix canonicalize adds, so projects and cwds are stored and shown as `D:\x` |
| Paths kept as text | split and checked on `/` | `midna_proto::paths` helpers (`base_name`, `program_name`, `is_absolute`, `is_under`, `inside`, `same_path`, `trim_end_sep`, `native`, `home_var`): `\` and `/` both separate, case doesn't count, `.exe` comes off a program's name, git's `C:/x` becomes `C:\x`, and `~` is `%USERPROFILE%`. On macOS each does what the inline code it replaced did |
| Agents' shell commands | Claude Code's `Bash` tool | `Bash` and `PowerShell` (`system::is_shell_tool`), which Windows agents mostly use: the load guard holds and queues PowerShell builds (`tool_command` reads `\` as a separator and the backtick as the escape), insights and links read its commands, and a rule for either shell covers both (`Bash(git push --force*)` also catches `PowerShell(git push --force)`) |
| Daemon start | launchd, `--launchd` | `midnad` detaches with `CREATE_NO_WINDOW` (breakaway from the job when allowed) |
| Installed app | `Midna.app` (found by its `Contents/MacOS` path) | a folder with `midna-bundle.txt` next to `midna-app.exe`, by default `%LOCALAPPDATA%\Programs\Midna` (see Installing) |
| Start at login | SMAppService LaunchAgent; "Allow in the Background" in Login Items | a value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, "Midna" (Midna Dev: "Midna Dev"), runs `midna-app.exe --start-daemon`. That starts `bin\current`'s midnad with no window, unless it's already answering, and exits. Switching it off in Settings ▸ Apps ▸ Startup or Task Manager is kept in `…\Explorer\StartupApproved\Run` (first byte odd = off) and shows as needing approval, which opens `ms-settings:startupapps`. Registering it starts midnad at once, as launchd does |
| `midna` CLI for agents | a symlink `~/.local/bin/midna` → `bin/current/midna` | a copy of `bin\current`'s `midna.exe` at `~\.local\bin\midna.exe` (a symlink needs a privilege), made again when that one changes size or time. A running copy is renamed aside and removed next time. A `midna.exe` that isn't midna's (it doesn't name the bundle id) is left alone |
| Quit from a script | Apple Events (`osascript … quit`) | `midna-app.exe --quit` sets a named event (`Local\midna-app-quit-<hash of MIDNA_HOME>`) the app waits on, and it quits as the Quit action does. Closing its windows instead would ask about, or close, their terminals |
| One copy | LaunchServices: one app per bundle | a named event per `MIDNA_HOME` (`Local\midna-app-show-<hash>`): a second launch sets it and exits, and the running app brings its window to the front (see Installing) |
| Uninstall entry | none (the app is dragged to the Trash) | a key under `HKCU\…\CurrentVersion\Uninstall` named for the bundle id; Uninstall runs `--uninstall` (see Installing) |
| Console | n/a | a release `midna-app.exe` is a GUI program (`windows_subsystem = "windows"`): no console window. Its headless options (`--login-item-status`, `--uninstall`, `--quit`) print to the console they're run from (`AttachConsole`) or to a pipe or file. Console programs it runs get `CREATE_NO_WINDOW`. Debug builds keep their console |
| Installed binaries | `bin/current` symlink | `bin/current` is a text file naming the version dir; midnad finds `midna.exe` there (`install::current_cli`) or next to itself. The global hooks run `bin\midna.exe`, a copy each install refreshes (`install::stable_cli`) |
| Typed `claude` / `codex` (adopt) | sh shims in `MIDNA_HOME/shims` | the sh shims for Git Bash, plus `claude.exe` and `codex.exe` there: copies of midna.exe that run as `midna shim <agent>` under those names (`adopt::exe_shims`, refreshed when midna.exe changes; a running one is renamed aside). Every shell runs them, cmd included, with no argument mangling. `midna-shims.txt` marks the dir so the shim never takes one for the real agent |
| Shell integration | zsh (`ZDOTDIR`), bash (`--init-file`) and fish (`XDG_DATA_DIRS`) run midna's startup file: shims kept first on PATH, `claude`/`codex` aliases routed through them; zsh marks its prompt (OSC 133) | **PowerShell** (5.1 and 7) starts with `-NoExit -EncodedCommand`, a command that runs `shell/powershell/midna.ps1` after the profiles (no execution policy applies to it). Its wrapper around the user's prompt keeps the shims first, makes PowerShell's location the process's working folder (`Set-Location` never changes it, and midna reads a terminal's folder from there), and marks the prompt and input (OSC 133 A, B). With PSReadLine, Enter marks the output (C) unless the command is unfinished. ConPTY on Windows 10 passes a mark on at once but text on its next paint, so a mark written with text arrives ahead of it: each mark is written on its own, after a 15 ms pause. **Git Bash** gets `init.bash` as bash does on macOS (matched by program name, `bash.exe`), with `MIDNA_SHIMS` turned into a `/c/…` path by `cygpath`. Like bash on macOS it has no marks. Its `cd` moves the process's folder. **cmd**: the shims are first on PATH at start, and `cd` moves the folder |
| In-place upgrade / restart | exec with the same pid, terminals kept | refused (`upgrade_windows.rs`): no exec on Windows. Stop the daemon and start the new one |
| Agents' config folders | `CLAUDE_CONFIG_DIR` / `CODEX_HOME`, else `~/.claude` / `~/.codex` under `$HOME` | the same, with `~` as `%USERPROFILE%` (`paths::home_var`, as everywhere midna expands `~/`). A midnad started at login has no `HOME` (Git Bash sets one, Windows doesn't), and it looked for a relative `.claude`, so Settings ▸ Agents said Claude Code wasn't set up |
| Codex `notify` | `sh -c` wrapper chains the previous notify | `midna.exe hook codex notify [--global] -- PREV…`; the CLI runs PREV with the payload |
| Title bar | hidden; AppKit draws the traffic lights and drags by the top | hidden; `ui/caption.rs` draws a 32px row above each window: the caption (`WindowControlArea::Drag`: move, Snap, double-click) and minimise / maximise / close |
| App icon and name | the bundle's `Midna.icns` and `CFBundleName` | `midna-app/build.rs` links in `packaging/assets/Midna.ico` (from `make-icon.py`; Midna Dev: `MidnaDev.ico`, inverted) as icon resource 1, where GPUI loads the window icon from, and a version resource whose `FileDescription` ("Midna", or "Midna Dev") is the name Task Manager and Startup apps show |
| Shortcuts | ⌘ | Ctrl+Shift in the terminal's place, plain Ctrl where there's no terminal (see Shortcuts below) |
| Notifications (app) | `UNUserNotificationCenter`; osascript in dev builds | WinRT toasts (`midna-app/src/notify_windows.rs`) under an AppUserModelID the app registers for the user at launch, `HKCU\Software\Classes\AppUserModelId\com.mrgnhnt.midna` with the name and icon (debug builds and Midna Dev: `com.mrgnhnt.midna.dev`, "Midna Dev"). No installer or shortcut needed. Group = the terminal (`clear`), tag = `notify.send`'s id (replace, `withdraw`). Clicks and buttons, from a banner or Action Center, come through a COM activator the app serves (`CustomActivator`; the toast's own `Activated` event never fires for a click on its body in Action Center), with what to report carried as JSON in the toast's arguments. A release build registers itself as the activator's server, so a click starts it if it isn't running. Dismissals come from the toast's `Dismissed` event. A sound file plays through `PlaySoundW` beside a silent toast. Windows has no prompt: the permission is whether the human turned midna's toasts off |
| "You're looking at it" | `is_window_active` | `windows::in_use` / `windows::active_window`: also midna must be the foreground app. GPUI's Windows backend calls a window active while it's its thread's active window, which it stays after another app comes in front or it's minimized |

Codex's notify can't go through a sh wrapper on Windows: Git Bash's runtime re-parses a native
command line and mangles JSON (`{"type":"x"}` arrives empty, `a"b` turns into `a\b`).

Windows production paths (`midna_proto::paths::production_paths`): `%LOCALAPPDATA%\com.mrgnhnt.midna`,
`%LOCALAPPDATA%\Programs\Midna`, `~\.local\bin\midna.exe` and the Start menu's `Midna.lnk`. Midna
Dev never writes them. A release build of the app also registers the toast app id
`com.mrgnhnt.midna` (above) and the "Midna" login entry; debug builds and Midna Dev never do.

## Shortcuts

GPUI's `cmd` is the Windows key off macOS, and Windows keeps most Win+key combinations for itself,
so the defaults differ there. In a terminal a plain Ctrl+letter belongs to the shell and the agents
(^C, ^R, ^T, ^W, ^K), so midna's own shortcuts work like Windows Terminal's: they add Shift.

- Settings (`keys.*`): `midna-proto/src/settings.rs` gives each a second default (`keys(mac, other)`),
  used everywhere but macOS. ⌘X → Ctrl+Shift+X; ⇧⌘X, which would clash with it, → Ctrl+Alt+X; ⌥⌘X
  → Alt+Shift+X; ⌥⇧⌘X → Ctrl+Alt+Shift+X. Some follow Windows instead: Settings Ctrl+, · next and
  previous terminal Ctrl+Tab / Ctrl+Shift+Tab · new window Ctrl+Shift+N · mute Ctrl+Shift+M · focus
  the other split Ctrl+] · approve and deny Ctrl+Enter / Ctrl+Backspace (both fall through to the
  terminal when nothing is waiting, as ⌘↩ does).
- The terminal's own keys (`actions::term_keys`, `TerminalView::own_key`): copy Ctrl+Shift+C, and
  Ctrl+C when something is selected (with nothing selected, or a second time on the same selection,
  it's ^C); paste Ctrl+V or Ctrl+Shift+V; cut Ctrl+Shift+X; select all Ctrl+Shift+A; paste as secret
  Alt+Shift+V; find Ctrl+Shift+F (F3 / Shift+F3 in it); scroll Ctrl+Shift+Home, End, PgUp, PgDn;
  undo and redo in Claude's input Ctrl+Z / Ctrl+Shift+Z (not Ctrl+Y, Claude Code's yank). Projects
  1–9 are Ctrl+1–9. `terminal.cmd_enter_sends` sends on Ctrl+↩.
- Overlays, text fields, Settings and clicks use plain Ctrl (`Modifiers::secondary()`, which is ⌘ on
  macOS): Ctrl+Z / Ctrl+Y, Ctrl+A/C/X/V, Ctrl+←/→ by words in text fields, Ctrl+click opens a
  link, Ctrl+K in Settings searches.
- Labels say "Ctrl+Shift+T" (`actions::pretty`); hints written in glyphs go through
  `actions::glyph_keys` (⌘ read as Ctrl).
- Hide (⌘H) and the composer's keys are macOS only. The macOS editing layer in the terminal (⌘← ⌘→
  ⌘⌫, ⌘↑ ⌘↓ in Claude's input) has no Windows twin: Ctrl+←/→, Home and End go to the shell or the
  agent as they are.
- AltGr is Ctrl+Alt to Windows. GPUI types the AltGr character when the focused view takes text, so
  a Ctrl+Alt shortcut on a key with an AltGr character (Ctrl+Alt+E is € on a German layout) types
  the character there instead.

Checked in the real app (temp home, keys sent with `SendInput`): Ctrl+Alt+T opened a terminal,
Ctrl+C with nothing selected stopped `ping`, Ctrl+Shift+K opened the command bar (its hints read
Ctrl+Shift+J, Ctrl+Alt+T …), Ctrl+, opened Settings, Ctrl+] stepped to Shortcuts (which lists the
Windows keys), and Ctrl+Shift+W closed Settings and then the terminal. Not checked by hand yet:
copying with Ctrl+C, pasting, Ctrl+Tab.

Still in macOS wording: setting descriptions in the catalog, the Settings blurbs, the setup screen
and most of the remaining hints (about 450 strings with ⌘ or ⌥, a third of them in tests). The
shortcut list and the menus and hints you see most are done.

## What works (verified on Windows 10 19045)

- `cargo build --workspace`, with no warnings.
- Tests:
  - `midna-proto`: 109/109.
  - `midnad --lib`: 227 pass, 1 ignored. This includes ConPTY (`a_console_app_runs_in_the_pseudoconsole`), MSVC argument quoting, `which` with PATHEXT, the process table, a TCP listener lookup and the socket peer pid.
  - `midna-cli`: all 51 unit tests and every integration test pass: `cli` 13, `mcp` 4, `secrets_cli` 6 (including `secret exec` masking), `system_wait` 1, `triggers_cli` 3, and `adopt_windows` 3 (a typed `claude` adopted and restarted in PowerShell, and in cmd; the fake agent is the test binary as `claude.exe`). `adopt.rs` (sh fakes) is Unix only.
  - midnad integration tests (`crates/midnad/tests/*.rs`): on Windows' own ConPTY, 194 of 204 passed in the last full run, 196 in the one before (4 ignored: three helpers the tests run as programs, and one ignored on every platform). The difference is which intermittent tests failed. On the newer ConPTY (`MIDNA_CONPTY`), one full run of midnad and midna-cli had 7 failures where Windows' own had 10. Three were `.cmd` arguments. Two were the Enter tests, which still expected Windows' own; they now check which ConPTY is in use and pass on both. Two were intermittent: `stream_keys_scroll_and_selection`, and `close_on_exit`, which passed 5 of 5 run alone on each ConPTY. What fails and why is item 2 under "Not done yet". `shell_integration_windows.rs` runs the real shells: PowerShell's script after its profiles (from a path with a space and a quote), its prompt wrapper, a PowerShell terminal through ConPTY whose prompts arrive marked with the typed command as input and its output not, and whose `cd` moves the folder midna reads, and Git Bash's login files with the shims kept first.
- Agent calls are as quick as on macOS: 0.3 ms an agent RPC and 0.6 ms an `agent.hook` against a test daemon (0.1 ms for a human's call), with one terminal or five. They were 20 ms and 88 ms, because finding the caller's terminal and agent took a snapshot of every process (about 10 ms each), up to a few dozen times a hook.
  - `midna-app`: 359 unit tests, including key dispatch through GPUI with this platform's defaults, toast XML that Windows' own parser takes, a real Start menu shortcut read back, and the CLI copy kept current without replacing someone else's `midna.exe`.
- End to end, with `midnad.exe --foreground` on a temp `MIDNA_HOME` and the real `midna.exe`:
  - Terminals open and run commands for Windows PowerShell 5.1 (the default shell), cmd and Git Bash. `send` and `read --screen` work, `list` shows the console titles, and cwd is honoured.
  - A terminal's `PATH` is midna's shims dir followed by the inherited entries. `MIDNA_SESSION` and `TERM_PROGRAM=midna` are set.
  - `midna` run inside a terminal claims its terminal, and midnad checks the claim against the caller's pid and its ancestry.
  - `close` ends the terminal. Killing the daemon by PID leaves none of its descendants alive (conhost, the shells).
- The app builds and draws through DirectX, and it has started a dev midnad and connected to it. Its windows move, snap, minimise, maximise and close through the title row, which was checked with `WM_NCHITTEST`. The exe carries the icon, so the taskbar shows it.
- Installed, as Midna Dev from `scripts/dev-app-windows.sh` (a release build), started outside
  the Claude desktop app's package:
  - The first launch installed midnad into the home, wrote the "Midna Dev" login entry and the
    Start menu entry, started midnad and copied the CLI into the home's `cli` folder, all within
    a second, then found the running daemon current.
  - `midna-app.exe` is a GUI program and runs without a console, and Windows names it "Midna Dev".
  - The login entry's command, with the daemon stopped, started it with no window and exited in
    0.4 s. Run again with the daemon up, it did nothing (0.2 s).
  - Switched off as Startup apps does it, the entry reads as needing approval. Switched back on,
    or with that value gone, it reads as enabled.
  - `--quit` quit the running app through its Quit action. `--uninstall` removed the login
    entry, the CLI copy and the Start menu entry.
- Toasts, in the real app on a temp home with the app in the background (read back from Windows' toast history): `notify test` and `notify send` post under `com.mrgnhnt.midna.dev`, with buttons. Sending the same id again replaces the toast, `withdraw` and `notify clear` take toasts away, and `notify clear --session` takes only that terminal's. A notification for the selected terminal still shows while midna is minimized. Clicked by hand in Action Center, with a full-screen game in front: a toast's body brought midna forward on its terminal, a button's label reached a waiting `notify send --wait`, and closing a toast with its X answered `dismissed`.

## Not done yet

In rough order of how much they matter:

1. **macOS wording.** The shortcuts work (see Shortcuts), but setting descriptions, blurbs, the setup
   screen and many hints still say ⌘ and ⌥.
2. **Integration tests: the product gaps they show.** The tests run on Windows through the helpers in `crates/midnad/tests/common/mod.rs` (and midna-cli's): `tmp()`, `sh()` and `posix_bin()` (Git for Windows' `usr\bin` tools, as 8.3 paths), `script()` (a `.sh` with a `.cmd` that runs it), and a copy of the test binary standing in for `nc -U`. Each test binary sets `MIDNA_SHELL` to Git's `sh`, so one-line commands get a POSIX shell as on macOS. What's Unix only is `cfg(unix)` with its reason: `shell_integration.rs`, in-place upgrade and restart (`upgrade.rs` checks the refusal instead), file modes, `/` as the root, and `adopt.rs`, whose fake agents are sh scripts (`adopt_windows.rs` covers Windows). The tests that still fail do so on the product:
   - **`.cmd` agents get broken arguments.** A `.cmd` runs as `cmd /d /s /c "…"`, and cmd drops every argument after a newline (Claude's system hint) and acts on `|`, `<`, `>` and `&` inside them (Codex's hint). npm's `claude.cmd` and `codex.cmd` are affected the same way (agent_args, global_hooks, and agent_screen_prompts' fake Codex, where cmd answers "| was unexpected at this time").
   - **Windows 10's own ConPTY**, used when the newer one isn't beside midnad (a cargo build without `.toolchain/conpty`), drops faint text (SGR 2), so Claude's dim placeholder reads as typed text and a queued restart waits forever (restart_queue). It also clears a new terminal's screen, so an exited agent's last screen isn't kept above its shell (shell_on_exit). That screen was also how midna told a "No" typed into Claude's folder-trust dialog from a "Yes"; it now also checks that the agent is still running (`trust::confirm`). It keeps an app's kitty keyboard push, and probably its mouse mode, to itself (prompt jumps in a full-screen view fail; the Enter tests expect legacy keys on it). Its first title is the program's path. The newer ConPTY fixes the rest (the title wasn't checked on it): restart_queue, shell_on_exit, the jumps and the kitty Enter tests pass on it. With real Claude in Midna Dev, a queued prompt and its follow-up were both typed and answered.
   - **One handle leaks** per terminal open and close on Windows' own ConPTY (`resources.rs`: 146 → 346 over 200 cycles). On the newer ConPTY `resources.rs` passes (twice so far: in a full run and alone).
   - **Intermittent: typed input gets lost.** Bytes sent to a fresh Git Bash terminal sometimes go missing: the first (`stream_attach_frames_roundtrip` once read `cho streamed-…`), a `\0` typed in a command, or the final Enter (`stream_keys_scroll_and_selection` fails about two runs in three). `steps_for_a_refused_prompt_clear_it_from_the_input_box`, `shell_session_input_read_close`, `apps_register_their_own_link_patterns`, `another_terminals_id_is_a_link_to_it` and `a_gui_size_undone_at_once_never_reaches_the_app` fail now and then too, in a full run only. Since the merge with beta.58, so have `osc_title_is_tracked`, `pr_refs_link_to_the_terminals_github_repo`, `link_pattern_previews_run_only_as_link_at_gave_them` and `keys_an_app_isnt_reading_dont_stop_its_output` (its 200 KB flood outlasts the 30 s wait under the full run's load); each passes alone. Whether the merge made this worse isn't known: a pre-merge build to compare with would have to fetch Ghostty's source again. Until the client set its read timeout on the reader's socket handle as well (a clone is a handle of its own on Windows), a lost frame hung `stream_attach_frames_roundtrip` for good instead of failing it.
3. **Shell integration, what's left.** A PowerShell alias for `claude` that names a full path skips the shims, and a profile that rebinds Enter to its own handler gets no output mark. Git Bash and cmd have no prompt marks: each prints its prompt in one write, which ConPTY would reorder. Marks depend on ConPTY painting within 15 ms, so on a very busy machine one may land early.
4. **Installing and upgrading.** midnad starts at login and the app installs itself on launch
   (see Installing), but:
   - There's no installer and no updater (the updater reports itself off: no update key).
     Uninstalling from Settings ▸ Apps leaves the app's folder (and the home) behind.
   - A new build can't take over a running daemon: `daemon.upgrade` is refused, the app says
     so, and the old daemon runs until it's stopped, which closes its terminals. Keeping them
     would need a separate pty-host process that owns the ConPTYs.
   - The login-entry wording in the app (onboarding, Settings ▸ General) still says System
     Settings ▸ Login Items.
5. **App stubs.** Badge, twilight, Kass composer, haptics and app link previews are missing. Commands that only exist on macOS (`sips`, `plutil`, `mdfind`, `xed`, `open -a`, `defaults`) do nothing, and menus and text still use macOS wording. Notifications show as toasts, but:
   - midnad's own notifications (`via: system`, no app connected) use osascript and afplay, so on Windows nothing shows.
   - Starting the app from a click after it quit is registered only by an installed Midna's release build (`LocalServer32`), and hasn't been tried: Midna Dev and debug builds don't register it, so their toasts do nothing then. A dismissal while the app isn't running isn't reported.
   - A toast holds at most 5 buttons, so `notify send` with 6 loses the last one.
   - `PlaySoundW` has no volume (`volume` 0 silences, anything else is full), and plays one sound at a time.
   - Sounds are skipped when Windows reports quiet hours, presentation mode or a full-screen app (`SHQueryUserNotificationState`). Whether Focus assist reports that way hasn't been checked.
6. **Global hooks.** `bin\current` is a text file, not a link, so the global entries name `bin\midna.exe` instead, a copy of the current `midna.exe` that stays at the same path (`install::stable_cli`, used by `global_hooks::global_cli`). `midnad --install-self` refreshes it after each update, and midnad does it again whenever it finds the copy out of date. A copy a hook is running can't be replaced, so it's renamed aside and removed later. Nothing in midna deletes it, `--uninstall` included (MIDNA_HOME stays). Codex's notify entry calls that midna.exe directly, with no shell to check that it's there first (the macOS sh wrapper checks `-x`), so if someone deletes MIDNA_HOME by hand without `midna hooks uninstall`, the previous notify command stops too. No program built into Windows could stand in for the check: cmd.exe would put Codex's JSON, and the model's text in it, through cmd's parser, and PowerShell 5.1 and cscript drop its quotes. Entries installed before this change name a versioned folder and show as out of date until they're reinstalled once.
7. **Console windows.**
   - midnad runs with a hidden console (`CREATE_NO_WINDOW`) so its git and other console children share it and no windows flash. With `DETACHED_PROCESS` each one opened a window.
   - A release `midna-app` has no console (see the table). `cmd` doesn't wait for a GUI program, so its headless options print after the prompt has come back, and in a batch file their output into a file is lost; PowerShell and Git Bash get it.
   - A daemon killed with `taskkill /F` can leave a headless `conhost.exe` (a terminal's host on Windows' own ConPTY) behind; a normal stop closes them. On the newer ConPTY, one such kill left no `OpenConsole.exe` or shell behind.
8. **Socket path length.** A `MIDNA_HOME` deep enough to push `midnad.sock` past 108 bytes fails with "path must be shorter than SUN_LEN". The default home is short; a deep temp dir isn't.
9. **Upstream features not on Windows yet** (merged with v0.1.0-beta.58):
   - **Nearby** (pairing with other Midnas, sharing with them, driving them) is built on Bonjour (`dns_sd`), CoreFoundation, `poll` and Unix file calls. On Windows `src/nearby_windows.rs` stands in for `src/nearby/` with the same calls: `peers.list` reports Nearby off ("Nearby isn't on Windows yet"), the other `peers.*`, `share.*`, `drive.*` and `inbox.list` calls refuse with that, and the hooks other modules call do nothing. The app hides Settings ▸ Nearby, the terminal menu's "Share with a Nearby Mac…" and the ⌃⌘1–9 Mac switching, and its owner check (`local_auth`, Touch ID on macOS) always fails. `nearby.rs` and `nearby_objects.rs` are Unix only.
   - **Streams' producers** start in their own process group (`sys::new_group`) and stop with their tree (`sys::signal_group`), but `stream_producers.rs` is Unix only (its producers are `/bin/sh` scripts, checked with `ps` and `setsid`), so they're untested on Windows.
   - **Open-file limit** (`fd_limit`): Windows has no soft limit to raise; running out of handles (ERROR_TOO_MANY_OPEN_FILES) gets the same "out of handles" message. **Zombie reaping** (`reaper`) is Unix only: Windows keeps no zombies. Their tests are Unix only.
   - **Link preview commands** moved into `midna_proto::preview_cmd`. On Windows a command runs with no console window and only it is killed at the timeout (no process groups); PATH comes from the environment (no login shell); a bare name is also looked up with `.exe`, `.cmd` and `.bat`. Apps declare no preview command (there's no bundle `Info.plist`), so only link patterns' `preview:` commands run.

## macOS side not compiled from here

This machine only has the Windows target, so the macOS build of the changed code hasn't been
compiled. To type-check it from Windows:

```sh
rustup target add aarch64-apple-darwin
cargo check --target aarch64-apple-darwin -p midna-proto -p midnad -p midna-cli
```

midna-app most likely needs a Mac, because gpui's macOS crates run bindgen against the SDK.
Before merging, run `scripts/test.sh` on a Mac.
