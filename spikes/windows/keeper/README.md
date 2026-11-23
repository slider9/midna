# Keeper spike (T164)

A throwaway check of the keeper design (`midna-pty.exe`, one per terminal, "Windows updates that
keep terminals" Part A). One binary plays every part:

| Role | Stands in for | Does |
|---|---|---|
| `launch` | the midnad that opens a terminal | starts a `keeper` (no handle inheritance, `DETACHED_PROCESS` or `CREATE_NO_WINDOW`, `CREATE_BREAKAWAY_FROM_JOB`), waits for `hello`, exits |
| `keeper` | `midna-pty.exe` | creates the pipes (output pipe size from `--buf`), the pseudoconsole, a job and `cmd.exe`; serves `\\.\pipe\<name>`: `hello`, `attach` (DuplicateHandle of input, output and the shell's process into the pipe client's pid), `resize`, `repaint <ms> <dcols> <drows>`, `peek`, `hangup`, `hangup-drain`, `kill` |
| `client` | a midnad that attaches | `type`, `hold` (killed while holding the handles), `repaint`, `flood`, `exit` |
| `flood`, `paint` | programs in the shell | print flat out timing every write; fill the screen with styled rows |
| `drive` | the test | runs the scenarios, prints JSON |

```sh
cargo build --release   # lands in the checkout's target/ (CARGO_TARGET_DIR)
SPIKE_DETACHED=1 target/release/keeper-spike.exe drive --dir <tmp> \
  --conpty 'D:\Projects\midna\.toolchain\conpty\conpty.dll;system' --what basic,flood,hangup \
  --bufs 0,65536,1048576,8388608 --pauses 0,1000
```

`results/` has the runs the write-up quotes (Windows 10 22H2, 19045.6456; bundled ConPTY
1.25.260930003). `basic.txt` and `flood.txt` are summaries of the JSON beside them.

## What it found

1. **The shell keeps its console while only the keeper holds the HPCON.** The launcher exits, a
   client killed with the handles in hand changes nothing, and later clients keep typing into the
   same shell through duplicated handles and collect its exit code through the duplicated process
   handle. Killing the keeper ends the console host and the shell. Same on both ConPTYs.
2. **The ConPTYs differ in three ways that matter:** the resize nudge, the output rate (bundled
   passes VT through, ~2 MB/s flat out here; Windows' own renders, ~0.7–1.2 MB/s) and
   `ClosePseudoConsole` with a full unread pipe (bundled returns at once but the shell lives on;
   Windows' own blocks the caller, >15 s).
3. **Output pipe: 8 MB.** At 1 MB a 1 s pause still blocked a flat-out writer for ~0.5 s on the
   bundled ConPTY; at 8 MB nothing blocked on either.
4. **The nudge redraws the whole screen on Windows' own ConPTY (exact text, styles and cursor), and
   nothing at all on the bundled one**, whatever the size change or delay. `peek` (the keeper
   attaches to the shell's console for a moment and reads it with `ReadConsoleOutputW`) gives the
   exact text and cursor on both in ~5 ms, with 16-colour styles.

Also: spawn the keeper with no handle inheritance (std's `Command` leaked the launcher's stdout
into it), and before a hang-up with no midnad attached the keeper must read and drop the output
itself (`hangup-drain` ended the shell in under 15 ms on both).
