//! midna-release: update-signing helper (docs/RELEASING.md).
//!
//!   midna-release keygen <secret.key>            write a PKCS#8 ed25519 key (0600), print the public key (base64)
//!   midna-release pubkey <secret.key>            print the public key (base64, raw 32 bytes)
//!   midna-release feed <secret.key> <archive> --version V --url URL [--notes TEXT]
//!       [--os macos|windows] [--min-macos 12.0] [--min-windows 10.0.17763] [--arch universal|x86_64|aarch64]
//!                                                print a signed feed entry (JSON); macOS entries are
//!                                                signed with midna-update-v1, Windows ones with v2
//!   midna-release verify <feed.json> <pubkeys> [archive]   check a feed entry (and the archive's sha256);
//!                                                <pubkeys> is one base64 key or several, comma- or space-separated
//!
//! The signed message must match `signed_message` in crates/midna-app/src/updater.rs.
use base64::Engine;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde_json::{Value, json};
use std::io::Read;

const DEFAULT_MIN_MACOS: &str = "12.0";
/// Windows 10 1809, the first with ConPTY.
const DEFAULT_MIN_WINDOWS: &str = "10.0.17763";

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn die(msg: &str) -> ! {
    eprintln!("midna-release: {msg}");
    std::process::exit(1);
}

/// The signed fields of a feed entry.
struct Signed {
    /// `macos` (signed with v1, which predates the field) or `windows` (v2).
    os: String,
    version: String,
    sha256: String,
    arch: String,
    min_macos: String,
    min_windows: String,
}

impl Signed {
    /// Read back from a feed entry, with the app's defaults for absent fields.
    fn from_entry(v: &Value) -> Signed {
        let s = |k: &str, d: &str| v.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
        let os = s("os", "macos");
        Signed {
            os: if os.is_empty() { "macos".into() } else { os },
            version: s("version", ""),
            sha256: s("sha256", ""),
            arch: s("arch", "universal"),
            min_macos: s("minimum_macos", DEFAULT_MIN_MACOS),
            min_windows: s("minimum_windows", DEFAULT_MIN_WINDOWS),
        }
    }

    /// The exact bytes the key signs. v1 (macOS) is frozen: every macOS build checks it.
    fn message(&self) -> String {
        let sha = self.sha256.to_ascii_lowercase();
        let Signed { os, version, arch, .. } = self;
        match os.as_str() {
            "macos" => format!("midna-update-v1\nversion={version}\nsha256={sha}\nminimum_macos={}\narch={arch}\n", self.min_macos),
            _ => format!("midna-update-v2\nos={os}\nversion={version}\nsha256={sha}\narch={arch}\nminimum_windows={}\n", self.min_windows),
        }
    }

    fn check(&self) -> Result<(), String> {
        if !matches!(self.os.as_str(), "macos" | "windows") {
            return Err(format!("--os is macos or windows, not {:?}", self.os));
        }
        if self.os == "windows" && self.arch == "universal" {
            return Err("a Windows build names its CPU (--arch x86_64), universal is a macOS bundle".into());
        }
        let fields = [&self.version, &self.sha256, &self.arch, &self.min_macos, &self.min_windows];
        if fields.iter().any(|f| f.is_empty() || f.contains(['\n', '\r'])) {
            return Err("a signed field is empty or has a line break".into());
        }
        Ok(())
    }
}

/// The feed entry for `s`, signed by `kp`. A macOS entry has exactly the fields it always had.
fn entry(kp: &Ed25519KeyPair, s: &Signed, url: &str, notes: &str, size: u64, pub_date: &str) -> Value {
    let sig = b64().encode(kp.sign(s.message().as_bytes()).as_ref());
    let mut e = json!({
        "version": s.version, "pub_date": pub_date, "notes": notes, "url": url,
        "sha256": s.sha256, "size": size,
    });
    let m = e.as_object_mut().unwrap();
    if s.os == "macos" {
        m.insert("minimum_macos".into(), json!(s.min_macos));
    } else {
        m.insert("os".into(), json!(s.os));
        m.insert("minimum_windows".into(), json!(s.min_windows));
    }
    m.insert("arch".into(), json!(s.arch));
    m.insert("signature".into(), json!(sig));
    e
}

