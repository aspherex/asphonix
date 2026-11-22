//! SONIX UI translations (Project Fluent) and locale-aware formatting. No GPUI: the UI crate wraps this in its
//! `I18n` global and re-renders on a language switch.
//!
//! - `locales/<lang>/sonix.ftl` are embedded with `include_str!`; only the active locale (plus English as the
//!   fallback) is ever parsed.
//! - [`Key`] is generated from the English file by `build.rs`: an unknown key does not compile.
//! - Hot path: every message is formatted once per locale into a table (`Catalog::plain`), so [`tr`] is an atomic
//!   load and an index, returning `&'static str` (free to turn into a `SharedString`). Only messages with arguments
//!   ([`tr_args`], `tr!(Key, n = 3)`) format at call time.
//! - Plurals come from Fluent (CLDR rules per language); numbers passed as arguments are grouped per locale
//!   ([`fmt::group`]), so `{ $count }` reads "1,234" / "1.234".
//! - `en-XA` is a pseudo-locale (not shipped, `SONIX_LOCALE=en-XA`): the English text accented, bracketed and
//!   ~40 % longer, to find hard-coded strings and clipped labels.
use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentResource, FluentValue};
use std::borrow::Cow;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

pub use fluent_bundle::FluentArgs;

pub mod fmt;
mod pseudo;

include!(concat!(env!("OUT_DIR"), "/keys.rs"));

impl Key {
    /// The Fluent message id (`nav-home`).
    pub fn id(self) -> &'static str {
        IDS[self as usize]
    }
    /// The Rust variant name (`NavHome`), for source scans in tests.
    pub fn name(self) -> &'static str {
        NAMES[self as usize]
    }
    /// The variables the English message uses (`["count"]`); empty for a plain message.
    pub fn vars(self) -> &'static [&'static str] {
        VARS[self as usize]
    }
}

/// The English source text.
pub const EN_FTL: &str = include_str!("../locales/en/sonix.ftl");
/// The German translation.
pub const DE_FTL: &str = include_str!("../locales/de/sonix.ftl");

/// A UI language.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    En,
    De,
    /// Pseudo-locale for testing (never offered in Settings).
    EnXa,
}

impl Lang {
    /// The languages the Settings picker offers.
    pub const SHIPPED: [Lang; 2] = [Lang::En, Lang::De];

    /// BCP-47 tag as stored in the preferences (`"en"`, `"de"`, `"en-XA"`).
    pub fn tag(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::De => "de",
            Lang::EnXa => "en-XA",
        }
    }

    /// The language's own name ("English", "Deutsch"): the picker shows it untranslated.
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::De => "Deutsch",
            Lang::EnXa => "Pseudo (en-XA)",
        }
    }

    /// `"de"`, `"de-AT"`, `"de_DE.UTF-8"` -> `De`; `"en-XA"` -> the pseudo-locale; anything else unsupported -> `None`.
    pub fn parse(tag: &str) -> Option<Lang> {
        let t = tag.trim().replace('_', "-");
        if t.eq_ignore_ascii_case("en-XA") {
            return Some(Lang::EnXa);
        }
        let lang = t.split(['-', '.', '@']).next().unwrap_or("").to_ascii_lowercase();
        match lang.as_str() {
            "en" => Some(Lang::En),
            "de" => Some(Lang::De),
            _ => None,
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    fn from_index(i: u8) -> Lang {
        match i {
            1 => Lang::De,
            2 => Lang::EnXa,
            _ => Lang::En,
        }
    }

    fn source(self) -> &'static str {
        match self {
            Lang::De => DE_FTL,
            Lang::En | Lang::EnXa => EN_FTL,
        }
    }

    fn langid(self) -> unic_langid::LanguageIdentifier {
        let tag = match self {
            Lang::De => "de",
            Lang::En | Lang::EnXa => "en",
        };
        tag.parse().unwrap_or_default()
    }
}

/// The OS UI language (`sys-locale`), if it is a shipped one.
pub fn os_lang() -> Option<Lang> {
    sys_locale::get_locale().and_then(|t| Lang::parse(&t)).filter(|l| Lang::SHIPPED.contains(l))
}

