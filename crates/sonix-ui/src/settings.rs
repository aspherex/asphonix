//! Tiny local UI preferences (theme). Stored next to the user's config, never in the
//! library: `%APPDATA%\SoniX.native\ui.json` (or `$HOME/.config/SoniX.native/ui.json`).
// the parity build never reads or writes the user's ui.json, so these are unused there
#![cfg_attr(feature = "parity", allow(dead_code))]
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiPrefs {
    pub light: bool,
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("SoniX.native").join("ui.json"))
}

pub fn load() -> UiPrefs {
    path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// `theme.svelte.ts: readStored()`: true when the user has picked a theme before (`ui.json` exists). Without one
/// the web follows `prefers-color-scheme`; `main.rs` then asks the OS (`App::window_appearance`).
pub fn has_stored() -> bool {
    path().is_some_and(|p| p.exists())
}

pub fn save(p: &UiPrefs) {
    if let Some(path) = path() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string(p) {
            let _ = std::fs::write(path, s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prefs_round_trip_and_default() {
        let p: UiPrefs = serde_json::from_str("{}").unwrap();
        assert!(!p.light);
        let s = serde_json::to_string(&UiPrefs { light: true }).unwrap();
        assert_eq!(s, r#"{"light":true}"#);
        assert!(serde_json::from_str::<UiPrefs>(&s).unwrap().light);
    }
}
