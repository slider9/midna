//! Windows updates, after the feed, its signature and the download have been checked
//! (`updater.rs`, the code macOS runs):
//!
//! 1. [`stage`] unpacks the zip into `MIDNA_HOME\updates\<version>\staged` and checks the build:
//!    [`BUNDLE_MARKER`] is there and its `midnad.exe --selftest` reports the feed's version.
//! 2. To apply, [`start_apply`] starts the staged copy's `midna-app.exe --apply-update <install
//!    folder> --wait <pid>` and the app quits.
//! 3. That helper ([`apply_main`]) waits for the app to exit and swaps the new build in file by
//!    file ([`swap_in`]): each old file is renamed to `<name>.old` (Windows renames a file that is
//!    running or open, as long as it was opened to allow that), the new one moved in, the marker
//!    last; a failure renames everything back. Then it starts the app and exits. Swapping the
//!    whole folder would fail whenever any process had a file open or its working folder in it.
//!    A swap that went all the way lists the `.old` files it made in the install's
//!    `midna-update-leftovers.txt`.
//! 4. The new app ([`after_update`]) deletes the listed `.old` files (nothing else) and starts
//!    as usual, which brings the daemon onto its build (`lifecycle::ensure_daemon_current`).
//!
//! `--uninstall` (what Settings ▸ Apps runs) uses the same after-exit step to take the install
//! folder away: [`remove_after_exit`] runs a copy of the app from `%TEMP%` as
//! `--remove-install <folder> --wait <pid>` ([`remove_main`]).
use crate::install::BUNDLE_MARKER;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const APP_EXE: &str = "midna-app.exe";
/// A zip that unpacks to more than this is refused.
const MAX_UNPACKED: u64 = 2 << 30;
/// How long a helper waits for the app it replaces to exit.
const WAIT_FOR_APP: Duration = Duration::from_secs(60);
const SELFTEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Where a failed `--apply-update` leaves its error for the app it starts (`MIDNA_HOME\updates`).
const FAILED_FILE: &str = "apply-failed.txt";
/// In the install: the `.old` files a finished swap set aside, one path (relative to the
/// install) per line, for the next launch to delete. Only a swap that went all the way writes
/// it, so the only copy of a file a failed or interrupted swap couldn't put back is never
/// deleted, and neither is anything else that happens to end in `.old`.
const LEFTOVERS_FILE: &str = "midna-update-leftovers.txt";

// ------------------------------------------------------------------ stage

/// Unpack `archive` (the verified zip) into `dir` and check the build in it. Returns the build's
/// folder: `dir` itself, or the one folder in it that holds the marker (a zip of the `Midna`
/// folder).
pub fn stage(archive: &Path, dir: &Path, expect_version: &str, _installed: &Path) -> Result<PathBuf, String> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    unzip(archive, dir)?;
    let bundle = find_bundle(dir)?;
    for exe in [APP_EXE, "midnad.exe"] {
        if !bundle.join(exe).is_file() {
            return Err(format!("the update has no {exe}"));
        }
    }
    let v = selftest_version(&bundle.join("midnad.exe"))?;
    if v != expect_version {
        return Err(format!("the update says it is {v}, the feed said {expect_version}"));
    }
    Ok(bundle)
}

/// Extract every file of the zip at `archive` under `dir`. A name that would land outside `dir`,
/// a link, or more than [`MAX_UNPACKED`] in all is refused.
pub fn unzip(archive: &Path, dir: &Path) -> Result<(), String> {
    let f = std::fs::File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| format!("the update isn't a zip: {e}"))?;
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| format!("the update's zip: {e}"))?;
        let name = entry.name().map(|n| n.to_string()).unwrap_or_else(|_| format!("entry {i}"));
        // zip makes some of these safe by itself (`C:/x` becomes `x`); refuse them all instead.
        let odd = name.contains(':') || name.starts_with(['/', '\\']) || name.split(['/', '\\']).any(|c| c == "..");
        let rel = entry.enclosed_name().filter(|p| !odd && p.components().all(|c| matches!(c, std::path::Component::Normal(_))));
        let Some(rel) = rel else {
            return Err(format!("the update has a file outside its folder: {name}"));
        };
        if entry.is_symlink() {
            return Err(format!("the update has a link in it: {name}"));
        }
        let dest = dir.join(&rel);
        let io = |e: std::io::Error| format!("{}: {e}", dest.display());
        if entry.is_dir() {
            std::fs::create_dir_all(&dest).map_err(io)?;
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let mut out = std::fs::File::create(&dest).map_err(io)?;
        // Count what comes out, not what the zip says it holds.
        let n = std::io::copy(&mut (&mut entry).take(MAX_UNPACKED - total + 1), &mut out).map_err(io)?;
        total += n;
        if total > MAX_UNPACKED {
            return Err("the update unpacks to more than 2 GiB".into());
        }
    }
    Ok(())
}

