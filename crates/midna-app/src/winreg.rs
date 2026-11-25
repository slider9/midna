//! The few registry calls the app makes, all under `HKEY_CURRENT_USER`: the toast app id
//! (`notify_windows.rs`), and the login entry that starts midnad and the Settings ▸ Apps entry
//! (`nonmac/install.rs`), and the user's `Path` (`Environment`), which the install adds the CLI's
//! folder to.
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_DWORD, REG_EXPAND_SZ, REG_SZ, RRF_NOEXPAND, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW,
    RegDeleteTreeW, RegGetValueW, RegSetKeyValueW,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Set a string value (None: the key's default value), creating the key.
pub fn set_string(key: &str, value: Option<&str>, data: &str) -> std::io::Result<()> {
    set_text(key, value, data, REG_SZ)
}

/// Set a string value that holds `%NAME%`s for Windows to expand (`REG_EXPAND_SZ`, what the user's
/// `Path` is), creating the key.
pub fn set_expand_string(key: &str, value: &str, data: &str) -> std::io::Result<()> {
    set_text(key, Some(value), data, REG_EXPAND_SZ)
}

fn set_text(key: &str, value: Option<&str>, data: &str, kind: u32) -> std::io::Result<()> {
    let (k, v, d) = (wide(key), value.map(wide), wide(data));
    let v = v.as_ref().map_or(std::ptr::null(), |v| v.as_ptr());
    // SAFETY: NUL-terminated UTF-16 strings; a string's size counts the NUL, in bytes.
    let err = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), v, kind, d.as_ptr().cast(), (d.len() * 2) as u32) };
    if err == 0 { Ok(()) } else { Err(std::io::Error::from_raw_os_error(err as i32)) }
}

/// Set a DWORD value, creating the key.
pub fn set_dword(key: &str, value: &str, data: u32) -> std::io::Result<()> {
    let (k, v) = (wide(key), wide(value));
    // SAFETY: NUL-terminated UTF-16 strings and a 4-byte value.
    let err = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), v.as_ptr(), REG_DWORD, (&data as *const u32).cast(), 4) };
    if err == 0 { Ok(()) } else { Err(std::io::Error::from_raw_os_error(err as i32)) }
}

/// A DWORD value, None when it (or its key) isn't there.
pub fn get_dword(key: &str, value: &str) -> Option<u32> {
    get(key, value, RRF_RT_REG_DWORD).and_then(|b| Some(u32::from_le_bytes(b.get(..4)?.try_into().ok()?)))
}

/// A string value, None when it (or its key) isn't there.
pub fn get_string(key: &str, value: &str) -> Option<String> {
    get(key, value, RRF_RT_REG_SZ).map(|b| text(&b))
}

/// A string value as stored, `%NAME%`s unexpanded, and whether it's a `REG_EXPAND_SZ`.
/// `Ok(None)` only when it (or its key) isn't there; one that isn't a string, or can't be read,
/// is an error, so a caller rewriting it never takes it for empty.
pub fn get_raw_string(key: &str, value: &str) -> std::io::Result<Option<(String, bool)>> {
    let r = read(key, value, RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND)?;
    Ok(r.map(|(buf, kind)| (text(&buf), kind == REG_EXPAND_SZ)))
}

fn text(buf: &[u8]) -> String {
    let units: Vec<u16> = buf.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
    String::from_utf16_lossy(&units)
}

/// A binary value, None when it (or its key) isn't there.
pub fn get_binary(key: &str, value: &str) -> Option<Vec<u8>> {
    get(key, value, RRF_RT_REG_BINARY)
}

fn get(key: &str, value: &str, flags: u32) -> Option<Vec<u8>> {
    read(key, value, flags).ok().flatten().map(|(b, _)| b)
}

