//! Credential storage via the OS keychain (`keyring` crate): Windows
//! Credential Manager / macOS Keychain. The server URL and username (not
//! secret, just "which account") are cached in a small JSON config file in
//! the app config dir so `auth_session_status` can answer instantly without
//! touching the keychain; the password itself is **never** written to a
//! plain file, only to the OS keychain entry.
//!
//! Service name is the app identifier (`tauri.conf.json` `identifier`, set
//! once at startup via [`set_service_name`]) so SONIX never reads or
//! clobbers another app's keychain entries — and a dev/test build run with
//! an overridden identifier never touches the installed app's credentials.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ipc::{IpcError, IpcErrorCode};

pub mod profiles;

/// Fallback when [`set_service_name`] was never called (unit/integration
/// tests); equals the shipped identifier.
const DEFAULT_SERVICE_NAME: &str = "dev.sonix.native";
static SERVICE_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Sets the keychain service name (the app identifier). First call wins.
pub fn set_service_name(identifier: &str) {
    let _ = SERVICE_NAME.set(identifier.to_string());
}

fn service_name() -> &'static str {
    SERVICE_NAME.get().map(String::as_str).unwrap_or(DEFAULT_SERVICE_NAME)
}
const CONFIG_FILE_NAME: &str = "connection.json";

/// Non-secret connection metadata, persisted as plain JSON in the app config
/// dir. The password lives only in the OS keychain, keyed by
/// [`profiles::profile_account`] (`"{username}@{url}"`; installs from before
/// profile switching used the bare `username`, see [`load_profile_password`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionConfig {
    pub url: String,
    pub username: String,
    #[serde(default)]
    pub last_sync_at: Option<String>,
}

fn config_path(config_dir: &Path) -> PathBuf {
    config_dir.join(CONFIG_FILE_NAME)
}

/// Reads the cached connection config (URL + username), if any. Pure local
/// file I/O, safe to call on the startup critical path.
pub fn load_config(config_dir: &Path) -> Option<ConnectionConfig> {
    let path = config_path(config_dir);
    let data = fs::read(path).ok()?;
    serde_json::from_slice(&data).ok()
}

/// Persists the connection config (called after a successful `auth_connect`,
/// and whenever `last_sync_at` advances).
pub fn save_config(config_dir: &Path, config: &ConnectionConfig) -> Result<(), IpcError> {
    fs::create_dir_all(config_dir).map_err(io_err)?;
    let data = serde_json::to_vec_pretty(config).map_err(|e| IpcError {
        code: IpcErrorCode::Io,
        message: format!("failed to serialize connection config: {e}"),
    })?;
    fs::write(config_path(config_dir), data).map_err(io_err)
}

/// Removes the cached connection config file (part of `auth_logout`).
pub fn clear_config(config_dir: &Path) -> Result<(), IpcError> {
    let path = config_path(config_dir);
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(e)),
    }
}

fn keyring_entry(username: &str) -> Result<keyring::Entry, IpcError> {
    keyring::Entry::new(service_name(), username).map_err(|e| IpcError {
        code: IpcErrorCode::Io,
        message: format!("failed to open OS keychain entry: {e}"),
    })
}

/// Stores `password` in the OS keychain under `username`.
pub fn save_password(username: &str, password: &str) -> Result<(), IpcError> {
    let entry = keyring_entry(username)?;
    entry.set_password(password).map_err(|e| IpcError {
        code: IpcErrorCode::Io,
        message: format!("failed to save credentials to OS keychain: {e}"),
    })
}

/// Reads the password for `username` from the OS keychain, if present.
pub fn load_password(username: &str) -> Result<Option<String>, IpcError> {
    let entry = keyring_entry(username)?;
    match entry.get_password() {
        Ok(pw) => Ok(Some(pw)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(IpcError {
            code: IpcErrorCode::Io,
            message: format!("failed to read credentials from OS keychain: {e}"),
        }),
    }
}

/// Removes the stored password for `username`, if present. Not an error if
/// there was nothing to remove.
pub fn delete_password(username: &str) -> Result<(), IpcError> {
    let entry = keyring_entry(username)?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(IpcError {
            code: IpcErrorCode::Io,
            message: format!("failed to delete credentials from OS keychain: {e}"),
        }),
    }
}

/// Stores a profile's password under its per-profile keychain account
/// (`"{username}@{url}"`).
pub fn save_profile_password(url: &str, username: &str, password: &str) -> Result<(), IpcError> {
    save_password(&profiles::profile_account(url, username), password)
}

/// Reads a profile's password. Falls back to the legacy entry keyed by the
/// bare `username` (installs from before profile switching) and migrates it:
/// copied to the per-profile key, then the legacy entry is deleted. A failed
/// migration still returns the password.
pub fn load_profile_password(url: &str, username: &str) -> Result<Option<String>, IpcError> {
    if let Some(pw) = load_password(&profiles::profile_account(url, username))? {
        return Ok(Some(pw));
    }
    let Some(pw) = load_password(username)? else {
        return Ok(None);
    };
    match save_profile_password(url, username, &pw) {
        Ok(()) => {
            if let Err(e) = delete_password(username) {
                log::warn!("migrated keychain entry but could not delete the legacy one: {}", e.message);
            }
        }
        Err(e) => log::warn!("could not migrate legacy keychain entry: {}", e.message),
    }
    Ok(Some(pw))
}

/// Deletes a profile's keychain entries: the per-profile one, and the legacy
/// username-keyed one unless `keep_legacy` (another saved profile with the
/// same username might still rely on it as its fallback).
pub fn delete_profile_password(url: &str, username: &str, keep_legacy: bool) -> Result<(), IpcError> {
    let new = delete_password(&profiles::profile_account(url, username));
    let legacy = if keep_legacy { Ok(()) } else { delete_password(username) };
    new.and(legacy)
}

pub(crate) fn io_err(e: std::io::Error) -> IpcError {
    IpcError {
        code: IpcErrorCode::Io,
        message: e.to_string(),
    }
}

