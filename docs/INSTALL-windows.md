# Installing Midna on Windows

Midna for Windows is an unzip-and-run app. There's no installer program: the first time you run
it from the unzipped folder, it copies itself into your user profile, adds itself to the Start
menu and Settings ▸ Apps, and starts. It needs no administrator rights.

**Needs:** 64-bit Windows 10 version 1809 (build 17763) or later, or Windows 11. Midna doesn't
install the programs you run in it: get Claude Code or Codex for agents, and Git for Windows if
you want a Git Bash terminal.

## 1. Download

Open <https://github.com/slider9/midna/releases> and pick the newest release whose title ends in
**for Windows** (`Midna 0.1.0-beta.58.w.1 for Windows`; its tag is `windows-v<version>`). While
the macOS beta it's built from is a beta, Windows releases are marked **Pre-release**. Under
Assets, download `Midna-<version>-windows-x86_64.zip`.

Leave the other things on that page alone:

- `windows-feed.json` is what the updater reads, not something to open.
- **Update channels** is a release that only holds the update feeds. It has nothing to
  download.
- Releases whose tag is `v<version>`, with no `windows-`, are macOS builds.

## 2. Unzip

Unzip it into a folder of its own, such as Downloads: right-click the zip ▸ **Extract All…**.
You get a `Midna` folder with `midna-app.exe`, `midnad.exe`, `midna.exe`, `midna-pty.exe`,
`conpty.dll`, `OpenConsole.exe`, `ConPTY-LICENSE.txt`, `midna-bundle.txt` and a `Fonts` folder.

Run it from a folder that holds only Midna. If `midna-bundle.txt` ends up among other files,
Midna refuses to install from there rather than copy them. The Midna that gets installed is a
copy; this folder is only where it starts from.

## 3. The SmartScreen warning

