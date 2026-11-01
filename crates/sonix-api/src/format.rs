//! Display formatting shared by every view (mirrors `src/lib/format.ts`).
use crate::types::{Track, TrackFormat};

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
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Top-bar compact count (`short()` in TopBar.svelte): full below 10k, else `51.9K`.
pub fn short_count(n: u64) -> String {
    if n < 10_000 {
        return fmt_int(n);
    }
    let (v, unit) = if n >= 1_000_000 { (n as f64 / 1e6, "M") } else { (n as f64 / 1e3, "K") };
    let r = (v * 10.0).round() / 10.0;
    if r.fract() == 0.0 { format!("{}{unit}", r as u64) } else { format!("{r}{unit}") }
}

#[cfg(test)]
mod tests {
    use super::*;
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
