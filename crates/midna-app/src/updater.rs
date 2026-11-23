//! Auto-update: feed, verification, download, staging and the atomic app swap.
//! The feed format is documented in docs/RELEASING.md; `packaging/make-update.sh` writes it.
//!
//! ```json
//! { "version": "0.1.1", "pub_date": "2026-10-03T21:00:00Z", "notes": "…",
//!   "url": "https://…/Midna-0.1.1.app.tar.gz", "sha256": "<hex>", "size": 12345678,
//!   "minimum_macos": "12.0", "arch": "aarch64", "signature": "<base64 ed25519>" }
//! ```
//!
//! A Windows entry also carries `"os": "windows"` and `"minimum_windows": "10.0.17763"`, and
//! lives in its own feed file (`windows-<channel>.json`, see [`feed_url`]).
//!
//! The ed25519 signature covers [`signed_message`]: `midna-update-v1` (version, sha256,
//! minimum macOS, arch) for a macOS entry, `midna-update-v2` (os, version, sha256, arch,
//! minimum Windows) for any other, so a feed can't pair a signed archive with a different
//! version and a macOS entry can never pass as a Windows one. The archive is checked against
//! `sha256` before anything is extracted. The public keys are embedded at build time
//! (`MIDNA_UPDATE_PUBKEY`: one or more base64 raw 32-byte keys, separated by commas or
//! whitespace; any one of them may sign); a build without one never updates.
//!
//! The swap is ours (not cargo-packager-updater's, which isn't atomic): the new bundle is
//! staged next to the installed one and exchanged with `renamex_np(RENAME_SWAP)`, so
//! `Midna.app` is always either the complete old or the complete new bundle.
//!
//! Windows has no such swap for a folder: once the app has quit, the staged build's own
//! `midna-app.exe --apply-update` replaces the install file by file, with rollback
//! (`update_windows.rs`).
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::process::Command;

/// The update keys this build trusts, as a list (see [`keys`]). Several keys let one release
/// accept both an old and a new signer, so the release key can move without stranding anyone.
/// Midna Dev never updates itself, whatever key it was built with.
pub const PUBKEY: Option<&str> = match midna_proto::paths::DEV_HOME {
    Some(_) => None,
    None => option_env!("MIDNA_UPDATE_PUBKEY"),
};
pub const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);
/// Windows 10 1809, the first with ConPTY.
pub const DEFAULT_MIN_WINDOWS: &str = "10.0.17763";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FeedEntry {
    pub version: String,
    #[serde(default)]
    pub pub_date: String,
    #[serde(default)]
    pub notes: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub size: Option<u64>,
    /// `windows` for a Windows build. Empty (or `macos`) is a macOS build: its entries predate
    /// the field and are signed with v1, which doesn't cover it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub os: String,
    #[serde(default = "default_min_macos")]
    pub minimum_macos: String,
    /// The oldest Windows a Windows build runs on ([`DEFAULT_MIN_WINDOWS`] when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_windows: Option<String>,
    /// `aarch64`, `x86_64` or `universal` (default; macOS only).
    #[serde(default = "default_arch")]
    pub arch: String,
    pub signature: String,
}

fn default_min_macos() -> String {
    "12.0".into()
}
fn default_arch() -> String {
    "universal".into()
}

impl FeedEntry {
    /// The OS this entry's build is for: `macos` when the entry doesn't say.
    pub fn os(&self) -> &str {
        if self.os.is_empty() { "macos" } else { &self.os }
    }

    pub fn minimum_windows(&self) -> &str {
        self.minimum_windows.as_deref().unwrap_or(DEFAULT_MIN_WINDOWS)
    }
}

/// The exact bytes the release key signs. Keep in sync with `packaging/release-tool`.
/// v1 (macOS) is frozen: every macOS build since the first checks it.
pub fn signed_message(e: &FeedEntry) -> String {
    let sha = e.sha256.to_ascii_lowercase();
    match e.os() {
        "macos" => format!("midna-update-v1\nversion={}\nsha256={sha}\nminimum_macos={}\narch={}\n", e.version, e.minimum_macos, e.arch),
        os => format!("midna-update-v2\nos={os}\nversion={}\nsha256={sha}\narch={}\nminimum_windows={}\n", e.version, e.arch, e.minimum_windows()),
    }
}

