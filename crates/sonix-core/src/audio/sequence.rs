//! Flow sequencing: orders tracks the way a DJ would, without ever touching
//! the audio path (no crossfade, no beatmatch: bit-perfect and gapless stay
//! exactly as they are). Everything here is pure and I/O-free.
//!
//! **Algorithm.** Flow is drawn lazily, one slot at a time, by `Queue`'s
//! rolling-window refill. For each slot [`pick`] scores up to
//! [`CANDIDATES`] ids taken from the front of the round's (seed-shuffled,
//! deduplicated) `remaining` list against the previously drawn track, then
//! refines the best [`LOOKAHEAD_TOP`] with a one-step lookahead (their
//! cheapest follow-up transition among the same candidates, half weight).
//! Cost per slot is `O(CANDIDATES * LOOKAHEAD_TOP)` whatever the pool size,
//! so a 50k-track library costs the same per refill as a 20-track playlist.
//! The winner is removed from `remaining`, so nothing repeats within a round.
//! Given the same shuffled `remaining` and features the result is
//! deterministic (strict `<`, first minimum wins).
//!
//! **Cost** (lower is better; a term contributes only when *both* tracks
//! have that datum, so missing data is neutral (0), never a penalty or a fake
//! value; with no features at all Flow is a genre-continuous, artist-spread
//! shuffle). Each known term is centred so that a *good* match scores below
//! the neutral 0 (a reward) and a poor one above it; otherwise "unknown"
//! would always beat "known but slightly different" and sparse data would
//! make Flow prefer untagged tracks:
//! - BPM: log-ratio distance to the nearest of 1x / 2x / 0.5x (half/double
//!   time are close), `d / 0.12` capped at 3, plus 0.15 for a half/double
//!   hop, minus 0.6 (break-even at ~7 % apart).
//! - Key (Camelot): same -0.5, relative major/minor -0.4, +-1 same letter
//!   -0.35, +-2 same letter +0.1, anything else +1.0.
//! - Loudness (from ReplayGain): `|delta dB| / 6` capped at 2, weight 0.5,
//!   minus 0.2.
//! - Genre: different (both known) +0.6, same -0.2.
//! - Artist: the same artist as the previous track costs 10, one back 6, two
//!   back 3 (hard-ish: only taken when every candidate collides).
//! - Arc: `4 * |energy - target(position)|` for build/peak, where energy is
//!   the analysis energy if present, else a BPM+loudness proxy, else unknown
//!   unknown. The arc spans [`ARC_SPAN`] tracks and repeats. The arc only
//!   ranks tracks with a known energy against each other; whether this slot
//!   takes a known or an unknown one is decided by [`Quota`] (known tracks
//!   are drawn at their share of the round). A plain penalty (unknown = 0)
//!   pushed every tagged track to the end of the round on a 13 %-tagged
//!   library, any reward pulled them all to the front (`sim13`).

use std::collections::HashMap;
use std::sync::Arc;

use crate::ipc::{FlowArc, FlowCoverage, FlowReason};

/// Candidates scored per slot.
pub const CANDIDATES: usize = 256;
/// How many best-by-first-ply candidates get the lookahead refinement.
pub const LOOKAHEAD_TOP: usize = 6;
/// Tracks per energy arc (the arc repeats after this many).
pub const ARC_SPAN: usize = 40;
/// How many preceding tracks the artist-spread constraint looks back over.
pub const ARTIST_WINDOW: usize = 3;

const W_BPM_CAP: f32 = 3.0;
const BPM_SCALE: f32 = 0.12;
const BPM_REWARD: f32 = 0.6;
const HALF_DOUBLE_PENALTY: f32 = 0.15;
const W_LOUD: f32 = 0.5;
const LOUD_SCALE_DB: f32 = 6.0;
const LOUD_CAP: f32 = 2.0;
const LOUD_REWARD: f32 = 0.2;
const GENRE_DIFF: f32 = 0.6;
const GENRE_SAME: f32 = -0.2;
const W_ARC: f32 = 4.0;
const LOOKAHEAD_WEIGHT: f32 = 0.5;

/// A Camelot-wheel key: number 1-12, `minor` = the "A" ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Camelot {
    pub num: u8,
    pub minor: bool,
}

impl Camelot {
    pub fn label(self) -> String {
        format!("{}{}", self.num, if self.minor { 'A' } else { 'B' })
    }