/// The folder under `dir` that holds a build: `dir`, or its one subfolder with the marker.
fn find_bundle(dir: &Path) -> Result<PathBuf, String> {
    if dir.join(BUNDLE_MARKER).is_file() {
        return Ok(dir.to_path_buf());
    }
    let found: Vec<PathBuf> = std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).filter(|p| p.join(BUNDLE_MARKER).is_file()).collect();
    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!("the update has no {BUNDLE_MARKER}")),
        _ => Err("the update holds more than one build".into()),
    }
}

/// The version `midnad --selftest` reports (bounded by [`SELFTEST_TIMEOUT`]).
fn selftest_version(midnad: &Path) -> Result<String, String> {
    let mut cmd = Command::new(midnad);
    cmd.arg("--selftest").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    crate::install::detach(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("couldn't run the update's midnad: {e}"))?;
    let t0 = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if t0.elapsed() > SELFTEST_TIMEOUT => {
                let _ = child.kill();
                return Err(format!("the update's midnad --selftest didn't finish in {} s", SELFTEST_TIMEOUT.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("the update's midnad failed its selftest: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|_| "the update's midnad --selftest printed no JSON".to_string())?;
    v.get("version").and_then(|v| v.as_str()).map(str::to_string).ok_or_else(|| "the update's midnad --selftest reports no version".into())
}

// ------------------------------------------------------------------ apply

/// Start the staged build's helper, which swaps it into `install` once this process exits, and
/// then (`launch`) starts it. The caller quits right after.
pub fn start_apply(staged: &Path, install: &Path, launch: bool) -> Result<(), String> {
    let exe = staged.join(APP_EXE);
    let mut cmd = Command::new(&exe);
    cmd.arg("--apply-update").arg(install).arg("--wait").arg(std::process::id().to_string());
    if !launch {
        cmd.arg("--no-launch");
    }
    // Its working folder is outside the install, which it is about to change.
    cmd.current_dir(staged).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    crate::install::detach(&mut cmd);
    cmd.spawn().map(|_| ()).map_err(|e| format!("couldn't start {}: {e}", exe.display()))
}

/// `midna-app.exe --apply-update <install folder> --wait <pid> [--no-launch]`, run from the
/// staged build. Returns the exit code.
pub fn apply_main(args: &[String]) -> i32 {
    let (Some(install), Some(pid)) = (value_of(args, "--apply-update"), value_of(args, "--wait").and_then(|p| p.parse::<u32>().ok())) else {
        eprintln!("usage: midna-app --apply-update <install folder> --wait <pid> [--no-launch]");
        return 2;
    };
    let install = PathBuf::from(install);
    let launch = !args.iter().any(|a| a == "--no-launch");
    let own = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)).unwrap_or_default();
    let refuse = |why: String| {
        log(&format!("--apply-update: {why}"));
        record_failure(&why);
        2
    };
    if !own.join(BUNDLE_MARKER).is_file() {
        return refuse(format!("{} isn't a Midna build", own.display()));
    }
    if !install.join(BUNDLE_MARKER).is_file() {
        return refuse(format!("{} isn't an installed Midna", install.display()));
    }
    if midna_proto::paths::same_path(&own.to_string_lossy(), &install.to_string_lossy()) {
        return refuse("the update and the install are the same folder".into());
    }
    if !wait_for_exit(pid, WAIT_FOR_APP) {
        // Never swap files under a running app; it keeps the update ready.
        return refuse(format!("the app (pid {pid}) was still running after {} s", WAIT_FOR_APP.as_secs()));
    }
    let code = match swap_in(&own, &install) {
        Ok(()) => {
            log(&format!("--apply-update: installed {} in {}", midna_proto::VERSION, install.display()));
            0
        }
        Err(e) => {
            log(&format!("--apply-update: {e}"));
            record_failure(&e);
            1
        }
    };
    // Whichever build is there now: the new one, or the old one put back.
    if launch {
        let mut cmd = Command::new(install.join(APP_EXE));
        cmd.current_dir(&install).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        if let Err(e) = cmd.spawn() {
            log(&format!("--apply-update: couldn't start {}: {e}", install.join(APP_EXE).display()));
        }
    }
    code
}

/// Replace the build in `install` with the one in `new`, file by file, or leave it as it was.
///
/// Every new file is first copied next to its place as `<name>.new`, so the slow part (and a
/// full disk) fails before anything in the install has changed. Then, one file at a time, the
/// old file is renamed to `<name>.old` and the new one renamed in; the marker goes last, so a
/// folder with the marker always holds a whole build. If any rename fails, the files already
/// swapped are renamed back. Files the new build doesn't have are left alone. Once every file is
/// in, the `.old` files are listed in [`LEFTOVERS_FILE`] for [`after_update`] to delete.
pub fn swap_in(new: &Path, install: &Path) -> Result<(), String> {
    let files = files_of(new)?;
    let mut copies = vec![];
    let drop_copies = |copies: &[PathBuf]| {
        for c in copies {
            let _ = std::fs::remove_file(c);
        }
    };
    for rel in &files {
        let to = install.join(rel);
        let copy = suffixed(&to, ".new");
        let r = (|| {
            midna_proto::paths::guard_write(&to)?;
            if let Some(dir) = to.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let _ = std::fs::remove_file(&copy);
            std::fs::copy(new.join(rel), &copy).map(|_| ())
        })();
        if let Err(e) = r {
            let _ = std::fs::remove_file(&copy);
            drop_copies(&copies);
            return Err(format!("couldn't write {}: {e}", copy.display()));
        }
        copies.push(copy);
    }
    let mut swapped: Vec<(PathBuf, Option<PathBuf>)> = vec![];
    for rel in &files {
        let to = install.join(rel);
        match swap_one(&to) {
            Ok(old) => swapped.push((to, old)),
            Err(e) => {
                let stuck = roll_back(&swapped);
                drop_copies(&copies);
                let mut msg = format!("couldn't replace {} ({e}); the old build is back", to.display());
                if !stuck.is_empty() {
                    msg = format!("couldn't replace {} ({e}), and couldn't put back {}", to.display(), stuck.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "));
                }
                return Err(msg);
            }
        }
    }
    let olds: Vec<PathBuf> = swapped.into_iter().filter_map(|(_, old)| old).collect();
    if let Err(e) = note_leftovers(install, &olds) {
        // The update is in; the .old files just stay until something lists them.
        log(&format!("--apply-update: couldn't list the old files in {}: {e}", install.join(LEFTOVERS_FILE).display()));
    }
    Ok(())
}

/// Add `olds` to the install's [`LEFTOVERS_FILE`] (written aside, then renamed in).
fn note_leftovers(install: &Path, olds: &[PathBuf]) -> std::io::Result<()> {
    let list = install.join(LEFTOVERS_FILE);
    let mut lines: Vec<String> = std::fs::read_to_string(&list).unwrap_or_default().lines().map(str::to_string).filter(|l| !l.is_empty()).collect();
    for old in olds {
        if let Ok(rel) = old.strip_prefix(install) {
            lines.push(rel.to_string_lossy().to_string());
        }
    }
    write_list(&list, &lines)
}

/// Write `lines` to `list`, or remove it when there are none.
fn write_list(list: &Path, lines: &[String]) -> std::io::Result<()> {
    if lines.is_empty() {
        return match std::fs::remove_file(list) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let tmp = suffixed(list, ".tmp");
    std::fs::write(&tmp, lines.join("\n") + "\n")?;
    retry(|| std::fs::rename(&tmp, list))
}

/// `to.new` in place of `to`, with the old `to` (if any) moved aside. Returns where it went.
fn swap_one(to: &Path) -> std::io::Result<Option<PathBuf>> {
    let copy = suffixed(to, ".new");
    let old = match to.symlink_metadata() {
        Ok(_) => {
            let old = free_old_name(to);
            retry(|| std::fs::rename(to, &old))?;
            Some(old)
        }
        Err(_) => None,
    };
    if let Err(e) = retry(|| std::fs::rename(&copy, to)) {
        if let Some(old) = &old {
            let _ = retry(|| std::fs::rename(old, to));
        }
        return Err(e);
    }
    Ok(old)
}

/// Undo [`swap_one`]s, newest first. Returns the files that couldn't be put back.
fn roll_back(swapped: &[(PathBuf, Option<PathBuf>)]) -> Vec<PathBuf> {
    let mut stuck = vec![];
    for (to, old) in swapped.iter().rev() {
        let r = match old {
            Some(old) => retry(|| std::fs::rename(old, to)),
            None => retry(|| std::fs::remove_file(to)),
        };
        if r.is_err() {
            stuck.push(to.clone());
        }
    }
    stuck
}

/// The first of `<name>.old`, `<name>.1.old`, `<name>.2.old`… that isn't there. One left by an
/// earlier update is never deleted here: it may still be running, or be the only copy of a file
/// a failed swap couldn't put back. [`after_update`] deletes the ones a finished swap listed.
fn free_old_name(to: &Path) -> PathBuf {
    let free = |p: &PathBuf| p.symlink_metadata().is_err();
    let plain = suffixed(to, ".old");
    if free(&plain) {
        return plain;
    }
    (1..).map(|n| suffixed(to, &format!(".{n}.old"))).find(free).unwrap_or(plain)
}

/// The files under `dir`, relative to it, the marker last and the app just before it.
fn files_of(dir: &Path) -> Result<Vec<PathBuf>, String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            let t = e.file_type()?;
            if t.is_dir() {
                walk(root, &e.path(), out)?;
            } else if t.is_file() {
                out.push(e.path().strip_prefix(root).unwrap_or(&e.path()).to_path_buf());
            }
        }
        Ok(())
    }
    let mut files = vec![];
    walk(dir, dir, &mut files).map_err(|e| format!("{}: {e}", dir.display()))?;
    let rank = |p: &PathBuf| match p.to_str() {
        Some(BUNDLE_MARKER) => 2,
        Some(APP_EXE) => 1,
        _ => 0,
    };
    files.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
    if files.last().is_none_or(|f| f.as_os_str() != BUNDLE_MARKER) {
        return Err(format!("{} has no {BUNDLE_MARKER}", dir.display()));
    }
    Ok(files)
}

/// What a finished update leaves for the new app (run at launch, from the install): delete the
/// `.old` files its swap listed in [`LEFTOVERS_FILE`], and the staged builds in
/// `MIDNA_HOME\updates` that are no newer than this one. Returns the error of an
/// `--apply-update` that failed, for the app to show.
pub fn after_update(install: &Path, home: &Path) -> Option<String> {
    if install.join(BUNDLE_MARKER).is_file() {
        let removed = remove_leftovers(install);
        if removed > 0 {
            log(&format!("removed {removed} file(s) the last update left in {}", install.display()));
        }
    }
    let updates = home.join("updates");
    for e in std::fs::read_dir(&updates).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if e.path().is_dir() && semver::Version::parse(&name).is_ok() && !crate::updater::is_newer(&name, midna_proto::VERSION) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
    let failed = updates.join(FAILED_FILE);
    let error = std::fs::read_to_string(&failed).ok()?;
    let _ = std::fs::remove_file(&failed);
    Some(error.trim().to_string()).filter(|e| !e.is_empty())
}

/// Delete the `.old` files listed in `install`'s [`LEFTOVERS_FILE`]; the ones still in use stay
/// listed for next time. A line that isn't a `.old` path inside the install is dropped unread.
fn remove_leftovers(install: &Path) -> usize {
    let list = install.join(LEFTOVERS_FILE);
    let Ok(text) = std::fs::read_to_string(&list) else { return 0 };
    let mut n = 0;
    let mut keep = vec![];
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let rel = Path::new(line);
        let inside = rel.components().all(|c| matches!(c, std::path::Component::Normal(_)));
        if !inside || !line.ends_with(".old") {
            continue;
        }
        match std::fs::remove_file(install.join(rel)) {
            Ok(()) => n += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => keep.push(line.to_string()),
        }
    }
    let _ = write_list(&list, &keep);
    n
}