/// The feed for `channel` on this OS: `{channel}` in `template` becomes `<channel>` on macOS
/// and `windows-<channel>` on Windows, so each platform reads only its own file.
pub fn feed_url(template: &str, channel: &str) -> String {
    feed_url_for(template, channel, std::env::consts::OS)
}

fn feed_url_for(template: &str, channel: &str, os: &str) -> String {
    match os {
        "macos" => template.replace("{channel}", channel),
        os => template.replace("{channel}", &format!("{os}-{channel}")),
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(15))
        .timeout_read(std::time::Duration::from_secs(60))
        .user_agent(&format!("midna/{}", midna_proto::VERSION))
        .build()
}

pub fn fetch_feed(url: &str) -> Result<FeedEntry, String> {
    let resp = agent().get(url).call().map_err(|e| format!("feed {url}: {e}"))?;
    let mut body = String::new();
    resp.into_reader().take(1 << 20).read_to_string(&mut body).map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| format!("feed {url}: {e}"))
}

/// The keys in a list like `MIDNA_UPDATE_PUBKEY`: separated by commas or whitespace.
pub fn keys(list: &str) -> impl Iterator<Item = &str> {
    list.split(|c: char| c == ',' || c.is_whitespace()).filter(|k| !k.is_empty())
}

/// Ok when any key in `pubkeys` (a [`keys`] list) signed `e`.
pub fn verify_signature(e: &FeedEntry, pubkeys: &str) -> Result<(), String> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let keys: Vec<Vec<u8>> = keys(pubkeys)
        .map(|k| b64.decode(k).ok().filter(|k| k.len() == 32).ok_or("an embedded update key isn't a base64 ed25519 key"))
        .collect::<Result<_, _>>()?;
    if keys.is_empty() {
        return Err("this build has no update key".into());
    }
    // One field per line: a line break inside one could shift the others.
    let signed = [&e.version, &e.sha256, &e.os, &e.minimum_macos, e.minimum_windows(), &e.arch];
    if signed.iter().any(|f| f.contains(['\n', '\r'])) {
        return Err("the update feed has a line break in a signed field".into());
    }
    let sig = b64.decode(e.signature.trim()).map_err(|_| "feed signature isn't base64")?;
    let msg = signed_message(e);
    if keys.iter().any(|k| ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, k).verify(msg.as_bytes(), &sig).is_ok()) {
        Ok(())
    } else {
        Err("update signature doesn't verify with midna's key".to_string())
    }
}

/// Strictly newer, by semver (pre-releases sort before their release).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (semver::Version::parse(candidate), semver::Version::parse(current)) {
        (Ok(a), Ok(b)) => a > b,
        _ => false,
    }
}

fn version_parts(v: &str) -> [u32; 3] {
    let mut it = v.split('.').map(|p| p.parse().unwrap_or(0));
    [it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0)]
}

/// What an entry must fit: this OS, its version and the CPU.
struct Host {
    os: &'static str,
    version: [u32; 3],
    arch: &'static str,
}

fn this_host() -> Host {
    #[cfg(windows)]
    let version = windows_version();
    #[cfg(not(windows))]
    let version = {
        let (major, minor) = crate::install::macos_version();
        [major, minor, 0]
    };
    Host { os: std::env::consts::OS, version, arch: std::env::consts::ARCH }
}

