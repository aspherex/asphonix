//! Tiny local UI preferences (theme, language), stored as `ui.json` in the app's config dir (`asphonix_real::config_dir`).
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiPrefs {
    pub light: bool,
    /// UI language tag (`"en"`, `"de"`); absent = follow the OS language.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// The visualizer switch (`crate::live::visuals`); absent = on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visualizer: Option<bool>,
}

fn path() -> Option<PathBuf> {
    Some(asphonix_real::config_dir().join("ui.json"))
}

pub fn load() -> UiPrefs {
    path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// True when the user has picked a theme before (`ui.json` exists). Without one `main.rs` follows the OS app
/// theme (`App::window_appearance`).
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

/// Load, change one field, save: a theme toggle keeps the stored language and vice versa.
pub fn update(f: impl FnOnce(&mut UiPrefs)) {
    let mut p = load();
    f(&mut p);
    save(&p);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prefs_round_trip_and_default() {
        let p: UiPrefs = serde_json::from_str("{}").unwrap();
        assert!(!p.light);
        assert_eq!(p.language, None);
        let s = serde_json::to_string(&UiPrefs { light: true, language: None, visualizer: None }).unwrap();
        assert_eq!(s, r#"{"light":true}"#);
        assert!(serde_json::from_str::<UiPrefs>(&s).unwrap().light);
        let s = serde_json::to_string(&UiPrefs { light: false, language: Some("de".into()), visualizer: None }).unwrap();
        assert_eq!(s, r#"{"light":false,"language":"de"}"#);
        assert_eq!(serde_json::from_str::<UiPrefs>(&s).unwrap().language.as_deref(), Some("de"));
    }
}
