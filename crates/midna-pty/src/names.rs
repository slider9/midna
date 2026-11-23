//! Where a keeper is found: its control pipe's name and its record in `MIDNA_HOME\pty`.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The keeper's file name, installed beside midnad.
pub const EXE: &str = "midna-pty.exe";

/// What a client of a keeper must be: midnad, from `MIDNA_HOME\bin` or beside the keeper.
pub const CLIENT_EXE: &str = "midnad.exe";

/// A session id that can go in a pipe name and a file name.
pub fn valid_session(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A short, stable name for `home`, so keepers of two homes (Midna and Midna Dev, temp test
/// homes) never share a pipe name. Paths on Windows ignore case and either slash.
pub fn home_hash(home: &Path) -> String {
    let real = midna_proto::paths::canonical(home).unwrap_or_else(|_| home.to_path_buf());
    let s = real.to_string_lossy().replace('/', "\\").to_lowercase();
    let s = s.trim_end_matches('\\');
    // FNV-1a: no secret here, only a name; the pipe's own access rules keep others out.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// `\\.\pipe\midna-pty-<home hash>-<session id>`.
pub fn pipe_name(home: &Path, session: &str) -> String {
    format!(r"\\.\pipe\midna-pty-{}-{session}", home_hash(home))
}

pub fn pty_dir(home: &Path) -> PathBuf {
    home.join("pty")
}

/// `MIDNA_HOME\pty\<session id>.json`.
pub fn record_path(home: &Path, session: &str) -> PathBuf {
    pty_dir(home).join(format!("{session}.json"))
}

/// What a keeper writes about itself once its shell is running, and removes when it exits. One
/// left behind by a keeper that was killed names a pid that's gone (or reused by another
/// program), so check the pid's program before trusting it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub pid: u32,
    pub pipe: String,
    pub protocol: u32,
    pub build: String,
    pub shell_pid: u32,
    /// The keeper's executable, in the version folder it runs from.
    pub exe: String,
}

/// Every record in `MIDNA_HOME\pty`, with its session id.
pub fn records(home: &Path) -> Vec<(String, Record)> {
    let mut out = vec![];
    for e in std::fs::read_dir(pty_dir(home)).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(session) = name.strip_suffix(".json").filter(|s| valid_session(s)) else { continue };
        if let Some(r) = std::fs::read(e.path()).ok().and_then(|b| serde_json::from_slice::<Record>(&b).ok()) {
            out.push((session.to_string(), r));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_that_fit_a_pipe_and_a_file_name() {
        assert!(valid_session("s-12_ab"));
        for bad in ["", "a/b", r"a\b", "a b", "..", "a.json", &"x".repeat(65)] {
            assert!(!valid_session(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_home_hash_ignores_case_slashes_and_a_trailing_slash() {
        let a = home_hash(Path::new(r"C:\Nowhere\Midna-Home"));
        assert_eq!(a.len(), 16);
        assert_eq!(a, home_hash(Path::new("c:/nowhere/midna-home/")));
        assert_ne!(a, home_hash(Path::new(r"C:\Nowhere\Midna-Home-2")));
        let p = pipe_name(Path::new(r"C:\Nowhere\Midna-Home"), "s1");
        assert_eq!(p, format!(r"\\.\pipe\midna-pty-{a}-s1"));
    }

    #[test]
    fn records_are_read_from_the_pty_folder() {
        let home = std::env::temp_dir().join(format!("midna-pty-records-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        assert!(records(&home).is_empty());
        std::fs::create_dir_all(pty_dir(&home)).unwrap();
        let r = Record { pid: 1, pipe: "p".into(), protocol: 1, build: "b".into(), shell_pid: 2, exe: r"C:\x\midna-pty.exe".into() };
        std::fs::write(record_path(&home, "s2"), serde_json::to_vec(&r).unwrap()).unwrap();
        std::fs::write(record_path(&home, "s1"), serde_json::to_vec(&r).unwrap()).unwrap();
        std::fs::write(pty_dir(&home).join("broken.json"), "{").unwrap();
        std::fs::write(pty_dir(&home).join("keeper.log"), "log").unwrap();
        let got = records(&home);
        assert_eq!(got.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(), ["s1", "s2"]);
        assert_eq!(got[0].1, r);
        let _ = std::fs::remove_dir_all(&home);
    }
}