/// The real Windows version (`RtlGetVersion`, which, unlike `GetVersionEx`, ignores the app's
/// manifest), or zeros if it can't be read.
#[cfg(windows)]
fn windows_version() -> [u32; 3] {
    #[repr(C)]
    struct OsVersionInfoW {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        service_pack: [u16; 128],
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfoW) -> i32;
    }
    let mut info = OsVersionInfoW { size: size_of::<OsVersionInfoW>() as u32, major: 0, minor: 0, build: 0, platform: 0, service_pack: [0; 128] };
    // SAFETY: `info` is an OSVERSIONINFOW with its size field set.
    if unsafe { RtlGetVersion(&mut info) } == 0 { [info.major, info.minor, info.build] } else { [0; 3] }
}

/// `universal` is a macOS bundle with both slices; elsewhere the arch must match exactly.
fn arch_fits(arch: &str, host: &Host) -> bool {
    (arch == "universal" && host.os == "macos") || arch == host.arch
}

fn os_name(os: &str) -> &str {
    match os {
        "macos" => "macOS",
        "windows" => "Windows",
        other => other,
    }
}

/// Why `e` can't be installed here (signature, OS, version, OS version, arch), or Ok.
pub fn check_entry(e: &FeedEntry, current: &str, pubkeys: &str) -> Result<bool, String> {
    check_entry_on(e, current, pubkeys, &this_host())
}

fn check_entry_on(e: &FeedEntry, current: &str, pubkeys: &str, host: &Host) -> Result<bool, String> {
    verify_signature(e, pubkeys)?;
    if e.os() != host.os {
        return Err(format!("the update feed offers midna {} for {}, not {}", e.version, os_name(e.os()), os_name(host.os)));
    }
    if !is_newer(&e.version, current) {
        return Ok(false);
    }
    if !arch_fits(&e.arch, host) {
        return Err(format!("midna {} is built for {}, this {} is {}", e.version, e.arch, midna_proto::wording::MAC, host.arch));
    }
    let (need, min) = match e.os() {
        // v1 builds only ever compared major.minor.
        "macos" => {
            let [major, minor, _] = version_parts(&e.minimum_macos);
            ([major, minor, 0], e.minimum_macos.as_str())
        }
        _ => (version_parts(e.minimum_windows()), e.minimum_windows()),
    };
    if host.version < need {
        return Err(format!("midna {} needs {} {min}", e.version, os_name(e.os())));
    }
    Ok(true)
}

/// Download to `dir/<file>` while hashing; fails unless the sha256 matches the (signed) feed.
pub fn download(e: &FeedEntry, dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    let name = e.url.rsplit('/').next().filter(|n| !n.is_empty() && !n.contains("..")).unwrap_or("Midna.app.tar.gz");
    let dest = dir.join(name);
    let part = dir.join(format!("{name}.part"));
    let resp = agent().get(&e.url).call().map_err(|err| format!("download {}: {err}", e.url))?;
    let mut r = resp.into_reader();
    let mut f = std::fs::File::create(&part).map_err(|err| err.to_string())?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0u64;
    loop {
        let n = r.read(&mut buf).map_err(|err| format!("download: {err}"))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > 2 << 30 {
            return Err("download: larger than 2 GiB".into());
        }
        ctx.update(&buf[..n]);
        std::io::Write::write_all(&mut f, &buf[..n]).map_err(|err| err.to_string())?;
    }
    drop(f);
    let got: String = ctx.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect();
    if got != e.sha256.to_ascii_lowercase() {
        let _ = std::fs::remove_file(&part);
        return Err(format!("download: sha256 mismatch (got {got})"));
    }
    std::fs::rename(&part, &dest).map_err(|err| err.to_string())?;
    Ok(dest)
}

/// Windows: unpack the zip and check the build in it (`update_windows.rs`).
#[cfg(windows)]
pub use crate::update_windows::stage;