/// The file `name` is a copy an update makes of: `<base>.new`, `<base>.old` or `<base>.<n>.old`.
fn leftover_of(name: &str) -> Option<&str> {
    if let Some(base) = name.strip_suffix(".new") {
        return Some(base);
    }
    let rest = name.strip_suffix(".old")?;
    match rest.rsplit_once('.') {
        Some((base, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => Some(base),
        _ => Some(rest),
    }
}

/// One of the build's own top-level files ([`BUILD_FILES`], the marker), or an update's copy of one.
fn is_build_file(name: &str) -> bool {
    let own = |n: &str| BUILD_FILES.contains(&n) || n == BUNDLE_MARKER;
    own(name) || leftover_of(name).is_some_and(own)
}

/// A font or license file in `Fonts`, or an update's copy of one.
fn is_font_file(name: &str) -> bool {
    let own = |n: &str| Path::new(n).extension().is_some_and(|x| ["ttf", "otf", "txt"].iter().any(|e| x.eq_ignore_ascii_case(e)));
    own(name) || leftover_of(name).is_some_and(own)
}

fn record_failure(error: &str) {
    let dir = midna_proto::paths::midna_home().join("updates");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(FAILED_FILE), error);
}

// ------------------------------------------------------------------ uninstall

/// What a Midna build's folder holds (`packaging/build-windows.sh`), besides the marker and the
/// `Fonts` folder. Only these are removed, so a build unpacked among other files takes only its
/// own away.
const BUILD_FILES: [&str; 7] = [APP_EXE, "midnad.exe", "midna.exe", "midna-pty.exe", "conpty.dll", "OpenConsole.exe", "ConPTY-LICENSE.txt"];

/// After `--uninstall` exits, take the build in `install` away: a copy of this executable in
/// `%TEMP%` waits for this process and removes it (an executable can't delete the folder it runs
/// from). Returns the copy's folder.
pub fn remove_after_exit(install: &Path) -> Result<PathBuf, String> {
    let tmp = std::env::temp_dir().join(format!("midna-uninstall-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let exe = tmp.join(APP_EXE);
    std::fs::copy(std::env::current_exe().map_err(|e| e.to_string())?, &exe).map_err(|e| format!("{}: {e}", exe.display()))?;
    let mut cmd = Command::new(&exe);
    cmd.arg("--remove-install").arg(install).arg("--wait").arg(std::process::id().to_string());
    cmd.current_dir(&tmp).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    crate::install::detach(&mut cmd);
    cmd.spawn().map_err(|e| format!("couldn't start {}: {e}", exe.display()))?;
    Ok(tmp)
}

/// `midna-app.exe --remove-install <install folder> --wait <pid>`, run from a copy in `%TEMP%`.
pub fn remove_main(args: &[String]) -> i32 {
    let (Some(install), Some(pid)) = (value_of(args, "--remove-install"), value_of(args, "--wait").and_then(|p| p.parse::<u32>().ok())) else {
        eprintln!("usage: midna-app --remove-install <install folder> --wait <pid>");
        return 2;
    };
    let install = PathBuf::from(install);
    wait_for_exit(pid, WAIT_FOR_APP);
    let code = match remove_install(&install) {
        Ok(left) if left.is_empty() => {
            log(&format!("uninstall: removed {}", install.display()));
            0
        }
        Ok(left) => {
            log(&format!("uninstall: removed Midna from {}; left what isn't Midna's: {}", install.display(), left.join(", ")));
            0
        }
        Err(e) => {
            log(&format!("uninstall: {e}"));
            1
        }
    };
    // This copy's own folder goes once it has exited.
    if let Some(own) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf))
        && own.file_name().is_some_and(|n| n.to_string_lossy().starts_with("midna-uninstall-"))
    {
        delete_after_exit(&own);
    }
    code
}

/// Remove the build in `dir` ([`BUILD_FILES`], `Fonts`, an update's copies of those and its
/// [`LEFTOVERS_FILE`], the marker last) and `dir` itself once it's empty. Refuses a folder without the marker. Returns the names of
/// what was left because it isn't Midna's.
pub fn remove_install(dir: &Path) -> Result<Vec<String>, String> {
    if !dir.join(BUNDLE_MARKER).is_file() {
        return Err(format!("{} isn't an installed Midna (no {BUNDLE_MARKER})", dir.display()));
    }
    midna_proto::paths::guard_write(dir).map_err(|e| e.to_string())?;
    let mut failed = vec![];
    let mut left = vec![];
    let mut remove = |p: &Path| {
        if retry_long(|| std::fs::remove_file(p)).is_err() {
            failed.push(p.display().to_string());
        }
    };
    for e in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten() {
        let (p, name) = (e.path(), e.file_name().to_string_lossy().to_string());
        let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
        if !is_dir && name != BUNDLE_MARKER && (is_build_file(&name) || name == LEFTOVERS_FILE) {
            remove(&p);
        } else if is_dir && name == "Fonts" {
            for f in std::fs::read_dir(&p).into_iter().flatten().flatten() {
                let fp = f.path();
                if f.file_type().is_ok_and(|t| t.is_file()) && is_font_file(&f.file_name().to_string_lossy()) {
                    remove(&fp);
                } else {
                    left.push(format!("Fonts\\{}", f.file_name().to_string_lossy()));
                }
            }
            let _ = std::fs::remove_dir(&p);
        } else if name != BUNDLE_MARKER {
            left.push(name);
        }
    }
    if !failed.is_empty() {
        return Err(format!("couldn't remove {} (in use?)", failed.join(", ")));
    }
    retry_long(|| std::fs::remove_file(dir.join(BUNDLE_MARKER))).map_err(|e| format!("{}: {e}", dir.join(BUNDLE_MARKER).display()))?;
    if left.is_empty() {
        retry_long(|| std::fs::remove_dir(dir)).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    Ok(left)
}

/// Delete `dir` once this process has exited.
fn delete_after_exit(dir: &Path) {
    let script = format!(
        "Wait-Process -Id {} -Timeout 30 -ErrorAction SilentlyContinue; Remove-Item -LiteralPath '{}' -Recurse -Force -ErrorAction SilentlyContinue",
        std::process::id(),
        dir.display().to_string().replace('\'', "''")
    );
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &script]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    crate::install::detach(&mut cmd);
    let _ = cmd.spawn();
}

// ------------------------------------------------------------------ helpers

fn log(msg: &str) {
    crate::lifecycle::log(msg);
}

fn value_of<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str)
}

