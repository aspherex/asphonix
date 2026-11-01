//! Saved accounts ("profiles") for profile switching.
//!
//! `profiles.json` (app config dir, next to `connection.json`) holds the
//! non-secret list of known accounts, `{url, username}` each, most recently
//! used first. Passwords stay in the OS keychain, one entry per profile under
//! the account name [`profile_account`] (`"{username}@{url}"`), so the same
//! username on two servers never collides.
//!
//! Backwards compatibility: installs from before profile switching only have
//! `connection.json` plus a keychain entry keyed by the bare username. The
//! list is therefore always seeded with the active account from
//! `connection.json` ([`seed_with_active`]), and a profile whose password is
//! missing under the new key falls back to the legacy username key, copying it
//! over (see `secrets::load_profile_password`).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::{io_err, ConnectionConfig};
use crate::ipc::{IpcError, IpcErrorCode};

const PROFILES_FILE_NAME: &str = "profiles.json";

/// Serializes read-modify-write cycles on `profiles.json` (connect, switch,
/// remove and logout can run concurrently as separate IPC calls).
static PROFILES_LOCK: Mutex<()> = Mutex::new(());

/// One saved account as persisted in `profiles.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredProfile {
    pub url: String,
    pub username: String,
}

impl StoredProfile {
    pub fn new(url: &str, username: &str) -> Self {
        Self { url: url.to_string(), username: username.to_string() }
    }

    pub fn id(&self) -> String {
        profile_id(&self.url, &self.username)
    }