/// Ok with the key that signed `v`, if any key in `keys` (comma- or space-separated) did.
fn verify(v: &Value, keys: &str) -> Result<String, String> {
    let s = Signed::from_entry(v);
    if [&s.version, &s.sha256, &s.os, &s.arch, &s.min_macos, &s.min_windows].iter().any(|f| f.contains(['\n', '\r'])) {
        return Err("a signed field has a line break".into());
    }
    let sig = b64().decode(v.get("signature").and_then(Value::as_str).unwrap_or("").trim()).map_err(|_| "signature isn't base64")?;
    let keys: Vec<&str> = keys.split(|c: char| c == ',' || c.is_whitespace()).filter(|k| !k.is_empty()).collect();
    if keys.is_empty() {
        return Err("no public key given".into());
    }
    let msg = s.message();
    for k in keys {
        let pk = b64().decode(k).map_err(|_| format!("public key {k} isn't base64"))?;
        if UnparsedPublicKey::new(&ED25519, pk).verify(msg.as_bytes(), &sig).is_ok() {
            return Ok(k.to_string());
        }
    }
    Err("signature does NOT verify".into())
}

fn load_key(path: &str) -> Ed25519KeyPair {
    let raw = std::fs::read_to_string(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    let der = b64().decode(raw.trim()).unwrap_or_else(|_| die("secret key isn't base64 PKCS#8"));
    Ed25519KeyPair::from_pkcs8(&der).unwrap_or_else(|_| die("secret key isn't an ed25519 PKCS#8 key"))
}

fn sha256_file(path: &str) -> (String, u64) {
    let mut f = std::fs::File::open(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 1 << 16];
    let mut n_total = 0u64;
    loop {
        let n = f.read(&mut buf).unwrap_or_else(|e| die(&e.to_string()));
        if n == 0 {
            break;
        }
        n_total += n as u64;
        ctx.update(&buf[..n]);
    }
    (ctx.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect(), n_total)
}

/// `secs` since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ` (days to civil date: Howard Hinnant's
/// algorithm), so the tool needs no `date` and runs the same on Windows.
fn rfc3339(secs: u64) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem / 60 % 60, rem % 60)
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn create_secret(path: &str) -> std::fs::File {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path).unwrap_or_else(|e| die(&e.to_string()))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("keygen") => {
            let path = args.get(1).unwrap_or_else(|| die("keygen <secret.key>"));
            if std::path::Path::new(path).exists() {
                die(&format!("{path} exists; refusing to overwrite a release key"));
            }
            let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap_or_else(|_| die("keygen failed"));
            let mut f = create_secret(path);
            std::io::Write::write_all(&mut f, format!("{}\n", b64().encode(pkcs8.as_ref())).as_bytes()).unwrap();
            let kp = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
            println!("{}", b64().encode(kp.public_key().as_ref()));
        }
        Some("pubkey") => {
            let kp = load_key(args.get(1).unwrap_or_else(|| die("pubkey <secret.key>")));
            println!("{}", b64().encode(kp.public_key().as_ref()));
        }
        Some("feed") => {
            let (key, archive) = match (args.get(1), args.get(2)) {
                (Some(k), Some(a)) => (k, a),
                _ => die("feed <secret.key> <archive> --version V --url URL"),
            };
            let version = flag(&args, "--version").unwrap_or_else(|| die("--version is required"));
            let url = flag(&args, "--url").unwrap_or_else(|| die("--url is required"));
            let notes = flag(&args, "--notes").unwrap_or_default();
            let os = flag(&args, "--os").unwrap_or_else(|| "macos".into());
            let default_arch = if os == "windows" { "x86_64" } else { "universal" };
            let kp = load_key(key);
            let (sha256, size) = sha256_file(archive);
            let s = Signed {
                arch: flag(&args, "--arch").unwrap_or_else(|| default_arch.into()),
                min_macos: flag(&args, "--min-macos").unwrap_or_else(|| DEFAULT_MIN_MACOS.into()),
                min_windows: flag(&args, "--min-windows").unwrap_or_else(|| DEFAULT_MIN_WINDOWS.into()),
                os,
                version,
                sha256,
            };
            s.check().unwrap_or_else(|e| die(&e));
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let e = entry(&kp, &s, &url, &notes, size, &rfc3339(now));
            println!("{}", serde_json::to_string_pretty(&e).unwrap());
        }
        Some("verify") => {
            let (feed, keys) = match (args.get(1), args.get(2)) {
                (Some(f), Some(k)) => (f, k),
                _ => die("verify <feed.json> <pubkeys> [archive]"),
            };
            let v: Value = serde_json::from_str(&std::fs::read_to_string(feed).unwrap_or_else(|e| die(&e.to_string()))).unwrap_or_else(|e| die(&e.to_string()));
            let by = verify(&v, keys).unwrap_or_else(|e| die(&e));
            let s = Signed::from_entry(&v);
            if let Some(a) = args.get(3) {
                let (sha, _) = sha256_file(a);
                if sha != s.sha256.to_ascii_lowercase() {
                    die("archive sha256 does NOT match the feed");
                }
            }
            println!("ok: {} for {} signed by {by}", s.version, s.os);
        }
        _ => die("usage: keygen | pubkey | feed | verify (see src/main.rs)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> (Ed25519KeyPair, String) {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
        let kp = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let public = b64().encode(kp.public_key().as_ref());
        (kp, public)
    }

    fn mac() -> Signed {
        Signed {
            os: "macos".into(),
            version: "0.1.1".into(),
            sha256: "AB".repeat(32),
            arch: "universal".into(),
            min_macos: DEFAULT_MIN_MACOS.into(),
            min_windows: DEFAULT_MIN_WINDOWS.into(),
        }
    }

    fn windows() -> Signed {
        Signed { os: "windows".into(), arch: "x86_64".into(), ..mac() }
    }

    /// The same bytes as the app's `updater::tests::v1_message_is_frozen` / `v2_message_…`.
    #[test]
    fn messages_match_the_app() {
        let sha = "ab".repeat(32);
        assert_eq!(mac().message(), format!("midna-update-v1\nversion=0.1.1\nsha256={sha}\nminimum_macos=12.0\narch=universal\n"));
        assert_eq!(windows().message(), format!("midna-update-v2\nos=windows\nversion=0.1.1\nsha256={sha}\narch=x86_64\nminimum_windows=10.0.17763\n"));
    }

    #[test]
    fn mac_entries_keep_their_fields() {
        let (kp, public) = key();
        let e = entry(&kp, &mac(), "https://x/Midna-0.1.1.app.tar.gz", "n", 7, "2026-11-23T00:00:00Z");
        let mut keys: Vec<&String> = e.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["arch", "minimum_macos", "notes", "pub_date", "sha256", "signature", "size", "url", "version"]);
        assert_eq!(verify(&e, &public), Ok(public.clone()));
        // The fields an old feed leaves out read back as the app's defaults.
        let mut bare = e.clone();
        bare.as_object_mut().unwrap().remove("minimum_macos");
        assert_eq!(verify(&bare, &public), Ok(public));
    }

    #[test]
    fn windows_entries_sign_os_and_minimum_windows() {
        let (kp, public) = key();
        let e = entry(&kp, &windows(), "https://x/Midna-0.1.1-windows-x86_64.zip", "", 7, "");
        assert_eq!((e["os"].as_str(), e["minimum_windows"].as_str(), e["arch"].as_str()), (Some("windows"), Some("10.0.17763"), Some("x86_64")));
        assert!(e.get("minimum_macos").is_none(), "{e}");
        assert_eq!(verify(&e, &public), Ok(public.clone()));
        for (k, v) in [("os", json!("macos")), ("minimum_windows", json!("10.0.10240")), ("arch", json!("aarch64")), ("version", json!("9.9.9"))] {
            let mut t = e.clone();
            t[k] = v;
            assert!(verify(&t, &public).is_err(), "{k}");
        }
        let mut no_os = e.clone();
        no_os.as_object_mut().unwrap().remove("os");
        assert!(verify(&no_os, &public).is_err(), "a Windows signature must not pass as macOS");
    }

    #[test]
    fn verify_takes_a_key_list() {
        let ((kp, new), (_, old)) = (key(), key());
        let e = entry(&kp, &windows(), "u", "", 1, "");
        assert_eq!(verify(&e, &format!("{old},{new}")), Ok(new.clone()));
        assert_eq!(verify(&e, &format!("{old} {new}\n")), Ok(new.clone()));
        assert!(verify(&e, &old).is_err());
        assert!(verify(&e, " ").is_err());
    }

    #[test]
    fn checks_what_it_signs() {
        assert!(mac().check().is_ok() && windows().check().is_ok());
        assert!(Signed { os: "linux".into(), ..windows() }.check().is_err());
        assert!(Signed { arch: "universal".into(), ..windows() }.check().is_err());
        assert!(Signed { version: "0.1.1\nx".into(), ..windows() }.check().is_err());
    }

    #[test]
    fn dates() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_795_464_689), "2026-11-23T20:11:29Z");
    }
}