    /// Accepts Camelot ("8A", "08b"), or standard notation ("Am", "C#m",
    /// "Bb", "F# minor", "C major"). `None` for anything else.
    pub fn parse(raw: &str) -> Option<Camelot> {
        let s = raw.trim();
        if s.is_empty() {
            return None;
        }
        let lower = s.to_ascii_lowercase();
        // Camelot: digits then a/b.
        let digits: String = lower.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            let rest = &lower[digits.len()..];
            let num: u8 = digits.parse().ok()?;
            if !(1..=12).contains(&num) {
                return None;
            }
            return match rest {
                "a" => Some(Camelot { num, minor: true }),
                "b" => Some(Camelot { num, minor: false }),
                _ => None,
            };
        }
        // Standard notation: note letter, optional #/b, optional quality.
        let mut chars = lower.chars();
        let letter = chars.next()?;
        let base: i32 = match letter {
            'c' => 0,
            'd' => 2,
            'e' => 4,
            'f' => 5,
            'g' => 7,
            'a' => 9,
            'b' => 11,
            _ => return None,
        };
        let rest: String = chars.collect();
        let (acc, rest) = if let Some(r) = rest.strip_prefix('#') {
            (1, r.to_string())
        } else if let Some(r) = rest.strip_prefix('\u{266F}') {
            (1, r.to_string())
        } else if let Some(r) = rest.strip_prefix('\u{266D}') {
            (-1, r.to_string())
        } else if let Some(r) = rest.strip_prefix('b') {
            (-1, r.to_string())
        } else {
            (0, rest)
        };
        let q = rest.trim();
        let minor = match q {
            "" | "maj" | "major" => false,
            "m" | "min" | "minor" => true,
            _ => return None,
        };
        let pc = (base + acc).rem_euclid(12);
        // Relative major's pitch class, then position on the circle of fifths.
        let major_pc = if minor { (pc + 3) % 12 } else { pc };
        let fifths = (major_pc * 7) % 12;
        let num = ((fifths + 7) % 12 + 1) as u8;
        Some(Camelot { num, minor })
    }

    /// Wheel distance cost, see the module doc.
    pub fn cost(self, other: Camelot) -> f32 {
        let step = {
            let d = (self.num as i32 - other.num as i32).rem_euclid(12);
            d.min(12 - d)
        };
        match (step, self.minor == other.minor) {
            (0, true) => -0.5,
            (0, false) => -0.4,
            (1, true) => -0.35,
            (2, true) => 0.1,
            _ => 1.0,
        }
    }
}

/// Per-track sequencing data. Every field is optional and real: a `None` is
/// never filled in with a guess.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlowTrack {
    pub bpm: Option<f32>,
    pub key: Option<Camelot>,
    /// Analysis energy 0..1 (nothing writes it today).
    pub energy: Option<f32>,
    /// Estimated loudness from ReplayGain (dB, only differences matter).
    pub loudness: Option<f32>,
    pub genre: Option<Arc<str>>,
    pub artist: Option<Arc<str>>,
}

/// Energy used for the arc: real analysis energy, else a proxy from the
/// real BPM and/or loudness, else unknown. Ordering only, never displayed.
pub fn energy_of(t: &FlowTrack) -> Option<f32> {
    if let Some(e) = t.energy {
        return Some(e.clamp(0.0, 1.0));
    }
    let bpm = t.bpm.map(|b| ((b - 70.0) / 100.0).clamp(0.0, 1.0));
    let loud = t.loudness.map(|l| ((l + 23.0) / 17.0).clamp(0.0, 1.0));
    match (bpm, loud) {
        (Some(a), Some(b)) => Some((a + b) / 2.0),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

/// Target energy at `pos` in 0..=1 along the arc.
pub fn arc_target(arc: FlowArc, pos: f32) -> Option<f32> {
    let p = pos.clamp(0.0, 1.0);
    match arc {
        FlowArc::Steady => None,
        FlowArc::Build => Some(0.15 + 0.8 * p),
        FlowArc::Peak => Some(if p < 0.6 { 0.2 + 0.8 * (p / 0.6) } else { 1.0 - 0.7 * ((p - 0.6) / 0.4) }),
    }
}

/// Position within the repeating arc for the `step`-th pick of a round of
/// `round_total` tracks.
pub fn arc_position(step: usize, round_total: usize) -> f32 {
    let span = round_total.clamp(2, ARC_SPAN);
    (step % span) as f32 / (span - 1) as f32
}

fn bpm_cost(a: f32, b: f32) -> f32 {
    if a <= 0.0 || b <= 0.0 {
        return 0.0;
    }
    let l = (b / a).ln().abs();
    let ln2 = std::f32::consts::LN_2;
    let half = (l - ln2).abs();
    let d = l.min(half);
    let mut c = (d / BPM_SCALE).min(W_BPM_CAP);
    if half < l {
        c += HALF_DOUBLE_PENALTY;
    }
    c - BPM_REWARD
}

/// Transition cost `a -> b` over BPM, key, loudness and genre (no artist, no
/// arc). Terms with a missing side contribute exactly 0.
pub fn transition_cost(a: &FlowTrack, b: &FlowTrack) -> f32 {
    let mut c = 0.0;
    if let (Some(x), Some(y)) = (a.bpm, b.bpm) {
        c += bpm_cost(x, y);
    }
    if let (Some(x), Some(y)) = (a.key, b.key) {
        c += x.cost(y);
    }
    if let (Some(x), Some(y)) = (a.loudness, b.loudness) {
        c += ((x - y).abs() / LOUD_SCALE_DB).min(LOUD_CAP) * W_LOUD - LOUD_REWARD;
    }
    if let (Some(x), Some(y)) = (&a.genre, &b.genre) {
        c += if x.eq_ignore_ascii_case(y) { GENRE_SAME } else { GENRE_DIFF };
    }
    c
}

fn same_artist(a: &FlowTrack, b: &FlowTrack) -> bool {
    matches!((&a.artist, &b.artist), (Some(x), Some(y)) if x.eq_ignore_ascii_case(y))
}

/// What a slot is being picked for.
pub struct Slot<'a> {
    /// Previously drawn track id, if any.
    pub prev: Option<&'a str>,
    /// Up to [`ARTIST_WINDOW`] most recent ids, newest first (includes `prev`).
    pub recent: &'a [&'a str],
    pub arc: FlowArc,
    /// Position in the arc, 0..=1 (see [`arc_position`]).
    pub arc_pos: f32,
    /// Build/Peak: which kind of track the quota wants for this slot
    /// ([`Quota::lean`]). Ignored for Steady.
    pub lean: Lean,
}

