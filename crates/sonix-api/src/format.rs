//! Display formatting shared by every view.
use crate::types::{FlowArc, FlowCoverage, FlowReason};
use crate::types::{OutputMode, PlayerState, Track, TrackFormat};

/// `m:ss` / `h:mm:ss`, `--:--` for unknown/negative/non-finite.
pub fn format_duration(seconds: Option<f64>) -> String {
    let Some(t) = seconds else { return "--:--".into() };
    if !t.is_finite() || t < 0.0 {
        return "--:--".into();
    }
    let s = t.floor() as u64;
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{sec:02}") } else { format!("{m}:{sec:02}") }
}

/// `FLAC 24/96`, `MP3 320K`, or the bare suffix.
pub fn format_track_format(f: &TrackFormat) -> String {
    let suffix = f.suffix.to_uppercase();
    if let Some(d) = f.bit_depth {
        return format!("{suffix} {d}/{}", (f.sampling_rate as f64 / 1000.0).round() as u32);
    }
    if let Some(b) = f.bit_rate {
        return format!("{suffix} {b}K");
    }
    suffix
}

/// `YYYY-MM-DD` of an ISO timestamp.
pub fn date_part(iso: &str) -> &str {
    iso.get(..10).unwrap_or(iso)
}

pub fn track_year(t: &Track) -> String {
    t.year.map(|y| y.to_string()).unwrap_or_else(|| "—".into())
}

