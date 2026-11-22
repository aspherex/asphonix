//! Locale-aware numbers and dates for the active UI language.
//!
//! Windows asks the OS (`GetLocaleInfoEx` for the group separator, once per language; `GetDateFormatEx` for short
//! dates); macOS/Linux use a small built-in table per supported language. The OS locale is used when it speaks the
//! UI language (an `en-GB` user with English UI gets British dates), else the language's main region.
use crate::Lang;
use std::sync::OnceLock;

/// The locale name the OS APIs get for `lang`: the user's own locale when its language matches, else the main region.
pub fn locale_name(lang: Lang) -> String {
    let base = match lang {
        Lang::De => "de-DE",
        Lang::En | Lang::EnXa => "en-US",
    };
    let os = sys_locale::get_locale().unwrap_or_default().replace('_', "-");
    let os = os.split('.').next().unwrap_or("").to_string();
    if !os.is_empty() && lang != Lang::EnXa && Lang::parse(&os) == Some(lang) { os } else { base.to_string() }
}

/// Built-in group separator (non-Windows, or when the OS has none).
fn table_sep(lang: Lang) -> &'static str {
    match lang {
        Lang::De => ".",
        Lang::En | Lang::EnXa => ",",
    }
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn os_info(locale: &str, lctype: u32) -> Option<String> {
    unsafe extern "system" {
        fn GetLocaleInfoEx(locale: *const u16, lctype: u32, data: *mut u16, cch: i32) -> i32;
    }
    let name = wide(locale);
    let mut buf = [0u16; 8];
    // SAFETY: `name` is NUL-terminated and `buf` (with its length) outlives the call.
    let n = unsafe { GetLocaleInfoEx(name.as_ptr(), lctype, buf.as_mut_ptr(), buf.len() as i32) };
    (n > 1).then(|| String::from_utf16_lossy(&buf[..n as usize - 1]))
}

#[cfg(windows)]
fn os_sep(locale: &str) -> Option<String> {
    os_info(locale, 0x0F) // LOCALE_STHOUSAND
}

/// The thousands separator of `lang` ("," / "."; whatever the OS says on Windows), looked up once per language.
pub fn thousands_sep(lang: Lang) -> &'static str {
    static SEP: [OnceLock<String>; 3] = [const { OnceLock::new() }; 3];
    SEP[lang as usize].get_or_init(|| {
        #[cfg(windows)]
        if let Some(s) = os_sep(&locale_name(lang)) {
            return s;
        }
        table_sep(lang).to_string()
    })
}

/// The decimal separator of `lang` ("." / ","), looked up once per language.
pub fn decimal_sep(lang: Lang) -> &'static str {
    static SEP: [OnceLock<String>; 3] = [const { OnceLock::new() }; 3];
    SEP[lang as usize].get_or_init(|| {
        #[cfg(windows)]
        if let Some(s) = os_info(&locale_name(lang), 0x0E) {
            // LOCALE_SDECIMAL
            return s;
        }
        match lang {
            Lang::De => ",".into(),
            Lang::En | Lang::EnXa => ".".into(),
        }
    })
}

/// Top bar count: grouped below 10,000, else one decimal with K/M ("51.9K" / "51,9K"), no trailing ",0".
pub fn short_count_in(n: u64, lang: Lang) -> String {
    if n < 10_000 {
        return group_i64(n as i64, lang);
    }
    let (v, unit) = if n >= 1_000_000 { (n as f64 / 1e6, "M") } else { (n as f64 / 1e3, "K") };
    let r = (v * 10.0).round() / 10.0;
    if r.fract() == 0.0 { format!("{}{unit}", r as u64) } else { format!("{}{}{}{unit}", r.trunc() as u64, decimal_sep(lang), ((r.fract() * 10.0).round() as u64)) }
}

/// [`short_count_in`] for the active language.
pub fn short_count(n: u64) -> String {
    short_count_in(n, crate::lang())
}

/// `n` with `sep` between groups of three digits.
pub fn group_with(n: i64, sep: &str) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 * sep.len() + 1);
    if n < 0 {
        out.push('-');
    }
    let first = digits.len() % 3;
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (i + 3 - first).is_multiple_of(3) {
            out.push_str(sep);
        }
        out.push(ch);
    }
    out
}