/// True when the arc can place this track (real or proxy energy known).
pub fn energy_known(t: &FlowTrack) -> bool {
    energy_of(t).is_some()
}

/// What the Build/Peak quota wants from the next slot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Lean {
    /// Either: unknown energy scores like the best known fit.
    #[default]
    Neutral,
    /// Known-energy tracks are behind their share: take one.
    Known,
    /// More than [`QUOTA_SLACK`] ahead of their share: take an unknown one.
    Unknown,
}

/// How many known-energy tracks the quota lets run ahead of their share
/// before it leans to unknown ones (transitions decide within this slack).
pub const QUOTA_SLACK: usize = 2;

/// Per-round quota for Build/Peak: how many of the round's tracks have a
/// known energy (`known`) and how many of those were drawn so far (`drawn`).
/// Keeps them at their share of the round, so the arc neither pushes the
/// tagged tracks to the end nor pulls them all to the front.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Quota {
    pub known: usize,
    pub drawn: usize,
}

impl Quota {
    /// The lean for the pick at `step` (0-based) of a `round_total`-track round.
    pub fn lean(&self, step: usize, round_total: usize) -> Lean {
        let t = round_total.max(1) as u64;
        let drawn = self.drawn as u64 * t;
        let expected = self.known as u64 * (step as u64 + 1);
        if self.drawn < self.known && drawn < expected {
            Lean::Known
        } else if drawn > expected + QUOTA_SLACK as u64 * t {
            Lean::Unknown
        } else {
            Lean::Neutral
        }
    }

    /// Records a drawn track.
    pub fn note(&mut self, t: Option<&FlowTrack>) {
        if t.is_some_and(energy_known) {
            self.drawn += 1;
        }
    }
}

static EMPTY: FlowTrack = FlowTrack { bpm: None, key: None, energy: None, loudness: None, genre: None, artist: None };

fn feat<'a>(feats: &'a HashMap<String, FlowTrack>, id: &str) -> &'a FlowTrack {
    feats.get(id).unwrap_or(&EMPTY)
}

/// Index into `cands` of the best next track. `cands` must be non-empty.
pub fn pick(feats: &HashMap<String, FlowTrack>, cands: &[&str], slot: &Slot) -> usize {
    debug_assert!(!cands.is_empty());
    let prev = slot.prev.map(|p| feat(feats, p));
    let recent: Vec<&FlowTrack> = slot.recent.iter().take(ARTIST_WINDOW).map(|id| feat(feats, id)).collect();
    let target = arc_target(slot.arc, slot.arc_pos);
    // Arc distance of each candidate (None = energy unknown, or no arc). An
    // unknown energy is scored relative to the best known fit, as the quota
    // leans ([`Lean`]): the arc ranks the known tracks, the quota decides
    // known vs unknown.
    let arc_cost: Vec<Option<f32>> = match target {
        Some(tg) => cands.iter().map(|id| energy_of(feat(feats, id)).map(|e| W_ARC * (e - tg).abs())).collect(),
        None => vec![None; cands.len()],
    };
    let best_known = arc_cost.iter().flatten().copied().fold(f32::INFINITY, f32::min);
    let unknown_arc = if !best_known.is_finite() {
        0.0
    } else {
        match slot.lean {
            Lean::Neutral => best_known,
            // Worse than any known fit (arc costs span at most W_ARC).
            Lean::Known => best_known + W_ARC + 1.0,
            // Better than the best known fit by more than the transition
            // rewards a known pair can add (BPM, loudness, lookahead).
            Lean::Unknown => best_known - 2.5,
        }
    };

    let first_ply = |i: usize| -> f32 {
        let id = cands[i];
        let t = feat(feats, id);
        let mut c = 0.0;
        if slot.prev == Some(id) {
            c += 100.0; // never the track that just played, unless it is the only one
        }
        if let Some(p) = prev {
            c += transition_cost(p, t);
        }
        for (back, r) in recent.iter().enumerate() {
            if same_artist(r, t) {
                c += 10.0 - 3.5 * back as f32;
                break;
            }
        }
        if target.is_some() {
            c += arc_cost[i].unwrap_or(unknown_arc);
        }
        c
    };

    let scores: Vec<f32> = (0..cands.len()).map(first_ply).collect();
    if cands.len() == 1 {
        return 0;
    }
    // Top-N by first-ply cost (stable: earlier index wins ties).
    let mut order: Vec<usize> = (0..cands.len()).collect();
    let top = LOOKAHEAD_TOP.min(order.len());
    order.select_nth_unstable_by(top - 1, |&a, &b| scores[a].total_cmp(&scores[b]).then(a.cmp(&b)));
    order.truncate(top);

    let mut best = order[0];
    let mut best_total = f32::INFINITY;
    for &i in &order {
        let ti = feat(feats, cands[i]);
        let mut follow = f32::INFINITY;
        for (j, id) in cands.iter().enumerate() {
            if j == i {
                continue;
            }
            let tj = feat(feats, id);
            let mut c = transition_cost(ti, tj);
            if same_artist(ti, tj) {
                c += 10.0;
            }
            if c < follow {
                follow = c;
            }
        }
        let total = scores[i] + LOOKAHEAD_WEIGHT * follow;
        if total < best_total || (total == best_total && i < best) {
            best_total = total;
            best = i;
        }
    }
    best
}

