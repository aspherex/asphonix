//! Runtime language switch at the catalog level (own test process: it changes the process-wide language). The UI
//! side (`asphonix-ui/src/i18n.rs` + `Shell::set_language`) swaps this catalog, then `refresh_windows` re-renders every
//! view uncached; every `tr!` on the next frame therefore reads the new table.
use asphonix_i18n::{Catalog, FluentArgs, Key, Lang, lang, set_lang, tr, tr_args};

#[test]
fn switching_changes_every_lookup_and_reuses_catalogs() {
    assert_eq!(lang(), Lang::En);
    assert_eq!(tr(Key::NavSettings), "Settings");
    let en: *const Catalog = Catalog::get(Lang::En);

    set_lang(Lang::De);
    assert_eq!(lang(), Lang::De);
    assert_eq!(tr(Key::NavSettings), "Einstellungen");
    assert_eq!(tr(Key::MenuPlayNext), "Als Nächstes abspielen");
    let mut a = FluentArgs::new();
    a.set("count", 2);
    assert_eq!(tr_args(Key::CountAlbums, &a), "2 Alben");
    // no stale English anywhere: every argument-less German message differs from English unless it is the same word
    let same: Vec<&str> = Key::ALL.iter().filter(|k| k.vars().is_empty() && Catalog::get(Lang::En).tr(**k) == tr(**k)).map(|k| k.id()).collect();
    let untranslated: Vec<&&str> = same.iter().filter(|id| !SAME_IN_GERMAN.contains(id)).collect();
    assert!(untranslated.is_empty(), "these read the same in German and English (translate them or add them to SAME_IN_GERMAN): {untranslated:?}");

    set_lang(Lang::En);
    assert_eq!(tr(Key::NavSettings), "Settings");
    set_lang(Lang::De);
    set_lang(Lang::En);
    // switching back and forth builds each catalog once
    assert!(std::ptr::eq(en, Catalog::get(Lang::En)));
    let de: *const Catalog = Catalog::get(Lang::De);
    set_lang(Lang::De);
    assert!(std::ptr::eq(de, Catalog::get(Lang::De)));
    set_lang(Lang::En);
}

/// Messages that are legitimately identical in English and German (loanwords, names, symbols).
const SAME_IN_GERMAN: &[&str] = &[
    "nav-genres",
    "topbar-stat-albums",
    "settings-language-system",
    "settings-panel-scrobbling",
    "settings-replaygain-album",
    "settings-panel-server",
    "settings-server-url",
    "settings-ping",
    "settings-panel-updates",
    "settings-panel-mcp",
    "flow-menu",
    "info-format",
    "info-bitrate",
    "info-mono",
    "info-stereo",
    "info-album",
    "info-genre",
    "col-album",
    "col-format",
    "home-heavy-rotation",
    "login-status-ok",
    "login-host",
    "login-engine",
    "login-status",
    "source-radio",
    "np-pause",
    "np-forward-10",
];