/// The language to start with: `$SONIX_LOCALE` (any supported tag, also `en-XA`), else the stored preference, else
/// the OS language, else English.
pub fn resolve(pref: Option<&str>) -> Lang {
    std::env::var("SONIX_LOCALE").ok().and_then(|t| Lang::parse(&t)).or_else(|| pref.and_then(Lang::parse)).or_else(os_lang).unwrap_or(Lang::En)
}

type Bundle = FluentBundle<FluentResource>;

/// The parsed messages of one language, plus every argument-less message pre-formatted.
pub struct Catalog {
    lang: Lang,
    bundle: Bundle,
    /// English, for a message the language lacks (`None` for English itself).
    fallback: Option<Bundle>,
    plain: Box<[Box<str>]>,
}

fn number_formatter(lang: Lang) -> fn(&FluentValue, &intl_memoizer_type::Memo) -> Option<String> {
    fn group_for(v: &FluentValue, lang: Lang) -> Option<String> {
        match v {
            FluentValue::Number(n) if n.value.fract() == 0.0 && n.value.abs() < 9.0e15 && n.options.minimum_fraction_digits.is_none() => {
                Some(fmt::group_i64(n.value as i64, lang))
            }
            _ => None,
        }
    }
    fn en(v: &FluentValue, _: &intl_memoizer_type::Memo) -> Option<String> {
        group_for(v, Lang::En)
    }
    fn de(v: &FluentValue, _: &intl_memoizer_type::Memo) -> Option<String> {
        group_for(v, Lang::De)
    }
    match lang {
        Lang::De => de,
        Lang::En | Lang::EnXa => en,
    }
}

/// The memoizer type the concurrent bundle's formatter hook receives.
mod intl_memoizer_type {
    pub type Memo = intl_memoizer::concurrent::IntlLangMemoizer;
}

/// The source with `\r\n` line ends turned into `\n`.
pub fn normalize(src: &str) -> String {
    src.replace("\r\n", "\n")
}

fn bundle(lang: Lang) -> Bundle {
    // a CRLF checkout (git autocrlf) must not leave a carriage return at the end of message text
    let res = match FluentResource::try_new(normalize(lang.source())) {
        Ok(r) => r,
        // a broken entry is skipped, the rest still loads (the `locales` test keeps shipped files clean)
        Err((r, _)) => r,
    };
    let mut b = Bundle::new_concurrent(vec![lang.langid()]);
    b.set_use_isolating(false); // no U+2068/U+2069 around arguments: they would be painted as glyphs
    b.set_formatter(Some(number_formatter(lang)));
    if lang == Lang::EnXa {
        b.set_transform(Some(pseudo::accent));
    }
    let _ = b.add_resource(res);
    b
}

impl Catalog {
    fn build(lang: Lang) -> Catalog {
        let bundle = bundle(lang);
        let fallback = (lang != Lang::En).then(bundle_en);
        let mut c = Catalog { lang, bundle, fallback, plain: Box::new([]) };
        c.plain = Key::ALL.iter().map(|&k| c.format(k, None).into_boxed_str()).collect();
        c
    }

    /// The catalog of `lang` (built on first use, then kept for the process lifetime).
    pub fn get(lang: Lang) -> &'static Catalog {
        static CATALOGS: [OnceLock<Catalog>; 3] = [const { OnceLock::new() }; 3];
        CATALOGS[lang.index()].get_or_init(|| Catalog::build(lang))
    }

    pub fn lang(&self) -> Lang {
        self.lang
    }

    /// The message without arguments (pre-formatted: a table lookup).
    pub fn tr(&'static self, key: Key) -> &'static str {
        &self.plain[key as usize]
    }

    /// The message formatted with `args` (plural selection, grouped numbers).
    pub fn tr_args(&self, key: Key, args: &FluentArgs) -> String {
        self.format(key, Some(args))
    }

    fn format(&self, key: Key, args: Option<&FluentArgs>) -> String {
        let id = key.id();
        let (b, msg) = match self.bundle.get_message(id) {
            Some(m) => (&self.bundle, m),
            None => match self.fallback.as_ref().and_then(|f| f.get_message(id).map(|m| (f, m))) {
                Some(x) => x,
                None => return id.to_string(),
            },
        };
        let Some(pattern) = msg.value() else { return id.to_string() };
        let mut errs = Vec::new();
        let s: Cow<str> = b.format_pattern(pattern, args, &mut errs);
        if self.lang == Lang::EnXa { pseudo::wrap(&s) } else { s.into_owned() }
    }
}