/// Why `cur` follows `prev`: real values only, `None` when there is nothing
/// real to say.
pub fn reason(prev: &FlowTrack, cur: &FlowTrack) -> Option<FlowReason> {
    let mut r = FlowReason::default();
    if let (Some(a), Some(b)) = (prev.bpm, cur.bpm) {
        r.bpm_from = Some(a.round() as u32);
        r.bpm_to = Some(b.round() as u32);
    }
    if let (Some(a), Some(b)) = (prev.key, cur.key) {
        r.key_from = Some(a.label());
        r.key_to = Some(b.label());
    }
    if let (Some(a), Some(b)) = (&prev.genre, &cur.genre) {
        if a.eq_ignore_ascii_case(b) {
            r.same_genre = Some(true);
            r.genre = Some(b.to_string());
        }
    }
    (r != FlowReason::default()).then_some(r)
}

/// Real coverage counts over a set of tracks.
pub fn coverage<'a>(tracks: impl Iterator<Item = &'a FlowTrack>) -> FlowCoverage {
    let mut c = FlowCoverage::default();
    for t in tracks {
        c.total += 1;
        c.bpm += t.bpm.is_some() as u32;
        c.key += t.key.is_some() as u32;
        c.loudness += t.loudness.is_some() as u32;
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::SmallRng;
    use rand::seq::SliceRandom;
    use rand::{Rng as _, SeedableRng};

    fn bpm(b: f32) -> FlowTrack {
        FlowTrack { bpm: Some(b), ..Default::default() }
    }
    fn key(k: &str) -> FlowTrack {
        FlowTrack { key: Camelot::parse(k), ..Default::default() }
    }

    #[test]
    fn bpm_distance_treats_half_and_double_time_as_close() {
        let same = transition_cost(&bpm(124.0), &bpm(124.0));
        let near = transition_cost(&bpm(124.0), &bpm(126.0));
        let half = transition_cost(&bpm(128.0), &bpm(64.0));
        let double = transition_cost(&bpm(70.0), &bpm(140.0));
        let far = transition_cost(&bpm(124.0), &bpm(150.0));
        assert!(same < near && near < 0.0, "a good match is rewarded: {same} {near}");
        assert!(half < 0.0 && double < 0.0, "{half} {double}");
        assert!(half > same, "a half-time hop is not free");
        assert!(far > 0.5, "{far}");
        // Not a plain difference: 64 -> 128 is a 64 bpm gap but cheap.
        assert!(transition_cost(&bpm(64.0), &bpm(128.0)) < transition_cost(&bpm(100.0), &bpm(140.0)));
    }

    #[test]
    fn camelot_parsing_and_wheel_costs() {
        assert_eq!(Camelot::parse("8A"), Some(Camelot { num: 8, minor: true }));
        assert_eq!(Camelot::parse("08b"), Some(Camelot { num: 8, minor: false }));
        assert_eq!(Camelot::parse("Am"), Camelot::parse("8A"));
        assert_eq!(Camelot::parse("C"), Camelot::parse("8B"));
        assert_eq!(Camelot::parse("C major"), Camelot::parse("8B"));
        assert_eq!(Camelot::parse("G"), Camelot::parse("9B"));
        assert_eq!(Camelot::parse("F"), Camelot::parse("7B"));
        assert_eq!(Camelot::parse("Em"), Camelot::parse("9A"));
        assert_eq!(Camelot::parse("F#m"), Camelot::parse("11A"));
        assert_eq!(Camelot::parse("Db"), Camelot::parse("3B"));
        assert_eq!(Camelot::parse("Bbm"), Camelot::parse("3A"));
        assert_eq!(Camelot::parse("Bm"), Camelot::parse("10A"));
        assert_eq!(Camelot::parse("B"), Camelot::parse("1B"));
        assert_eq!(Camelot::parse("13A"), None);
        assert_eq!(Camelot::parse("H"), None);
        assert_eq!(Camelot::parse(""), None);
        assert_eq!(Camelot::parse("8C"), None);
        let k = |s: &str| Camelot::parse(s).unwrap();
        assert!(k("8A").cost(k("8A")) < k("8A").cost(k("8B")), "identical is best");
        assert!(k("8A").cost(k("9A")) < k("8A").cost(k("10A")));
        assert!(k("8A").cost(k("8B")) < k("8A").cost(k("9A")), "relative key is the closest non-identical");
        assert_eq!(k("12A").cost(k("1A")), k("8A").cost(k("9A")), "the wheel wraps");
        assert!(k("8A").cost(k("2B")) > k("8A").cost(k("10A")));
        assert!(transition_cost(&key("8A"), &key("9A")) < transition_cost(&key("8A"), &key("3B")));
    }

    #[test]
    fn missing_features_contribute_nothing() {
        let none = FlowTrack::default();
        let full = FlowTrack {
            bpm: Some(174.0),
            key: Camelot::parse("5A"),
            energy: Some(0.9),
            loudness: Some(-9.0),
            genre: Some("Techno".into()),
            artist: Some("A".into()),
        };
        assert_eq!(transition_cost(&none, &full), 0.0);
        assert_eq!(transition_cost(&full, &none), 0.0);
        assert_eq!(transition_cost(&none, &none), 0.0);
        // One side only has bpm, the other only key: no shared datum.
        assert_eq!(transition_cost(&bpm(100.0), &key("8A")), 0.0);
        assert!(reason(&none, &full).is_none());
        assert!(reason(&bpm(100.0), &key("8A")).is_none());
        assert_eq!(energy_of(&none), None);
    }

    #[test]
    fn reasons_carry_only_real_values() {
        let a = FlowTrack { bpm: Some(123.6), key: Camelot::parse("8A"), genre: Some("Techno".into()), ..Default::default() };
        let b = FlowTrack { bpm: Some(126.0), key: Camelot::parse("9A"), genre: Some("techno".into()), ..Default::default() };
        let r = reason(&a, &b).unwrap();
        assert_eq!((r.bpm_from, r.bpm_to), (Some(124), Some(126)));
        assert_eq!((r.key_from.as_deref(), r.key_to.as_deref()), (Some("8A"), Some("9A")));
        assert_eq!(r.same_genre, Some(true));
        assert_eq!(r.genre.as_deref(), Some("techno"));
        let c = FlowTrack { genre: Some("House".into()), ..Default::default() };
        let r2 = reason(&a, &c).map(|r| r.same_genre);
        assert_eq!(r2, None, "different genre says nothing");
        let json = serde_json::to_string(&reason(&bpm(120.0), &bpm(121.0)).unwrap()).unwrap();
        assert_eq!(json, r#"{"bpmFrom":120,"bpmTo":121}"#);
    }

    #[test]
    fn arc_shapes() {
        assert_eq!(arc_target(FlowArc::Steady, 0.5), None);
        let b0 = arc_target(FlowArc::Build, 0.0).unwrap();
        let b1 = arc_target(FlowArc::Build, 1.0).unwrap();
        assert!(b1 > b0 + 0.6);
        let mut prev = -1.0;
        for i in 0..=10 {
            let v = arc_target(FlowArc::Build, i as f32 / 10.0).unwrap();
            assert!(v > prev);
            prev = v;
        }
        let p = |x: f32| arc_target(FlowArc::Peak, x).unwrap();
        assert!(p(0.6) > p(0.0) && p(0.6) > p(1.0), "peak rises then cools");
        assert!(p(0.3) < p(0.6) && p(0.8) > p(1.0));
        assert_eq!(arc_position(0, 100), 0.0);
        assert_eq!(arc_position(ARC_SPAN - 1, 100), 1.0);
        assert_eq!(arc_position(ARC_SPAN, 100), 0.0, "the arc repeats");
        assert_eq!(arc_position(3, 1), 1.0, "tiny rounds clamp the span to 2");
    }

    #[test]
    fn energy_proxy_uses_only_what_is_known() {
        assert_eq!(energy_of(&FlowTrack { energy: Some(0.4), bpm: Some(170.0), ..Default::default() }), Some(0.4));
        let slow = energy_of(&bpm(75.0)).unwrap();
        let fast = energy_of(&bpm(160.0)).unwrap();
        assert!(fast > slow);
        let loud = energy_of(&FlowTrack { loudness: Some(-8.0), ..Default::default() }).unwrap();
        assert!(loud > 0.8);
    }

    fn feats_from(v: Vec<(&str, FlowTrack)>) -> HashMap<String, FlowTrack> {
        v.into_iter().map(|(k, t)| (k.to_string(), t)).collect()
    }

    fn slot<'a>(prev: Option<&'a str>, recent: &'a [&'a str], arc: FlowArc, pos: f32) -> Slot<'a> {
        Slot { prev, recent, arc, arc_pos: pos, lean: Lean::Neutral }
    }

    #[test]
    fn pick_prefers_the_smoothest_transition() {
        let f = feats_from(vec![("p", bpm(124.0)), ("far", bpm(160.0)), ("near", bpm(125.0)), ("mid", bpm(135.0))]);
        let cands = ["far", "mid", "near"];
        let i = pick(&f, &cands, &slot(Some("p"), &["p"], FlowArc::Steady, 0.0));
        assert_eq!(cands[i], "near");
    }

    #[test]
    fn pick_avoids_recent_artists_and_the_track_that_just_played() {
        let a = |artist: &str, b: f32| FlowTrack { artist: Some(artist.into()), bpm: Some(b), ..Default::default() };
        let f = feats_from(vec![("p", a("X", 124.0)), ("same", a("X", 124.0)), ("other", a("Y", 140.0))]);
        let cands = ["same", "other"];
        let i = pick(&f, &cands, &slot(Some("p"), &["p"], FlowArc::Steady, 0.0));
        assert_eq!(cands[i], "other", "artist spread beats a perfect tempo match");
        // Same id as prev is only taken when it is the only choice.
        let g = feats_from(vec![("p", bpm(120.0)), ("q", bpm(200.0))]);
        let i = pick(&g, &["p", "q"], &slot(Some("p"), &["p"], FlowArc::Steady, 0.0));
        assert_eq!(i, 1);
        assert_eq!(pick(&g, &["p"], &slot(Some("p"), &["p"], FlowArc::Steady, 0.0)), 0);
    }

    #[test]
    fn build_arc_starts_low_and_peak_arc_ends_cool() {
        let f = feats_from(vec![("lo", bpm(80.0)), ("mid", bpm(120.0)), ("hi", bpm(165.0))]);
        let cands = ["hi", "mid", "lo"];
        let start = pick(&f, &cands, &slot(None, &[], FlowArc::Build, 0.0));
        assert_eq!(cands[start], "lo");
        let end = pick(&f, &cands, &slot(None, &[], FlowArc::Build, 1.0));
        assert_eq!(cands[end], "hi");
        let cool = pick(&f, &cands, &slot(None, &[], FlowArc::Peak, 1.0));
        assert_ne!(cands[cool], "hi", "peak cools down at the end of the arc");
        // Steady has no arc term: with no prev every candidate ties -> first.
        let st = pick(&f, &cands, &slot(None, &[], FlowArc::Steady, 1.0));
        assert_eq!(st, 0);
    }

    #[test]
    fn arc_is_neutral_without_any_energy_data() {
        let f = feats_from(vec![("a", FlowTrack::default()), ("b", FlowTrack::default())]);
        let i = pick(&f, &["a", "b"], &slot(None, &[], FlowArc::Build, 0.0));
        assert_eq!(i, 0);
    }

    /// Drains a whole round the way `Queue` does, for the property tests.
    pub(super) fn run_round(feats: &HashMap<String, FlowTrack>, ids: &[String], seed: u64, arc: FlowArc) -> Vec<String> {
        let mut remaining: Vec<String> = ids.to_vec();
        remaining.shuffle(&mut SmallRng::seed_from_u64(seed));
        let total = remaining.len();
        let mut quota = Quota { known: remaining.iter().filter(|id| feats.get(*id).is_some_and(energy_known)).count(), drawn: 0 };
        let mut out: Vec<String> = Vec::new();
        while !remaining.is_empty() {
            let k = remaining.len().min(CANDIDATES);
            let cands: Vec<&str> = remaining.iter().take(k).map(String::as_str).collect();
            let recent: Vec<&str> = out.iter().rev().take(ARTIST_WINDOW).map(String::as_str).collect();
            let step = total - remaining.len();
            let s = Slot {
                prev: out.last().map(String::as_str),
                recent: &recent,
                arc,
                arc_pos: arc_position(step, total),
                lean: quota.lean(step, total),
            };
            let i = pick(feats, &cands, &s);
            let id = remaining.remove(i);
            quota.note(feats.get(&id));
            out.push(id);
        }
        out
    }

    fn synthetic(n: usize, seed: u64) -> (Vec<String>, HashMap<String, FlowTrack>) {
        let mut rng = SmallRng::seed_from_u64(seed);
        let genres: Vec<Arc<str>> = ["Techno", "House", "Ambient", "Rock", "Jazz"].iter().map(|g| Arc::from(*g)).collect();
        let ids: Vec<String> = (0..n).map(|i| format!("t{i}")).collect();
        let mut m = HashMap::new();
        for id in &ids {
            m.insert(
                id.clone(),
                FlowTrack {
                    bpm: rng.gen_bool(0.6).then(|| rng.gen_range(70.0..170.0)),
                    key: rng.gen_bool(0.4).then(|| Camelot { num: rng.gen_range(1..=12), minor: rng.gen_bool(0.5) }),
                    energy: None,
                    loudness: rng.gen_bool(0.3).then(|| rng.gen_range(-16.0..-8.0)),
                    genre: rng.gen_bool(0.9).then(|| genres[rng.gen_range(0..genres.len())].clone()),
                    artist: Some(Arc::from(format!("artist{}", rng.gen_range(0..(n / 6).max(2))).as_str())),
                },
            );
        }
        (ids, m)
    }

    #[test]
    fn a_round_is_a_permutation_with_no_repeats_and_is_deterministic_per_seed() {
        let (ids, f) = synthetic(300, 1);
        let a = run_round(&f, &ids, 42, FlowArc::Steady);
        let b = run_round(&f, &ids, 42, FlowArc::Steady);
        let c = run_round(&f, &ids, 43, FlowArc::Steady);
        assert_eq!(a, b, "same seed, same order");
        assert_ne!(a, c);
        let mut sorted = a.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
    }

    #[test]
    fn flow_beats_a_shuffle_on_transition_cost_and_artist_adjacency() {
        let (ids, f) = synthetic(400, 2);
        let flow = run_round(&f, &ids, 7, FlowArc::Steady);
        let mut shuffled = ids.clone();
        shuffled.shuffle(&mut SmallRng::seed_from_u64(7));
        let total = |v: &[String]| -> f32 { v.windows(2).map(|w| transition_cost(&f[&w[0]], &f[&w[1]])).sum() };
        let adjacent = |v: &[String]| v.windows(2).filter(|w| same_artist(&f[&w[0]], &f[&w[1]])).count();
        assert!(total(&flow) < total(&shuffled) * 0.5, "{} vs {}", total(&flow), total(&shuffled));
        assert!(adjacent(&flow) <= adjacent(&shuffled) / 3 + 1, "{} vs {}", adjacent(&flow), adjacent(&shuffled));
    }

    #[test]
    fn with_no_features_flow_is_just_an_artist_spread_shuffle() {
        let ids: Vec<String> = (0..60).map(|i| format!("t{i}")).collect();
        let f: HashMap<String, FlowTrack> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| (id.clone(), FlowTrack { artist: Some(format!("a{}", i % 12).as_str().into()), ..Default::default() }))
            .collect();
        let v = run_round(&f, &ids, 3, FlowArc::Peak);
        let adj = v.windows(2).filter(|w| same_artist(&f[&w[0]], &f[&w[1]])).count();
        assert_eq!(adj, 0);
        let mut s = v.clone();
        s.sort();
        s.dedup();
        assert_eq!(s.len(), 60);
        assert_ne!(v, ids, "still shuffled");
    }

    #[test]
    fn build_arc_rises_across_the_arc_span() {
        let (ids, mut f) = synthetic(500, 5);
        for (i, id) in ids.iter().enumerate() {
            let t = f.get_mut(id).unwrap();
            t.bpm = Some(70.0 + ((i * 37) % 100) as f32);
            t.loudness = None;
            t.key = None;
            t.genre = None;
        }
        let v = run_round(&f, &ids, 9, FlowArc::Build);
        let e = |i: usize| energy_of(&f[&v[i]]).unwrap();
        let first: f32 = (0..8).map(e).sum::<f32>() / 8.0;
        let last: f32 = (ARC_SPAN - 8..ARC_SPAN).map(e).sum::<f32>() / 8.0;
        assert!(last > first + 0.3, "{first} -> {last}");
    }

    #[test]
    fn coverage_counts_real_data() {
        let v = [bpm(100.0), FlowTrack::default(), FlowTrack { bpm: Some(90.0), key: Camelot::parse("1A"), loudness: Some(-10.0), ..Default::default() }];
        let c = coverage(v.iter());
        assert_eq!((c.total, c.bpm, c.key, c.loudness), (3, 2, 1, 1));
    }

    /// Release-only timing (debug builds are far slower):
    /// `cargo test --release flow_timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn flow_timing_2k_and_50k() {
        for n in [2_000usize, 50_000] {
            let (ids, f) = synthetic(n, 11);
            let mut shuffled: Vec<String> = ids.clone();
            shuffled.shuffle(&mut SmallRng::seed_from_u64(1));
            // A VecDeque like `Queue` uses (removal near the front is cheap).
            let mut remaining: std::collections::VecDeque<String> = shuffled.into();
            let mut out: Vec<String> = Vec::new();
            let t0 = std::time::Instant::now();
            let mut worst = std::time::Duration::ZERO;
            // A window refill = 20 picks; time each batch and the whole round.
            while !remaining.is_empty() {
                let b0 = std::time::Instant::now();
                for _ in 0..20 {
                    if remaining.is_empty() {
                        break;
                    }
                    let k = remaining.len().min(CANDIDATES);
                    let cands: Vec<&str> = remaining.iter().take(k).map(String::as_str).collect();
                    let recent: Vec<&str> = out.iter().rev().take(ARTIST_WINDOW).map(String::as_str).collect();
                    let s = Slot { prev: out.last().map(String::as_str), recent: &recent, arc: FlowArc::Peak, arc_pos: arc_position(out.len(), n), lean: Lean::Neutral };
                    let i = pick(&f, &cands, &s);
                    let id = remaining.remove(i).unwrap();
                    out.push(id);
                }
                worst = worst.max(b0.elapsed());
            }
            println!("flow n={n}: whole round {:?}, worst 20-pick refill {:?}", t0.elapsed(), worst);
        }
    }

    #[test]
    fn a_refill_of_a_big_pool_stays_cheap_even_in_debug() {
        let (ids, f) = synthetic(50_000, 12);
        let mut remaining: Vec<String> = ids;
        remaining.shuffle(&mut SmallRng::seed_from_u64(1));
        let mut out: Vec<String> = Vec::new();
        let t0 = std::time::Instant::now();
        for _ in 0..20 {
            let cands: Vec<&str> = remaining.iter().take(CANDIDATES).map(String::as_str).collect();
            let recent: Vec<&str> = out.iter().rev().take(ARTIST_WINDOW).map(String::as_str).collect();
            let s = Slot { prev: out.last().map(String::as_str), recent: &recent, arc: FlowArc::Build, arc_pos: 0.3, lean: Lean::Neutral };
            let i = pick(&f, &cands, &s);
            out.push(remaining.remove(i));
        }
        // Loose on purpose (debug build, loaded CI machine); release is ~100x less.
        assert!(t0.elapsed() < std::time::Duration::from_millis(500), "{:?}", t0.elapsed());
    }
}

