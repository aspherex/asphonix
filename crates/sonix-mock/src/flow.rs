//! Port of `src/lib/ipc/mockFlow.ts` (itself a mirror of `audio/sequence.rs`): same cost terms and weights, same
//! candidate sampling + one-step lookahead, same Build/Peak quota. Features are deterministic per track
//! (BPM on ~70 %, a key on ~28 %, loudness from the mock ReplayGain).
use crate::library::{Library, Rng};
use sonix_api::{FlowArc, FlowCoverage, FlowReason};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Key {
    pub num: u8,
    pub minor: bool,
}

#[derive(Clone, Debug, Default)]
pub struct FlowTrack {
    pub bpm: Option<f64>,
    pub key: Option<Key>,
    pub loudness: Option<f64>,
    pub genre: Option<String>,
    pub artist: Option<String>,
}

pub const CANDIDATES: usize = 256;
const LOOKAHEAD_TOP: usize = 6;
pub const ARC_SPAN: usize = 40;
pub const ARTIST_WINDOW: usize = 3;
pub const QUOTA_SLACK: usize = 2;

/// Per-track feature cache (the mock holds at most what a session touched).
#[derive(Default)]
pub struct Features {
    cache: HashMap<u32, Arc<FlowTrack>>,
}

impl Features {
    /// `mockFlowFeatures`
    pub fn get(&mut self, lib: &Library, ix: u32) -> Arc<FlowTrack> {
        if let Some(f) = self.cache.get(&ix) {
            return f.clone();
        }
        let t = lib.track(ix as usize);
        let mut r = Rng::for_seed(ix as u64 * 131 + 71);
        let mut f = FlowTrack { genre: Some(t.genre.clone().unwrap_or_default()).filter(|g| !g.is_empty()), artist: Some(t.artist.clone()), ..Default::default() };
        if r.next() < 0.7 {
            f.bpm = Some((78.0 + r.next() * 95.0).round());
            if r.next() < 0.4 {
                let num = 1 + (r.next() * 12.0).floor() as u8;
                let minor = r.next() < 0.5;
                f.key = Some(Key { num, minor });
            }
        }
        if let Some(g) = t.replay_gain.as_ref().and_then(|g| g.track_gain_db) {
            f.loudness = Some(-18.0 - g);
        }
        let f = Arc::new(f);
        self.cache.insert(ix, f.clone());
        f
    }
}

pub fn key_label(k: Key) -> String {
    format!("{}{}", k.num, if k.minor { "A" } else { "B" })
}

fn key_cost(a: Key, b: Key) -> f64 {
    let d = ((a.num as i32 - b.num as i32) % 12 + 12) % 12;
    let step = d.min(12 - d);
    let same = a.minor == b.minor;
    if step == 0 {
        return if same { -0.5 } else { -0.4 };
    }
    if step == 1 && same {
        return -0.35;
    }
    if step == 2 && same {
        return 0.1;
    }
    1.0
}

fn bpm_cost(a: f64, b: f64) -> f64 {
    if a <= 0.0 || b <= 0.0 {
        return 0.0;
    }
    let l = (b / a).ln().abs();
    let half = (l - std::f64::consts::LN_2).abs();
    let mut c = (l.min(half) / 0.12).min(3.0);
    if half < l {
        c += 0.15;
    }
    c - 0.6
}

fn same(a: &Option<String>, b: &Option<String>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => a.to_lowercase() == b.to_lowercase(),
        _ => false,
    }
}

pub fn transition_cost(a: &FlowTrack, b: &FlowTrack) -> f64 {
    let mut c = 0.0;
    if let (Some(x), Some(y)) = (a.bpm, b.bpm) {
        c += bpm_cost(x, y);
    }
    if let (Some(x), Some(y)) = (a.key, b.key) {
        c += key_cost(x, y);
    }
    if let (Some(x), Some(y)) = (a.loudness, b.loudness) {
        c += ((x - y).abs() / 6.0).min(2.0) * 0.5 - 0.2;
    }
    if a.genre.as_deref().is_some_and(|g| !g.is_empty()) && b.genre.as_deref().is_some_and(|g| !g.is_empty()) {
        c += if same(&a.genre, &b.genre) { -0.2 } else { 0.6 };
    }
    c
}

fn energy_of(t: &FlowTrack) -> Option<f64> {
    let bpm = t.bpm.map(|b| ((b - 70.0) / 100.0).clamp(0.0, 1.0));
    let loud = t.loudness.map(|l| ((l + 23.0) / 17.0).clamp(0.0, 1.0));
    match (bpm, loud) {
        (Some(b), Some(l)) => Some((b + l) / 2.0),
        (b, l) => b.or(l),
    }
}

fn arc_target(arc: FlowArc, pos: f64) -> Option<f64> {
    let p = pos.clamp(0.0, 1.0);
    match arc {
        FlowArc::Build => Some(0.15 + 0.8 * p),
        FlowArc::Peak => Some(if p < 0.6 { 0.2 + 0.8 * (p / 0.6) } else { 1.0 - 0.7 * ((p - 0.6) / 0.4) }),
        FlowArc::Steady => None,
    }
}

