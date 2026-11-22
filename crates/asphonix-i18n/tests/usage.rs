//! Source scans of `crates/asphonix-ui/src` (plain text, no compiler):
//! - every English message is used somewhere (`tr!(Name ..)` or `Key::Name`), with the variables the message has;
//! - CI guard: no English literal is passed to the UI's text constructors (`txt`, tooltips, menu items, buttons,
//!   badges, dialogs, toasts, placeholders, labels). New UI text goes into `locales/*/asphonix.ftl`.
use asphonix_i18n::Key;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn ui_sources() -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push((p.clone(), std::fs::read_to_string(&p).unwrap()));
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../asphonix-ui/src");
    let mut out = Vec::new();
    walk(&root, &mut out);
    assert!(out.len() > 20, "asphonix-ui sources not found at {}", root.display());
    out
}

/// The code of a file without `//` comments and without its trailing `#[cfg(test)] mod tests` (tests may use any
/// text). String contents are kept; a `//` inside a string literal does not start a comment.
fn code_of(src: &str) -> String {
    let src = src.replace("\r\n", "\n");
    // drop `#[cfg(test)] mod tests { .. }` blocks (brace-matched; they can sit in the middle of a file)
    let mut body = String::with_capacity(src.len());
    let mut rest = src.as_str();
    while let Some(i) = rest.find("#[cfg(test)]\nmod tests") {
        body.push_str(&rest[..i]);
        let after = &rest[i..];
        let Some(open) = after.find('{') else { break };
        let mut depth = 0;
        let mut end = after.len();
        for (k, c) in after[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + k + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        rest = &after[end..];
    }
    body.push_str(rest);
    let body = body.as_str();
    let mut out = String::with_capacity(body.len());
    for line in body.lines() {
        let b = line.as_bytes();
        let (mut in_str, mut esc, mut cut) = (false, false, line.len());
        for i in 0..b.len() {
            let c = b[i];
            if in_str {
                if esc {
                    esc = false;
                } else if c == b'\\' {
                    esc = true;
                } else if c == b'"' {
                    in_str = false;
                }
            } else if c == b'"' {
                in_str = true;
            } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
                cut = i;
                break;
            } else if c == b'\'' && b.get(i + 2) == Some(&b'\'') {
                // a char literal like '"' must not open a string
            }
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

/// The top-level, comma-separated arguments of the call whose `(` is at `open`, and the index after its `)`.
fn call_args(s: &str, open: usize) -> Option<(Vec<String>, usize)> {
    let b = s.as_bytes();
    let (mut depth, mut in_str, mut esc) = (0i32, false, false);
    let mut args = Vec::new();
    let mut start = open + 1;
    let mut i = open;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'\'' if i + 2 < b.len() && b[i + 2] == b'\'' => i += 2,
                b'\'' if i + 3 < b.len() && b[i + 1] == b'\\' && b[i + 3] == b'\'' => i += 3,
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        args.push(s[start..i].trim().to_string());
                        return Some((args, i + 1));
                    }
                }
                b',' if depth == 1 => {
                    args.push(s[start..i].trim().to_string());
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Every `tr!(Name ...)` and `Key::Name` in the UI code: name -> argument-name sets of the `tr!` calls.
/// key name -> (argument names, file) of every `tr!` call
type Calls = BTreeMap<String, Vec<(BTreeSet<String>, String)>>;

fn key_uses(files: &[(PathBuf, String)]) -> (BTreeSet<String>, Calls) {
    let mut used = BTreeSet::new();
    let mut calls: Calls = BTreeMap::new();
    for (path, src) in files {
        let code = code_of(src);
        for (i, _) in code.match_indices("Key::") {
            if i > 0 && is_ident(code.as_bytes()[i - 1]) {
                continue;
            }
            let name: String = code[i + 5..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            used.insert(name);
        }
        for (i, _) in code.match_indices("tr!(") {
            let Some((args, _)) = call_args(&code, i + 3) else { continue };
            let Some(name) = args.first().cloned() else { continue };
            let vars: BTreeSet<String> = args[1..].iter().filter_map(|a| a.split_once('=').map(|(n, _)| n.trim().to_string())).collect();
            used.insert(name.clone());
            calls.entry(name).or_default().push((vars, path.display().to_string()));
        }
    }
    (used, calls)
}

#[test]
fn every_english_message_is_used_with_its_variables() {
    let files = ui_sources();
    let (used, calls) = key_uses(&files);
    let unused: Vec<&str> = Key::ALL.iter().filter(|k| !used.contains(k.name())).map(|k| k.id()).collect();
    assert!(unused.is_empty(), "{} message(s) in en/asphonix.ftl are not used by asphonix-ui (remove them): {unused:?}", unused.len());
    let names: BTreeMap<&str, Key> = Key::ALL.iter().map(|k| (k.name(), *k)).collect();
    for (name, uses) in &calls {
        let Some(k) = names.get(name.as_str()) else { panic!("tr!({name}) is not a key (the compiler should have said so)") };
        let want: BTreeSet<String> = k.vars().iter().map(|s| s.to_string()).collect();
        for (got, file) in uses {
            assert_eq!(got, &want, "tr!({name}, ..) in {file} passes {got:?}, the message uses {want:?}");
        }
    }
}

/// UI constructors and which (0-based) argument carries visible text.
const TEXT_CALLS: &[(&str, &[usize])] = &[
    ("txt(", &[2]),
    (".tip(", &[0]),
    (" tip(", &[1]),
    (".tip_opt(", &[0]),
    ("MenuItem::action(", &[0]),
    ("MenuItem::submenu(", &[0]),
    (".hint(", &[0]),
    ("button(", &[3]),
    ("badge(", &[2]),
    ("segmented(", &[2]),
    ("TextInput::new(", &[1]),
    ("field(", &[3]),
    ("show_toast(", &[1]),
    ("prompt(", &[1, 2, 3]),
    ("confirm(", &[1, 2, 3]),
    (".child(", &[0]),
    ("Self::row(", &[1]),
    ("Self::panel(", &[1]),
    ("Self::hint(", &[2]),
    ("Self::group(", &[1]),
    ("section_title(", &[1]),
    ("block_title(", &[1]),
    ("header_cell(", &[1]),
    ("checkbox(", &[3]),
    ("new_btn(", &[1]),
    ("play_btn(", &[2]),
    ("sec_btn(", &[3]),
    ("stat(", &[0]),
    ("plain(", &[0]),
    ("label(", &[0]),
    ("key(", &[0, 1]),
    ("header(", &[1]),
    ("wrap_text(", &[2]),
    ("text_link(", &[2]),
    ("mono(", &[0]),
];

/// Words that are not English UI text: units, format tags, key names, the brand.
const TOKENS: &[&str] = &[
    "ASPHONIX", "MS", "ms", "KHZ", "kHz", "HZ", "Hz", "KB", "MB", "GB", "B", "BIT", "bit", "kbps", "dB", "FFT", "PT", "BAND", "RG", "INF",
    "ESC", "API", "Subsonic", "K", "F", "MIX", "LOSSLESS", "STEREO", "PEAK", "HOLD", "POS", "FAST", "FOURIER", "TRANSFORM", "SPEC",
    "SYS_PROTOCOL", "V", "A", "J", "L", "R", "T", "M", "Z", "v", "s",
];

/// Whole literals that are fine as they are (symbols, examples, decorative technical tags).
const LITERALS: &[&str] = &["A-Z", "J / K", "https://music.example.com:4533", "flac, mp3, m4a…", "★ (1-5)", "SYS_PROTOCOL // V1.16.1 SPEC"];

/// The string literal an argument starts with (`"..."`, `format!("...")`, `&format!("...")`), if any.
fn literal_of(arg: &str) -> Option<String> {
    let a = arg.trim_start_matches('&');
    let rest = a.strip_prefix("format!(").unwrap_or(a);
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut esc = false;
    for c in rest.chars() {
        if esc {
            out.push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == '"' {
            return Some(out);
        } else {
            out.push(c);
        }
    }
    None
}

/// True when a literal reads as English UI text: after removing `{...}` placeholders, some word of two or more
/// letters is not a known token.
fn is_english(lit: &str) -> bool {
    if LITERALS.contains(&lit) {
        return false;
    }
    let mut s = String::new();
    let mut depth = 0;
    for c in lit.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            _ if depth == 0 => s.push(c),
            _ => {}
        }
    }
    s.split(|c: char| !(c.is_alphabetic() || c == '_')).filter(|w| w.chars().count() >= 2).any(|w| !TOKENS.contains(&w))
}

#[test]
fn no_english_literals_in_ui_text_constructors() {
    let mut hits = Vec::new();
    for (path, src) in ui_sources() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let code = code_of(&src);
        for &(pat, idx) in TEXT_CALLS {
            for (i, _) in code.match_indices(pat) {
                // `txt(` must be the function, not `foo_txt(`; leading-dot/space patterns carry their own boundary
                let first = pat.as_bytes()[0];
                if is_ident(first) && i > 0 && is_ident(code.as_bytes()[i - 1]) {
                    continue;
                }
                let open = i + pat.len() - 1;
                let Some((args, _)) = call_args(&code, open) else { continue };
                // segmented options: the label of every ("value", "Label") pair
                let lits: Vec<String> = if pat == "segmented(" {
                    args.get(2).map(|a| a.split("(\"").skip(1).filter_map(|t| t.split_once(", ").and_then(|(_, l)| literal_of(l.trim()))).collect()).unwrap_or_default()
                } else {
                    idx.iter().filter_map(|&k| args.get(k)).filter_map(|a| literal_of(a)).collect()
                };
                for lit in lits {
                    if is_english(&lit) {
                        let line = code[..i].lines().count();
                        hits.push(format!("{name}:{line}: {pat} \"{lit}\""));
                    }
                }
            }
        }
    }
    assert!(hits.is_empty(), "user-facing English literals in asphonix-ui (add a key to locales/en and locales/de and use tr!):\n{}", hits.join("\n"));
}

#[test]
fn guard_recognises_english_and_tokens() {
    assert!(is_english("New playlist"));
    assert!(is_english("{} tracks"));
    assert!(is_english("Play"));
    assert!(!is_english("·"));
    assert!(!is_english("--:--"));
    assert!(!is_english("{}MS"));
    assert!(!is_english("{} KHZ"));
    assert!(!is_english("v{}"));
    assert!(!is_english("A-Z"));
    assert_eq!(literal_of("format!(\"{} tracks\", n)").as_deref(), Some("{} tracks"));
    assert_eq!(literal_of("&format!(\"x\\\"y\")").as_deref(), Some("x\"y"));
    assert_eq!(literal_of("tr!(NavHome)"), None);
    let (args, _) = call_args("f(a, g(b, c), \"x, y\")", 1).unwrap();
    assert_eq!(args, ["a", "g(b, c)", "\"x, y\""]);
    // the guard really scans: an English literal in a txt() call is found
    let code = code_of("fn f() { txt(&th, BODY_SM, \"Hello world\"); // txt(&th, X, \"comment\")\n}");
    let i = code.find("txt(").unwrap();
    let (args, _) = call_args(&code, i + 3).unwrap();
    assert_eq!(literal_of(&args[2]).as_deref(), Some("Hello world"));
    assert!(!code.contains("comment"));
}