/// Extract `archive` (`.tar.gz` / `.tgz` / `.zip`) into `dir` and validate the `Midna.app` in
/// it: bundle id, version, a valid code signature, and the same signing team as this app.
#[cfg(not(windows))]
pub fn stage(archive: &Path, dir: &Path, expect_version: &str, installed: &Path) -> Result<PathBuf, String> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let name = archive.to_string_lossy();
    let st = if name.ends_with(".zip") {
        Command::new("/usr/bin/ditto").args(["-x", "-k"]).arg(archive).arg(dir).status()
    } else {
        Command::new("/usr/bin/tar").arg("-xzf").arg(archive).arg("-C").arg(dir).status()
    };
    if !st.is_ok_and(|s| s.success()) {
        return Err("couldn't extract the update".into());
    }
    let app = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "app"))
        .ok_or("the update has no .app inside")?;
    let info = crate::install::read_plist(&app.join("Contents/Info.plist")).ok_or("the update has no Info.plist")?;
    let mine = crate::install::read_plist(&installed.join("Contents/Info.plist")).unwrap_or_default();
    let id = |v: &serde_json::Value| v.get("CFBundleIdentifier").and_then(|s| s.as_str()).unwrap_or("").to_string();
    if id(&info) != id(&mine) {
        return Err(format!("the update is {}, not {}", id(&info), id(&mine)));
    }
    let v = info.get("CFBundleShortVersionString").and_then(|s| s.as_str()).unwrap_or("");
    if v != expect_version {
        return Err(format!("the update says it is {v}, the feed said {expect_version}"));
    }
    let o = Command::new("/usr/bin/codesign").args(["--verify", "--deep", "--strict"]).arg(&app).output().map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err(format!("the update's code signature is invalid: {}", String::from_utf8_lossy(&o.stderr).trim()));
    }
    let (want, got) = (team_id(installed), team_id(&app));
    if want.is_some() && want != got {
        return Err(format!("the update is signed by {:?}, this app by {:?}", got, want));
    }
    Ok(app)
}

/// The Developer ID team of a signed bundle (`None` for ad-hoc / unsigned).
#[cfg(not(windows))]
pub fn team_id(app: &Path) -> Option<String> {
    let o = Command::new("/usr/bin/codesign").args(["-dv", "--verbose=2"]).arg(app).output().ok()?;
    let text = String::from_utf8_lossy(&o.stderr).to_string();
    text.lines().find_map(|l| l.strip_prefix("TeamIdentifier=")).filter(|t| *t != "not set").map(str::to_string)
}

/// Swap `staged` into `installed` atomically. The staged copy is moved next to `installed`
/// first (same volume; a copy if it was elsewhere), then exchanged with `renamex_np`
/// `RENAME_SWAP`; the old bundle ends up at the staging path and is deleted.
#[cfg(not(windows))]
pub fn apply(staged: &Path, installed: &Path) -> Result<(), String> {
    let parent = installed.parent().ok_or("app has no parent directory")?;
    let side = parent.join(format!(".{}.update-{}", installed.file_name().unwrap_or_default().to_string_lossy(), std::process::id()));
    let _ = std::fs::remove_dir_all(&side);
    if std::fs::rename(staged, &side).is_err() {
        // different volume: copy, preserving signatures/xattrs
        let st = Command::new("/usr/bin/ditto").arg(staged).arg(&side).status().map_err(|e| e.to_string())?;
        if !st.success() {
            let _ = std::fs::remove_dir_all(&side);
            return Err(format!("can't write next to {} (is the folder writable?)", installed.display()));
        }
    }
    swap(&side, installed).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&side);
    })?;
    // `side` now holds the old bundle.
    let _ = std::fs::remove_dir_all(&side);
    Ok(())
}

/// Off macOS there's no bundle to swap (Windows applies with `update_windows::start_apply`).
#[cfg(not(any(target_os = "macos", windows)))]
fn swap(_a: &Path, _b: &Path) -> Result<(), String> {
    Err("updates aren't available on this platform yet".into())
}