/// The "13 % tagged" scenario (the real library: BPM on ~13 %, ReplayGain on
/// ~25 %, no keys): where do tracks with a known energy land in a round, and
/// does the arc still shape them? `sim_13pct` prints the table
/// (`cargo test --release --lib sim_13pct -- --ignored --nocapture`);
/// `sparse_energy_*` asserts the same properties on a smaller pool.
#[cfg(test)]
mod sim13 {
    use super::tests::run_round;
    use super::*;
    use rand::rngs::SmallRng;
    use rand::seq::SliceRandom;
    use rand::{Rng as _, SeedableRng};

    fn library(n: usize, seed: u64) -> (Vec<String>, HashMap<String, FlowTrack>) {
        let mut rng = SmallRng::seed_from_u64(seed);
        let genres: Vec<Arc<str>> = (0..30).map(|g| Arc::from(format!("g{g}").as_str())).collect();
        let ids: Vec<String> = (0..n).map(|i| format!("t{i}")).collect();
        let mut m = HashMap::new();
        for id in &ids {
            m.insert(
                id.clone(),
                FlowTrack {
                    bpm: rng.gen_bool(0.13).then(|| rng.gen_range(80.0..170.0)),
                    key: None,
                    energy: None,
                    loudness: rng.gen_bool(0.25).then(|| rng.gen_range(-14.0..-6.0)),
                    genre: rng.gen_bool(0.9).then(|| genres[(rng.gen_range(0.0f64..1.0).powi(2) * 30.0) as usize].clone()),
                    artist: Some(Arc::from(format!("a{}", rng.gen_range(0..300)).as_str())),
                },
            );
        }
        (ids, m)
    }

