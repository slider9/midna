# Releasing midna

How a `Midna.app` is built, signed, published as an update, and installed. Code:
`packaging/`, `crates/midna-app/src/{install,updater,lifecycle}.rs`, and `midnad --launchd`
(`crates/midnad/src/main.rs`). Design decisions are in `docs/DECISIONS.md` ("Packaging,
install and auto-update").

## TL;DR: cut a release

```sh
tb release cut v0.2.0          # public release, from main
tb release cut v0.3.0-beta.1   # beta (prerelease), from any branch
```

`tb release cut` runs `scripts/release.sh` with `TB_RELEASE` set and records the release on the
task board when it succeeds. Run on its own, `release.sh` refuses unless you pass `--force`.
`release.sh` checks the version is newer than the last one (`scripts/newest-version.py`), writes it
into `Cargo.toml`/`Cargo.lock` (`scripts/set-version.sh`), commits, tags `vX.Y.Z` and pushes.
The tag runs `.github/workflows/release.yml`, in three jobs that hand their files on as
artifacts, so **Re-run failed jobs** picks up where it stopped instead of rebuilding:

- **build** (`macos-15`, the slow one)
  1. checks the files' version matches the tag, sets up `.toolchain/` (`scripts/setup-toolchain.sh`)
  2. imports the Developer ID certificate into a throwaway keychain (`packaging/ci-keychain.sh`)
  3. `packaging/build-app.sh` with the **release** update public key compiled in
  4. builds `packaging/release-tool`, which signs the feed in the next job
- **package** (`macos-15`)
  5. `packaging/notarize.sh` (App Store Connect API key), staples
  6. `scripts/build-dmg.sh` (branded window, `packaging/assets/dmg`), signs, notarizes and staples the DMG
  7. release notes = this version's `CHANGELOG.md` section, else its `## Unreleased` section
  8. `packaging/make-update.sh`: the `.app.tar.gz` and its signed feed entry
- **publish** (`ubuntu-latest`)
  9. publishes the GitHub release (DMG + archive; betas as prereleases), or replaces its files
     when a re-run finds it already made
  10. updates the feeds on the `channels` release: `stable.json` (public releases) and `beta.json`
      (newest of any; a stable hotfix older than the current beta leaves it alone)

Apple's notary and timestamp services drop connections now and then (`The network connection
was lost`, `stapler` error 68). `notarize.sh` and the DMG's `codesign --timestamp` retry those
calls through `scripts/retry.sh` (4 tries, 15s/30s/60s apart); if a job still fails, re-run it.

### A release failed

**Never skip a version.** If the workflow fails or is cancelled before the GitHub release is
published, nobody has that version, so cut the same number again:

1. A flaky step (Apple's network, a runner hiccup): **Re-run failed jobs**. Stop there.
2. Something to fix in the code: cancel any run still going (`gh run cancel <id>`), merge
   the fix to `main`, then delete the tag and cut it again:
   `git push origin :refs/tags/vX && git tag -d vX && tb release cut vX`.
   `main` already has version X, so `release.sh` has nothing to commit and tags the new head.

`release.sh` refuses to cut a newer version while the newest tag has no GitHub release, and
prints those commands. Once a release *is* published, its number is used up: installed copies
may already have it, and updates never go backwards.

### Changelog, website and test builds

Before tagging, rename `## Unreleased` in `CHANGELOG.md` to `## 0.2.0 — <date>`. The website
(`site/`, deployed by `.github/workflows/site.yml`) shows the changelog, and its `/download/` page
starts the DMG from the latest non-prerelease.

`.github/workflows/build-macos.yml` (manual) builds an ad-hoc signed app as a CI artifact
without publishing anything.

### Repository secrets and variables

| Name | Kind | What |
|---|---|---|
| `DEVELOPER_ID_CERTIFICATE` | secret | base64 of the Developer ID Application `.p12` |
| `DEVELOPER_ID_CERTIFICATE_PASSWORD` | secret | its password |
| `APP_STORE_CONNECT_API_KEY` | secret | contents of the `AuthKey_<id>.p8` (notarization) |
| `MIDNA_UPDATE_KEY` | secret | contents of `~/.config/midna-release/update-ed25519.key` |
| `DEVELOPER_ID_IDENTITY` | variable | `Developer ID Application: Morgan Hunt (U2G2XV3688)` |
| `APP_STORE_CONNECT_KEY_ID` / `APP_STORE_CONNECT_ISSUER_ID` | variable | the API key's ids |
| `MIDNA_UPDATE_PUBKEY` | variable | contents of `update-ed25519.pub`; the workflow checks it matches the secret |

### By hand (no CI)

```sh
export MIDNA_UPDATE_PUBKEY="$(cat ~/.config/midna-release/update-ed25519.pub)"
packaging/build-app.sh --version 0.2.0                   # -> dist/0.2.0/Midna.app (Developer ID signed)
packaging/notarize.sh dist/0.2.0/Midna.app               # OPT-IN: uploads to Apple, then staples
packaging/make-update.sh --app dist/0.2.0/Midna.app \
    --url-base https://github.com/mrgnhnt96/midna/releases/download/v0.2.0 --channel stable \
    --key ~/.config/midna-release/update-ed25519.key --notes "What changed"
```

`--version` overrides the workspace version for one build (every binary reports
`midna_proto::VERSION`, from `MIDNA_BUILD_VERSION`).

## Windows releases

Windows builds ship from the fork, `slider9/midna`, with the fork's own update key and no
Authenticode signature yet. Only slider9 runs them (the workflow checks `github.actor`).

```sh
git tag windows-v0.1.0-beta.58.w.1 && git push origin windows-v0.1.0-beta.58.w.1
```

A `windows-v` tag runs `.github/workflows/release-windows.yml`; a bare `v` tag runs
`release.yml` (macOS) and never this one. Two jobs:

- **build** (`windows-2022`; `windows-latest`'s MSVC 14.52 needs a newer Clang than Zig 0.15.2 has)
  1. checks the tag's version (below) and that the files say its upstream base
     (`scripts/set-version.sh --check <base>`)
  2. sets up `.toolchain/`: Zig and Ghostty's source (`scripts/setup-toolchain.sh`, which on
     Windows fetches Zig's Windows build) and Microsoft's newer ConPTY (`scripts/setup-conpty.sh`)
  3. `packaging/build-windows.sh --version <v>` with `MIDNA_UPDATE_PUBKEY` compiled in (it
     refuses to build without the variable) and checks `midnad.exe --version` says `<v>`
  4. checks the secret's public half is one of `MIDNA_UPDATE_PUBKEY`'s keys, then
     `packaging/make-update.sh --windows`: `Midna-<v>-windows-x86_64.zip` and its signed v2 entry
     `windows-feed.json`, verified against the whole key list
- **publish** (`ubuntu-latest`)
  5. publishes the GitHub release `windows-v<v>` with the zip and `windows-feed.json` (betas as
     prereleases), or replaces its files when a re-run finds it already made
  6. updates the `channels` release: `windows-stable.json` (builds of a stable base) and
     `windows-beta.json` (the newest of any, by `scripts/newest-version.py`). `stable.json` and
     `beta.json` are macOS's and stay as they are.

**Version.** A Windows version is the upstream version the fork last merged, which is what
`Cargo.toml` says, plus a Windows counter:

| Upstream base | Windows builds |
|---|---|
| `0.1.0-beta.58` (a beta) | `0.1.0-beta.58.w.1`, `0.1.0-beta.58.w.2`, … |
| `0.2.0` (stable) | `0.2.0-w.1`, `0.2.0-w.2`, … |

The files keep the upstream version, so a merge from upstream never conflicts on it; the
counter lives only in the tag (and in `MIDNA_BUILD_VERSION`, so every binary reports it). The
counter restarts at 1 for each new base. By semver, which the updater and
`newest-version.py` both use, `0.1.0-beta.58 < 0.1.0-beta.58.w.1 < 0.1.0-beta.58.w.2 <
0.1.0-beta.59 < 0.2.0-w.1 < 0.2.0`: more parts win when the rest match, numbers compare as
numbers, and `beta` sorts before `w`. `0.2.0-w.1` is a semver pre-release of `0.2.0`, which is
fine because no Windows build is ever plain `0.2.0`; it still counts as a stable release here
(it has no `-beta.`), so it goes in `windows-stable.json` and is published as the latest release.
Build metadata (`0.2.0+w.1`) wouldn't work: semver ignores it, so `+w.2` would never be newer.

**Trying it without a release.** Run the workflow by hand on a branch with a version, and it
makes a **draft** release carrying the zip and `windows-feed.json` and skips the channels:

```sh
gh workflow run release-windows.yml --ref <branch> -f version=0.1.0-beta.58.w.1 -f draft=true
gh release download windows-v0.1.0-beta.58.w.1 --pattern '*' --dir /tmp/rw   # drafts too
packaging/release-tool/target/release/midna-release verify /tmp/rw/windows-feed.json \
    "$(gh variable get MIDNA_UPDATE_PUBKEY)" /tmp/rw/Midna-0.1.0-beta.58.w.1-windows-x86_64.zip
gh release delete windows-v0.1.0-beta.58.w.1 --yes
```

GitHub only dispatches a workflow it knows, so the file has to be on the default branch (or
have run once from the branch). Run by hand on a `windows-v` tag with `draft` off, it publishes
that tag like a push would; on a branch it only ever makes drafts. A draft run refuses a
version that is already published, and a real run replaces a leftover draft of its version
with a fresh release before any channel points at it.

The same rules hold as for macOS: never skip a version, and a failed run is re-run or cut again
at the same number. The fork's `MIDNA_UPDATE_KEY` secret and `MIDNA_UPDATE_PUBKEY` variable
are the fork's key; its secret half is kept offline (`%USERPROFILE%\.config\midna-release-windows`).
The workflow publishes with `GITHUB_TOKEN` (`contents: write`), and the fork has no ruleset, so
it needs no deploy key.

## The bundle (`packaging/build-app.sh`)

```
Midna.app/Contents/
  Info.plist                     id com.mrgnhnt.midna, version, LSMinimumSystemVersion 12.0,
                                 KassDictationHandshake, NSAccessibilityUsageDescription, …
  MacOS/midna-app                the GUI (CFBundleExecutable)
  MacOS/midnad                   the daemon (copied to MIDNA_HOME/bin on first launch)
  MacOS/midna                    the CLI (copied next to midnad)
  Library/LaunchAgents/com.mrgnhnt.midna.daemon.plist
  Resources/Midna.icns           the app icon (packaging/make-icon.py draws it)
  Resources/Fonts/               Atkinson Hyperlegible Next + JetBrains Mono and their OFL
                                 licenses (the app embeds the same files with include_bytes!)
```

- Binaries are built with `cargo build --release` into `target/package` (separate from dev
  builds), `MACOSX_DEPLOYMENT_TARGET=12.0`, host architecture only. Universal builds
  (`lipo` of aarch64 + x86_64) are a TODO; the feed's `arch` field already allows them.
- **LaunchAgent plist**: `BundleProgram Contents/MacOS/midnad`, arguments `--launchd`,
  `RunAtLoad`, `KeepAlive {SuccessfulExit: false}`, `ProcessType Interactive`,
  `AssociatedBundleIdentifiers [com.mrgnhnt.midna]`, env `MIDNA_SIGTERM=stop`. launchd always
  starts the bundle's binary (SMAppService requires `BundleProgram`), and `--launchd` makes it
  exec the stable copy at `MIDNA_HOME/bin/current/midnad` right away (installing itself there
  if there is none). That's what `spikes/install-update` proved.
- **Signing** is inside-out: `midnad` (identifier `com.mrgnhnt.midna.daemon`), `midna`
  (`com.mrgnhnt.midna.cli`), then the bundle. Hardened runtime, secure timestamp,
  `packaging/entitlements.plist` (deliberately empty). The identity is the first "Developer ID
  Application" certificate in the keychain (team U2G2XV3688), else ad-hoc. `MIDNA_SIGN_IDENTITY`
  picks one, `--adhoc` forces ad-hoc, and `MIDNA_SIGN_TIMESTAMP=0` signs offline.
- **TCC**: grants (Accessibility) are keyed to the designated requirement, which is the same
  for every Developer ID build, so they survive updates. An ad-hoc build's requirement is its
  hash, so each ad-hoc build is a new app to TCC.

## Notarization (`packaging/notarize.sh`)

It uploads the app to Apple, so only the release workflow runs it on its own: the **package**
job notarizes the app and the DMG of every tag, with an App Store Connect API key
(`MIDNA_NOTARY_KEY`, `MIDNA_NOTARY_KEY_ID`, `MIDNA_NOTARY_ISSUER`). Nothing local runs it
(`build-app.sh`, `make-update.sh` and the e2e test don't). To notarize a local build by hand,
set up a profile once:
`xcrun notarytool store-credentials midna-notary --apple-id … --team-id U2G2XV3688`. Then
`packaging/notarize.sh dist/<v>/Midna.app` zips, submits with `--wait`, staples, and runs
`spctl`, retrying the submit and the staple (`MIDNA_NOTARY_ATTEMPTS`, default 4). Given a signed
`.dmg` it submits that as is. Notarize **before** `make-update.sh` so the archive carries the stapled ticket.
Un-notarized Developer ID builds run fine for the developer; Gatekeeper blocks them on other
Macs when they're downloaded through a browser (quarantine). Updates fetched by midna itself
aren't quarantined.

## First launch (what the app does)

`crates/midna-app/src/lifecycle.rs` runs this on a background thread on every launch. It's
idempotent, so later launches only check:

1. **Install the daemon**: if `MIDNA_HOME/bin/current/midnad` isn't byte-identical to the
   bundled one, run `Contents/MacOS/midnad --install-self`. That copies `midnad` and `midna` to
   `bin/<version>-<hash>/` and swaps the `current` symlink atomically. Old versions are pruned
   to the current one plus two.
2. **Register the LaunchAgent**: `SMAppService.agent(plistName:)` on macOS 13+. If macOS says
   *requires approval*, Settings ▸ Permissions ▸ Login item shows "Needs your OK in Login Items"
   with **Fix** (`SMAppService.openSystemSettingsLoginItems()`), and the app polls until it's on.
   On macOS 12 it writes `~/Library/LaunchAgents/<label>.plist` (program = `bin/current/midnad`)
   and runs `launchctl bootstrap`.
3. **The CLI for agents**: symlink `~/.local/bin/midna` → `MIDNA_HOME/bin/current/midna` when
   `~/.local/bin` is on the login shell's `PATH` (read with `$SHELL -l -i -c 'echo $PATH'`). If
   it isn't, Settings offers "Install to ~/.local/bin" plus the `export PATH=…` line to add
   yourself. midna never edits shell rc files. A `midna` there that isn't our symlink is left
   alone. midna's own terminals always have the CLI dir on `PATH`.
4. **Connect**, and if the running daemon isn't `bin/current/midnad` (compared through
   `daemon.info.binary`), call `daemon.upgrade` with it. That's a same-PID re-exec, and the shells
   survive. If the agent is enabled but not answering after about 4s, `launchctl kickstart`.

**Dev mode** (`MIDNA_DEV=1`, or running outside a `.app`, e.g. `target/debug/midna-app`)
skips all of that. If midnad isn't answering, it starts the `midnad` next to the binary
(`MIDNA_NO_SPAWN=1` turns that off). `MIDNA_BACKEND=fake` does nothing at all.

**Uninstall**: `Midna.app/Contents/MacOS/midna-app --uninstall` runs `daemon.stop` (which hangs
up every terminal), unregisters the login item and removes our CLI link. It leaves
`MIDNA_HOME` in place. `--login-item-status` prints the SMAppService state.

## Auto-update

**Feed**: the setting `updates.feed_url` (human only; default
`https://github.com/mrgnhnt96/midna/releases/download/channels/{channel}.json`, where `{channel}`
is replaced by `updates.channel`, `stable` or `beta`; `midna_proto::settings::DEFAULT_FEED_URL`). `MIDNA_UPDATE_FEED_URL` overrides it (tests). The feed is one JSON object:

```json
{
  "version": "0.1.1",
  "pub_date": "2026-10-03T21:40:00Z",
  "notes": "What changed (shown in Settings)",
  "url": "https://github.com/mrgnhnt96/midna/releases/download/v0.1.1/Midna-0.1.1.app.tar.gz",
  "sha256": "9f2c…(64 hex)",
  "size": 31457280,
  "minimum_macos": "12.0",
  "arch": "aarch64",
  "signature": "base64 ed25519 signature"
}
```

- `signature` is ed25519 over exactly
  `"midna-update-v1\nversion=<version>\nsha256=<lowercase sha256>\nminimum_macos=<min>\narch=<arch>\n"`.
  The public keys (base64, raw 32 bytes each) are compiled in from `MIDNA_UPDATE_PUBKEY` at build
  time. A build without a key never updates (Settings says so). `url`, `notes` and `pub_date`
  aren't signed: the signed sha256 pins the bytes, so a mirror can serve the file. A signed field
  with a line break in it is refused.
- `arch`: `aarch64`, `x86_64` or `universal` (default). `minimum_macos` defaults to `12.0`.
- Archive: `.tar.gz` (or `.zip`) containing `Midna.app` at the top level.

**Windows feed.** Windows reads its own file: `{channel}` becomes `windows-<channel>`, so the
`channels` release holds `windows-stable.json` and `windows-beta.json` beside macOS's
`stable.json` and `beta.json`. A Windows entry adds `os` and `minimum_windows` (`10.0.17763`,
Windows 10 1809, the first with ConPTY, when absent) and drops `minimum_macos`:

```json
{
  "version": "0.1.0-beta.58.w.1",
  "url": "https://github.com/slider9/midna/releases/download/windows-v0.1.0-beta.58.w.1/Midna-0.1.0-beta.58.w.1-windows-x86_64.zip",
  "sha256": "…", "size": 41943040, "pub_date": "…", "notes": "…",
  "os": "windows",
  "minimum_windows": "10.0.17763",
  "arch": "x86_64",
  "signature": "base64 ed25519 signature"
}
```

- Its `signature` covers a different message, `midna-update-v2`:
  `"midna-update-v2\nos=<os>\nversion=<version>\nsha256=<lowercase sha256>\narch=<arch>\nminimum_windows=<min>\n"`.
  An entry without `os` (or with `os: "macos"`) is a macOS entry and is checked with v1, so a
  signed macOS entry relabelled `windows` no longer verifies, and each app refuses an entry for
  another OS outright. macOS still signs and checks v1, byte for byte as before.
- `arch` names the CPU (`x86_64`); `universal` is a macOS bundle and a Windows app refuses it.
  The Windows version is read with `RtlGetVersion` and compared as major.minor.build.
- Archive: a `.zip` with the folder `packaging/build-windows.sh` makes (`Midna/`, holding
  `midna-bundle.txt`) at the top level. `make-update.sh --windows dist/<v>-windows/Midna` writes
  it and its entry.

**Several keys.** `MIDNA_UPDATE_PUBKEY` is a list: base64 keys separated by commas or
whitespace, and an entry signed by any one of them passes. That is how the release key moves
(say, from the fork's to upstream's): one bridging release embeds both keys and is signed with
the old one; releases after it are signed with the new key, and the old key is dropped from the
list once nobody runs a build older than the bridge. `midna-release verify` takes the same list,
and both release workflows check that `MIDNA_UPDATE_KEY`'s public half is one of
`MIDNA_UPDATE_PUBKEY`'s keys, so the bridging release only changes the variable.

**Client flow** (`updater.rs`). It runs about 5s after launch, then every 6h (Settings has
"Check now"):

1. Fetch the feed, verify the signature, and skip unless the version is newer by semver
   (pre-releases sort first; there are no downgrades). The arch and macOS version must fit.
2. Download to `MIDNA_HOME/updates/<v>/` while hashing; on a sha256 mismatch, drop it.
3. Extract with `tar` / `ditto`, then check that `Midna.app` has the same bundle id, has the
   feed's version, passes `codesign --verify --deep --strict`, and has the **same Team ID** as
   the running app (when that one is Developer ID signed).
4. State becomes **Ready**: the status bar shows "↻ Update ready (v) · restart to apply" and
   Settings ▸ Updates shows "Restart to apply". Quitting midna with an update ready also
   installs it (no relaunch).
5. **Apply**: the staged app is moved next to the installed one (a `ditto` copy if it's on
   another volume) and exchanged with `renamex_np(…, RENAME_SWAP)`. That's a single atomic
   swap, so `Midna.app` is always the complete old or the complete new bundle. Volumes without
   swap support fall back to two renames with rollback. Then the old bundle is deleted, a
   detached helper waits for this process to exit and runs `open Midna.app`, and the app quits.
6. The new app's launch path (above) installs its midnad into `bin/` and `daemon.upgrade`s the
   running daemon in place. Terminals keep running, with the same session ids, pids and screens
   (`crates/midnad/tests/upgrade.rs`, and the e2e test below).

Debug: `MIDNA_UPDATE_CHECK_SECS=N` changes the interval, and `MIDNA_DEBUG_UPDATE=apply`
applies as soon as an update is ready. The app logs to `MIDNA_HOME/app.log`, the daemon to
`MIDNA_HOME/midnad.log`.

## Keys

- **Dev/test**: `packaging/gen-update-key.sh` writes `~/.config/midna-dev/update-ed25519.key`
  (PKCS#8, mode 0600) and `.pub`. `build-app.sh` embeds that `.pub` when `MIDNA_UPDATE_PUBKEY`
  isn't set, and `make-update.sh` signs with that `.key` unless `--key` / `MIDNA_UPDATE_KEY`
  says otherwise.
- **Release**: generate a separate key the same way and keep the secret half off the dev
  machine (CI secret). Builds embed only the public half, so the release key and the dev key
  are not interchangeable. An app built with the dev key only accepts dev-signed updates.
- `*.key` is in `.gitignore`. `packaging/release-tool` (`midna-release keygen|pubkey|feed|verify`)
  is the only code that touches secret keys.
- **Push**: main only takes PRs (a GitHub ruleset; force-pushes and deletes are refused too).
  `release.sh` pushes its version commit with a deploy key the ruleset lets past,
  `~/.ssh/midna-release` (or `MIDNA_RELEASE_DEPLOY_KEY`). On a new machine, make one and add it
  with write access: `ssh-keygen -t ed25519 -N "" -f ~/.ssh/midna-release`, then
  `gh repo deploy-key add ~/.ssh/midna-release.pub --allow-write --title release`.

## End-to-end test

`packaging/e2e/update-e2e.sh` (about 6 min, most of it the two release builds; `--no-build`
reuses them, `--keep` skips deleting the installs):

- builds **test-flavored** v0.1.0 and v0.1.1: bundle id `com.mrgnhnt.midna.test`, label
  `com.mrgnhnt.midna.test.daemon`, and `MIDNA_HOME=/tmp/mdt-e2e` in both `LSEnvironment` and
  the LaunchAgent's `EnvironmentVariables`, so the real install and its TCC grants are never
  touched
- installs v0.1.0 to `~/Applications/MidnaTest/Midna.app` and launches it; SMAppService
  registers the test label
- opens a `zsh -f` session and runs a command in it
- publishes v0.1.1 with `make-update.sh` on `python3 -m http.server` (127.0.0.1)
- waits for the swap, the relaunch at 0.1.1, and the daemon re-exec at 0.1.1, then asserts the
  same daemon pid, the same session id and shell pid, the screen kept, and the shell still
  interactive
- cleans up: quits the app, runs `--uninstall` (daemon.stop + unregister), boots out the label
  as a fallback, deletes the install, the test home and the LaunchServices registration, and
  stops the server

## Checklist

- [ ] `CHANGELOG.md`: `## Unreleased` renamed to the version; `cargo test` green
- [ ] `tb release cut v<version>` (a beta first for anything risky)
- [ ] the Release workflow is green; the DMG opens without a Gatekeeper warning on another Mac
- [ ] an installed copy on that channel picks the update up (Settings › Updates › Check now)
