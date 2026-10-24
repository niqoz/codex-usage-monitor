//! Monitor-owned Kimi keys live in Windows Credential Manager, never settings.json.
use serde::{Deserialize, Serialize};
use windows::core::{HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::ERROR_NOT_FOUND;
use windows::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_MAX_CREDENTIAL_BLOB_SIZE,
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
};

use crate::native_interop::wide_str;

const TARGET: &str = "CodexUsage:kimi-code";
pub const MAX_KEY_LENGTH: usize = 2048;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    #[default]
    China,
    International,
}

impl Region {
    pub fn base_url(self) -> &'static str {
        match self {
            Self::China => "https://api.kimi.com/coding/v1",
            Self::International => "https://api.kimi.ai/coding/v1",
        }
    }
}

// Deliberately no Debug implementation: diagnostics must never expose the key.
#[derive(Serialize, Deserialize)]
pub struct SavedKey {
    pub api_key: String,
    pub region: Region,
}

pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_KEY_LENGTH
        && key.bytes().all(|byte| byte.is_ascii_graphic())
}

pub fn load() -> Result<Option<SavedKey>, String> {
    load_from(TARGET)
}

pub fn save(key: &SavedKey) -> Result<(), String> {
    save_to(TARGET, key)
}

pub fn remove() -> Result<(), String> {
    remove_from(TARGET)
}

pub fn watch_signature() -> String {
    match read_blob(TARGET) {
        Ok(Some((_, written))) => format!("kimi-key|saved|{written}"),
        Ok(None) => "kimi-key|missing".to_string(),
        Err(_) => "kimi-key|unreadable".to_string(),
    }
}

fn read_blob(target: &str) -> Result<Option<(Vec<u8>, u64)>, String> {
    let target = wide_str(target);
    let mut credential = std::ptr::null_mut();
    unsafe {
        if let Err(error) = CredReadW(
            PCWSTR(target.as_ptr()),
            CRED_TYPE_GENERIC,
            0,
            &mut credential,
        ) {
            if error.code() == HRESULT::from_win32(ERROR_NOT_FOUND.0) {
                return Ok(None);
            }
            return Err(error.to_string());
        }
        if credential.is_null() {
            return Err("Windows returned an invalid credential.".into());
        }
        let saved = &*credential;
        let bytes = if saved.CredentialBlobSize == 0 || saved.CredentialBlob.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(saved.CredentialBlob, saved.CredentialBlobSize as usize)
                .to_vec()
        };
        let written = (u64::from(saved.LastWritten.dwHighDateTime) << 32)
            | u64::from(saved.LastWritten.dwLowDateTime);
        CredFree(credential.cast());
        Ok(Some((bytes, written)))
    }
}

fn load_from(target: &str) -> Result<Option<SavedKey>, String> {
    let Some((bytes, _)) = read_blob(target)? else {
        return Ok(None);
    };
    // Do not include serde's input-dependent error text in diagnostics or dialogs.
    let key: SavedKey = serde_json::from_slice(&bytes)
        .map_err(|_| "The saved Kimi Code credential is invalid.".to_string())?;
    if !valid_key(&key.api_key) {
        return Err("The saved Kimi Code credential is invalid.".into());
    }
    Ok(Some(key))
}

fn save_to(target: &str, key: &SavedKey) -> Result<(), String> {
    if !valid_key(&key.api_key) {
        return Err("Enter a valid Kimi Code API key.".into());
    }
    let mut bytes = serde_json::to_vec(key)
        .map_err(|_| "Unable to encode the Kimi Code credential.".to_string())?;
    if bytes.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
        return Err("The Kimi Code API key is too long.".into());
    }
    let mut target = wide_str(target);
    let mut username = wide_str("Kimi Code");
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target.as_mut_ptr()),
        CredentialBlobSize: bytes.len() as u32,
        CredentialBlob: bytes.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: PWSTR(username.as_mut_ptr()),
        ..Default::default()
    };
    unsafe { CredWriteW(&credential, 0) }.map_err(|error| error.to_string())
}

fn remove_from(target: &str) -> Result<(), String> {
    let target = wide_str(target);
    match unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, 0) } {
        Ok(()) => Ok(()),
        Err(error) if error.code() == HRESULT::from_win32(ERROR_NOT_FOUND.0) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn keys_reject_whitespace_controls_and_oversized_values() {
        assert!(valid_key("sk-kimi-fixture"));
        for invalid in ["", "key with spaces", "key\r\nheader", "clé"] {
            assert!(!valid_key(invalid));
        }
        assert!(!valid_key(&"a".repeat(MAX_KEY_LENGTH + 1)));
        assert_eq!(
            Region::International.base_url(),
            "https://api.kimi.ai/coding/v1"
        );
    }

    #[test]
    fn credential_manager_round_trip_uses_an_isolated_target() {
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = remove_from(&self.0);
            }
        }
        let target = Cleanup(format!(
            "CodexUsage:kimi-code:test:{}:{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(load_from(&target.0).unwrap().is_none());
        save_to(
            &target.0,
            &SavedKey {
                api_key: "sk-kimi-fixture".into(),
                region: Region::International,
            },
        )
        .unwrap();
        let saved = load_from(&target.0).unwrap().unwrap();
        assert_eq!(saved.api_key, "sk-kimi-fixture");
        assert_eq!(saved.region, Region::International);
        remove_from(&target.0).unwrap();
        assert!(load_from(&target.0).unwrap().is_none());
    }
}