    struct Stats {
        /// Share of energy-known tracks in the first / last fifth of the round.
        known_first: f32,
        known_last: f32,
        /// Same for BPM-tagged tracks.
        bpm_first: f32,
        bpm_last: f32,
        /// Mean |energy - arc target| over the known tracks.
        fit: f32,
        /// Mean energy of known tracks in the first / last quarter of each arc span.
        span_lo: f32,
        span_hi: f32,
        /// Mean energy of known tracks around 60 % of each span (Peak's top).
        span_mid: f32,
    }

    fn stats(f: &HashMap<String, FlowTrack>, v: &[String], arc: FlowArc) -> Stats {
        let n = v.len();
        let fifth = n / 5;
        let share = |r: std::ops::Range<usize>, p: &dyn Fn(&FlowTrack) -> bool| {
            r.clone().filter(|&i| p(&f[&v[i]])).count() as f32 / r.len().max(1) as f32
        };
        let span = n.clamp(2, ARC_SPAN);
        let mut fit = (0.0, 0);
        let (mut lo, mut hi, mut mid) = ((0.0, 0), (0.0, 0), (0.0, 0));
        for (i, id) in v.iter().enumerate() {
            let Some(e) = energy_of(&f[id]) else { continue };
            if let Some(tg) = arc_target(arc, arc_position(i, n)) {
                fit = (fit.0 + (e - tg).abs(), fit.1 + 1);
            }
            let p = (i % span) as f32 / (span - 1) as f32;
            if p < 0.25 {
                lo = (lo.0 + e, lo.1 + 1);
            } else if p > 0.75 {
                hi = (hi.0 + e, hi.1 + 1);
            }
            if (0.45..=0.7).contains(&p) {
                mid = (mid.0 + e, mid.1 + 1);
            }
        }
        let avg = |(s, c): (f32, i32)| s / c.max(1) as f32;
        Stats {
            known_first: share(0..fifth, &energy_known),
            known_last: share(n - fifth..n, &energy_known),
            bpm_first: share(0..fifth, &|t: &FlowTrack| t.bpm.is_some()),
            bpm_last: share(n - fifth..n, &|t: &FlowTrack| t.bpm.is_some()),
            fit: avg(fit),
            span_lo: avg(lo),
            span_hi: avg(hi),
            span_mid: avg(mid),
        }
    }

