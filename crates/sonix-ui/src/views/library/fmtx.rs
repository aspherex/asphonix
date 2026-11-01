//! Formatting helpers of `format.ts` that the library routes need (`formatTotalDuration`, `summarizeFormats`,
//! `formatRelativeTime`) and `Date#toLocaleDateString()` (en-US, local time zone) for the album telemetry panel.
use sonix_api::Track;
use sonix_api::format::format_track_format;
use std::collections::BTreeSet;

/// `formatTotalDuration`: "2 HR 5 MIN" / "37 MIN".
pub fn total_duration(total_seconds: f64) -> String {
    let total_min = (total_seconds / 60.0).round() as i64;
    let (h, m) = (total_min / 60, total_min % 60);
    if h > 0 { format!("{h} HR {m} MIN") } else { format!("{m} MIN") }
}

/// `summarizeFormats`: one label when every track agrees, else the suffixes.
pub fn summarize_formats(tracks: &[Track]) -> String {
    if tracks.is_empty() {
        return "—".into();
    }
    let labels: Vec<String> = tracks.iter().map(|t| format_track_format(&t.format)).collect();
    let uniq: BTreeSet<&String> = labels.iter().collect();
    if uniq.len() == 1 {
        return labels[0].clone();
    }
    // JS Set order = first appearance
    let mut suffixes: Vec<String> = Vec::new();
    for t in tracks {
        let s = t.format.suffix.to_uppercase();
        if !suffixes.contains(&s) {
            suffixes.push(s);
        }
    }
    if suffixes.len() == 1 { format!("{} · MIXED RATES", suffixes[0]) } else { suffixes.join(" · ") }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// ISO-8601 (`2026-10-31T12:00:00.000Z`) -> ms since the epoch (UTC).
pub fn parse_iso_ms(iso: &str) -> Option<i64> {
    let b = iso.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |a: usize, z: usize| iso.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, s) = (num(0, 4)?, num(5, 7)?, num(8, 10)?, num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let ms = if b.get(19) == Some(&b'.') { iso.get(20..23).and_then(|f| f.parse::<i64>().ok()).unwrap_or(0) } else { 0 };
    Some(((days_from_civil(y, mo, d) * 24 + h) * 60 + mi) * 60_000 + s * 1000 + ms)
}

#[cfg(windows)]
fn local_offset_ms(utc_ms: i64) -> i64 {
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
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
        fn SystemTimeToTzSpecificLocalTime(tz: *const std::ffi::c_void, utc: *const SystemTime, local: *mut SystemTime) -> i32;
    }
    let days = utc_ms.div_euclid(86_400_000);
    let rem = utc_ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    let u = SystemTime { year: y as u16, month: m as u16, day: d as u16, hour: (rem / 3_600_000) as u16, minute: (rem / 60_000 % 60) as u16, second: (rem / 1000 % 60) as u16, ..Default::default() };
    let mut l = SystemTime::default();
    // SAFETY: both pointers reference live, correctly laid out SYSTEMTIME values; a null time zone means "the current one".
    if unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &u, &mut l) } == 0 {
        return 0;
    }
    let local_ms = ((days_from_civil(l.year as i64, l.month as i64, l.day as i64) * 24 + l.hour as i64) * 60 + l.minute as i64) * 60_000 + l.second as i64 * 1000;
    local_ms - (utc_ms - utc_ms.rem_euclid(1000))
}
#[cfg(not(windows))]
fn local_offset_ms(_: i64) -> i64 {
    0
}

/// The user's short date format for a calendar date (what Chromium's `toLocaleDateString()` follows on Windows: the
/// OS regional settings).
#[cfg(windows)]
fn system_short_date(y: i64, m: i64, d: i64) -> Option<String> {
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
    let mut buf = [0u16; 64];
    // SAFETY: `st` and `buf` outlive the call; null locale/format/calendar select the user default and the short date format (DATE_SHORTDATE = 1).
    let n = unsafe { GetDateFormatEx(std::ptr::null(), 1, &st, std::ptr::null(), buf.as_mut_ptr(), buf.len() as i32, std::ptr::null()) };
    (n > 1).then(|| String::from_utf16_lossy(&buf[..n as usize - 1]))
}

/// `new Date(iso).toLocaleDateString()` in en-US: `M/D/YYYY` in the local time zone.
pub fn locale_date(iso: &str) -> String {
    let Some(ms) = parse_iso_ms(iso) else { return "Invalid Date".into() };
    let (y, m, d) = civil_from_days((ms + local_offset_ms(ms)).div_euclid(86_400_000));
    #[cfg(windows)]
    if let Some(s) = system_short_date(y, m, d) {
        return s;
    }
    format!("{m}/{d}/{y}")
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// `formatRelativeTime` against `now`.
pub fn relative_time(iso: Option<&str>, now: i64) -> String {
    let Some(then) = iso.and_then(parse_iso_ms) else { return "—".into() };
    let diff_sec = ((now - then) / 1000).max(0);
    if diff_sec < 5 {
        return "JUST NOW".into();
    }
    if diff_sec < 60 {
        return format!("{diff_sec}S AGO");
    }
    let min = diff_sec / 60;
    if min < 60 {
        return format!("{min}MIN AGO");
    }
    let hr = min / 60;
    if hr < 24 { format!("{hr}H AGO") } else { format!("{}D AGO", hr / 24) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_duration_matches_format_ts() {
        assert_eq!(total_duration(37.0 * 60.0), "37 MIN");
        assert_eq!(total_duration(125.0 * 60.0), "2 HR 5 MIN");
        assert_eq!(total_duration(0.0), "0 MIN");
    }

    #[test]
    fn iso_parsing_and_dates() {
        let ms = parse_iso_ms("2026-10-31T12:00:00.000Z").unwrap();
        assert_eq!(ms, 1_793_448_000_000);
        assert_eq!(civil_from_days(ms.div_euclid(86_400_000)), (2026, 10, 31));
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert!(parse_iso_ms("x").is_none());
        // the local date is within a day of the UTC one
        let d = locale_date("2026-10-31T12:00:00.000Z");
        assert!(d == "10/31/2026" || d == "11/1/2026" || d == "10/30/2026", "{d}");
    }

    #[test]
    fn relative_time_buckets() {
        let t0 = parse_iso_ms("2026-10-31T12:00:00.000Z").unwrap();
        let iso = "2026-10-31T12:00:00.000Z";
        assert_eq!(relative_time(Some(iso), t0 + 2_000), "JUST NOW");
        assert_eq!(relative_time(Some(iso), t0 + 42_000), "42S AGO");
        assert_eq!(relative_time(Some(iso), t0 + 7 * 60_000), "7MIN AGO");
        assert_eq!(relative_time(Some(iso), t0 + 5 * 3_600_000), "5H AGO");
        assert_eq!(relative_time(Some(iso), t0 + 3 * 86_400_000), "3D AGO");
        assert_eq!(relative_time(None, t0), "—");
    }
}
