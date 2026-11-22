//! The shipped `.ftl` files: they parse, German has exactly the English messages, every message uses the same
//! variables in both, the generated `Key` matches the file, plurals and number grouping per locale.
use fluent_syntax::ast::{self, Entry, Expression, InlineExpression, PatternElement};
use asphonix_i18n::{Catalog, DE_FTL, EN_FTL, FluentArgs, Key, Lang};
use std::collections::{BTreeMap, BTreeSet};

fn vars_inline(e: &InlineExpression<&str>, out: &mut BTreeSet<String>) {
    match e {
        InlineExpression::VariableReference { id } => {
            out.insert(id.name.to_string());
        }
        InlineExpression::FunctionReference { arguments, .. } => {
            for a in &arguments.positional {
                vars_inline(a, out);
            }
            for n in &arguments.named {
                vars_inline(&n.value, out);
            }
        }
        InlineExpression::TermReference { arguments: Some(arguments), .. } => {
            for a in &arguments.positional {
                vars_inline(a, out);
            }
        }
        InlineExpression::Placeable { expression } => vars_expr(expression, out),
        _ => {}
    }
}

fn vars_expr(e: &Expression<&str>, out: &mut BTreeSet<String>) {
    match e {
        Expression::Select { selector, variants } => {
            vars_inline(selector, out);
            for v in variants {
                vars_pattern(&v.value, out);
            }
        }
        Expression::Inline(i) => vars_inline(i, out),
    }
}

fn vars_pattern(p: &ast::Pattern<&str>, out: &mut BTreeSet<String>) {
    for el in &p.elements {
        if let PatternElement::Placeable { expression } = el {
            vars_expr(expression, out);
        }
    }
}

/// id -> variables of every message; panics on a parse error.
fn messages(name: &str, src: &'static str) -> BTreeMap<String, BTreeSet<String>> {
    let res = match fluent_syntax::parser::parse(src) {
        Ok(r) => r,
        Err((_, errs)) => panic!("{name}: {} parse error(s): {errs:?}", errs.len()),
    };
    let mut out = BTreeMap::new();
    for e in &res.body {
        match e {
            Entry::Message(m) => {
                assert!(m.attributes.is_empty(), "{name}: {} uses attributes (the Key enum covers values only)", m.id.name);
                let p = m.value.as_ref().unwrap_or_else(|| panic!("{name}: {} has no value", m.id.name));
                let mut v = BTreeSet::new();
                vars_pattern(p, &mut v);
                assert!(out.insert(m.id.name.to_string(), v).is_none(), "{name}: duplicate id {}", m.id.name);
            }
            Entry::Junk { content } => panic!("{name}: junk entry {content:?}"),
            _ => {}
        }
    }
    out
}

#[test]
fn every_locale_parses_and_matches_english() {
    let en = messages("en", EN_FTL);
    let de = messages("de", DE_FTL);
    let missing: Vec<_> = en.keys().filter(|k| !de.contains_key(*k)).collect();
    assert!(missing.is_empty(), "de is missing {} message(s): {missing:?}", missing.len());
    let extra: Vec<_> = de.keys().filter(|k| !en.contains_key(*k)).collect();
    assert!(extra.is_empty(), "de has message(s) English does not: {extra:?}");
    for (id, v) in &en {
        assert_eq!(v, &de[id], "variables of `{id}` differ between en and de");
    }
}

#[test]
fn generated_keys_match_the_english_file() {
    let en = messages("en", EN_FTL);
    assert_eq!(Key::ALL.len(), en.len());
    for k in Key::ALL {
        let v = en.get(k.id()).unwrap_or_else(|| panic!("{} not in en", k.id()));
        let gen_vars: BTreeSet<String> = k.vars().iter().map(|s| s.to_string()).collect();
        assert_eq!(&gen_vars, v, "build.rs variables of {}", k.id());
    }
}

#[test]
fn every_message_formats_without_errors() {
    for lang in [Lang::En, Lang::De, Lang::EnXa] {
        let c = Catalog::get(lang);
        for k in Key::ALL {
            let s = if k.vars().is_empty() {
                c.tr(k).to_string()
            } else {
                let mut a = FluentArgs::new();
                for v in k.vars() {
                    a.set(*v, 2);
                }
                c.tr_args(k, &a)
            };
            assert!(!s.is_empty() || k.id().ends_with("-empty"), "{lang:?} {}: empty", k.id());
            assert!(!s.contains("{$") && !s.contains("{ $") && !s.contains("???"), "{lang:?} {}: unresolved: {s}", k.id());
            assert!(!s.contains('\u{2068}') && !s.contains('\u{2069}'), "{lang:?} {}: bidi isolation marks", k.id());
        }
    }
}