    #[test]
    #[ignore]
    fn sim_13pct() {
        let (ids, f) = library(2000, 1);
        let known = ids.iter().filter(|id| energy_known(&f[*id])).count();
        let bpm = ids.iter().filter(|id| f[*id].bpm.is_some()).count();
        println!("pool 2000: energy known {known} ({:.1}%), bpm {bpm} ({:.1}%)", known as f32 / 20.0, bpm as f32 / 20.0);
        println!("arc     seed  known first400 last400  bpm first400 last400  fit    span lo/mid/hi");
        for arc in [FlowArc::Steady, FlowArc::Build, FlowArc::Peak] {
            for seed in 0..5u64 {
                let s = stats(&f, &run_round(&f, &ids, seed, arc), arc);
                println!(
                    "{arc:<7?} {seed}     {:>5.1}%  {:>5.1}%       {:>5.1}%  {:>5.1}%   {:.3}  {:.2}/{:.2}/{:.2}",
                    s.known_first * 100.0, s.known_last * 100.0, s.bpm_first * 100.0, s.bpm_last * 100.0, s.fit, s.span_lo, s.span_mid, s.span_hi
                );
            }
        }
        let mut sh = ids.clone();
        sh.shuffle(&mut SmallRng::seed_from_u64(9));
        for arc in [FlowArc::Build, FlowArc::Peak] {
            let s = stats(&f, &sh, arc);
            println!("shuffle vs {arc:?}: fit {:.3}, span lo/mid/hi {:.2}/{:.2}/{:.2}", s.fit, s.span_lo, s.span_mid, s.span_hi);
        }
    }