    pub fn matches(&self, url: &str, username: &str) -> bool {
        self.url == url && self.username == username
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProfilesFile {
    #[serde(default)]
    profiles: Vec<StoredProfile>,
}

/// Stable profile id: FNV-1a 64 of `"{url}\n{username}"` as 16 hex digits.
/// (Not `DefaultHasher`, whose output may change between Rust releases.)
pub fn profile_id(url: &str, username: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in url.bytes().chain(std::iter::once(b'\n')).chain(username.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Keychain account name for a profile's password.
pub fn profile_account(url: &str, username: &str) -> String {
    format!("{username}@{url}")
}

/// Makes sure the account in `connection.json` is in the list (an install
/// from before profiles existed has no `profiles.json`). Appended at the front
/// when missing, since it is the most recently used one.
pub fn seed_with_active(list: &mut Vec<StoredProfile>, active: Option<&ConnectionConfig>) {
    if let Some(cfg) = active {
        if !list.iter().any(|p| p.matches(&cfg.url, &cfg.username)) {
            list.insert(0, StoredProfile::new(&cfg.url, &cfg.username));
        }
    }
}

/// Inserts or moves the profile to the front (most recently used first).
pub fn upsert(list: &mut Vec<StoredProfile>, url: &str, username: &str) {
    list.retain(|p| !p.matches(url, username));
    list.insert(0, StoredProfile::new(url, username));
}

/// Removes a non-active profile by id and returns it. Removing the active
/// profile is refused (`invalid_request`; the UI uses Sign out for that), an
/// unknown id is `not_found`.
pub fn remove_inactive(
    list: &mut Vec<StoredProfile>,
    id: &str,
    active: Option<&ConnectionConfig>,
) -> Result<StoredProfile, IpcError> {
    if active.is_some_and(|cfg| profile_id(&cfg.url, &cfg.username) == id) {
        return Err(IpcError {
            code: IpcErrorCode::InvalidRequest,
            message: "cannot remove the active profile; sign out instead".into(),
        });
    }
    let pos = list.iter().position(|p| p.id() == id).ok_or_else(|| not_found(id))?;
    Ok(list.remove(pos))
}

pub fn find<'a>(list: &'a [StoredProfile], id: &str) -> Result<&'a StoredProfile, IpcError> {
    list.iter().find(|p| p.id() == id).ok_or_else(|| not_found(id))
}

fn not_found(id: &str) -> IpcError {
    IpcError { code: IpcErrorCode::NotFound, message: format!("no saved profile with id {id}") }
}

fn profiles_path(config_dir: &Path) -> PathBuf {
    config_dir.join(PROFILES_FILE_NAME)
}

/// Reads `profiles.json` (missing = empty) and seeds it with `active`. Pure
/// local file I/O, no keychain access. An unparseable file is moved aside
/// (`profiles.json.bad`) rather than silently overwritten later.
pub fn load(config_dir: &Path, active: Option<&ConnectionConfig>) -> Vec<StoredProfile> {
    let _guard = PROFILES_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    load_unlocked(config_dir, active)
}

fn load_unlocked(config_dir: &Path, active: Option<&ConnectionConfig>) -> Vec<StoredProfile> {
    let path = profiles_path(config_dir);
    let mut list = match fs::read(&path) {
        Ok(data) => match serde_json::from_slice::<ProfilesFile>(&data) {
            Ok(file) => file.profiles,
            Err(e) => {
                log::warn!("profiles.json is unreadable ({e}); moving it aside");
                let _ = fs::rename(&path, config_dir.join(format!("{PROFILES_FILE_NAME}.bad")));
                Vec::new()
            }
        },
        Err(_) => Vec::new(),
    };
    // Drop exact duplicates a hand edit might have introduced (first wins).
    let mut seen = std::collections::HashSet::new();
    list.retain(|p| seen.insert((p.url.clone(), p.username.clone())));
    seed_with_active(&mut list, active);
    list
}

fn save_unlocked(config_dir: &Path, list: &[StoredProfile]) -> Result<(), IpcError> {
    fs::create_dir_all(config_dir).map_err(io_err)?;
    let data = serde_json::to_vec_pretty(&ProfilesFile { profiles: list.to_vec() }).map_err(|e| IpcError {
        code: IpcErrorCode::Io,
        message: format!("failed to serialize profiles: {e}"),
    })?;
    let tmp = config_dir.join(format!("{PROFILES_FILE_NAME}.tmp"));
    fs::write(&tmp, data).map_err(io_err)?;
    fs::rename(&tmp, profiles_path(config_dir)).map_err(io_err)
}

/// Load (seeded with `active`), apply `f`, save — under one lock. The list is
/// only written when `f` succeeds.
pub fn update<R>(
    config_dir: &Path,
    active: Option<&ConnectionConfig>,
    f: impl FnOnce(&mut Vec<StoredProfile>) -> Result<R, IpcError>,
) -> Result<(R, Vec<StoredProfile>), IpcError> {
    let _guard = PROFILES_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut list = load_unlocked(config_dir, active);
    let out = f(&mut list)?;
    save_unlocked(config_dir, &list)?;
    Ok((out, list))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(url: &str, username: &str) -> ConnectionConfig {
        ConnectionConfig { url: url.into(), username: username.into(), last_sync_at: None }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sonix-profiles-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn profile_id_is_stable_and_distinguishes_url_and_username() {
        let a = profile_id("http://a:4533", "alice");
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a, profile_id("http://a:4533", "alice"), "deterministic");
        assert_ne!(a, profile_id("http://b:4533", "alice"), "same user, other server");
        assert_ne!(a, profile_id("http://a:4533", "bob"));
        // The separator keeps "ab"+"c" and "a"+"bc" apart.
        assert_ne!(profile_id("ab", "c"), profile_id("a", "bc"));
        assert_eq!(profile_account("http://a:4533", "alice"), "alice@http://a:4533");
    }

    #[test]
    fn profile_id_matches_reference_fnv1a() {
        // The id must never change across builds: check against an
        // independent FNV-1a 64 implementation and a published test vector.
        fn reference(bytes: &[u8]) -> u64 {
            let mut h: u64 = 14_695_981_039_346_656_037;
            for b in bytes {
                h ^= *b as u64;
                h = h.wrapping_mul(1_099_511_628_211);
            }
            h
        }
        assert_eq!(profile_id("u", "n"), format!("{:016x}", reference(b"u\nn")));
        // Known FNV-1a 64 test vector: "a" -> af63dc4c8601ec8c.
        assert_eq!(reference(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn upsert_moves_to_front_without_duplicates() {
        let mut list = Vec::new();
        upsert(&mut list, "http://a", "alice");
        upsert(&mut list, "http://b", "bob");
        upsert(&mut list, "http://c", "carol");
        assert_eq!(list.iter().map(|p| p.username.as_str()).collect::<Vec<_>>(), ["carol", "bob", "alice"]);
        upsert(&mut list, "http://a", "alice");
        assert_eq!(list.iter().map(|p| p.username.as_str()).collect::<Vec<_>>(), ["alice", "carol", "bob"]);
        // Same username on another server is a separate profile.
        upsert(&mut list, "http://b", "alice");
        assert_eq!(list.len(), 4);
        assert_eq!(list[0], StoredProfile::new("http://b", "alice"));
    }

    #[test]
    fn remove_refuses_active_and_unknown() {
        let mut list = vec![StoredProfile::new("http://a", "alice"), StoredProfile::new("http://b", "bob")];
        let active = cfg("http://a", "alice");
        let err = remove_inactive(&mut list, &profile_id("http://a", "alice"), Some(&active)).unwrap_err();
        assert_eq!(err.code, IpcErrorCode::InvalidRequest);
        assert_eq!(list.len(), 2, "nothing removed");
        let err = remove_inactive(&mut list, "0000000000000000", Some(&active)).unwrap_err();
        assert_eq!(err.code, IpcErrorCode::NotFound);
        let removed = remove_inactive(&mut list, &profile_id("http://b", "bob"), Some(&active)).unwrap();
        assert_eq!(removed, StoredProfile::new("http://b", "bob"));
        assert_eq!(list, vec![StoredProfile::new("http://a", "alice")]);
        // Not connected: any profile can go.
        remove_inactive(&mut list, &profile_id("http://a", "alice"), None).unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn legacy_install_is_seeded_from_connection_json() {
        let dir = temp_dir("legacy");
        // No profiles.json at all: the list is just the active account.
        let active = cfg("http://nas:4533", "alice");
        assert!(load(&dir, None).is_empty());
        let list = load(&dir, Some(&active));
        assert_eq!(list, vec![StoredProfile::new("http://nas:4533", "alice")]);
        assert!(!profiles_path(&dir).exists(), "loading never writes");

        // Connecting a second account keeps the seeded one, behind it.
        update(&dir, Some(&active), |l| {
            upsert(l, "http://lab:4533", "guest");
            Ok(())
        })
        .unwrap();
        let on_disk = load(&dir, None);
        assert_eq!(
            on_disk,
            vec![StoredProfile::new("http://lab:4533", "guest"), StoredProfile::new("http://nas:4533", "alice")]
        );
        // Seeding an account that is already present does not duplicate/reorder.
        assert_eq!(load(&dir, Some(&active)), on_disk);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_update_does_not_write_and_bad_file_is_moved_aside() {
        let dir = temp_dir("bad");
        fs::create_dir_all(&dir).unwrap();
        fs::write(profiles_path(&dir), "not json").unwrap();
        let active = cfg("http://a", "alice");
        assert_eq!(load(&dir, Some(&active)), vec![StoredProfile::new("http://a", "alice")]);
        assert!(dir.join("profiles.json.bad").exists());

        let r = update(&dir, Some(&active), |l| remove_inactive(l, &profile_id("http://a", "alice"), Some(&active)));
        assert!(r.is_err());
        assert!(!profiles_path(&dir).exists(), "refused update wrote nothing");
        let _ = fs::remove_dir_all(&dir);
    }
}