/// `1,234,567` with thousands separators.
pub fn fmt_int(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Top-bar compact count: full below 10k, else `51.9K`.
pub fn short_count(n: u64) -> String {
    if n < 10_000 {
        return fmt_int(n);
    }
    let (v, unit) = if n >= 1_000_000 { (n as f64 / 1e6, "M") } else { (n as f64 / 1e3, "K") };
    let r = (v * 10.0).round() / 10.0;
    if r.fract() == 0.0 { format!("{}{unit}", r as u64) } else { format!("{r}{unit}") }
}

/// "EXCL · 16/48 · FLAC" (`formatOutputReadout`): the engine's real output format.
pub fn output_readout(ps: &PlayerState, fallback: &str) -> String {
    let Some(sr) = ps.sample_rate else { return fallback.to_string() };
    let label = ps.format_label.clone().unwrap_or_default();
    if ps.float_mix == Some(true) {
        return format!("SHARED · {}F MIX · {} KHZ · {label}", ps.bit_depth.unwrap_or(32), trim_float(sr as f64 / 1000.0));
    }
    let mode = if ps.output_mode == OutputMode::Exclusive { "EXCL" } else { "SHARED" };
    let depth = ps.bit_depth.map(|d| d.to_string()).unwrap_or_else(|| "—".into());
    format!("{mode} · {depth}/{} · {label}", (sr as f64 / 1000.0).round() as u32)
}

/// "SHR 32F/44.1" / "EXCL 16/44" (`formatOutputShort`).
pub fn output_short(ps: &PlayerState, fallback: &str) -> String {
    let Some(sr) = ps.sample_rate else { return fallback.to_string() };
    if ps.float_mix == Some(true) {
        return format!("SHR {}F/{}", ps.bit_depth.unwrap_or(32), trim_float(sr as f64 / 1000.0));
    }
    let mode = if ps.output_mode == OutputMode::Exclusive { "EXCL" } else { "SHR" };
    let depth = ps.bit_depth.map(|d| d.to_string()).unwrap_or_else(|| "—".into());
    format!("{mode} {depth}/{}", (sr as f64 / 1000.0).round() as u32)
}

/// Rounded to one decimal, no trailing ".0".
fn trim_float(x: f64) -> String {
    let r = (x * 10.0).round() / 10.0;
    if r.fract() == 0.0 { format!("{}", r as i64) } else { format!("{r}") }
}

/// Dock volume label: "MUTE", "0.0 dB", "-45.0 dB".
pub fn volume_label(ps: &PlayerState) -> String {
    if ps.volume <= 0.0 {
        return "MUTE".into();
    }
    let db = ps.volume_db.unwrap_or(60.0 * (ps.volume.min(1.0) - 1.0));
    format!("{:.1} dB", if db == 0.0 { 0.0 } else { db })
}

/// Cover tag of the *source file* (Now Playing): "24/96 LOSSLESS FLAC", "MP3 320K".
pub fn source_tag(f: &TrackFormat) -> String {
    let suffix = f.suffix.to_uppercase();
    match (f.bit_depth, f.bit_rate) {
        (Some(d), _) => format!("{d}/{} LOSSLESS {suffix}", trim_float(f.sampling_rate as f64 / 1000.0)),
        (None, Some(b)) => format!("{suffix} {b}K"),
        _ => suffix,
    }
}

/// Header line of Now Playing: "FLAC 16-BIT / 48.0 KHZ".
pub fn source_line(f: &TrackFormat) -> String {
    let suffix = f.suffix.to_uppercase();
    let khz = format!("{:.1} KHZ", f.sampling_rate as f64 / 1000.0);
    match f.bit_depth {
        Some(d) => format!("{suffix} {d}-BIT / {khz}"),
        None => format!("{suffix} / {khz}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readouts_format_as_expected() {
        let mut ps = PlayerState::idle();
        assert_eq!(output_readout(&ps, "OUTPUT: —"), "OUTPUT: —");
        ps.sample_rate = Some(48000);
        ps.bit_depth = Some(16);
        ps.format_label = Some("FLAC".into());
        assert_eq!(output_readout(&ps, ""), "EXCL · 16/48 · FLAC");
        assert_eq!(output_short(&ps, "—"), "EXCL 16/48");
        ps.output_mode = OutputMode::Shared;
        ps.float_mix = Some(true);
        ps.bit_depth = Some(32);
        ps.sample_rate = Some(44100);
        assert_eq!(output_readout(&ps, ""), "SHARED · 32F MIX · 44.1 KHZ · FLAC");
        assert_eq!(output_short(&ps, ""), "SHR 32F/44.1");
        ps.volume = 0.5;
        ps.volume_db = Some(-30.0);
        assert_eq!(volume_label(&ps), "-30.0 dB");
        ps.volume = 1.0;
        ps.volume_db = Some(0.0);
        assert_eq!(volume_label(&ps), "0.0 dB");
        ps.volume = 0.0;
        assert_eq!(volume_label(&ps), "MUTE");
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(Some(0.0)), "0:00");
        assert_eq!(format_duration(Some(65.9)), "1:05");
        assert_eq!(format_duration(Some(3725.0)), "1:02:05");
        assert_eq!(format_duration(None), "--:--");
        assert_eq!(format_duration(Some(f64::NAN)), "--:--");
    }
    #[test]
    fn counts() {
        assert_eq!(fmt_int(1234567), "1,234,567");
        assert_eq!(fmt_int(12), "12");
        assert_eq!(short_count(9999), "9,999");
        assert_eq!(short_count(51900), "51.9K");
        assert_eq!(short_count(50000), "50K");
    }
}

// ------------------------------------------------------------ local time (History)

/// `(year, month, day, hour, minute, weekday 0=Sun)` of a unix-ms timestamp in the OS time zone
/// (DST applied per timestamp; UTC off Windows).
pub fn local_parts(ms: i64) -> (i32, u32, u32, u32, u32, u32) {
    #[cfg(windows)]
    {
        #[repr(C)]
        #[derive(Default, Clone, Copy)]
        struct SystemTime {
            y: u16,
            mo: u16,
            dow: u16,
            d: u16,
            h: u16,
            mi: u16,
            s: u16,
            ms: u16,
        }
        #[repr(C)]
        struct FileTime {
            lo: u32,
            hi: u32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn FileTimeToSystemTime(f: *const FileTime, s: *mut SystemTime) -> i32;
            fn SystemTimeToTzSpecificLocalTime(tz: *const u8, utc: *const SystemTime, local: *mut SystemTime) -> i32;
        }
        let ft = ms as i128 * 10_000 + 116_444_736_000_000_000i128;
        if ft > 0 {
            let f = FileTime { lo: (ft & 0xffff_ffff) as u32, hi: (ft >> 32) as u32 };
            let (mut utc, mut loc) = (SystemTime::default(), SystemTime::default());
            // SAFETY: plain out-parameters of two kernel32 calls.
            let ok = unsafe { FileTimeToSystemTime(&f, &mut utc) != 0 && SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut loc) != 0 };
            if ok {
                return (loc.y as i32, loc.mo as u32, loc.d as u32, loc.h as u32, loc.mi as u32, loc.dow as u32);
            }
        }
    }
    utc_parts(ms)
}

/// Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((y + (m <= 2) as i64) as i32, m, d)
}

pub fn utc_parts(ms: i64) -> (i32, u32, u32, u32, u32, u32) {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil(days);
    (y, m, d, (rem / 3600) as u32, ((rem % 3600) / 60) as u32, ((days + 4).rem_euclid(7)) as u32)
}

/// `formatClock`: local `HH:MM`.
pub fn format_clock(ms: i64) -> String {
    let (_, _, _, h, m, _) = local_parts(ms);
    format!("{h:02}:{m:02}")
}

/// Local `YYYY-MM-DD` (the history day key).
pub fn local_day(ms: i64) -> String {
    let (y, m, d, ..) = local_parts(ms);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `formatDayHeader(day, now)`: TODAY / YESTERDAY / `THU 15 JAN 2026` (en-GB, commas dropped, upper case).
pub fn format_day_header(day: &str, now_ms: i64) -> String {
    let mut it = day.split('-').filter_map(|p| p.parse::<i64>().ok());
    let (Some(y), Some(m), Some(d)) = (it.next(), it.next(), it.next()) else { return day.to_string() };
    // days since epoch of a civil date
    let days_from = |y: i64, m: i64, d: i64| {
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y.rem_euclid(400);
        let mp = if m > 2 { m - 3 } else { m + 9 };
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    };
    let this = days_from(y, m, d);
    let (ny, nm, nd, ..) = local_parts(now_ms);
    let diff = days_from(ny as i64, nm as i64, nd as i64) - this;
    match diff {
        0 => "TODAY".into(),
        1 => "YESTERDAY".into(),
        _ => {
            const DOW: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
            const MON: [&str; 12] = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEPT", "OCT", "NOV", "DEC"];
            let dow = (this + 4).rem_euclid(7) as usize;
            format!("{} {} {} {}", DOW[dow], d, MON[(m - 1).clamp(0, 11) as usize], y)
        }
    }
}

/// `formatTotalDuration`: `2 HR 5 MIN` / `37 MIN`.
pub fn format_total_duration(total_seconds: f64) -> String {
    let total_min = (total_seconds / 60.0).round() as i64;
    let (h, m) = (total_min / 60, total_min % 60);
    if h > 0 { format!("{h} HR {m} MIN") } else { format!("{m} MIN") }
}

/// `summarizeFormats`
pub fn summarize_formats(tracks: &[Track]) -> String {
    if tracks.is_empty() {
        return "—".into();
    }
    let mut labels: Vec<String> = Vec::new();
    for t in tracks {
        let l = format_track_format(&t.format);
        if !labels.contains(&l) {
            labels.push(l);
        }
    }
    if labels.len() == 1 {
        return labels.remove(0);
    }
    let mut suffixes: Vec<String> = Vec::new();
    for t in tracks {
        let s = t.format.suffix.to_uppercase();
        if !suffixes.contains(&s) {
            suffixes.push(s);
        }
    }
    if suffixes.len() == 1 { format!("{} · MIXED RATES", suffixes[0]) } else { suffixes.join(" · ") }
}

#[cfg(test)]
mod time_tests {
    use super::*;
    #[test]
    fn civil_and_headers() {
        // 2026-10-31T12:00:00Z = Saturday
        let (y, m, d, h, mi, dow) = utc_parts(1_793_448_000_000);
        assert_eq!((y, m, d, h, mi, dow), (2026, 10, 31, 12, 0, 6));
        assert_eq!(format_day_header("2026-10-31", 1_793_448_000_000), if local_parts(1_793_448_000_000).2 == 31 { "TODAY" } else { "YESTERDAY" });
        assert_eq!(format_day_header("2026-10-18", 1_793_448_000_000), "SUN 18 OCT 2026");
        assert_eq!(format_total_duration(3725.0), "1 HR 2 MIN");
    }
}

/// ISO 8601 UTC timestamp (`2026-10-31T12:00:00.000Z`) of a unix-ms value.
pub fn utc_iso(ms: i64) -> String {
    let (y, mo, d, h, mi, _) = utc_parts(ms);
    let s = ms.div_euclid(1000).rem_euclid(60);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{:03}Z", ms.rem_euclid(1000))
}
// ---- Flow text helpers: real numbers only

fn pct(n: u32, total: u32) -> String {
    if n == 0 || total == 0 {
        return "0%".into();
    }
    let p = ((100.0 * n as f64) / total as f64).round() as u32;
    if p == 0 { "<1%".into() } else { format!("{p}%") }
}

/// How much of the pool Flow actually has data for.
pub fn coverage_hint(c: &FlowCoverage, noun: &str) -> String {
    if c.total == 0 {
        return String::new();
    }
    if c.bpm == 0 {
        if c.loudness > 0 {
            return format!("No BPM tags in this {noun} yet: Flow orders by genre, artist spread and loudness (ReplayGain on {})", pct(c.loudness, c.total));
        }
        return format!("No BPM tags in this {noun} yet: Flow orders by genre and artist spread");
    }
    let key = if c.key > 0 { format!(" · key {}", pct(c.key, c.total)) } else { String::new() };
    format!("BPM known for {} of this {noun}{key}", pct(c.bpm, c.total))
}

/// One compact mono line for a row's `flow` reason: `124→126 BPM · 8A→9A · Techno`; "" when there is nothing to say.
pub fn flow_line(r: Option<&FlowReason>) -> String {
    let Some(r) = r else { return String::new() };
    let mut parts: Vec<String> = Vec::new();
    if let (Some(a), Some(b)) = (r.bpm_from, r.bpm_to) {
        parts.push(if a == b { format!("{b} BPM") } else { format!("{a}→{b} BPM") });
    }
    if let (Some(a), Some(b)) = (&r.key_from, &r.key_to) {
        parts.push(if a == b { b.clone() } else { format!("{a}→{b}") });
    }
    if r.same_genre == Some(true)
        && let Some(g) = &r.genre
    {
        parts.push(g.clone());
    }
    parts.join(" · ")
}

/// Queue rail suffix for a Flow source: ` · Flow` (+ the arc when not steady).
pub fn flow_mode_label(arc: Option<FlowArc>) -> String {
    match arc {
        Some(FlowArc::Build) => " · Flow · Build".into(),
        Some(FlowArc::Peak) => " · Flow · Peak".into(),
        _ => " · Flow".into(),
    }
}

#[cfg(test)]
mod flow_text_tests {
    use super::*;

    #[test]
    fn hint_and_line_match_flow_text_ts() {
        let c = FlowCoverage { total: 200, bpm: 140, key: 56, loudness: 0 };
        assert_eq!(coverage_hint(&c, "view"), "BPM known for 70% of this view · key 28%");
        let r = FlowReason { bpm_from: Some(124), bpm_to: Some(126), key_from: Some("8A".into()), key_to: Some("9A".into()), genre: Some("Techno".into()), same_genre: Some(true) };
        assert_eq!(flow_line(Some(&r)), "124→126 BPM · 8A→9A · Techno");
        assert_eq!(flow_line(None), "");
    }
}
