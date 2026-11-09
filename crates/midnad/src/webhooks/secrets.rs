//! Secret values: webhook signing secrets and the human's pasted secrets (the vault).
//! Human-only to set; never returned over RPC (except `secret.exec_env`) or logged.
//!
//! - Keychain (default): generic password, service `com.mrgnhnt.midna.webhook` (account =
//!   trigger id) or `com.mrgnhnt.midna.secret` (account = secret id).
//! - File (`MIDNA_SECRETS=file`, used by tests): `$MIDNA_HOME/secrets/<trigger id>` or
//!   `$MIDNA_HOME/vault/<secret id>`, mode 0600.
//!
//! Secrets are cached in memory after the first read so a delivery doesn't hit the Keychain
//! for every candidate trigger.
//!
//! Windows: the "keychain" is the Windows Credential Manager, a generic credential per secret
//! named `<service>/<id>` (Control Panel ▸ Credential Manager ▸ Windows Credentials).
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

#[cfg(target_os = "macos")]
mod keychain {
    pub fn set(service: &str, id: &str, secret: &[u8]) -> Result<(), String> {
        security_framework::passwords::set_generic_password(service, id, secret).map_err(|e| format!("keychain: {e}"))
    }
    pub fn get(service: &str, id: &str) -> Option<Vec<u8>> {
        security_framework::passwords::get_generic_password(service, id).ok()
    }
    pub fn delete(service: &str, id: &str) {
        let _ = security_framework::passwords::delete_generic_password(service, id);
    }
}

#[cfg(windows)]
mod keychain {
    use windows_sys::Win32::Security::Credentials::{CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW};

    fn target(service: &str, id: &str) -> Vec<u16> {
        format!("{service}/{id}").encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn set(service: &str, id: &str, secret: &[u8]) -> Result<(), String> {
        let mut name = target(service, id);
        let mut user: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
        let mut c: CREDENTIALW = unsafe { std::mem::zeroed() };
        c.Type = CRED_TYPE_GENERIC;
        c.TargetName = name.as_mut_ptr();
        c.UserName = user.as_mut_ptr();
        c.CredentialBlobSize = secret.len() as u32;
        c.CredentialBlob = secret.as_ptr() as *mut u8;
        c.Persist = CRED_PERSIST_LOCAL_MACHINE;
        if unsafe { CredWriteW(&c, 0) } == 0 {
            return Err(format!("credential manager: {}", std::io::Error::last_os_error()));
        }
        Ok(())
    }

    pub fn get(service: &str, id: &str) -> Option<Vec<u8>> {
        let name = target(service, id);
        let mut out: *mut CREDENTIALW = std::ptr::null_mut();
        if unsafe { CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut out) } == 0 || out.is_null() {
            return None;
        }
        let c = unsafe { &*out };
        let v = if c.CredentialBlob.is_null() { vec![] } else { unsafe { std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize) }.to_vec() };
        unsafe { CredFree(out as *const _) };
        Some(v)
    }

    pub fn delete(service: &str, id: &str) {
        let name = target(service, id);
        unsafe { CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) };
    }
}

pub const KEYCHAIN_SERVICE: &str = "com.mrgnhnt.midna.webhook";
pub const VAULT_SERVICE: &str = "com.mrgnhnt.midna.secret";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Keychain,
    File,
}

impl Mode {
    /// `MIDNA_SECRETS=file` selects the file store; anything else is the Keychain.
    pub fn from_env() -> Mode {
        match std::env::var("MIDNA_SECRETS").as_deref() {
            Ok("file") => Mode::File,
            _ => Mode::Keychain,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Keychain => "keychain",
            Mode::File => "file",
        }
    }
}

pub struct Store {
    pub mode: Mode,
    service: &'static str,
    dir: PathBuf,
    cache: Mutex<HashMap<String, Vec<u8>>>,
}

fn valid_account(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl Store {
    pub fn new(mode: Mode, home: &std::path::Path) -> Store {
        Store { mode, service: KEYCHAIN_SERVICE, dir: home.join("secrets"), cache: Mutex::new(HashMap::new()) }
    }

    /// The human's pasted secrets (`secret.*`).
    pub fn vault(mode: Mode, home: &std::path::Path) -> Store {
        Store { mode, service: VAULT_SERVICE, dir: home.join("vault"), cache: Mutex::new(HashMap::new()) }
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<u8>>> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set(&self, id: &str, secret: &[u8]) -> Result<(), String> {
        if !valid_account(id) {
            return Err("bad id".into());
        }
        match self.mode {
            Mode::Keychain => keychain::set(self.service, id, secret)?,
            Mode::File => {
                std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
                let _ = crate::sys::set_private(&self.dir);
                let path = self.dir.join(id);
                let tmp = self.dir.join(format!(".{id}.tmp"));
                let _ = std::fs::remove_file(&tmp);
                let mut f = crate::sys::private_file(std::fs::OpenOptions::new().write(true).create_new(true)).open(&tmp).map_err(|e| e.to_string())?;
                f.write_all(secret).map_err(|e| e.to_string())?;
                f.sync_all().map_err(|e| e.to_string())?;
                std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
            }
        }
        self.cache().insert(id.to_string(), secret.to_vec());
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<Vec<u8>> {
        if !valid_account(id) {
            return None;
        }
        if let Some(s) = self.cache().get(id) {
            return Some(s.clone());
        }
        let v = match self.mode {
            Mode::Keychain => keychain::get(self.service, id)?,
            Mode::File => std::fs::read(self.dir.join(id)).ok()?,
        };
        self.cache().insert(id.to_string(), v.clone());
        Some(v)
    }

    pub fn delete(&self, id: &str) {
        if !valid_account(id) {
            return;
        }
        self.cache().remove(id);
        match self.mode {
            Mode::Keychain => keychain::delete(self.service, id),
            Mode::File => {
                let _ = std::fs::remove_file(self.dir.join(id));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_store_roundtrip_is_0600() {
        let home = std::env::temp_dir().join(format!("midna-secrets-{}", std::process::id()));
        let s = Store::new(Mode::File, &home);
        s.set("t_abc123", b"s3cret").unwrap();
        let meta = std::fs::metadata(home.join("secrets/t_abc123")).unwrap();
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o777, 0o600);
        assert_eq!(meta.len(), 6);
        // A fresh store (no cache) reads it back from disk.
        assert_eq!(Store::new(Mode::File, &home).get("t_abc123").as_deref(), Some(&b"s3cret"[..]));
        s.delete("t_abc123");
        assert!(Store::new(Mode::File, &home).get("t_abc123").is_none());
        assert!(s.set("../evil", b"x").is_err());
        let _ = std::fs::remove_dir_all(&home);
    }
}