/// `p` with `suffix` added to its file name (`midna-app.exe` -> `midna-app.exe.old`).
fn suffixed(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// A rename or delete can fail for a moment while a virus scanner or the indexer has the file
/// open: try a few times.
fn retry(mut op: impl FnMut() -> std::io::Result<()>) -> std::io::Result<()> {
    retry_for(&mut op, 5, Duration::from_millis(100))
}

/// For files of a process that has only just exited.
fn retry_long(mut op: impl FnMut() -> std::io::Result<()>) -> std::io::Result<()> {
    retry_for(&mut op, 20, Duration::from_millis(250))
}

fn retry_for(op: &mut impl FnMut() -> std::io::Result<()>, tries: u32, gap: Duration) -> std::io::Result<()> {
    let mut last = op();
    for _ in 1..tries {
        if last.as_ref().is_ok() || last.as_ref().is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
            break;
        }
        std::thread::sleep(gap);
        last = op();
    }
    last
}

/// Wait up to `timeout` for process `pid` to exit. True once it has (or there is no such
/// process).
fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
    // SAFETY: a handle we open, wait on and close.
    unsafe {
        let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if h.is_null() {
            return true;
        }
        let r = WaitForSingleObject(h, timeout.as_millis().min(u32::MAX as u128 - 1) as u32);
        CloseHandle(h);
        r == WAIT_OBJECT_0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("midna-update-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A build in `dir`: `files` as (relative path, contents), plus the marker.
    fn build(dir: &Path, files: &[(&str, &str)], marker: &str) {
        for (rel, text) in files {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        std::fs::write(dir.join(BUNDLE_MARKER), marker).unwrap();
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        v.sort();
        v
    }

    /// Open `p` the way a program that forbids renaming it does (no FILE_SHARE_DELETE).
    fn hold(p: &Path) -> std::fs::File {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().read(true).share_mode(1 /* FILE_SHARE_READ */).open(p).unwrap()
    }

    const OLD: [(&str, &str); 3] = [("midna-app.exe", "app 1"), ("midnad.exe", "daemon 1"), ("Fonts/a.ttf", "font 1")];
    const NEW: [(&str, &str); 4] = [("midna-app.exe", "app 2"), ("midnad.exe", "daemon 2"), ("Fonts/a.ttf", "font 2"), ("midna-pty.exe", "keeper 2")];

    #[test]
    fn the_marker_goes_last_and_the_app_just_before_it() {
        let d = scratch("order");
        build(&d, &[("zz.dll", ""), ("midna-app.exe", ""), ("Fonts/a.ttf", ""), ("a.exe", "")], "");
        let files = files_of(&d).unwrap();
        let n = files.len();
        assert_eq!((files[n - 2].to_str(), files[n - 1].to_str()), (Some(APP_EXE), Some(BUNDLE_MARKER)));
        std::fs::remove_file(d.join(BUNDLE_MARKER)).unwrap();
        assert!(files_of(&d).unwrap_err().contains("no midna-bundle.txt"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn apply_swaps_every_file_and_the_next_launch_clears_the_old_ones() {
        let d = scratch("apply");
        let (new, install, home) = (d.join("staged"), d.join("Midna"), d.join("home"));
        build(&new, &NEW, "v2");
        build(&install, &OLD, "v1");
        // The user's own files, some named like an update's copies.
        for mine in ["notes.txt", "notes.old", "draft.new", "Fonts/mine.ttf.old"] {
            std::fs::write(install.join(mine), "mine").unwrap();
        }
        swap_in(&new, &install).unwrap();
        for (rel, text) in NEW {
            assert_eq!(read(&install.join(rel)), text, "{rel}");
        }
        assert_eq!(read(&install.join(BUNDLE_MARKER)), "v2");
        assert_eq!(read(&install.join("midna-app.exe.old")), "app 1");
        assert_eq!(read(&install.join("Fonts/a.ttf.old")), "font 1");
        assert!(!install.join("midna-pty.exe.old").exists(), "a file new in this build has nothing to set aside");
        // The staged build stays whole (its midna-app.exe is the helper doing this).
        assert_eq!(read(&new.join("midna-app.exe")), "app 2");
        assert_eq!(read(&install.join(LEFTOVERS_FILE)), "Fonts\\a.ttf.old\nmidnad.exe.old\nmidna-app.exe.old\nmidna-bundle.txt.old\n");
        // Next launch: the .old files the swap listed go, and the list with them; every other
        // file stays, whatever it's called.
        assert_eq!(after_update(&install, &home), None);
        assert_eq!(names(&install), ["Fonts", "draft.new", "midna-app.exe", "midna-bundle.txt", "midna-pty.exe", "midnad.exe", "notes.old", "notes.txt"]);
        assert_eq!(names(&install.join("Fonts")), ["a.ttf", "mine.ttf.old"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_running_exe_is_renamed_aside() {
        // A running exe can be renamed but not deleted, even with its working folder in the
        // install: the swap still goes through.
        let d = scratch("running");
        let (new, install) = (d.join("staged"), d.join("Midna"));
        build(&new, &NEW, "v2");
        build(&install, &OLD, "v1");
        let exe = install.join("midnad.exe");
        std::fs::copy(r"C:\Windows\System32\PING.EXE", &exe).unwrap();
        let mut running = Command::new(&exe).args(["-n", "60", "127.0.0.1"]).current_dir(&install).stdout(Stdio::null()).spawn().unwrap();
        swap_in(&new, &install).unwrap();
        assert_eq!(read(&exe), "daemon 2");
        // Still running: its leftover can't go yet, and the next update sets its own aside
        // under another name.
        assert!(std::fs::remove_file(install.join("midnad.exe.old")).is_err());
        assert_eq!(remove_leftovers(&install), 3, "every .old but midnad.exe's");
        assert!(install.join("midnad.exe.old").exists());
        build(&new, &[("midnad.exe", "daemon 3")], "v3");
        swap_in(&new, &install).unwrap();
        assert_eq!(read(&exe), "daemon 3");
        assert_eq!(read(&install.join("midnad.exe.1.old")), "daemon 2");
        let _ = running.kill();
        let _ = running.wait();
        remove_leftovers(&install);
        assert_eq!(names(&install), ["Fonts", "midna-app.exe", "midna-bundle.txt", "midna-pty.exe", "midnad.exe"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_file_held_open_rolls_everything_back() {
        let d = scratch("held");
        let (new, install) = (d.join("staged"), d.join("Midna"));
        build(&new, &NEW, "v2");
        build(&install, &OLD, "v1");
        // Swapped in order: Fonts/a.ttf, midna-pty.exe and midnad.exe, then midna-app.exe, then the
        // marker. midna-app.exe is held, so the three before it are put back.
        let held = hold(&install.join("midna-app.exe"));
        let err = swap_in(&new, &install).unwrap_err();
        assert!(err.contains("midna-app.exe") && err.contains("the old build is back"), "{err}");
        drop(held);
        for (rel, text) in OLD {
            assert_eq!(read(&install.join(rel)), text, "{rel}");
        }
        assert_eq!(read(&install.join(BUNDLE_MARKER)), "v1");
        assert_eq!(names(&install), ["Fonts", "midna-app.exe", "midna-bundle.txt", "midnad.exe"], "no .new, .old or new-only files left");
        assert_eq!(names(&install.join("Fonts")), ["a.ttf"]);
        // Once it's let go, the same update goes through.
        swap_in(&new, &install).unwrap();
        assert_eq!(read(&install.join("midna-app.exe")), "app 2");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_swap_that_didnt_finish_leaves_its_old_files_alone() {
        // A helper killed half way: midnad.exe is new, its .old is the only copy of the old one,
        // and nothing listed it. The next launch, and the next update, keep it.
        let d = scratch("interrupted");
        let (new, install, home) = (d.join("staged"), d.join("Midna"), d.join("home"));
        build(&install, &[("midna-app.exe", "app 1"), ("midnad.exe", "daemon 2"), ("midnad.exe.old", "daemon 1")], "v1");
        assert_eq!(after_update(&install, &home), None);
        assert_eq!(read(&install.join("midnad.exe.old")), "daemon 1");
        build(&new, &NEW, "v2");
        swap_in(&new, &install).unwrap();
        assert_eq!(read(&install.join("midnad.exe.old")), "daemon 1");
        assert_eq!(read(&install.join("midnad.exe.1.old")), "daemon 2");
        after_update(&install, &home);
        assert_eq!(read(&install.join("midnad.exe.old")), "daemon 1", "only what the finished swap listed goes");
        assert!(!install.join("midnad.exe.1.old").exists() && !install.join(LEFTOVERS_FILE).exists());
        // A list naming something outside the install, or not a .old file, deletes nothing.
        std::fs::write(d.join("outside.old"), "x").unwrap();
        std::fs::write(install.join(LEFTOVERS_FILE), "..\\outside.old\nmidnad.exe\nC:\\x.old\n").unwrap();
        assert_eq!(remove_leftovers(&install), 0);
        assert!(d.join("outside.old").exists() && install.join("midnad.exe").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_copy_that_fails_changes_nothing() {
        let d = scratch("copy");
        let (new, install) = (d.join("staged"), d.join("Midna"));
        build(&new, &NEW, "v2");
        build(&install, &OLD, "v1");
        // A folder where a new file must go: its copy can't be written.
        std::fs::create_dir_all(install.join("midna-pty.exe.new").join("x")).unwrap();
        let err = swap_in(&new, &install).unwrap_err();
        assert!(err.contains("midna-pty.exe.new"), "{err}");
        for (rel, text) in OLD {
            assert_eq!(read(&install.join(rel)), text, "{rel}");
        }
        assert!(!install.join("midnad.exe.new").exists() && !install.join("Fonts/a.ttf.new").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    fn zip_of(path: &Path, entries: &[(&str, &[u8])]) {
        let mut w = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            if name.ends_with('/') {
                w.add_directory(*name, opts).unwrap();
            } else {
                w.start_file(*name, opts).unwrap();
                w.write_all(data).unwrap();
            }
        }
        w.finish().unwrap();
    }

    #[test]
    fn unzip_finds_the_build_in_its_folder_and_refuses_paths_outside() {
        let d = scratch("unzip");
        let z = d.join("Midna.zip");
        zip_of(&z, &[("Midna/", b""), ("Midna/midna-app.exe", b"app"), ("Midna/Fonts/a.ttf", b"font"), ("Midna/midna-bundle.txt", b"m")]);
        let out = d.join("staged");
        std::fs::create_dir_all(&out).unwrap();
        unzip(&z, &out).unwrap();
        assert_eq!(find_bundle(&out).unwrap(), out.join("Midna"));
        assert_eq!(read(&out.join("Midna/Fonts/a.ttf")), "font");
        // A zip of the folder's contents works too.
        let flat = d.join("flat");
        std::fs::create_dir_all(&flat).unwrap();
        zip_of(&z, &[("midna-app.exe", b"app"), ("midna-bundle.txt", b"m")]);
        unzip(&z, &flat).unwrap();
        assert_eq!(find_bundle(&flat).unwrap(), flat);
        for bad in ["../evil.exe", "Midna/../../evil.exe", "C:/evil.exe", "/evil.exe", "\\\\server\\share\\evil.exe"] {
            let into = d.join("bad");
            let _ = std::fs::remove_dir_all(&into);
            std::fs::create_dir_all(&into).unwrap();
            zip_of(&z, &[(bad, b"x")]);
            assert!(unzip(&z, &into).unwrap_err().contains("outside its folder"), "{bad}");
            assert!(!d.join("evil.exe").exists() && !Path::new("C:/evil.exe").exists());
        }
        let none = d.join("none");
        std::fs::create_dir_all(&none).unwrap();
        assert!(find_bundle(&none).unwrap_err().contains("no midna-bundle.txt"));
        std::fs::write(&z, b"not a zip").unwrap();
        assert!(unzip(&z, &none).unwrap_err().contains("isn't a zip"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn stage_checks_the_marker_and_the_daemons_version() {
        let d = scratch("stage");
        let z = d.join("Midna.zip");
        zip_of(&z, &[("Midna/midna-app.exe", b"app"), ("Midna/midna-bundle.txt", b"m")]);
        assert!(stage(&z, &d.join("s1"), "9.9.9", &d).unwrap_err().contains("no midnad.exe"));
        zip_of(&z, &[("Midna/midna-app.exe", b"app"), ("Midna/midnad.exe", b"daemon")]);
        assert!(stage(&z, &d.join("s2"), "9.9.9", &d).unwrap_err().contains("no midna-bundle.txt"));
        // A midnad that isn't one: it can't run.
        zip_of(&z, &[("Midna/midna-app.exe", b"app"), ("Midna/midnad.exe", b"MZ not really"), ("Midna/midna-bundle.txt", b"m")]);
        assert!(stage(&z, &d.join("s3"), "9.9.9", &d).is_err());
        // The real midnad from this workspace reports its version.
        let midnad = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("midnad.exe");
        if midnad.is_file() {
            let bin = std::fs::read(&midnad).unwrap();
            zip_of(&z, &[("Midna/midna-app.exe", b"app"), ("Midna/midnad.exe", &bin), ("Midna/midna-bundle.txt", b"m")]);
            let err = stage(&z, &d.join("s4"), "9.9.9", &d).unwrap_err();
            assert_eq!(err, format!("the update says it is {}, the feed said 9.9.9", midna_proto::VERSION));
            let staged = stage(&z, &d.join("s5"), midna_proto::VERSION, &d).unwrap();
            assert_eq!(staged, d.join("s5").join("Midna"));
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn after_update_reports_a_failed_apply_once_and_clears_older_staged_builds() {
        let d = scratch("after");
        let (install, home) = (d.join("Midna"), d.join("home"));
        build(&install, &OLD, "v1");
        let updates = home.join("updates");
        for v in ["0.0.1", midna_proto::VERSION, "999.0.0"] {
            std::fs::create_dir_all(updates.join(v).join("staged")).unwrap();
        }
        std::fs::write(updates.join(FAILED_FILE), "couldn't replace x\n").unwrap();
        assert_eq!(after_update(&install, &home).as_deref(), Some("couldn't replace x"));
        assert_eq!(after_update(&install, &home), None);
        assert_eq!(names(&updates), ["999.0.0"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn uninstall_removes_the_build_and_only_the_build() {
        let d = scratch("remove");
        let install = d.join("Midna");
        build(&install, &[("midna-app.exe", ""), ("midnad.exe", ""), ("conpty.dll", ""), ("Fonts/a.ttf", ""), ("Fonts/OFL.txt", ""), ("Fonts/a.ttf.old", ""), ("midna.exe.old", ""), ("midnad.exe.2.old", ""), ("midna-app.exe.new", ""), ("midna-bundle.txt.old", ""), (LEFTOVERS_FILE, "")], "");
        assert_eq!(remove_install(&install), Ok(vec![]));
        assert!(!install.exists());
        // Unpacked among someone's files: only Midna's go, and the folder stays.
        let downloads = d.join("Downloads");
        build(&downloads, &[("midna-app.exe", ""), ("midnad.exe", ""), ("report.pdf", ""), ("report.old", ""), ("tool.exe.new", ""), ("Fonts/a.ttf", ""), ("Fonts/mine.psd", ""), ("Fonts/mine.psd.old", "")], "");
        let left = remove_install(&downloads).unwrap();
        assert_eq!(left, ["Fonts\\mine.psd", "Fonts\\mine.psd.old", "report.old", "report.pdf", "tool.exe.new"]);
        assert_eq!(names(&downloads), ["Fonts", "report.old", "report.pdf", "tool.exe.new"]);
        assert_eq!(names(&downloads.join("Fonts")), ["mine.psd", "mine.psd.old"]);
        // Never a folder without the marker.
        assert!(remove_install(&downloads).unwrap_err().contains("isn't an installed Midna"));
        assert_eq!(names(&downloads), ["Fonts", "report.old", "report.pdf", "tool.exe.new"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn waiting_for_a_process_ends_when_it_exits() {
        let mut child = Command::new("cmd.exe").args(["/c", "ping -n 2 127.0.0.1 >nul"]).stdout(Stdio::null()).spawn().unwrap();
        assert!(!wait_for_exit(child.id(), Duration::from_millis(50)));
        assert!(wait_for_exit(child.id(), Duration::from_secs(10)));
        let _ = child.wait();
        assert!(wait_for_exit(u32::MAX - 3, Duration::from_millis(10)), "no such process");
    }
}
