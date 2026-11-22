//! Credential storage via the OS keychain (`keyring` crate): Windows
//! Credential Manager / macOS Keychain. The server URL and username (not
//! secret, just "which account") are cached in a small JSON config file in
//! the app config dir so `auth_session_status` can answer instantly without
//! touching the keychain; the password itself is **never** written to a
//! plain file, only to the OS keychain entry.
//!
//! Service name is the app identifier (set once at startup via
//! [`set_service_name`]) so SONIX never reads or clobbers another app's
//! keychain entries — and a dev/test build run with an overridden
//! identifier never touches the regular build's credentials.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ipc::{IpcError, IpcErrorCode};

pub mod profiles;

/// Fallback when [`set_service_name`] was never called (unit/integration
/// tests); equals the shipped identifier.
const DEFAULT_SERVICE_NAME: &str = "dev.sonix.app";
static SERVICE_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Sets the keychain service name (the app identifier). First call wins.
pub fn set_service_name(identifier: &str) {
    let _ = SERVICE_NAME.set(identifier.to_string());
}

fn service_name() -> &'static str {
    SERVICE_NAME.get().map(String::as_str).unwrap_or(DEFAULT_SERVICE_NAME)
}
const CONFIG_FILE_NAME: &str = "connection.json";
const SETTINGS_FILE_NAME: &str = "settings.json";

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

/// App-wide user preferences, independent of any one connected account (kept
/// separate from `ConnectionConfig` so toggling a setting never touches the
/// account cache, and the setting survives logout/re-login/account switch).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Submit "now playing"/scrobbles to the connected Subsonic server (see
    /// `scrobble::`). Defaults OFF: network-visible features are opt-in.
    #[serde(default)]
    pub scrobbling_enabled: bool,
    /// "Spread artists in shuffle": nudge a real shuffle so the same artist
    /// rarely plays back-to-back (`audio::queue::Queue::spread_artists`).
    /// Defaults off — a real shuffle is already correct; this is an
    /// optional taste preference.
    #[serde(default)]
    pub spread_artists_in_shuffle: bool,
    /// "Queue window" (`audio::queue`'s rolling window, 10-100): how many
    /// auto/source-drawn tracks are materialized in the queue at once, so
    /// shuffling a 1,980-track playlist doesn't put all 1,980 of them in
    /// "the queue". Defaults to `DEFAULT_WINDOW_SIZE` (20).
    #[serde(default = "default_queue_window_size")]
    pub queue_window_size: u32,
    // --- continuity ---
    /// "Autoplay similar music when the queue ends". Defaults OFF.
    #[serde(default)]
    pub autoplay_enabled: bool,
    /// "Sync play queue with server" (`savePlayQueue`). Defaults OFF.
    #[serde(default)]
    pub sync_play_queue: bool,
    /// "Volume normalization (ReplayGain)". Defaults off.
    #[serde(default)]
    pub replay_gain_mode: crate::audio::replaygain::ReplayGainMode,
    /// "Check for updates automatically" (`updates::`). Defaults OFF.
    #[serde(default)]
    pub auto_check_updates: bool,
    /// "Count a play after" N seconds of real listening, for both listening
    /// history and scrobbles (`scrobble::threshold`). Defaults to 30.
    #[serde(default = "default_play_threshold_sec")]
    pub play_threshold_sec: u32,
    /// "Show track preview": the corner card on a track change
    /// while SONIX isn't focused. Defaults on.
    #[serde(default = "default_true")]
    pub track_overlay: bool,
    /// "Free memory when minimized": release the UI's resources after the
    /// window was minimized for 30 s. Defaults on.
    #[serde(default = "default_true")]
    pub free_memory_when_minimized: bool,
    /// Corner of the screen for that card. Defaults to top left.
    #[serde(default)]
    pub track_overlay_corner: crate::overlay::OverlayCorner,
    // --- mcp-bridge block ---
    /// "Claude (MCP)": the local bridge listener (`mcp_bridge`). Defaults off.
    #[serde(default)]
    pub mcp_enabled: bool,
    /// "Allow playback control" for MCP clients. Defaults off.
    #[serde(default)]
    pub mcp_allow_control: bool,
}

fn default_true() -> bool {
    true
}

fn default_queue_window_size() -> u32 {
    crate::audio::queue::DEFAULT_WINDOW_SIZE as u32
}

fn default_play_threshold_sec() -> u32 {
    crate::scrobble::threshold::DEFAULT_PLAY_THRESHOLD_SEC
}

/// Clamps a "count a play after" value to the setting's range.
pub fn clamp_play_threshold_sec(sec: u32) -> u32 {
    use crate::scrobble::threshold::{MAX_PLAY_THRESHOLD_SEC, MIN_PLAY_THRESHOLD_SEC};
    sec.clamp(MIN_PLAY_THRESHOLD_SEC, MAX_PLAY_THRESHOLD_SEC)
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            scrobbling_enabled: false,
            spread_artists_in_shuffle: false,
            queue_window_size: default_queue_window_size(),
            autoplay_enabled: false,
            sync_play_queue: false,
            replay_gain_mode: Default::default(),
            auto_check_updates: false,
            play_threshold_sec: default_play_threshold_sec(),
            track_overlay: true,
            free_memory_when_minimized: true,
            track_overlay_corner: Default::default(),
            mcp_enabled: false,
            mcp_allow_control: false,
        }
    }
}

fn settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join(SETTINGS_FILE_NAME)
}

/// Reads persisted settings, or `Settings::default()` if there is no file
/// yet (first run) or it fails to parse. Pure local file I/O, safe to call
/// on the startup critical path.
pub fn load_settings(config_dir: &Path) -> Settings {
    let mut settings: Settings = fs::read(settings_path(config_dir))
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default();
    // A hand-edited/out-of-range value: clamp here so `settings_get`
    // reports what the engine actually uses (`Queue::set_window_size`
    // clamps the same way).
    settings.queue_window_size = settings.queue_window_size.clamp(
        crate::audio::queue::MIN_WINDOW_SIZE as u32,
        crate::audio::queue::MAX_WINDOW_SIZE as u32,
    );
    settings.play_threshold_sec = clamp_play_threshold_sec(settings.play_threshold_sec);
    settings
}

pub fn save_settings(config_dir: &Path, settings: &Settings) -> Result<(), IpcError> {
    fs::create_dir_all(config_dir).map_err(io_err)?;
    let data = serde_json::to_vec_pretty(settings).map_err(|e| IpcError {
        code: IpcErrorCode::Io,
        message: format!("failed to serialize settings: {e}"),
    })?;
    fs::write(settings_path(config_dir), data).map_err(io_err)
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
                tracing::warn!(error = %e.message, "migrated keychain entry but could not delete the legacy one");
            }
        }
        Err(e) => tracing::warn!(error = %e.message, "could not migrate legacy keychain entry"),
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

