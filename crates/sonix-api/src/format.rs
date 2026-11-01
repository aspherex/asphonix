//! Display formatting shared by every view (mirrors `src/lib/format.ts`).
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

/// Top-bar compact count (`short()` in TopBar.svelte): full below 10k, else `51.9K`.
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

/// JS `Number(x.toFixed(1))`: one decimal, no trailing ".0".
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
    fn readouts_match_the_svelte_formatters() {
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

// ---- Flow text helpers (`lib/flowText.ts`): real numbers only

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