These builds aren't signed with a code-signing certificate yet, so the first time you run
`midna-app.exe`, Windows may show a blue **Windows protected your PC** box ("Microsoft Defender
SmartScreen prevented an unrecognized app from starting"). It shows because the file came from the
internet and isn't signed, not because the check found anything.

To go on: click **More info**, then **Run anyway**.

You answer it once. The install drops the "downloaded from the internet" mark (the
`Zone.Identifier` stream) from the copies it makes, so the installed `midna-app.exe`, which the
Start menu and the login entry start, doesn't show the box again. The zip and the unzipped folder
keep the mark, so running `midna-app.exe` from them again shows it again; start Midna from the
Start menu instead. An update that Midna downloads itself carries no mark either.

If you'd rather check the file first: **Properties** on the zip, ticking **Unblock** under
Security before you extract it, has the same effect for the unzipped files, and the zip's
checksum is in `windows-feed.json` (`sha256`), the signed feed that Midna checks every update
against.

## 4. First run

Double-click `midna-app.exe` in the unzipped folder. Nothing is installed yet, so it:

1. copies the folder into `%LOCALAPPDATA%\Programs\Midna` (that is,
   `C:\Users\<you>\AppData\Local\Programs\Midna`);
2. starts the copy there and exits.

The installed copy then:

- installs the background daemon `midnad` into Midna's home (see "Where Midna keeps its files"),
  and starts it, along with the terminals' keeper;
- adds **Midna** to the Start menu;
- adds **Midna** to Settings ▸ Apps (Apps & features), with its version, folder and an Uninstall
  button;
- adds a login entry, so the daemon is already running when you sign in (Settings ▸ Apps ▸
  Startup, or Task Manager's Startup tab, switches it off; Midna leaves it off);
- copies the `midna` command into `%USERPROFILE%\.local\bin`, if that folder is on your PATH (see
  below).

A toast tells you where it installed. From now on, start Midna from the Start menu. The unzipped
folder can be deleted.

Run `midna-app.exe` from the unzipped folder again and it starts the installed copy (which
comes to the front if it's already running). When the zip is a newer version than the installed
one, running it updates the installed copy; when it's older than the installed one, Midna refuses
and tells you to uninstall the newer one first.

## 5. The `midna` command

`midna.exe` is the command-line tool that agents, and you, use to drive Midna (`midna --help`,
`midna ls`, …). Terminals that Midna opens already have it. For a terminal opened elsewhere,
Midna copies it to `%USERPROFILE%\.local\bin\midna.exe` on a launch, but only when that folder is
already on your PATH. Midna never edits your PATH itself. To put the folder on it, in PowerShell:

```powershell
[Environment]::SetEnvironmentVariable('Path', "$env:USERPROFILE\.local\bin;" + [Environment]::GetEnvironmentVariable('Path', 'User'), 'User')
```

Then open a new terminal and run `midna --help`. (Or Start ▸ "Edit environment variables for your
account" ▸ Path ▸ Edit ▸ New.) Midna copies the command in on its next launch. An existing
`midna.exe` there that isn't Midna's is left alone.

The copy is made again whenever the installed one changes, so it stays current through updates.

## 6. Updates

An installed Midna checks for updates by itself, every 6 hours, and downloads a new version in
the background. Settings ▸ General ▸ **Updates** shows the state: *Up to date*, *Downloading…*,
or *<version> ready* with **Restart to apply**. **Check now** checks right away. Applying takes a
moment, and your terminals keep running through the restart. A downloaded update that you don't
apply is applied when you quit.

Where the updates come from:

- **Feed.** Windows releases look at the `channels` release of `slider9/midna` on GitHub
  (`https://github.com/slider9/midna/releases/download/channels/windows-<channel>.json`). It's
  compiled into the build; `updates.feed_url` in Settings changes it.
- **Channel.** `updates.channel` (Settings ▸ General ▸ Updates ▸ Channel) is `beta` or `stable`.
  A build of a beta version (the `0.1.0-beta.58.w.1` kind) follows `beta` until you pick one,
  because those builds are published only to the beta feed. A release of a stable version follows
  `stable` unless you pick `beta`. `beta` offers the newest Windows build of any kind; `stable`
  only builds of a stable version, and until the first of those is published, it reads as up to
  date.
- **Signature.** Every feed entry is signed. Midna checks the signature against the update key
  built into it, and the zip's sha256, before it unpacks anything, then checks that the unpacked
  build is the version the feed named. A feed or zip that fails any check is refused.

An update changes the files in `%LOCALAPPDATA%\Programs\Midna` in place and keeps the old ones
until the new build has started, putting them back if anything fails. If one fails, Settings ▸
Updates shows why.

## 7. Uninstall

Settings ▸ Apps ▸ Installed apps ▸ **Midna** ▸ **Uninstall** (there is no Modify or Repair). It
quits Midna, stops the daemon (**this closes every terminal in it**), and removes the login
entry, the `midna.exe` copy in `.local\bin`, the Start menu entry, the Settings ▸ Apps entry and
the files in `%LOCALAPPDATA%\Programs\Midna`. Anything else you put in that folder stays, and so
does the folder if it isn't empty.

It leaves Midna's **home** (`%LOCALAPPDATA%\com.mrgnhnt.midna`): your projects, settings,
terminal history, sounds and logs. Delete that folder yourself for a clean slate; installing again
without deleting it picks up where you left off. The global agent hooks Midna put in Claude's or
Codex's settings stay too. `midna hooks uninstall` (run before you uninstall) removes them.

## Where Midna keeps its files

| What | Where |
|---|---|
| The app | `%LOCALAPPDATA%\Programs\Midna` (`midna-app.exe`, `midnad.exe`, `midna.exe`, `Fonts`, …) |
| The home: state, settings, logs (`app.log`, `midnad.log`), hooks, shims, the daemon's socket, staged updates (`updates\`) | `%LOCALAPPDATA%\com.mrgnhnt.midna` |
| The daemon's installed builds (`midnad`, the ConPTY files and the terminal keeper, one folder per version) | `%LOCALAPPDATA%\com.mrgnhnt.midna\bin` |
| The `midna` command | `%USERPROFILE%\.local\bin\midna.exe` |
| Start menu entry | `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Midna.lnk` |
| Login entry | value `Midna` under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` |
| Settings ▸ Apps entry | `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\com.mrgnhnt.midna` |

Everything is under your user profile: nothing needs administrator rights, and nothing is written
to `Program Files` or `HKLM`.

## When something goes wrong

- **Nothing happens when you run `midna-app.exe`.** Midna says why in the console you ran it
  from, or in a message box. `%LOCALAPPDATA%\com.mrgnhnt.midna\app.log` has the same. Run it from
  a folder that holds only the unzipped build.
- **"… is already installed … which is newer."** Uninstall the installed Midna (above), then run
  this one.
- **A failed update.** Settings ▸ Updates shows the error; `%LOCALAPPDATA%\com.mrgnhnt.midna\updates\apply-failed.txt`
  has it. The old version keeps running. Download the zip from the release page and run it, which
  updates the installed copy.
- **Claude's desktop app.** Run from inside it, Windows redirects new files under AppData to the
  app's own package folder, so Midna's home isn't where this guide says. Install from a normal
  PowerShell or Explorer instead.