fn bundle_en() -> Bundle {
    bundle(Lang::En)
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// The active UI language.
pub fn lang() -> Lang {
    Lang::from_index(CURRENT.load(Ordering::Relaxed))
}

/// Switches the active UI language (builds its catalog if this is its first use). The caller re-renders.
pub fn set_lang(lang: Lang) {
    let _ = Catalog::get(lang);
    CURRENT.store(lang.index() as u8, Ordering::Relaxed);
}

/// The active catalog.
pub fn catalog() -> &'static Catalog {
    Catalog::get(lang())
}

/// A message without arguments in the active language.
pub fn tr(key: Key) -> &'static str {
    catalog().tr(key)
}

/// A message with arguments in the active language.
pub fn tr_args(key: Key, args: &FluentArgs) -> String {
    catalog().tr_args(key, args)
}

/// `tr!(NavHome)` -> `&'static str`; `tr!(TracksCount, count = n)` -> `String` (variables by name; numbers select
/// the plural form and are grouped per locale).
#[macro_export]
macro_rules! tr {
    ($key:ident) => {
        $crate::tr($crate::Key::$key)
    };
    ($key:ident, $($name:ident = $val:expr),+ $(,)?) => {{
        let mut a = $crate::FluentArgs::new();
        $(a.set(stringify!($name), $val);)+
        $crate::tr_args($crate::Key::$key, &a)
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_parse_and_tags() {
        assert_eq!(Lang::parse("de-AT"), Some(Lang::De));
        assert_eq!(Lang::parse("de_DE.UTF-8"), Some(Lang::De));
        assert_eq!(Lang::parse("EN-us"), Some(Lang::En));
        assert_eq!(Lang::parse("en-XA"), Some(Lang::EnXa));
        assert_eq!(Lang::parse("fr"), None);
        for l in [Lang::En, Lang::De, Lang::EnXa] {
            assert_eq!(Lang::parse(l.tag()), Some(l));
            assert_eq!(Lang::from_index(l.index() as u8), l);
        }
    }

    #[test]
    fn plain_lookup_is_preformatted_and_stable() {
        let c = Catalog::get(Lang::De);
        let a = c.tr(Key::NavSettings);
        let b = c.tr(Key::NavSettings);
        assert_eq!(a.as_ptr(), b.as_ptr(), "no formatting on the hot path");
        assert_eq!(a, "Einstellungen");
        assert_eq!(Catalog::get(Lang::En).tr(Key::NavSettings), "Settings");
    }

    #[test]
    fn no_isolation_marks() {
        let mut a = FluentArgs::new();
        a.set("count", 3);
        let s = Catalog::get(Lang::En).tr_args(Key::CountTracks, &a);
        assert!(!s.contains('\u{2068}') && !s.contains('\u{2069}'), "{s:?}");
    }

    #[test]
    fn pseudo_locale_is_bracketed_accented_and_longer() {
        let en = Catalog::get(Lang::En).tr(Key::NavSettings);
        let xa = Catalog::get(Lang::EnXa).tr(Key::NavSettings);
        assert!(xa.starts_with('[') && xa.ends_with(']'), "{xa}");
        assert!(xa.chars().count() as f32 >= en.chars().count() as f32 * 1.35, "{xa}");
        assert!(!xa.contains("Settings"), "{xa}");
        // arguments keep their value (only the message text is accented)
        let mut a = FluentArgs::new();
        a.set("count", 1234);
        let s = Catalog::get(Lang::EnXa).tr_args(Key::CountTracks, &a);
        assert!(s.contains("234") && s.starts_with('['), "{s}");
    }
}