/// Upper case is a render-time style (`TypeSpec::caps`), so translations are written in normal case. Short
/// abbreviations and key names are the exception.
#[test]
fn translations_are_not_written_in_upper_case() {
    const ALLOWED: &[&str] = &["topbar-stat-tracks", "topbar-stat-albums", "topbar-stat-artists", "topbar-search-kbd"];
    for (name, src) in [("en", EN_FTL), ("de", DE_FTL)] {
        for (id, _) in messages(name, src) {
            if ALLOWED.contains(&id.as_str()) {
                continue;
            }
            let k = Key::ALL.into_iter().find(|k| k.id() == id).unwrap();
            let lang = if name == "de" { Lang::De } else { Lang::En };
            let text = if k.vars().is_empty() { Catalog::get(lang).tr(k).to_string() } else { String::new() };
            let letters: Vec<char> = text.chars().filter(|c| c.is_alphabetic()).collect();
            let shouting = letters.len() > 4 && letters.iter().all(|c| c.is_uppercase());
            assert!(!shouting, "{name} {id} is written in upper case: {text:?}");
        }
    }
}

fn count(lang: Lang, key: Key, n: i64) -> String {
    let mut a = FluentArgs::new();
    a.set("count", n);
    Catalog::get(lang).tr_args(key, &a)
}

#[test]
fn plurals_per_locale() {
    let en: Vec<String> = [0, 1, 2, 5, 21].iter().map(|&n| count(Lang::En, Key::CountTracks, n)).collect();
    assert_eq!(en, ["0 tracks", "1 track", "2 tracks", "5 tracks", "21 tracks"]);
    let en: Vec<String> = [0, 1, 2, 5, 21].iter().map(|&n| count(Lang::En, Key::CountAlbums, n)).collect();
    assert_eq!(en, ["0 albums", "1 album", "2 albums", "5 albums", "21 albums"]);
    let de: Vec<String> = [0, 1, 2, 5, 21].iter().map(|&n| count(Lang::De, Key::CountAlbums, n)).collect();
    assert_eq!(de, ["0 Alben", "1 Album", "2 Alben", "5 Alben", "21 Alben"]);
    let de: Vec<String> = [0, 1, 2, 5, 21].iter().map(|&n| count(Lang::De, Key::CountTracks, n)).collect();
    assert_eq!(de, ["0 Titel", "1 Titel", "2 Titel", "5 Titel", "21 Titel"]);
}

#[test]
fn numbers_in_messages_are_grouped_per_locale() {
    let en = count(Lang::En, Key::CountTracks, 1_234_567);
    let de = count(Lang::De, Key::CountTracks, 1_234_567);
    let (se, sd) = (asphonix_i18n::fmt::thousands_sep(Lang::En), asphonix_i18n::fmt::thousands_sep(Lang::De));
    assert_eq!(en, format!("1{se}234{se}567 tracks"));
    assert_eq!(de, format!("1{sd}234{sd}567 Titel"));
    // a string argument passes through unchanged (unknown counts render as "—")
    let mut a = FluentArgs::new();
    a.set("count", "—");
    assert_eq!(Catalog::get(Lang::De).tr_args(Key::CountAlbums, &a), "— Alben");
}

#[test]
fn short_counts_per_locale() {
    use asphonix_i18n::fmt::{decimal_sep, short_count_in};
    assert_eq!(short_count_in(9_999, Lang::En), format!("9{}999", asphonix_i18n::fmt::thousands_sep(Lang::En)));
    assert_eq!(short_count_in(50_000, Lang::De), "50K");
    assert_eq!(short_count_in(51_900, Lang::En), format!("51{}9K", decimal_sep(Lang::En)));
    assert_eq!(short_count_in(51_900, Lang::De), format!("51{}9K", decimal_sep(Lang::De)));
    assert_eq!(short_count_in(2_500_000, Lang::De), format!("2{}5M", decimal_sep(Lang::De)));
}

/// A CRLF checkout (git autocrlf on Windows CI) parses to the same text: no carriage return reaches a message.
#[test]
fn crlf_sources_yield_no_carriage_returns() {
    let crlf = EN_FTL.replace("\r\n", "\n").replace('\n', "\r\n");
    let norm = asphonix_i18n::normalize(&crlf);
    let res = fluent_syntax::parser::parse(norm.as_str()).unwrap_or_else(|(_, e)| panic!("{e:?}"));
    for e in &res.body {
        if let Entry::Message(m) = e {
            for el in &m.value.as_ref().unwrap().elements {
                if let PatternElement::TextElement { value } = el {
                    assert!(!value.contains('\r'), "{}: {value:?}", m.id.name);
                }
            }
        }
    }
    assert_eq!(norm, EN_FTL.replace("\r\n", "\n"));
}