/// `n` grouped for `lang`: "1,234,567" / "1.234.567".
pub fn group_i64(n: i64, lang: Lang) -> String {
    group_with(n, thousands_sep(lang))
}

/// `n` grouped for the active language (the top bar tooltip, sidebar and page counts).
pub fn int(n: u64) -> String {
    group_i64(n.min(i64::MAX as u64) as i64, crate::lang())
}

/// Short numeric date from the built-in table: `10/31/2026` / `31.10.2026`.
pub fn short_date_table(y: i64, m: i64, d: i64, lang: Lang) -> String {
    match lang {
        Lang::De => format!("{d:02}.{m:02}.{y}"),
        Lang::En | Lang::EnXa => format!("{m}/{d}/{y}"),
    }
}

#[cfg(windows)]
fn os_short_date(locale: &str, y: i64, m: i64, d: i64) -> Option<String> {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        dow: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        ms: u16,
    }
    unsafe extern "system" {
        fn GetDateFormatEx(locale: *const u16, flags: u32, date: *const SystemTime, format: *const u16, out: *mut u16, cch: i32, cal: *const u16) -> i32;
    }
    let st = SystemTime { year: y as u16, month: m as u16, day: d as u16, ..Default::default() };
    let name = wide(locale);
    let mut buf = [0u16; 64];
    // SAFETY: `name` is NUL-terminated; `st` and `buf` outlive the call; null format/calendar = the locale's short
    // date format (DATE_SHORTDATE = 1).
    let n = unsafe { GetDateFormatEx(name.as_ptr(), 1, &st, std::ptr::null(), buf.as_mut_ptr(), buf.len() as i32, std::ptr::null()) };
    (n > 1).then(|| String::from_utf16_lossy(&buf[..n as usize - 1]))
}

/// The short date of a calendar date in `lang` (the OS format on Windows, else the table).
pub fn short_date_in(y: i64, m: i64, d: i64, lang: Lang) -> String {
    #[cfg(windows)]
    if let Some(s) = os_short_date(&locale_name(lang), y, m, d) {
        return s;
    }
    short_date_table(y, m, d, lang)
}

/// The short date of a calendar date in the active language.
pub fn short_date(y: i64, m: i64, d: i64) -> String {
    short_date_in(y, m, d, crate::lang())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouping() {
        assert_eq!(group_with(0, ","), "0");
        assert_eq!(group_with(999, ","), "999");
        assert_eq!(group_with(1000, ","), "1,000");
        assert_eq!(group_with(1_234_567, "."), "1.234.567");
        assert_eq!(group_with(-12_345, "."), "-12.345");
        assert_eq!(group_with(100_000, "\u{a0}"), "100\u{a0}000");
    }

    #[test]
    fn separators_per_locale() {
        assert_eq!(table_sep(Lang::En), ",");
        assert_eq!(table_sep(Lang::De), ".");
        // the OS answer follows the user's locale (an en-DE user groups with "."), never with a digit
        for l in [Lang::En, Lang::De] {
            assert!(!thousands_sep(l).is_empty() && !thousands_sep(l).chars().any(|c| c.is_ascii_digit()));
        }
        #[cfg(windows)]
        {
            assert_eq!(os_sep("en-US").as_deref(), Some(","));
            assert_eq!(os_sep("de-DE").as_deref(), Some("."));
        }
    }

    #[test]
    fn dates_per_locale() {
        assert_eq!(short_date_table(2026, 10, 31, Lang::En), "10/31/2026");
        assert_eq!(short_date_table(2026, 3, 5, Lang::De), "05.03.2026");
        for l in [Lang::En, Lang::De] {
            let s = short_date_in(2026, 10, 31, l);
            assert!(s.contains("2026") || s.contains("26"), "{l:?}: {s}");
            assert!(s.contains("31"), "{l:?}: {s}");
        }
        assert_eq!(locale_name(Lang::EnXa), "en-US");
    }
}