#[cfg(target_os = "macos")]
fn swap(a: &Path, b: &Path) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let ca = std::ffi::CString::new(a.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let cb = std::ffi::CString::new(b.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    if unsafe { libc::renamex_np(ca.as_ptr(), cb.as_ptr(), libc::RENAME_SWAP) } == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    // Volumes without RENAME_SWAP (non-APFS): two renames with rollback. Not atomic, but the
    // window is one rename long and a failure restores the old bundle.
    if matches!(err.raw_os_error(), Some(libc::ENOTSUP) | Some(libc::EINVAL)) {
        let old = b.with_extension("app.old");
        let _ = std::fs::remove_dir_all(&old);
        std::fs::rename(b, &old).map_err(|e| e.to_string())?;
        if let Err(e) = std::fs::rename(a, b) {
            let _ = std::fs::rename(&old, b);
            return Err(e.to_string());
        }
        return std::fs::rename(&old, a).map_err(|e| e.to_string());
    }
    Err(format!("swap failed: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> FeedEntry {
        FeedEntry {
            version: "0.1.1".into(),
            pub_date: String::new(),
            notes: "n".into(),
            url: "http://x/Midna.app.tar.gz".into(),
            sha256: "AB".repeat(32),
            size: None,
            os: String::new(),
            minimum_macos: "12.0".into(),
            minimum_windows: None,
            arch: "universal".into(),
            signature: String::new(),
        }
    }

    fn windows_entry() -> FeedEntry {
        FeedEntry {
            url: "http://x/Midna-0.1.1-windows-x86_64.zip".into(),
            os: "windows".into(),
            minimum_windows: Some(DEFAULT_MIN_WINDOWS.into()),
            arch: "x86_64".into(),
            ..entry()
        }
    }

    struct Key {
        pair: ring::signature::Ed25519KeyPair,
        public: String,
    }

    fn key() -> Key {
        use ring::signature::KeyPair;
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let public = base64::engine::general_purpose::STANDARD.encode(pair.public_key().as_ref());
        Key { pair, public }
    }

    fn signed(mut e: FeedEntry, k: &Key) -> FeedEntry {
        e.signature = base64::engine::general_purpose::STANDARD.encode(k.pair.sign(signed_message(&e).as_bytes()).as_ref());
        e
    }

    const MAC: Host = Host { os: "macos", version: [14, 5, 0], arch: "aarch64" };
    const PC: Host = Host { os: "windows", version: [10, 0, 19045], arch: "x86_64" };

    /// Every macOS build ever shipped checks these bytes; they must never change.
    #[test]
    fn v1_message_is_frozen() {
        let e = FeedEntry { sha256: "AB".repeat(32), ..entry() };
        let want = format!("midna-update-v1\nversion=0.1.1\nsha256={}\nminimum_macos=12.0\narch=universal\n", "ab".repeat(32));
        assert_eq!(signed_message(&e), want);
        // An explicit `os: macos` is still a v1 entry.
        assert_eq!(signed_message(&FeedEntry { os: "macos".into(), ..e }), want);
    }

    #[test]
    fn v2_message_covers_os_and_minimum_windows() {
        let want = format!("midna-update-v2\nos=windows\nversion=0.1.1\nsha256={}\narch=x86_64\nminimum_windows=10.0.17763\n", "ab".repeat(32));
        assert_eq!(signed_message(&windows_entry()), want);
        // Absent, minimum_windows is the default, in the message too.
        assert_eq!(signed_message(&FeedEntry { minimum_windows: None, ..windows_entry() }), want);
    }

    #[test]
    fn mac_entries_check_on_a_mac_only() {
        let k = key();
        let e = signed(entry(), &k);
        assert_eq!(check_entry_on(&e, "0.1.0", &k.public, &MAC), Ok(true));
        assert_eq!(check_entry_on(&e, "0.1.1", &k.public, &MAC), Ok(false));
        let err = check_entry_on(&e, "0.1.0", &k.public, &PC).unwrap_err();
        assert!(err.contains("for macOS, not Windows"), "{err}");
        // Relabelling a signed macOS entry as Windows breaks its signature (v1 -> v2 bytes).
        let relabelled = FeedEntry { os: "windows".into(), arch: "x86_64".into(), ..e.clone() };
        assert!(verify_signature(&relabelled, &k.public).is_err());
        let old_mac = Host { version: [11, 7, 0], ..MAC };
        assert!(check_entry_on(&e, "0.1.0", &k.public, &old_mac).unwrap_err().contains("needs macOS 12.0"));
        // v1 compares major.minor only, as every macOS build before it did.
        let patch = signed(FeedEntry { minimum_macos: "14.5.3".into(), ..entry() }, &k);
        assert_eq!(check_entry_on(&patch, "0.1.0", &k.public, &MAC), Ok(true));
    }

    #[test]
    fn windows_entries_check_on_windows_only() {
        let k = key();
        let e = signed(windows_entry(), &k);
        assert_eq!(check_entry_on(&e, "0.1.0", &k.public, &PC), Ok(true));
        assert_eq!(check_entry_on(&e, "0.1.1", &k.public, &PC), Ok(false));
        let err = check_entry_on(&e, "0.1.0", &k.public, &MAC).unwrap_err();
        assert!(err.contains("for Windows, not macOS"), "{err}");
        // os and minimum_windows are signed.
        for tampered in [
            FeedEntry { os: String::new(), ..e.clone() },
            FeedEntry { os: "linux".into(), ..e.clone() },
            FeedEntry { minimum_windows: Some("10.0.10240".into()), ..e.clone() },
            FeedEntry { arch: "aarch64".into(), ..e.clone() },
            FeedEntry { version: "9.9.9".into(), ..e.clone() },
            FeedEntry { sha256: "cd".repeat(32), ..e.clone() },
        ] {
            assert!(verify_signature(&tampered, &k.public).is_err(), "{tampered:?}");
        }
        // 1803 has no ConPTY.
        let old_pc = Host { version: [10, 0, 17134], ..PC };
        assert!(check_entry_on(&e, "0.1.0", &k.public, &old_pc).unwrap_err().contains("needs Windows 10.0.17763"));
        assert_eq!(check_entry_on(&e, "0.1.0", &k.public, &Host { version: [10, 0, 17763], ..PC }), Ok(true));
        // `universal` is a macOS-only idea; a Windows entry names its CPU.
        let universal = signed(FeedEntry { arch: "universal".into(), ..windows_entry() }, &k);
        assert!(check_entry_on(&universal, "0.1.0", &k.public, &PC).unwrap_err().contains("built for universal"));
        let arm = Host { arch: "aarch64", ..PC };
        assert!(check_entry_on(&e, "0.1.0", &k.public, &arm).unwrap_err().contains("built for x86_64"));
    }

    #[test]
    fn any_listed_key_may_sign() {
        let (old, new, stranger) = (key(), key(), key());
        let e = signed(windows_entry(), &new);
        for list in [format!("{},{}", old.public, new.public), format!("{}\n{}", old.public, new.public), format!(" {} , {} ", new.public, old.public)] {
            assert_eq!(verify_signature(&e, &list), Ok(()), "{list:?}");
        }
        assert!(verify_signature(&e, &old.public).is_err());
        assert!(verify_signature(&signed(windows_entry(), &stranger), &format!("{},{}", old.public, new.public)).is_err());
        assert_eq!(verify_signature(&e, " , "), Err("this build has no update key".into()));
        // One bad key spoils the list rather than being skipped quietly.
        assert!(verify_signature(&e, &format!("{},not-a-key", new.public)).unwrap_err().contains("isn't a base64 ed25519 key"));
        assert!(verify_signature(&e, &format!("{},AAAA", new.public)).is_err());
    }

    #[test]
    fn line_breaks_in_signed_fields_are_refused() {
        let k = key();
        let e = signed(FeedEntry { version: "0.1.1\nsha256=00".into(), ..windows_entry() }, &k);
        assert!(verify_signature(&e, &k.public).unwrap_err().contains("line break"));
        let e = signed(FeedEntry { minimum_windows: Some("10.0.17763\r".into()), ..windows_entry() }, &k);
        assert!(verify_signature(&e, &k.public).unwrap_err().contains("line break"));
    }

    #[test]
    fn feed_json() {
        // A macOS entry from before v2 still parses, and writes back without the new fields.
        let mac = r#"{"version":"0.1.1","url":"u","sha256":"ab","minimum_macos":"12.0","arch":"aarch64","signature":"s"}"#;
        let e: FeedEntry = serde_json::from_str(mac).unwrap();
        assert_eq!((e.os(), e.minimum_windows.as_deref()), ("macos", None));
        let back = serde_json::to_value(&e).unwrap();
        assert!(back.get("os").is_none() && back.get("minimum_windows").is_none(), "{back}");
        let win = r#"{"version":"0.1.1","url":"u","sha256":"ab","os":"windows","minimum_windows":"10.0.17763","arch":"x86_64","signature":"s"}"#;
        let e: FeedEntry = serde_json::from_str(win).unwrap();
        assert_eq!((e.os(), e.minimum_windows(), e.arch.as_str()), ("windows", "10.0.17763", "x86_64"));
    }

    #[test]
    fn each_platform_reads_its_own_feed() {
        let t = "https://h/releases/download/channels/{channel}.json";
        assert_eq!(feed_url_for(t, "stable", "macos"), "https://h/releases/download/channels/stable.json");
        assert_eq!(feed_url_for(t, "beta", "macos"), "https://h/releases/download/channels/beta.json");
        assert_eq!(feed_url_for(t, "stable", "windows"), "https://h/releases/download/channels/windows-stable.json");
        assert_eq!(feed_url_for(t, "beta", "windows"), "https://h/releases/download/channels/windows-beta.json");
        assert_eq!(feed_url_for("http://127.0.0.1:8791/feed.json", "beta", "windows"), "http://127.0.0.1:8791/feed.json");
    }

    #[cfg(windows)]
    #[test]
    fn reads_the_real_windows_version() {
        let [major, _, build] = windows_version();
        assert!(major >= 10 && build >= 10240, "{:?}", windows_version());
    }

    /// The feed is for macOS builds (`check_entry` checks the macOS version).
    #[cfg(target_os = "macos")]
    #[test]
    fn signature_roundtrip_and_tamper() {
        use ring::signature::KeyPair;
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let kp = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD;
        let pubkey = b64.encode(kp.public_key().as_ref());
        let mut e = entry();
        e.signature = b64.encode(kp.sign(signed_message(&e).as_bytes()).as_ref());
        assert!(verify_signature(&e, &pubkey).is_ok());
        assert_eq!(check_entry(&e, "0.1.0", &pubkey), Ok(true));
        assert_eq!(check_entry(&e, "0.1.1", &pubkey), Ok(false));
        let mut bumped = e.clone();
        bumped.version = "9.9.9".into();
        assert!(verify_signature(&bumped, &pubkey).is_err());
        let mut other = e.clone();
        other.sha256 = "cd".repeat(32);
        assert!(verify_signature(&other, &pubkey).is_err());
        // the url and notes aren't signed (the sha256 pins the bytes)
        let mut moved = e.clone();
        moved.url = "https://mirror/x.tar.gz".into();
        assert!(verify_signature(&moved, &pubkey).is_ok());
    }

    #[test]
    fn versions() {
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("0.2.0-beta.1", "0.1.9"));
        assert!(!is_newer("0.2.0-beta.1", "0.2.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("garbage", "0.1.0"));
        assert_eq!(feed_url_for("https://h/{channel}.json", "beta", "macos"), "https://h/beta.json");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn swap_exchanges_directories() {
        let tmp = std::env::temp_dir().join(format!("midna-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let (a, b) = (tmp.join("staged/Midna.app"), tmp.join("Apps/Midna.app"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("v"), "new").unwrap();
        std::fs::write(b.join("v"), "old").unwrap();
        apply(&a, &b).unwrap();
        assert_eq!(std::fs::read_to_string(b.join("v")).unwrap(), "new");
        assert!(!a.exists());
        let leftovers: Vec<_> = std::fs::read_dir(tmp.join("Apps")).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(leftovers.len(), 1, "{leftovers:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
