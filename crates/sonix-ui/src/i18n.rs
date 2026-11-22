//! The UI language as a GPUI global ([`I18n`]) on top of `sonix_i18n` (which holds the active catalog process-wide,
//! so menu builders and background tasks without a `cx` translate too). Views call `tr!(Key)` / `tr!(Key, n = 3)`.
//!
//! A switch ([`switch`]) sets the language, then `refresh_windows` re-renders every window with view caching
//! bypassed (`AnyView::cached` is skipped while `window.refreshing`), so cached views and parts (`crate::part`,
//! `views/shell_cache.rs`, `crate::live`) rebuild their text. State that holds already-translated text (the search
//! placeholder) is refreshed by the shell (`Shell::set_language`).
use gpui::{App, Global};
use sonix_i18n::Lang;

/// The active UI language and the user's choice (`None` = follow the OS).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct I18n {
    pub lang: Lang,
    pub pref: Option<Lang>,
}
impl Global for I18n {}

/// The language for a stored preference: `$SONIX_LOCALE` first (also `en-XA`), then the preference, then the OS.
pub fn resolve(pref: Option<&str>) -> I18n {
    I18n { lang: sonix_i18n::resolve(pref), pref: pref.and_then(Lang::parse).filter(|l| Lang::SHIPPED.contains(l)) }
}

/// Startup: make `i` the active language and the global.
pub fn init(cx: &mut App, i: I18n) {
    sonix_i18n::set_lang(i.lang);
    cx.set_global(i);
}

/// The language a choice resolves to at runtime (the picker overrides `$SONIX_LOCALE`).
pub fn lang_for(pref: Option<Lang>) -> Lang {
    pref.or_else(sonix_i18n::os_lang).unwrap_or(Lang::En)
}

/// Applies a new choice: sets the active catalog and the global; `true` when the visible language changed (the
/// caller then refreshes the windows and any stored text).
pub fn switch(cx: &mut App, pref: Option<Lang>) -> bool {
    let lang = lang_for(pref);
    let old = cx.try_global::<I18n>().copied();
    sonix_i18n::set_lang(lang);
    cx.set_global(I18n { lang, pref });
    old.map(|o| o.lang) != Some(lang)
}

/// The active choice (`None` = System).
pub fn pref(cx: &App) -> Option<Lang> {
    cx.try_global::<I18n>().and_then(|i| i.pref)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolve_keeps_only_shipped_preferences() {
        assert_eq!(resolve(Some("de")).pref, Some(Lang::De));
        assert_eq!(resolve(Some("en-XA")).pref, None);
        assert_eq!(resolve(Some("xx")).pref, None);
        assert_eq!(lang_for(Some(Lang::De)), Lang::De);
    }
}