pub fn arc_position(step: usize, round_total: usize) -> f64 {
    let span = ARC_SPAN.min(2.max(round_total));
    (step % span) as f64 / (span - 1) as f64
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lean {
    Neutral,
    Known,
    Unknown,
}

pub fn energy_known(t: &FlowTrack) -> bool {
    energy_of(t).is_some()
}

/// `quotaLean`
pub fn quota_lean(known: usize, drawn: usize, step: usize, round_total: usize) -> Lean {
    let t = 1.max(round_total);
    let expected = known * (step + 1);
    if drawn < known && drawn * t < expected {
        return Lean::Known;
    }
    if drawn * t > expected + QUOTA_SLACK * t {
        return Lean::Unknown;
    }
    Lean::Neutral
}

pub struct Slot {
    pub prev: Option<Arc<FlowTrack>>,
    pub prev_id: Option<u32>,
    /// Newest first, includes `prev`.
    pub recent: Vec<Arc<FlowTrack>>,
    pub arc: FlowArc,
    pub arc_pos: f64,
    pub lean: Lean,
}

/// Index into `cands` of the best next track (`sequence::pick`).
pub fn pick(cands: &[(u32, Arc<FlowTrack>)], slot: &Slot) -> usize {
    let target = arc_target(slot.arc, slot.arc_pos);
    let arc_cost: Vec<Option<f64>> = cands
        .iter()
        .map(|(_, t)| match (target, energy_of(t)) {
            (Some(tg), Some(e)) => Some(4.0 * (e - tg).abs()),
            _ => None,
        })
        .collect();
    let best_known = arc_cost.iter().flatten().cloned().fold(None, |m: Option<f64>, c| Some(m.map_or(c, |x| x.min(c))));
    let unknown_arc = match best_known {
        None => 0.0,
        Some(b) => match slot.lean {
            Lean::Known => b + 5.0,
            Lean::Unknown => b - 2.5,
            Lean::Neutral => b,
        },
    };
    let first = |i: usize| -> f64 {
        let (id, t) = (&cands[i].0, &cands[i].1);
        let mut c = 0.0;
        if slot.prev_id == Some(*id) {
            c += 100.0;
        }
        if let Some(p) = &slot.prev {
            c += transition_cost(p, t);
        }
        for (back, r) in slot.recent.iter().take(ARTIST_WINDOW).enumerate() {
            if same(&r.artist, &t.artist) {
                c += 10.0 - 3.5 * back as f64;
                break;
            }
        }
        if target.is_some() {
            c += arc_cost[i].unwrap_or(unknown_arc);
        }
        c
    };
    let scores: Vec<f64> = (0..cands.len()).map(first).collect();
    if cands.len() == 1 {
        return 0;
    }
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|&a, &b| {
        let d = scores[a] - scores[b];
        if d != 0.0 { d.partial_cmp(&0.0).unwrap() } else { a.cmp(&b) }
    });
    order.truncate(LOOKAHEAD_TOP);
    let mut best = order[0];
    let mut best_total = f64::INFINITY;
    for &i in &order {
        let ti = &cands[i].1;
        let mut follow = f64::INFINITY;
        for (j, (_, tj)) in cands.iter().enumerate() {
            if j == i {
                continue;
            }
            let mut c = transition_cost(ti, tj);
            if same(&ti.artist, &tj.artist) {
                c += 10.0;
            }
            if c < follow {
                follow = c;
            }
        }
        let total = scores[i] + 0.5 * follow;
        if total < best_total || (total == best_total && i < best) {
            best_total = total;
            best = i;
        }
    }
    best
}

/// `reason`
pub fn reason(prev: &FlowTrack, cur: &FlowTrack) -> Option<FlowReason> {
    let mut r = FlowReason::default();
    let mut any = false;
    if let (Some(a), Some(b)) = (prev.bpm, cur.bpm) {
        r.bpm_from = Some(a.round() as i32);
        r.bpm_to = Some(b.round() as i32);
        any = true;
    }
    if let (Some(a), Some(b)) = (prev.key, cur.key) {
        r.key_from = Some(key_label(a));
        r.key_to = Some(key_label(b));
        any = true;
    }
    if same(&prev.genre, &cur.genre) {
        r.same_genre = Some(true);
        r.genre = cur.genre.clone();
        any = true;
    }
    any.then_some(r)
}

pub fn coverage<'a>(tracks: impl Iterator<Item = &'a FlowTrack>) -> FlowCoverage {
    let mut c = FlowCoverage { total: 0, bpm: 0, key: 0, loudness: 0 };
    for t in tracks {
        c.total += 1;
        c.bpm += t.bpm.is_some() as u32;
        c.key += t.key.is_some() as u32;
        c.loudness += t.loudness.is_some() as u32;
    }
    c
}