    #[test]
    fn sparse_energy_keeps_known_tracks_at_their_share_and_still_follows_the_arc() {
        let (ids, f) = library(800, 2);
        let base = ids.iter().filter(|id| energy_known(&f[*id])).count() as f32 / ids.len() as f32;
        let mut sh = ids.clone();
        sh.shuffle(&mut SmallRng::seed_from_u64(3));
        for arc in [FlowArc::Build, FlowArc::Peak] {
            let random = stats(&f, &sh, arc);
            for seed in 0..2u64 {
                let s = stats(&f, &run_round(&f, &ids, seed, arc), arc);
                assert!((s.known_first - base).abs() < 0.08, "{arc:?}/{seed}: first fifth {} vs base {base}", s.known_first);
                assert!((s.known_last - base).abs() < 0.08, "{arc:?}/{seed}: last fifth {} vs base {base}", s.known_last);
                assert!(s.fit < random.fit * 0.6, "{arc:?}/{seed}: arc fit {} vs shuffle {}", s.fit, random.fit);
                match arc {
                    FlowArc::Build => assert!(s.span_hi > s.span_lo + 0.2, "{arc:?}/{seed}: {} -> {}", s.span_lo, s.span_hi),
                    _ => assert!(s.span_mid > s.span_lo + 0.15 && s.span_mid > s.span_hi, "{arc:?}/{seed}: {}/{}/{}", s.span_lo, s.span_mid, s.span_hi),
                }
            }
        }
    }
}