/// The value's bytes and its type (`REG_SZ`, …). `Ok(None)` only when it (or its key) isn't
/// there; any other failure, a type `flags` doesn't allow included, is an error.
fn read(key: &str, value: &str, flags: u32) -> std::io::Result<Option<(Vec<u8>, u32)>> {
    const ERROR_FILE_NOT_FOUND: u32 = 2;
    const ERROR_MORE_DATA: u32 = 234;
    let (k, v) = (wide(key), wide(value));
    let mut kind = 0u32;
    // SAFETY: a size query (no buffer), then reads into a buffer of the size it gave.
    let mut len = 0u32;
    let mut err = unsafe { RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), v.as_ptr(), flags, &mut kind, std::ptr::null_mut(), &mut len) };
    // The value can grow between the size query and the read: ask again with the new size.
    for _ in 0..4 {
        if err != 0 {
            break;
        }
        let mut buf = vec![0u8; len as usize];
        err = unsafe { RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), v.as_ptr(), flags, &mut kind, buf.as_mut_ptr().cast(), &mut len) };
        if err == 0 {
            buf.truncate(len as usize);
            return Ok(Some((buf, kind)));
        }
        if err == ERROR_MORE_DATA {
            err = 0;
        }
    }
    match err {
        ERROR_FILE_NOT_FOUND => Ok(None),
        0 => Err(std::io::Error::other("the value kept changing size while being read")),
        e => Err(std::io::Error::from_raw_os_error(e as i32)),
    }
}

/// Delete a value; one that isn't there is fine.
pub fn delete(key: &str, value: &str) -> std::io::Result<()> {
    let (k, v) = (wide(key), wide(value));
    // SAFETY: NUL-terminated UTF-16 strings.
    let err = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), v.as_ptr()) };
    // ERROR_FILE_NOT_FOUND: nothing to delete.
    if err == 0 || err == 2 { Ok(()) } else { Err(std::io::Error::from_raw_os_error(err as i32)) }
}

/// Delete a key with everything in it; one that isn't there is fine.
pub fn delete_key(key: &str) -> std::io::Result<()> {
    let k = wide(key);
    // SAFETY: a NUL-terminated UTF-16 string.
    let err = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, k.as_ptr()) };
    // ERROR_FILE_NOT_FOUND: nothing to delete.
    if err == 0 || err == 2 { Ok(()) } else { Err(std::io::Error::from_raw_os_error(err as i32)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip_under_a_scratch_key() {
        let key = format!("Software\\midna-test-{}", std::process::id());
        set_string(&key, Some("name"), "Midna Dev — ü").unwrap();
        assert_eq!(get_string(&key, "name").as_deref(), Some("Midna Dev — ü"));
        assert_eq!(get_binary(&key, "name"), None, "a string isn't binary");
        delete(&key, "name").unwrap();
        assert_eq!(get_string(&key, "name"), None);
        delete(&key, "name").unwrap();
        set_dword(&key, "n", 7).unwrap();
        assert_eq!(get_dword(&key, "n"), Some(7));
        assert_eq!(get_string(&key, "n"), None, "a DWORD isn't a string");
        delete_key(&key).unwrap();
        assert_eq!(get_dword(&key, "n"), None);
        delete_key(&key).unwrap();
    }

    #[test]
    fn a_raw_string_keeps_its_names_and_says_whether_it_expands() {
        let key = format!(r"Software\midna-test-raw-{}", std::process::id());
        set_expand_string(&key, "Path", r"%USERPROFILE%\bin;C:\x").unwrap();
        assert_eq!(get_raw_string(&key, "Path").unwrap(), Some((r"%USERPROFILE%\bin;C:\x".into(), true)));
        let profile = std::env::var("USERPROFILE").unwrap();
        assert_eq!(get_string(&key, "Path"), Some(format!(r"{profile}\bin;C:\x")), "get_string expands it");
        set_string(&key, Some("Plain"), "a;b").unwrap();
        assert_eq!(get_raw_string(&key, "Plain").unwrap(), Some(("a;b".into(), false)));
        set_dword(&key, "n", 1).unwrap();
        assert!(get_raw_string(&key, "n").is_err(), "a DWORD is an error, not a missing value");
        assert_eq!(get_raw_string(&key, "missing").unwrap(), None);
        assert_eq!(get_raw_string(r"Software\midna-test-no-such-key", "Path").unwrap(), None);
        delete_key(&key).unwrap();
    }
}
