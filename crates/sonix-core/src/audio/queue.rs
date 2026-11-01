//! Pure queue/transport logic: track id order, shuffle, repeat, next/prev,
//! and the rolling source window. Deliberately free of any I/O, threads, or
//! IPC types (beyond the small wire enums it shares) so it's trivially
//! unit-testable; `audio::engine` is the only caller.
//!
//! ## The rolling window
//!
//! Playing a playlist/artist/"all tracks"/radio no longer materializes every
//! track up front. Instead `Queue` holds a [`Source`]: the full id pool (a
//! `Vec<String>` — a few MB even at 50k ids, so keeping it in memory is
//! fine) plus `remaining`, the not-yet-drawn tail of the current round's
//! permutation (shuffled once per round for `SourceMode::Shuffle`, the
//! pool's own order for `SourceMode::InOrder`). Only a small window's worth
//! of ids is ever pulled from `remaining` into `entries` (the actual
//! play-order list the UI pages through) — so shuffling a 1,980-track
//! playlist only ever puts ~20 of them in "the queue" at once, not all
//! 1,980 (this is what `CLAUDE.md`'s RAM budget and this module's own
//! `audio::engine` sibling's lazy-metadata-resolution doc comment are both
//! protecting). `refill_if_needed` tops the window back up once fewer than
//! half of it is still ahead of the current track; when a round's
//! `remaining` empties, `repeat: All` draws a fresh permutation of the whole
//! pool (never starting on the track that just finished), `repeat: Off`
//! just lets the window run out normally. An id drawn out of `remaining`
//! into `entries` is never returned there even if the user removes it
//! (`remove`) — "no repeats until the whole source has played" is about the
//! *pool*, not about re-offering a row the user explicitly dismissed.
//!
//! A plain explicit list (`set_queue`, used for ad hoc/"play this
//! selection" plays with no real source identity) and a windowed
//! `Source` (`set_source`) are different entry points but share the same
//! `entries`/`current` representation and the same `next`/`previous`/
//! `reorder`/`remove`/`jump_to` methods — a source whose pool is smaller
//! than the window behaves exactly like a plain static queue, since its
//! very first draw already empties `remaining` (see `set_source`'s doc
//! comment: "sources <= the window behave as today").
//!
//! User-queued ids ("Play next"/"Add to queue") are tagged
//! [`QueueItemOrigin::User`] and always inserted right after `current`, so
//! they always play before any `Source`-origin id queued behind them
//! (Spotify-style "Next in queue" vs. "Next from <source>") — see
//! `enqueue_next`/`enqueue_end`.
//!
//! Real shuffling only: every shuffle (initial draw, wraparound/"fresh
//! round" redraw, `set_shuffle` toggle) is freshly seeded from OS entropy
//! (`SmallRng::from_entropy`, re-seeded every single time — never just once
//! at construction) and uses `SliceRandom::shuffle`, an unbiased
//! Fisher-Yates (`rand`'s `Uniform`-based index sampling has no modulo
//! bias, unlike a hand-rolled `next_u64() % n`). Tests that need a
//! reproducible permutation use the test-only `shuffle_in_place_seeded`
//! instead, so they stay deterministic without the production path ever
//! reusing a seed.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use rand::{Rng as _, SeedableRng};
use serde::{Deserialize, Serialize};

use super::sequence::{self, FlowTrack, Quota};
use crate::ipc::{FlowArc, FlowCoverage, FlowReason, QueueItemOrigin, RepeatMode, SourceKind, SourceMode};

/// Default `queueWindowSize` (`Settings`), before any user override.
pub const DEFAULT_WINDOW_SIZE: usize = 20;
pub const MIN_WINDOW_SIZE: usize = 10;
pub const MAX_WINDOW_SIZE: usize = 100;

fn fresh_rng() -> SmallRng {
    SmallRng::from_entropy()
}

/// Drops repeated ids, keeping each one's first occurrence. A shuffled
/// round is "every *track* once": real playlists can list the same track
/// many times (the dev server's 5,178-entry "Liked Songs" holds only 1,571
/// distinct tracks, some 8 times), and shuffling those duplicates would
/// replay a track several times per round.
fn dedup_keep_first(ids: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::with_capacity(ids.len());
    ids.into_iter().filter(|id| seen.insert(id.clone())).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    id: String,
    origin: QueueItemOrigin,
}

/// A playlist/artist/all-tracks/radio/search source behind a rolling
/// window. `pool` is the whole, fixed id list (radio's "finite source of its
/// initial ~50 + local top-ups" is just a `pool` that never grows again
/// after `set_source`); `remaining` is the current round's not-yet-drawn
/// tail, consumed front-to-back by `Queue::draw_into_window`.
#[derive(Debug, Clone)]
struct Source {
    kind: SourceKind,
    label: String,
    /// The playlist/artist id behind `label` (None for all-tracks/radio/
    /// search), so the UI can link "Playing from <label>" precisely instead
    /// of resolving it by name. Pure passthrough, set via `set_source_id`.
    id: Option<String>,
    mode: SourceMode,
    /// Flow only: the energy arc (`Steady` for every other mode).
    arc: FlowArc,
    /// Size of the current round (`remaining.len()` when it was built), so
    /// Flow knows its position in the round for the arc.
    round_total: usize,
    /// Flow Build/Peak: energy-known tracks of this round vs drawn so far
    /// (`sequence::Quota`), rebuilt with every round, restore and data change.
    quota: Quota,
    pool: Vec<String>,
    remaining: VecDeque<String>,
}

#[derive(Debug, Clone)]
pub struct Queue {
    /// The play-order list: everything currently materialized, whether
    /// still ahead of `current`, already played, user-queued, or drawn from
    /// a `Source`. Bounded to roughly `2 * window_size` entries whenever a
    /// `Source` is active (see `prune_history`) — never the whole source.
    entries: Vec<Entry>,
    /// Position within `entries`, or `None` if nothing is loaded.
    current: Option<usize>,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    /// Pristine add-order ids for the current *static* (`source: None`)
    /// queue, so turning shuffle back off restores it — a `Source`-backed
    /// queue instead re-derives from `Source::pool`/`remaining` (see
    /// `rederive_source_window`), which already preserves pool order.
    static_natural_order: Vec<String>,
    /// id -> artist name, for the optional "spread artists in shuffle"
    /// post-process (`apply_artist_spread`). Only ever used to nudge
    /// adjacent same-artist tracks apart; pure data handed in by the caller
    /// (`set_source`), never fetched here (this module stays free of I/O).
    artist_of: HashMap<String, String>,
    pub spread_artists: bool,
    window_size: usize,
    source: Option<Source>,
    /// Flow sequencing data per id (bpm/key/loudness/genre/artist), handed
    /// in by the caller like `artist_of`; empty for non-Flow sources.
    flow: Arc<HashMap<String, FlowTrack>>,
    /// Cached coverage of the Flow pool (computed when the data is set, so
    /// `GetQueue` never walks a 50k pool).
    flow_cov: Option<FlowCoverage>,
}

impl Default for Queue {
    fn default() -> Self {
        Self::new()
    }
}

impl Queue {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            current: None,
            shuffle: false,
            repeat: RepeatMode::Off,
            static_natural_order: Vec::new(),
            artist_of: HashMap::new(),
            spread_artists: false,
            window_size: DEFAULT_WINDOW_SIZE,
            source: None,
            flow: Arc::default(),
            flow_cov: None,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn window_size(&self) -> usize {
        self.window_size
    }

    /// Clamped to `MIN_WINDOW_SIZE..=MAX_WINDOW_SIZE` (matches the
    /// `queueWindowSize` setting's UI range) and takes effect immediately —
    /// a lowered window doesn't trim what's already materialized, a raised
    /// one tops the window straight back up if a `Source` is active.
    pub fn set_window_size(&mut self, size: usize) {
        self.window_size = size.clamp(MIN_WINDOW_SIZE, MAX_WINDOW_SIZE);
        self.refill_if_needed();
    }

    // --- Static (no-source) play: explicit id lists -----------------------

    /// Replaces the whole queue with a plain, explicit id list (no rolling
    /// window — this is the "ad hoc selection" path, e.g. a multi-select
    /// "Play", not a named playlist/artist/library source) and starts at
    /// `start_index` (into `ids`, pre-shuffle).
    pub fn set_queue(&mut self, ids: Vec<String>, start_index: usize) {
        self.source = None;
        self.artist_of.clear();
        self.flow = Arc::default();
        self.flow_cov = None;
        self.static_natural_order = ids;
        self.rebuild_static_entries();
        if self.entries.is_empty() {
            self.current = None;
            return;
        }
        let start_index = start_index.min(self.static_natural_order.len() - 1);
        if self.shuffle {
            let start_id = self.static_natural_order[start_index].clone();
            if let Some(pos) = self.entries.iter().position(|e| e.id == start_id) {
                self.entries.swap(0, pos);
            }
            self.current = Some(0);
        } else {
            // Not shuffled: `entries` is in the exact same order as
            // `static_natural_order`, so the index lines up directly
            // (correct even with duplicate ids, unlike an id lookup).
            self.current = Some(start_index.min(self.entries.len() - 1));
        }
    }

    /// Rebuilds `entries` from `static_natural_order`: identity order, or a
    /// fresh shuffle (+ optional artist spread) when `self.shuffle`.
    fn rebuild_static_entries(&mut self) {
        let mut order: Vec<usize> = (0..self.static_natural_order.len()).collect();
        if self.shuffle {
            order.shuffle(&mut fresh_rng());
        }
        self.entries = order
            .into_iter()
            .map(|i| Entry { id: self.static_natural_order[i].clone(), origin: QueueItemOrigin::Source })
            .collect();
        if self.shuffle {
            self.apply_artist_spread();
        }
    }

    // --- Source-backed play: the rolling window ---------------------------

    /// Starts a windowed `Source` (playlist/artist/all-tracks/radio/search):
    /// only up to `window_size` ids are drawn into `entries` right away — a
    /// `pool` no bigger than the window (an album, a short playlist, radio's
    /// ~50) drains `remaining` on this very first draw and then behaves
    /// exactly like a plain static queue (every id already materialized, no
    /// further draws ever needed except a repeat-all wrap). `artists[i]` is
    /// `track_ids[i]`'s artist name (for `spread_artists`), same length as
    /// `track_ids` or empty to skip artist spreading entirely.
    /// `start_track_id`, if given, is where playback starts (e.g.
    /// double-click on a specific row); for `SourceMode::InOrder` this plays
    /// from that id to the end of `pool` (not wrapping back to the start).
    pub fn set_source(
        &mut self,
        kind: SourceKind,
        label: String,
        mode: SourceMode,
        pool: Vec<String>,
        artists: Vec<Option<String>>,
        start_track_id: Option<&str>,
    ) {
        self.set_source_inner(kind, label, mode, FlowArc::Steady, pool, artists, start_track_id, HashMap::new(), None);
    }

    /// Starts a Flow source (`SourceMode::Flow`): `features` is the pool's
    /// sequencing data (`library::flow::load_flow_features`); ids without
    /// an entry simply have no data. `start_track_id` plays first, the
    /// flow continues from it. Flow does not touch the global shuffle flag.
    pub fn set_source_flow(
        &mut self,
        kind: SourceKind,
        label: String,
        arc: FlowArc,
        pool: Vec<String>,
        features: HashMap<String, FlowTrack>,
        start_track_id: Option<&str>,
    ) {
        self.set_source_inner(kind, label, SourceMode::Flow, arc, pool, Vec::new(), start_track_id, features, None);
    }

    /// Test hook: like `set_source_flow` but with a reproducible round shuffle.
    #[cfg(test)]
    fn set_source_flow_seeded(&mut self, arc: FlowArc, pool: Vec<String>, features: HashMap<String, FlowTrack>, seed: u64) {
        self.set_source_inner(SourceKind::Playlist, "Test Flow".into(), SourceMode::Flow, arc, pool, Vec::new(), None, features, Some(seed));
    }

    #[allow(clippy::too_many_arguments)]
    fn set_source_inner(
        &mut self,
        kind: SourceKind,
        label: String,
        mode: SourceMode,
        arc: FlowArc,
        pool: Vec<String>,
        artists: Vec<Option<String>>,
        start_track_id: Option<&str>,
        features: HashMap<String, FlowTrack>,
        seed: Option<u64>,
    ) {
        self.entries.clear();
        self.current = None;
        self.static_natural_order.clear();
        self.artist_of.clear();
        self.flow = Arc::new(features);
        if artists.len() == pool.len() {
            for (id, artist) in pool.iter().zip(artists) {
                if let Some(a) = artist {
                    self.artist_of.insert(id.clone(), a);
                }
            }
        }
        self.shuffle = matches!(mode, SourceMode::Shuffle);
        let remaining: VecDeque<String> = match mode {
            SourceMode::Shuffle => {
                let mut ids = dedup_keep_first(pool.clone());
                ids.shuffle(&mut fresh_rng());
                if let Some(start_id) = start_track_id {
                    if let Some(pos) = ids.iter().position(|id| id == start_id) {
                        ids.swap(0, pos);
                    }
                }
                ids.into()
            }
            SourceMode::InOrder => match start_track_id.and_then(|sid| pool.iter().position(|id| id == sid)) {
                Some(idx) => pool[idx..].to_vec().into(),
                None => pool.clone().into(),
            },
            SourceMode::Flow => {
                // The round's id set, seed-shuffled: Flow scores the front
                // of it, so the shuffle is what makes each pick's candidate
                // sample random (and the whole thing reproducible per seed).
                let mut ids = dedup_keep_first(pool.clone());
                match seed {
                    Some(seed) => ids.shuffle(&mut SmallRng::seed_from_u64(seed)),
                    None => ids.shuffle(&mut fresh_rng()),
                }
                ids.into()
            }
        };
        let round_total = remaining.len();
        let mut remaining = remaining;
        let mut opener: Option<Entry> = None;
        if matches!(mode, SourceMode::Flow) {
            if let Some(pos) = start_track_id.and_then(|sid| remaining.iter().position(|id| id == sid)) {
                let id = remaining.remove(pos).expect("position is in range");
                opener = Some(Entry { id, origin: QueueItemOrigin::Source });
            }
        }
        self.source = Some(Source { kind, label, id: None, mode, arc, round_total, quota: Quota::default(), pool, remaining });
        self.recompute_flow_coverage();
        let opener_known = opener.as_ref().is_some_and(|e| self.flow.get(&e.id).is_some_and(sequence::energy_known));
        if let Some(src) = self.source.as_mut() {
            Self::recount_quota(src, &self.flow, opener_known as usize);
        }
        let drawn_opener = opener.is_some();
        if let Some(e) = opener {
            self.entries.push(e);
        }
        self.draw_into_window(self.window_size.saturating_sub(drawn_opener as usize));
        self.spread_source_entries_from(0);
        if !self.entries.is_empty() {
            self.current = Some(0);
        }
    }

    /// Real counts only (never an estimate): `(kind, label, mode,
    /// not-yet-drawn-into-the-window count, source id)` — backs "+ 1,960
    /// more from Liked Songs" in the queue rail.
    pub fn source_info(&self) -> Option<(SourceKind, String, SourceMode, u64, Option<String>)> {
        self.source.as_ref().map(|s| (s.kind, s.label.clone(), s.mode, s.remaining.len() as u64, s.id.clone()))
    }

    /// Attaches the playlist/artist id to the active source (no-op without
    /// one). Call right after `set_source`.
    pub fn set_source_id(&mut self, id: Option<String>) {
        if let Some(s) = self.source.as_mut() {
            s.id = id;
        }
    }

    /// True while the active source is a Flow source.
    pub fn is_flow(&self) -> bool {
        self.source.as_ref().is_some_and(|s| matches!(s.mode, SourceMode::Flow))
    }

    /// Flow sources: the arc and the real data coverage of the whole pool
    /// (distinct tracks), for the queue rail's honest hint.
    pub fn flow_info(&self) -> Option<(FlowArc, FlowCoverage)> {
        let src = self.source.as_ref().filter(|s| matches!(s.mode, SourceMode::Flow))?;
        Some((src.arc, self.flow_cov.unwrap_or_default()))
    }

    fn recompute_flow_coverage(&mut self) {
        self.flow_cov = self.source.as_ref().filter(|s| matches!(s.mode, SourceMode::Flow)).map(|src| {
            let mut seen = std::collections::HashSet::with_capacity(src.pool.len());
            let empty = FlowTrack::default();
            sequence::coverage(src.pool.iter().filter(|id| seen.insert(id.as_str())).map(|id| self.flow.get(id).unwrap_or(&empty)))
        });
    }

    /// Why the entry at `index` follows the one before it (Flow sources,
    /// auto-drawn entries only; real values only, `None` when there is
    /// nothing to say).
    pub fn flow_reason(&self, index: usize) -> Option<FlowReason> {
        if !self.is_flow() || index == 0 {
            return None;
        }
        let cur = self.entries.get(index)?;
        if cur.origin != QueueItemOrigin::Source {
            return None;
        }
        let prev = self.entries.get(index - 1)?;
        sequence::reason(self.flow.get(&prev.id)?, self.flow.get(&cur.id)?)
    }

    /// Ids a Flow source needs sequencing data for (pool + materialized
    /// entries): the engine's restore path asks the library for these.
    pub fn flow_wanted_ids(&self) -> Vec<String> {
        match &self.source {
            Some(s) if matches!(s.mode, SourceMode::Flow) => {
                s.pool.iter().cloned().chain(self.entries.iter().map(|e| e.id.clone())).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Supplies sequencing data after a restore (it isn't persisted).
    pub fn set_flow_features(&mut self, features: HashMap<String, FlowTrack>) {
        self.flow = Arc::new(features);
        self.recompute_flow_coverage();
        // The round's quota isn't persisted either: everything known in the
        // round (the distinct pool) that is no longer in `remaining` counts
        // as drawn.
        if let Some(src) = self.source.as_mut().filter(|s| matches!(s.mode, SourceMode::Flow)) {
            let flow = &self.flow;
            let known = |id: &str| flow.get(id).is_some_and(sequence::energy_known);
            let mut seen = std::collections::HashSet::with_capacity(src.pool.len());
            let distinct: Vec<&String> = src.pool.iter().filter(|id| seen.insert(id.as_str())).collect();
            let left = src.remaining.iter().filter(|id| known(id.as_str())).count();
            let drawn = distinct.iter().filter(|id| known(id.as_str())).count().saturating_sub(left);
            src.round_total = distinct.len().max(src.remaining.len());
            Self::recount_quota(src, flow, drawn);
        }
    }

    /// Flow: (re)counts the round's energy-known tracks for the Build/Peak
    /// quota; `known_drawn` of them were already drawn this round.
    fn recount_quota(src: &mut Source, flow: &HashMap<String, FlowTrack>, known_drawn: usize) {
        if !matches!(src.mode, SourceMode::Flow) {
            src.quota = Quota::default();
            return;
        }
        let left = src.remaining.iter().filter(|id| flow.get(*id).is_some_and(sequence::energy_known)).count();
        src.quota = Quota { known: left + known_drawn, drawn: known_drawn };
    }

    /// Pulls up to `want` fresh ids from `source.remaining` into `entries`
    /// (tagged `Source`), regenerating a fresh round (`regenerate_remaining`)
    /// when `remaining` empties and `repeat == All`; does nothing without an
    /// active `Source`, or once `remaining` is empty and repeat isn't `All`.
    fn draw_into_window(&mut self, want: usize) {
        let mut drawn = 0;
        // At most one regeneration per call: a pool smaller than `want`
        // should top the window up with exactly one fresh round, never
        // stage a second round ahead of it too (that would both waste the
        // "avoid starting on the last-played track" nudge on a round that
        // isn't about to play yet, and double-draw on every subsequent
        // threshold refill for a small, fast-looping source).
        let mut regenerated = false;
        while drawn < want {
            let can_regenerate = !regenerated && self.repeat == RepeatMode::All;
            let Some(src) = self.source.as_mut() else { break };
            if src.remaining.is_empty() {
                if can_regenerate && !src.pool.is_empty() {
                    regenerated = true;
                    // The new round's first id lands right behind the last
                    // materialized entry (not necessarily `current`: a
                    // threshold refill draws while part of the window is
                    // still ahead), so that's the one it must not repeat.
                    let avoid = self.entries.last().map(|e| e.id.clone());
                    let src = self.source.as_mut().expect("checked above");
                    Self::regenerate_remaining(src, &self.flow, avoid.as_deref());
                } else {
                    break;
                }
            }
            let src = self.source.as_mut().expect("checked above");
            let id = if matches!(src.mode, SourceMode::Flow) {
                Self::pick_flow(src, &self.flow, &self.entries)
            } else {
                src.remaining.pop_front()
            };
            let Some(id) = id else { break };
            self.entries.push(Entry { id, origin: QueueItemOrigin::Source });
            drawn += 1;
        }
    }

    /// One Flow draw: scores the front `sequence::CANDIDATES` of
    /// `remaining` against the last materialized entry and removes the
    /// winner (so a round never repeats a track).
    fn pick_flow(src: &mut Source, flow: &HashMap<String, FlowTrack>, entries: &[Entry]) -> Option<String> {
        let k = src.remaining.len().min(sequence::CANDIDATES);
        if k == 0 {
            return None;
        }
        let prev = entries.last().map(|e| e.id.as_str());
        let recent: Vec<&str> = entries.iter().rev().take(sequence::ARTIST_WINDOW).map(|e| e.id.as_str()).collect();
        let idx = {
            let cands: Vec<&str> = src.remaining.iter().take(k).map(String::as_str).collect();
            let step = src.round_total.saturating_sub(src.remaining.len());
            let slot = sequence::Slot {
                prev,
                recent: &recent,
                arc: src.arc,
                arc_pos: sequence::arc_position(step, src.round_total),
                lean: src.quota.lean(step, src.round_total),
            };
            sequence::pick(flow, &cands, &slot)
        };
        let id = src.remaining.remove(idx)?;
        src.quota.note(flow.get(&id));
        Some(id)
    }

    /// A fresh round: a new permutation of the whole pool for
    /// `SourceMode::Shuffle` (nudged so it doesn't start with `avoid_first`
    /// — the track that just finished), or the pool's own order again for
    /// `SourceMode::InOrder`.
    fn regenerate_remaining(src: &mut Source, flow: &HashMap<String, FlowTrack>, avoid_first: Option<&str>) {
        let mut ids = src.pool.clone();
        if matches!(src.mode, SourceMode::Shuffle | SourceMode::Flow) {
            ids = dedup_keep_first(ids);
            ids.shuffle(&mut fresh_rng());
            if let Some(avoid) = avoid_first {
                if ids.len() > 1 && ids[0] == avoid {
                    let j = fresh_rng().gen_range(1..ids.len());
                    ids.swap(0, j);
                }
            }
        }
        src.round_total = ids.len();
        src.remaining = ids.into();
        Self::recount_quota(src, flow, 0);
    }

    /// How many `Source`-origin entries are still ahead of `current` —
    /// `refill_if_needed`'s trigger is this falling below half the window.
    fn ahead_source_count(&self) -> usize {
        match self.current {
            Some(pos) => self.entries.iter().skip(pos + 1).filter(|e| e.origin == QueueItemOrigin::Source).count(),
            None => 0,
        }
    }

    /// Tops the window back up once fewer than `window_size / 2` auto items
    /// remain ahead of `current`, then prunes stale history so `entries`
    /// never grows unbounded over a long session.
    fn refill_if_needed(&mut self) {
        if self.source.is_none() {
            return;
        }
        let ahead = self.ahead_source_count();
        if ahead >= self.window_size / 2 {
            return;
        }
        let drawn_from = self.entries.len();
        self.draw_into_window(self.window_size.saturating_sub(ahead));
        self.spread_source_entries_from(drawn_from);
        self.prune_history();
    }

    /// Drops materialized entries more than `window_size` behind `current`
    /// (oldest first) — keeps a small amount of history for `previous`
    /// without ever holding the *whole* source. A no-op without a `Source`
    /// (a static queue is never pruned).
    fn prune_history(&mut self) {
        if self.source.is_none() {
            return;
        }
        let Some(pos) = self.current else { return };
        if pos > self.window_size {
            let drop = pos - self.window_size;
            self.entries.drain(0..drop);
            self.current = Some(pos - drop);
        }
    }

    /// Re-derives the remaining permutation from the not-yet-played set
    /// (every future `Source`-origin entry still in the window, plus
    /// whatever's left in `source.remaining`) when shuffle is toggled while
    /// a `Source` is active — `current` (and anything already played, or
    /// user-queued ahead of it) is untouched.
    fn rederive_source_window(&mut self, shuffle_enabled: bool) {
        let Some(cur) = self.current else { return };
        if self.is_flow() {
            // Flow -> Shuffle: the sequencing data is no longer needed, but
            // its artists are what "Spread artists" needs from now on (a Flow
            // source never filled `artist_of`).
            for (id, t) in self.flow.iter() {
                if let Some(a) = &t.artist {
                    self.artist_of.insert(id.clone(), a.to_string());
                }
            }
            self.flow = Arc::default();
            self.flow_cov = None;
        }
        let mut future_source_ids: Vec<String> = Vec::new();
        let mut keep: Vec<Entry> = Vec::with_capacity(self.entries.len());
        for (i, e) in std::mem::take(&mut self.entries).into_iter().enumerate() {
            if i > cur && e.origin == QueueItemOrigin::Source {
                future_source_ids.push(e.id);
            } else {
                keep.push(e);
            }
        }
        self.entries = keep;
        let Some(src) = self.source.as_mut() else { return };
        let mut pool_ids = future_source_ids;
        pool_ids.extend(src.remaining.drain(..));
        src.mode = if shuffle_enabled { SourceMode::Shuffle } else { SourceMode::InOrder };
        if shuffle_enabled {
            pool_ids = dedup_keep_first(pool_ids);
            pool_ids.shuffle(&mut fresh_rng());
        } else {
            let index_of: HashMap<&str, usize> = src.pool.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
            pool_ids.sort_by_key(|id| index_of.get(id.as_str()).copied().unwrap_or(usize::MAX));
        }
        src.remaining = pool_ids.into();
        let drawn_from = self.entries.len();
        self.draw_into_window(self.window_size);
        self.spread_source_entries_from(drawn_from);
    }

    pub fn current_id(&self) -> Option<&str> {
        self.current.and_then(|pos| self.entries.get(pos)).map(|e| e.id.as_str())
    }

    /// Origin of the current entry (`listen::`: user-queued vs drawn).
    pub fn current_origin(&self) -> Option<QueueItemOrigin> {
        self.current.and_then(|pos| self.entries.get(pos)).map(|e| e.origin)
    }

    /// Borrowed identity of the active source (`listen::`, no allocation).
    pub fn source_ref(&self) -> Option<(SourceKind, Option<&str>, &str)> {
        self.source.as_ref().map(|s| (s.kind, s.id.as_deref(), s.label.as_str()))
    }

    pub fn current_position(&self) -> Option<usize> {
        self.current
    }

    /// Ids in current playback order. Only used by tests (`page` is
    /// `player_get_queue`'s real, paged accessor) — a small, simple way to
    /// assert "it's a real permutation of exactly these ids".
    #[allow(dead_code)]
    pub fn ordered_ids(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.id.clone()).collect()
    }

    /// A page of `(id, origin)` without ever materializing/cloning more than
    /// `limit` entries — `player_get_queue`'s paging. Already small (bounded
    /// by `prune_history`), but kept paged for the same reason the old
    /// full-source design was: never assume the caller wants everything.
    /// Every materialized id (play order, duplicates included), borrowed —
    /// for the engine's `track_cache` bound, which keeps exactly these.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.id.as_str())
    }

    pub fn page(&self, offset: usize, limit: usize) -> Vec<(String, QueueItemOrigin)> {
        self.entries.iter().skip(offset).take(limit).map(|e| (e.id.clone(), e.origin)).collect()
    }

    /// Advances to the next track per repeat/shuffle/source rules. Returns
    /// the new current id, or `None` if playback should stop (queue
    /// exhausted, repeat off).
    #[allow(clippy::should_implement_trait)] // transport control, not an Iterator
    pub fn next(&mut self) -> Option<&str> {
        if self.entries.is_empty() {
            return None;
        }
        match self.repeat {
            RepeatMode::One => {}
            _ => {
                let Some(pos) = self.current else {
                    self.current = Some(0);
                    self.refill_if_needed();
                    return self.current_id();
                };
                if pos + 1 < self.entries.len() {
                    self.current = Some(pos + 1);
                    self.refill_if_needed();
                } else if self.source.is_some() {
                    // Reached the end of what's materialized: try to draw
                    // more (a repeat-all source regenerates a fresh round;
                    // otherwise this is just the natural end of the queue).
                    let drawn_from = self.entries.len();
                    self.draw_into_window(self.window_size);
                    self.spread_source_entries_from(drawn_from);
                    if pos + 1 < self.entries.len() {
                        self.current = Some(pos + 1);
                        self.refill_if_needed();
                    } else {
                        self.current = None;
                        return None;
                    }
                } else if self.repeat == RepeatMode::All {
                    if self.shuffle {
                        self.reshuffle_static_wrap(pos);
                    }
                    self.current = Some(0);
                } else {
                    self.current = None;
                    return None;
                }
            }
        }
        self.current_id()
    }

    /// Reshuffles for a *static* queue's repeat-all wraparound.
    /// `just_finished_pos` is nudged away from the new position 0 so the
    /// track that just finished doesn't immediately play again.
    fn reshuffle_static_wrap(&mut self, just_finished_pos: usize) {
        let avoid_id = self.entries[just_finished_pos].id.clone();
        self.rebuild_static_entries();
        if self.entries.len() > 1 && self.entries[0].id == avoid_id {
            let j = fresh_rng().gen_range(1..self.entries.len());
            self.entries.swap(0, j);
        }
    }

    /// Goes to the previous track, unless `elapsed_sec > 3.0`, in which case
    /// the caller should instead restart the current track (standard
    /// player UX) — this method only handles the "actually go back" case;
    /// the engine checks `elapsed_sec` itself before calling it.
    pub fn previous(&mut self) -> Option<&str> {
        if self.entries.is_empty() {
            return None;
        }
        match self.current {
            Some(pos) if pos > 0 => self.current = Some(pos - 1),
            // A static queue's repeat-all wraps backward too; a source-
            // backed one just stays at the first still-materialized entry
            // (the "whole queue" it would wrap into isn't held in memory).
            Some(_) if self.repeat == RepeatMode::All && self.source.is_none() => {
                self.current = Some(self.entries.len() - 1)
            }
            _ => {}
        }
        self.current_id()
    }

    /// Makes the entry at `pos` (a position in *play order*, same indexing
    /// as `page`/`remove`/`reorder`) the current one. Returns false (and
    /// changes nothing) if `pos` is out of range.
    pub fn jump_to(&mut self, pos: usize) -> bool {
        if pos >= self.entries.len() {
            return false;
        }
        self.current = Some(pos);
        self.refill_if_needed();
        true
    }

    pub fn set_shuffle(&mut self, enabled: bool) {
        if self.shuffle == enabled {
            return;
        }
        self.shuffle = enabled;
        if self.source.is_some() {
            self.rederive_source_window(enabled);
        } else {
            let current_id = self.current_id().map(str::to_string);
            self.rebuild_static_entries();
            if let Some(id) = &current_id {
                if self.shuffle {
                    if let Some(pos) = self.entries.iter().position(|e| &e.id == id) {
                        self.entries.swap(0, pos);
                    }
                }
                self.current = self.entries.iter().position(|e| &e.id == id);
            }
        }
    }

    /// Just flips the mode — `Queue` only ever draws a fresh round (`repeat
    /// == All`'s whole reason to exist) lazily, from `next()`/
    /// `refill_if_needed()`, once the window actually needs more material;
    /// turning repeat-all on doesn't itself pre-draw a second round before
    /// the first one has even finished playing.
    pub fn set_repeat(&mut self, mode: RepeatMode) {
        self.repeat = mode;
    }

    /// Moves the entry at `from_index` to `to_index` (both play-order
    /// positions). Pure index arithmetic — correct even with duplicate ids,
    /// unlike looking the moved entry back up by value.
    pub fn reorder(&mut self, from_index: usize, to_index: usize) {
        if from_index >= self.entries.len() || to_index >= self.entries.len() {
            return;
        }
        let moved = self.entries.remove(from_index);
        self.entries.insert(to_index, moved);
        if let Some(cur) = self.current {
            self.current = Some(if cur == from_index {
                to_index
            } else if from_index < cur && cur <= to_index {
                cur - 1
            } else if to_index <= cur && cur < from_index {
                cur + 1
            } else {
                cur
            });
        }
        self.refill_if_needed();
    }

    /// Removes the entry at `index` (a position in *play order*, matching
    /// `RemoveFromQueueRequest.index` as documented in `docs/IPC.md`). An
    /// auto (`Source`-origin) entry removed this way is never re-drawn —
    /// it's already out of `source.remaining`, and removal doesn't put it
    /// back.
    pub fn remove(&mut self, index: usize) {
        if index >= self.entries.len() {
            return;
        }
        let removed_current = self.current == Some(index);
        self.entries.remove(index);
        if removed_current {
            // The entry that slid into `index` becomes current. If the
            // removed one was the last materialized entry, let a source
            // draw more first so playback moves forward to a fresh track
            // instead of back onto the one that already played.
            if index >= self.entries.len() && self.source.is_some() {
                let drawn_from = self.entries.len();
                self.draw_into_window(self.window_size);
                self.spread_source_entries_from(drawn_from);
            }
            self.current = if index < self.entries.len() { Some(index) } else { self.entries.len().checked_sub(1) };
        } else if let Some(idx) = self.current.filter(|&idx| idx > index) {
            self.current = Some(idx - 1);
        }
        self.refill_if_needed();
    }

    /// Inserts `ids` (tagged `User`) right after `current` — "Play next":
    /// ahead of anything else already queued, auto or user.
    pub fn enqueue_next(&mut self, ids: Vec<String>) {
        let at = self.current.map(|p| p + 1).unwrap_or(0).min(self.entries.len());
        for (offset, id) in ids.into_iter().enumerate() {
            self.entries.insert(at + offset, Entry { id, origin: QueueItemOrigin::User });
        }
    }

    /// "Add to queue". With an active `Source`, inserts `ids` (tagged
    /// `User`) at the end of the user-queued run right after `current` —
    /// behind whatever's already user-queued, but still ahead of every
    /// `Source`-origin (auto) entry. Without one (a plain static queue —
    /// there's no auto-filled run to stay ahead of), appends to the literal
    /// end of the queue instead, matching a plain play queue's ordinary
    /// "add to queue" semantics.
    pub fn enqueue_end(&mut self, ids: Vec<String>) {
        let at = if self.source.is_some() {
            let mut at = self.current.map(|p| p + 1).unwrap_or(0).min(self.entries.len());
            while at < self.entries.len() && self.entries[at].origin == QueueItemOrigin::User {
                at += 1;
            }
            at
        } else {
            self.entries.len()
        };
        for (offset, id) in ids.into_iter().enumerate() {
            self.entries.insert(at + offset, Entry { id, origin: QueueItemOrigin::User });
        }
    }

    /// Test-only: reshuffles `entries` with a reproducible seed instead of
    /// real entropy (`apply_artist_spread` still runs after, same as a real
    /// shuffle) — lets a test assert an exact/repeatable permutation without
    /// the production path (`rebuild_static_entries`/`set_source`/
    /// `regenerate_remaining`, all real-entropy-only) ever reusing a seed.
    #[cfg(test)]
    fn shuffle_in_place_seeded(&mut self, seed: u64) {
        self.entries.shuffle(&mut SmallRng::seed_from_u64(seed));
        self.apply_artist_spread();
    }

    /// "Spread artists" for a freshly drawn batch of a *shuffled* source.
    /// Only touches `Source`-origin entries at or after `start` (the batch
    /// just appended), so it never moves the playing track, history, or
    /// user-queued items, and never reorders an in-order source (a playlist
    /// played in order, radio's relevance order).
    fn spread_source_entries_from(&mut self, start: usize) {
        if !self.spread_artists || self.artist_of.is_empty() {
            return;
        }
        if !self.source.as_ref().is_some_and(|s| matches!(s.mode, SourceMode::Shuffle)) {
            return;
        }
        for i in start.max(1)..self.entries.len() {
            if self.entries[i].origin != QueueItemOrigin::Source {
                continue;
            }
            let Some(prev_artist) = self.artist_of.get(&self.entries[i - 1].id) else { continue };
            if self.artist_of.get(&self.entries[i].id) != Some(prev_artist) {
                continue;
            }
            let found = (i + 1..self.entries.len()).find(|&j| {
                self.entries[j].origin == QueueItemOrigin::Source && self.artist_of.get(&self.entries[j].id) != Some(prev_artist)
            });
            if let Some(j) = found {
                self.entries.swap(i, j);
            }
        }
    }

    /// Best-effort O(n) pass (worst case O(n^2) for a pathological run of
    /// one artist back-to-back, which can't be fully separated anyway):
    /// whenever two adjacent entries share an artist, swaps in the nearest
    /// later entry with a different one. A no-op unless `spread_artists` is
    /// on and `artist_of` has data for this queue's ids.
    fn apply_artist_spread(&mut self) {
        if !self.spread_artists || self.artist_of.is_empty() {
            return;
        }
        for i in 1..self.entries.len() {
            let Some(prev_artist) = self.artist_of.get(&self.entries[i - 1].id).cloned() else { continue };
            if self.artist_of.get(&self.entries[i].id).map(String::as_str) == Some(prev_artist.as_str()) {
                if let Some(j) = (i + 1..self.entries.len())
                    .find(|&j| self.artist_of.get(&self.entries[j].id).map(String::as_str) != Some(prev_artist.as_str()))
                {
                    self.entries.swap(i, j);
                }
            }
        }
    }
}

// --- continuity: session snapshot/restore, autoplay continuation ----------

/// One materialized queue entry, as persisted in the session file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntrySnapshot {
    pub id: String,
    pub origin: QueueItemOrigin,
}

/// Identity of the active `Source` without its (potentially 50k-id) pools:
/// the session writer only re-writes the pools file when this changes, and a
/// restore refuses pools whose stamp doesn't match the core file's (a crash
/// between the two writes), dropping the source instead of mixing rounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStamp {
    pub kind: SourceKind,
    pub label: String,
    pub mode: SourceMode,
    pub pool_len: usize,
    pub remaining_len: usize,
    pub remaining_first: Option<String>,
    pub remaining_last: Option<String>,
    /// `QueueSourceInfo.id` (playlist/artist/genre/smart id), so a restored
    /// queue keeps Now Playing's "Playing from" link. Absent in older files.
    #[serde(default)]
    pub id: Option<String>,
    /// Flow sources: the energy arc (absent in older files and other modes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arc: Option<FlowArc>,
}

/// Everything about the queue except the source's id pools (small: the
/// materialized window plus history, bounded by `prune_history`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueCore {
    pub entries: Vec<EntrySnapshot>,
    pub current: Option<usize>,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    /// Only for a static (source-less) queue; empty otherwise.
    #[serde(default)]
    pub static_natural_order: Vec<String>,
    #[serde(default)]
    pub source: Option<SourceStamp>,
}

/// The source's full pool and the current round's not-yet-drawn tail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcePools {
    pub stamp: SourceStamp,
    pub pool: Vec<String>,
    pub remaining: Vec<String>,
}

/// Outcome of [`Queue::restore`], so the caller can be honest about what was
/// dropped.
pub struct Restored {
    pub queue: Queue,
    /// False when the saved current track no longer exists (the saved
    /// position then no longer applies to whatever is current now).
    pub current_kept: bool,
    /// How many materialized entries were dropped as unknown/deleted.
    pub dropped: usize,
}

impl Queue {
    pub fn snapshot_core(&self) -> QueueCore {
        QueueCore {
            entries: self.entries.iter().map(|e| EntrySnapshot { id: e.id.clone(), origin: e.origin }).collect(),
            current: self.current,
            shuffle: self.shuffle,
            repeat: self.repeat,
            static_natural_order: if self.source.is_none() { self.static_natural_order.clone() } else { Vec::new() },
            source: self.source_stamp(),
        }
    }

    pub fn snapshot_pools(&self) -> Option<SourcePools> {
        let src = self.source.as_ref()?;
        Some(SourcePools {
            stamp: self.source_stamp()?,
            pool: src.pool.clone(),
            remaining: src.remaining.iter().cloned().collect(),
        })
    }

    /// Cheap identity of the active source (no id lists cloned).
    pub fn source_stamp(&self) -> Option<SourceStamp> {
        self.source.as_ref().map(|s| SourceStamp {
            kind: s.kind,
            label: s.label.clone(),
            mode: s.mode,
            pool_len: s.pool.len(),
            remaining_len: s.remaining.len(),
            remaining_first: s.remaining.front().cloned(),
            remaining_last: s.remaining.back().cloned(),
            id: s.id.clone(),
            arc: matches!(s.mode, SourceMode::Flow).then_some(s.arc),
        })
    }

    /// Rebuilds a queue from a saved session. Ids for which `exists` is false
    /// (deleted from the library, or from another account) are dropped from
    /// the entries and from the source pools; the current entry moves to the
    /// nearest surviving one. `pools` is ignored unless it matches the core's
    /// stamp. Artist data for "spread artists" is not persisted; the caller
    /// re-derives it from the library with `fill_artists`.
    pub fn restore(
        core: QueueCore,
        pools: Option<SourcePools>,
        exists: &dyn Fn(&str) -> bool,
        window_size: usize,
        spread_artists: bool,
    ) -> Restored {
        let mut q = Queue::new();
        q.window_size = window_size.clamp(MIN_WINDOW_SIZE, MAX_WINDOW_SIZE);
        q.spread_artists = spread_artists;
        q.shuffle = core.shuffle;
        q.repeat = core.repeat;
        let total = core.entries.len();
        let mut new_current = None;
        let mut current_kept = false;
        for (i, e) in core.entries.into_iter().enumerate() {
            let keep = exists(&e.id);
            if core.current == Some(i) && keep {
                current_kept = true;
            }
            if keep {
                // First survivor at or after the saved current index.
                if new_current.is_none() && core.current.is_some_and(|c| i >= c) {
                    new_current = Some(q.entries.len());
                }
                q.entries.push(Entry { id: e.id, origin: e.origin });
            }
        }
        let dropped = total - q.entries.len();
        q.current = match (core.current, new_current) {
            (None, _) => None,
            (Some(_), Some(n)) => Some(n),
            (Some(_), None) => q.entries.len().checked_sub(1),
        };
        if core.source.is_none() {
            q.static_natural_order = core.static_natural_order.into_iter().filter(|id| exists(id)).collect();
        } else if let (Some(stamp), Some(p)) = (core.source, pools) {
            if p.stamp == stamp {
                let pool: Vec<String> = p.pool.into_iter().filter(|id| exists(id)).collect();
                let remaining: VecDeque<String> = p.remaining.into_iter().filter(|id| exists(id)).collect();
                if !pool.is_empty() {
                    let round_total = pool.len().max(remaining.len());
                    q.source = Some(Source {
                        kind: stamp.kind,
                        label: stamp.label,
                        id: stamp.id,
                        mode: stamp.mode,
                        arc: stamp.arc.unwrap_or_default(),
                        round_total,
                        quota: Quota::default(),
                        pool,
                        remaining,
                    });
                }
            }
        }
        Restored { queue: q, current_kept, dropped }
    }

    /// True when nothing is left to play after the current entry: it is the
    /// last materialized one, repeat is off and the source (if any) has
    /// nothing left to draw. This is the autoplay trigger condition.
    pub fn is_at_end(&self) -> bool {
        if self.repeat != RepeatMode::Off {
            return false;
        }
        match self.current {
            Some(pos) => {
                let source_drained = match &self.source {
                    Some(s) => s.remaining.is_empty(),
                    None => true,
                };
                pos + 1 >= self.entries.len() && source_drained
            }
            None => false,
        }
    }

    /// After the queue ran out (`next()` left `current` empty) makes the last
    /// entry current again if it is `expected` — the autoplay answer that
    /// arrived late continues from there. False (nothing changed) otherwise.
    pub fn reopen_last(&mut self, expected: &str) -> bool {
        if self.current.is_some() || self.entries.last().map(|e| e.id.as_str()) != Some(expected) {
            return false;
        }
        self.current = Some(self.entries.len() - 1);
        true
    }

    /// Appends a finite, in-order source *behind* the current entry without
    /// disturbing it or the history (autoplay radio): the queue keeps
    /// `current`, the new source's first window is drawn after it, so the
    /// engine's next gapless hand-off picks it up like any other next track.
    /// The source is always drawn in order (`SourceMode::InOrder`, the
    /// relevance order), but unlike `set_source` the global `shuffle` flag is
    /// left as the user set it: the queue ending is no reason to silently
    /// change how their next album/playlist plays. (Toggling shuffle while
    /// the radio plays re-derives it like any other source.)
    pub fn continue_with_source(&mut self, kind: SourceKind, label: String, pool: Vec<String>, artists: Vec<Option<String>>) {
        self.static_natural_order.clear();
        // The previous source (if any) is exhausted: its artists are useless.
        self.artist_of.clear();
        self.flow = Arc::default();
        self.flow_cov = None;
        if artists.len() == pool.len() {
            for (id, artist) in pool.iter().zip(artists) {
                if let Some(a) = artist {
                    self.artist_of.insert(id.clone(), a);
                }
            }
        }
        let remaining: VecDeque<String> = pool.clone().into();
        let round_total = remaining.len();
        self.source = Some(Source { kind, label, id: None, mode: SourceMode::InOrder, arc: FlowArc::Steady, round_total, quota: Quota::default(), pool, remaining });
        self.draw_into_window(self.window_size);
    }

    /// Test-only: how many ids have artist data (`fill_artists`/`set_source`).
    #[cfg(test)]
    pub(crate) fn artists_known(&self) -> usize {
        self.artist_of.len()
    }

    /// Re-derives the "spread artists" map after a restore (it isn't
    /// persisted): `artist(id)` for every id of the restored source's pool and
    /// materialized entries. A static queue never carries artist data
    /// (`set_queue` clears it), so this is a no-op without a source.
    pub fn fill_artists(&mut self, artist: &dyn Fn(&str) -> Option<String>) {
        let Some(src) = &self.source else { return };
        for id in src.pool.iter().map(String::as_str).chain(self.entries.iter().map(|e| e.id.as_str())) {
            if self.artist_of.contains_key(id) {
                continue;
            }
            if let Some(a) = artist(id) {
                self.artist_of.insert(id.to_string(), a);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("t{i}")).collect()
    }

    /// Test-only: starts a shuffled/in-order `Source` with a *reproducible*
    /// permutation, bypassing `set_source`'s real-entropy shuffle — mirrors
    /// `shuffle_in_place_seeded` below. Builds the queue by hand (not via
    /// `set_source`, which always reseeds from OS entropy) so refill/
    /// exhaustion/repeat tests aren't flaky.
    fn seeded_source(pool: Vec<String>, mode: SourceMode, window: usize, seed: u64) -> Queue {
        let mut q = Queue::new();
        q.window_size = window;
        q.shuffle = matches!(mode, SourceMode::Shuffle);
        let mut remaining: Vec<String> = pool.clone();
        if matches!(mode, SourceMode::Shuffle) {
            remaining.shuffle(&mut SmallRng::seed_from_u64(seed));
        }
        let round_total = remaining.len();
        q.source = Some(Source { kind: SourceKind::Playlist, label: "Test Source".into(), id: None, mode, arc: FlowArc::Steady, round_total, quota: Quota::default(), pool, remaining: remaining.into() });
        q.draw_into_window(window);
        if !q.entries.is_empty() {
            q.current = Some(0);
        }
        q
    }

    #[test]
    fn jump_to_sets_current_by_play_order_position() {
        let mut q = Queue::new();
        q.set_queue(vec!["a".into(), "b".into(), "c".into()], 0);
        assert!(q.jump_to(2));
        assert_eq!(q.current_id(), Some("c"));
        assert!(!q.jump_to(3), "out of range is ignored");
        assert_eq!(q.current_id(), Some("c"));
        q.reorder(2, 0); // c moves to the front; play order is now c, a, b
        assert!(q.jump_to(1));
        assert_eq!(q.current_id(), Some("a"));
    }

    #[test]
    fn set_queue_starts_at_requested_index() {
        let mut q = Queue::new();
        assert!(q.is_empty());
        q.set_queue(ids(5), 2);
        assert!(!q.is_empty());
        assert_eq!(q.current_id(), Some("t2"));
    }

    #[test]
    fn next_advances_and_stops_at_end_when_repeat_off() {
        let mut q = Queue::new();
        q.set_queue(ids(3), 0);
        assert_eq!(q.next(), Some("t1"));
        assert_eq!(q.next(), Some("t2"));
        assert_eq!(q.next(), None);
        assert_eq!(q.current_id(), None);
    }

    #[test]
    fn repeat_all_wraps_to_start() {
        let mut q = Queue::new();
        q.set_queue(ids(3), 0);
        q.set_repeat(RepeatMode::All);
        q.next();
        q.next();
        assert_eq!(q.next(), Some("t0"));
    }

    #[test]
    fn repeat_one_stays_on_current_track() {
        let mut q = Queue::new();
        q.set_queue(ids(3), 1);
        q.set_repeat(RepeatMode::One);
        assert_eq!(q.next(), Some("t1"));
        assert_eq!(q.next(), Some("t1"));
    }

    #[test]
    fn previous_steps_back_without_restart_logic() {
        let mut q = Queue::new();
        q.set_queue(ids(3), 2);
        assert_eq!(q.previous(), Some("t1"));
        assert_eq!(q.previous(), Some("t0"));
        // At the start with repeat off: stays put (engine decides restart
        // vs. no-op; this layer just reports "still t0").
        assert_eq!(q.previous(), Some("t0"));
    }

    #[test]
    fn shuffle_keeps_currently_playing_track_in_place() {
        let mut q = Queue::new();
        q.set_queue(ids(20), 7);
        let playing = q.current_id().unwrap().to_string();
        q.set_shuffle(true);
        assert_eq!(q.current_id(), Some(playing.as_str()));
        // Every id should still be present exactly once.
        let mut ordered = q.ordered_ids();
        ordered.sort();
        let mut expected = ids(20);
        expected.sort();
        assert_eq!(ordered, expected);
    }

    #[test]
    fn remove_current_track_advances_to_next_in_play_order() {
        let mut q = Queue::new();
        q.set_queue(ids(4), 1);
        q.remove(1); // remove "t1", currently playing
        assert_eq!(q.current_id(), Some("t2"));
        assert_eq!(q.ordered_ids(), vec!["t0", "t2", "t3"]);
    }

    #[test]
    fn page_matches_a_slice_of_ordered_ids() {
        let mut q = Queue::new();
        q.set_queue(ids(10), 0);
        let page_ids: Vec<String> = q.page(3, 4).into_iter().map(|(id, _)| id).collect();
        assert_eq!(page_ids, q.ordered_ids()[3..7]);
        let tail: Vec<String> = q.page(8, 10).into_iter().map(|(id, _)| id).collect();
        assert_eq!(tail, q.ordered_ids()[8..10], "limit past the end is clamped");
    }

    #[test]
    fn enqueue_next_inserts_right_after_current() {
        let mut q = Queue::new();
        q.set_queue(ids(3), 0);
        q.enqueue_next(vec!["x".into(), "y".into()]);
        assert_eq!(q.ordered_ids(), vec!["t0", "x", "y", "t1", "t2"]);
    }

    #[test]
    fn enqueue_end_appends() {
        let mut q = Queue::new();
        q.set_queue(ids(2), 0);
        q.enqueue_end(vec!["z".into()]);
        assert_eq!(q.ordered_ids(), vec!["t0", "t1", "z"]);
    }

    #[test]
    fn reorder_moves_item_and_tracks_current() {
        let mut q = Queue::new();
        q.set_queue(ids(4), 0);
        q.reorder(3, 0);
        assert_eq!(q.ordered_ids(), vec!["t3", "t0", "t1", "t2"]);
        assert_eq!(q.current_id(), Some("t0"));
    }

    // -----------------------------------------------------------------
    // Real-shuffle regression tests (static/no-source path). The bug: the
    // old implementation seeded a single xorshift RNG once (a constant
    // `0x5EED`) in `Queue::new` and only incremented it by 1 per shuffle, so
    // every fresh app start replayed the exact same sequence of "random"
    // permutations, and `next_u64() % n` was modulo-biased on top of that.
    // These tests pin down both "really seeded from entropy" and "really
    // unbiased".
    // -----------------------------------------------------------------

    #[test]
    fn shuffle_reseeds_every_time_not_just_once_at_construction() {
        // Two independent `Queue`s simulate two app starts. With real
        // entropy the chance of an identical 20-item permutation is
        // 1-in-20!; with the old constant-seed bug it was 100%.
        let mut a = Queue::new();
        a.set_queue(ids(20), 0);
        a.set_shuffle(true);
        let mut b = Queue::new();
        b.set_queue(ids(20), 0);
        b.set_shuffle(true);
        assert_ne!(a.ordered_ids(), b.ordered_ids(), "two fresh queues produced the identical permutation");
    }

    #[test]
    fn shuffle_reshuffles_differently_within_the_same_session() {
        let mut q = Queue::new();
        q.set_queue(ids(20), 0);
        q.set_shuffle(true);
        let first = q.ordered_ids();
        q.set_shuffle(false);
        q.set_shuffle(true);
        let second = q.ordered_ids();
        assert_ne!(first, second, "reshuffling produced the same order again");
    }

    #[test]
    fn repeat_all_wrap_reshuffles_and_avoids_the_same_track_first() {
        let mut q = Queue::new();
        q.set_queue(ids(8), 0);
        q.set_shuffle(true);
        q.set_repeat(RepeatMode::All);
        for _ in 0..q.ordered_ids().len() - 1 {
            q.next();
        }
        let last_before_wrap = q.current_id().unwrap().to_string();
        let order_before = q.ordered_ids();
        q.next(); // wraps: reshuffle + jump to position 0
        assert_eq!(q.current_position(), Some(0));
        assert_ne!(
            q.current_id(),
            Some(last_before_wrap.as_str()),
            "the track that just finished played again immediately after wrap"
        );
        let mut after = q.ordered_ids();
        after.sort();
        let mut expected = ids(8);
        expected.sort();
        assert_eq!(after, expected, "wrap must still be a full permutation of the same ids");
        assert_ne!(q.ordered_ids(), order_before, "wrap did not draw a fresh permutation");
    }

    /// Statistical sanity check (10k shuffles of 5 items): every item's
    /// position distribution should be roughly uniform, plus a
    /// first-position chi-square check across items. A modulo-biased
    /// picker (the old `next_u64() % n`) skews these measurably.
    #[test]
    fn shuffle_distribution_is_statistically_uniform() {
        const N: usize = 5;
        const TRIALS: u32 = 10_000;
        let mut position_counts = [[0u32; N]; N]; // [item][position]
        let item_ids = ids(N);
        let index_of: HashMap<&str, usize> =
            item_ids.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
        for _ in 0..TRIALS {
            let mut order = item_ids.clone();
            order.shuffle(&mut fresh_rng());
            for (pos, id) in order.iter().enumerate() {
                position_counts[index_of[id.as_str()]][pos] += 1;
            }
        }
        let expected = f64::from(TRIALS) / N as f64; // 2000
        for (item, counts) in position_counts.iter().enumerate() {
            for (pos, &count) in counts.iter().enumerate() {
                let observed = f64::from(count);
                let deviation = (observed - expected).abs() / expected;
                assert!(
                    deviation < 0.25,
                    "item {item} landed in position {pos} {observed} times (expected ~{expected}, {:.0}% off)",
                    deviation * 100.0
                );
            }
        }
        // Chi-square goodness-of-fit for the first-position distribution
        // (4 degrees of freedom; critical value at p=0.001 is ~18.5 — a
        // looser bound here avoids rare flakes while still catching real
        // bias).
        let chi_square: f64 = position_counts
            .iter()
            .map(|counts| {
                let observed = f64::from(counts[0]);
                (observed - expected).powi(2) / expected
            })
            .sum();
        assert!(chi_square < 30.0, "first-position chi-square too high: {chi_square} (biased shuffle?)");
    }

    #[test]
    fn spread_artists_reduces_adjacent_same_artist_pairs() {
        let mut q = Queue::new();
        let item_ids: Vec<String> = (0..9).map(|i| format!("t{i}")).collect();
        // 6 tracks by "A", 2 by "B", 1 by "C" — heavily skewed so an
        // unprocessed shuffle very likely has adjacent "A" pairs.
        let artist_names = ["A", "A", "A", "A", "A", "A", "B", "B", "C"];
        q.spread_artists = true;
        q.set_source(
            SourceKind::Playlist,
            "Test".into(),
            SourceMode::InOrder,
            item_ids.clone(),
            artist_names.iter().map(|a| Some((*a).to_string())).collect(),
            None,
        );
        // `set_source` drew everything in natural (unshuffled) order since
        // the pool is <= the window; drive a *seeded* shuffle + the same
        // artist-spread pass deterministically instead of depending on a
        // real shuffle's outcome.
        q.shuffle_in_place_seeded(7);
        let artist_of: HashMap<&str, &str> =
            item_ids.iter().map(String::as_str).zip(artist_names.iter().copied()).collect();
        let artist_sequence: Vec<&str> = q.ordered_ids().iter().map(|id| artist_of[id.as_str()]).collect();
        let adjacent_same_artist = artist_sequence.windows(2).filter(|w| w[0] == w[1]).count();
        // 6 "A"s among 9 slots can be split into at most 4 separated blocks
        // using only the 3 non-"A" items as dividers, so at least 6-4=2
        // adjacent "A" pairs are mathematically unavoidable; allow a little
        // slack above that floor for the heuristic (vs. an unprocessed
        // shuffle, which averages far more).
        assert!(
            adjacent_same_artist <= 3,
            "artist spread left {adjacent_same_artist} adjacent same-artist pairs: {artist_sequence:?}"
        );
    }

    // -----------------------------------------------------------------
    // Rolling window tests (the rolling-queue feature).
    // -----------------------------------------------------------------

    #[test]
    fn source_smaller_than_window_behaves_like_a_static_queue() {
        let mut q = Queue::new();
        q.set_source(SourceKind::Album, "Small Album".into(), SourceMode::InOrder, ids(8), vec![], None);
        assert_eq!(q.len(), 8, "a pool <= the window draws everything up front");
        assert_eq!(q.source_info().unwrap().3, 0, "nothing left undrawn");
        assert_eq!(q.current_id(), Some("t0"));
    }

    #[test]
    fn window_refills_once_fewer_than_half_remain_ahead() {
        // Window 10, pool 100: starts with 10 drawn, 90 remaining.
        let mut q = seeded_source(ids(100), SourceMode::Shuffle, 10, 1);
        assert_eq!(q.len(), 10);
        assert_eq!(q.source_info().unwrap().3, 90);
        // Advance until fewer than 5 remain ahead (window_size/2 = 5): no
        // refill should have happened yet while ahead count is still >= 5.
        for _ in 0..4 {
            q.next();
            assert_eq!(q.len(), 10, "no refill while still >= half the window ahead");
        }
        // One more step: now only 5 ahead (<10/2 is false, exactly at
        // threshold is still "no refill" since the check is `< half`), so
        // step once more to cross below.
        q.next();
        assert!(q.len() > 10, "window should have refilled once ahead count fell below half");
    }

    #[test]
    fn refill_draws_no_repeats_within_a_round() {
        // repeat off (default): exhausting the 50-id pool stops `next()`
        // (`None`) rather than ever looping back — checking each advance's
        // *return value* (not a post-hoc snapshot, which `prune_history`
        // would make an unreliable witness of the whole round) catches a
        // repeat regardless of how much history has since been pruned.
        let mut q = seeded_source(ids(50), SourceMode::Shuffle, 10, 2);
        let mut seen = std::collections::HashSet::new();
        seen.insert(q.current_id().unwrap().to_string());
        for _ in 0..60 {
            let Some(id) = q.next() else { break };
            assert!(seen.insert(id.to_string()), "a round must never draw the same id twice: repeated {id}");
        }
        assert_eq!(seen.len(), 50, "the whole 50-id pool should have been drawn exactly once");
    }

    #[test]
    fn exhausting_the_source_with_repeat_off_stops() {
        let mut q = seeded_source(ids(5), SourceMode::Shuffle, 10, 3); // pool < window: all drawn already
        for _ in 0..4 {
            assert!(q.next().is_some());
        }
        assert_eq!(q.next(), None, "repeat off: stops once the pool is exhausted");
        assert_eq!(q.current_id(), None);
    }

    #[test]
    fn exhausting_the_source_with_repeat_all_draws_a_fresh_permutation() {
        let mut q = seeded_source(ids(5), SourceMode::Shuffle, 5, 4);
        q.set_repeat(RepeatMode::All);
        // Force the exact "just exhausted" state directly rather than
        // stepping `next()` to it: with a window this size relative to the
        // pool, threshold-based refilling (`refill_if_needed`, exercised by
        // the dedicated `window_refills_once_fewer_than_half_remain_ahead`
        // test) would otherwise draw the fresh round a step or two early —
        // intentional (it keeps the window topped up well ahead of time),
        // but it would make *this* test's exhaustion point a moving target.
        q.current = Some(q.entries.len() - 1);
        q.source.as_mut().unwrap().remaining.clear();
        let last_of_round = q.current_id().unwrap().to_string();
        q.next(); // exhausted -> fresh round
        let fresh_id = q.current_id().map(str::to_string);
        assert!(fresh_id.is_some(), "repeat all redraws instead of stopping");
        assert_ne!(fresh_id.as_deref(), Some(last_of_round.as_str()), "fresh round must not start on the track that just finished");
        assert!(ids(5).contains(fresh_id.as_ref().unwrap()), "fresh round must still draw from the same pool");
    }

    #[test]
    fn user_queued_items_stay_ahead_of_source_items() {
        let mut q = seeded_source(ids(50), SourceMode::Shuffle, 10, 5);
        let next_from_source = q.page(1, 1)[0].0.clone();
        q.enqueue_next(vec!["user-a".into()]);
        q.enqueue_end(vec!["user-b".into()]);
        let page = q.page(0, 4);
        assert_eq!(page[1], ("user-a".to_string(), QueueItemOrigin::User));
        assert_eq!(page[2], ("user-b".to_string(), QueueItemOrigin::User));
        assert_eq!(page[3], (next_from_source, QueueItemOrigin::Source), "user items precede the rest of the source window");
        // Advancing plays through user items before the rest of the source.
        assert_eq!(q.next(), Some("user-a"));
        assert_eq!(q.next(), Some("user-b"));
    }

    #[test]
    fn removed_auto_item_is_not_redrawn() {
        let mut q = seeded_source(ids(30), SourceMode::Shuffle, 10, 6);
        let removed_id = q.page(3, 1)[0].0.clone();
        q.remove(3);
        for _ in 0..40 {
            let Some(id) = q.next() else { break };
            assert_ne!(id, removed_id.as_str(), "a removed auto item must never be redrawn this round");
        }
    }

    /// The not-yet-played count (what's still ahead in the window, plus
    /// whatever `Source::remaining` hasn't been drawn yet) is exactly
    /// conserved across a shuffle toggle — ids only ever move between
    /// "materialized ahead of current" and "not yet drawn", never created,
    /// dropped, or replayed.
    fn not_yet_played_count(q: &Queue) -> u64 {
        let pos = q.current_position().unwrap();
        let ahead_in_window = (q.ordered_ids().len() - pos - 1) as u64;
        ahead_in_window + q.source_info().unwrap().3
    }

    #[test]
    fn shuffle_toggle_mid_source_keeps_current_and_rederives_the_rest() {
        let mut q = seeded_source(ids(40), SourceMode::Shuffle, 10, 7);
        q.next();
        q.next();
        let current = q.current_id().unwrap().to_string();
        let before = not_yet_played_count(&q);

        q.set_shuffle(false);
        assert_eq!(q.current_id(), Some(current.as_str()), "toggling shuffle must not change the current track");
        assert_eq!(not_yet_played_count(&q), before, "re-deriving in-order must not gain or lose ids");

        q.set_shuffle(true);
        assert_eq!(q.current_id(), Some(current.as_str()), "toggling shuffle back must not change the current track");
        assert_eq!(not_yet_played_count(&q), before, "re-deriving shuffled must not gain or lose ids");
    }

    #[test]
    fn previous_still_walks_back_after_refills_and_pruning() {
        let mut q = seeded_source(ids(200), SourceMode::Shuffle, 10, 8);
        let mut played = vec![q.current_id().unwrap().to_string()];
        for _ in 0..40 {
            played.push(q.next().unwrap().to_string());
        }
        assert!(q.len() <= 3 * q.window_size(), "history is pruned: {} entries", q.len());
        // A full window's worth of history survives pruning.
        for k in 1..=q.window_size() {
            let expected = &played[played.len() - 1 - k];
            assert_eq!(q.previous(), Some(expected.as_str()), "previous step {k}");
        }
        // Then forward again replays the same (already drawn) order.
        let resume_at = played.len() - 1 - q.window_size();
        for expected in &played[resume_at + 1..] {
            assert_eq!(q.next(), Some(expected.as_str()));
        }
    }

    #[test]
    fn repeat_all_fresh_round_never_repeats_the_track_right_before_it() {
        // Pool 6, window 10: the refill that starts a new round happens
        // while part of the old round is still ahead, so the id to avoid is
        // the last materialized one, not `current`. Many trials, since the
        // fresh round is real-entropy shuffled.
        for trial in 0..300 {
            let mut q = seeded_source(ids(6), SourceMode::Shuffle, 10, trial);
            q.set_repeat(RepeatMode::All);
            let mut prev = q.current_id().unwrap().to_string();
            for _ in 0..30 {
                let id = q.next().unwrap().to_string();
                assert_ne!(id, prev, "trial {trial}: same track twice in a row across a round boundary");
                prev = id;
            }
        }
    }

    #[test]
    fn artist_spread_never_reorders_in_order_sources_or_moves_played_entries() {
        let pool = ids(30);
        let artists: Vec<Option<String>> = (0..30).map(|i| Some(if i % 3 == 0 { "B" } else { "A" }.to_string())).collect();
        let mut q = Queue::new();
        q.spread_artists = true;
        q.set_source(SourceKind::Playlist, "P".into(), SourceMode::InOrder, pool.clone(), artists.clone(), None);
        let mut played = vec![q.current_id().unwrap().to_string()];
        for _ in 0..29 {
            played.push(q.next().unwrap().to_string());
        }
        assert_eq!(played, pool, "an in-order source plays in pool order even with spread on");

        // Shuffled: once an entry has been reached, a later refill's spread
        // pass must not swap it out from under playback.
        let mut q = Queue::new();
        q.spread_artists = true;
        q.set_window_size(10);
        q.set_source(SourceKind::Playlist, "P".into(), SourceMode::Shuffle, pool, artists, None);
        for _ in 0..25 {
            let before: Vec<String> = q.ordered_ids()[..=q.current_position().unwrap()].to_vec();
            q.next();
            // Pruning only ever drops from the front, so the old played/
            // current run must still sit right before the new current.
            let after = q.ordered_ids();
            let tail_before = &before[before.len().saturating_sub(5)..];
            let cur_pos = q.current_position().unwrap();
            let tail_after = &after[cur_pos - tail_before.len()..cur_pos];
            assert_eq!(tail_after, tail_before, "played/current entries were reordered by a refill");
        }
    }

    #[test]
    fn removing_the_last_materialized_current_entry_moves_forward() {
        let mut q = seeded_source(ids(40), SourceMode::Shuffle, 10, 9);
        let last = q.len() - 1;
        let behind = q.page(last - 1, 1)[0].0.clone();
        assert!(q.jump_to(last));
        // jump_to refilled behind it; force the "current is the last
        // materialized entry" state the remove fix is about.
        q.entries.truncate(last + 1);
        q.remove(last);
        assert_ne!(q.current_id(), Some(behind.as_str()), "must not step back onto the previous track");
        assert!(q.current_id().is_some());
        assert_eq!(q.current_position(), Some(last));
    }

    #[test]
    fn shuffled_source_plays_each_distinct_track_once_per_round_despite_duplicate_refs() {
        // Like the real "Liked Songs": every track listed several times.
        let pool: Vec<String> = (0..4).flat_map(|_| ids(30)).collect();
        let mut q = Queue::new();
        q.set_window_size(10);
        q.set_source(SourceKind::Playlist, "Dupes".into(), SourceMode::Shuffle, pool, vec![], None);
        let mut seen = std::collections::HashSet::new();
        seen.insert(q.current_id().unwrap().to_string());
        while let Some(id) = q.next() {
            assert!(seen.insert(id.to_string()), "{id} played twice in one round");
        }
        assert_eq!(seen.len(), 30);
    }

    #[test]
    fn window_size_is_clamped_to_the_allowed_range() {
        let mut q = Queue::new();
        q.set_window_size(1);
        assert_eq!(q.window_size(), MIN_WINDOW_SIZE);
        q.set_window_size(1000);
        assert_eq!(q.window_size(), MAX_WINDOW_SIZE);
        q.set_window_size(42);
        assert_eq!(q.window_size(), 42);
    }

    // --- continuity -------------------------------------------------------

    fn all_exist(_: &str) -> bool {
        true
    }

    #[test]
    fn snapshot_roundtrips_a_shuffled_source_with_the_same_remaining_round() {
        let mut q = Queue::new();
        q.set_window_size(10);
        q.set_source(SourceKind::Playlist, "Liked".into(), SourceMode::Shuffle, ids(100), vec![], None);
        q.next();
        q.next();
        let core = q.snapshot_core();
        let pools = q.snapshot_pools().unwrap();
        // Through JSON, as the session files do.
        let core: QueueCore = serde_json::from_str(&serde_json::to_string(&core).unwrap()).unwrap();
        let pools: SourcePools = serde_json::from_str(&serde_json::to_string(&pools).unwrap()).unwrap();
        let r = Queue::restore(core, Some(pools), &all_exist, 10, false);
        assert!(r.current_kept);
        assert_eq!(r.dropped, 0);
        assert_eq!(r.queue.ordered_ids(), q.ordered_ids());
        assert_eq!(r.queue.current_id(), q.current_id());
        assert_eq!(r.queue.source_info(), q.source_info(), "same label/mode and the same remaining count");
        // And the *same* remaining round: draining both yields identical order.
        let (mut a, mut b) = (q, r.queue);
        a.repeat = RepeatMode::Off;
        loop {
            let (x, y) = (a.next().map(str::to_string), b.next().map(str::to_string));
            assert_eq!(x, y);
            if x.is_none() {
                break;
            }
        }
    }

    #[test]
    fn restore_drops_deleted_ids_and_moves_current_to_the_nearest_survivor() {
        let mut q = Queue::new();
        q.set_queue(ids(6), 3); // t3 current
        q.enqueue_next(vec!["u1".into()]);
        let core = q.snapshot_core();
        // t3 (current) and t1 were deleted.
        let exists = |id: &str| id != "t3" && id != "t1";
        let r = Queue::restore(core.clone(), None, &exists, 20, false);
        assert_eq!(r.dropped, 2);
        assert!(!r.current_kept);
        // Play order was t0 t1 t2 t3 u1 t4 t5.
        let expected: Vec<String> = ["t0", "t2", "u1", "t4", "t5"].iter().map(|s| s.to_string()).collect();
        assert_eq!(r.queue.ordered_ids(), expected);
        assert_eq!(r.queue.current_id(), Some("u1"), "first survivor after the deleted current");

        // Everything deleted: an empty queue with nothing current.
        let r = Queue::restore(core, None, &|_: &str| false, 20, false);
        assert!(r.queue.is_empty());
        assert_eq!(r.queue.current_id(), None);
    }

    #[test]
    fn restore_ignores_pools_that_do_not_match_the_core_stamp() {
        let mut q = Queue::new();
        q.set_source(SourceKind::Playlist, "P".into(), SourceMode::InOrder, ids(100), vec![], None);
        let core = q.snapshot_core();
        let mut pools = q.snapshot_pools().unwrap();
        pools.stamp.remaining_len += 1; // written by a different save
        let r = Queue::restore(core, Some(pools), &all_exist, 20, false);
        assert!(r.queue.source_info().is_none(), "stale pools drop the source, entries stay");
        assert_eq!(r.queue.len(), q.len());
    }

    #[test]
    fn at_end_is_the_autoplay_trigger_and_only_with_repeat_off_and_a_drained_source() {
        let mut q = Queue::new();
        assert!(!q.is_at_end(), "empty queue: nothing playing, nothing to continue from");
        q.set_queue(ids(2), 0);
        assert!(!q.is_at_end());
        q.next();
        assert!(q.is_at_end(), "last track of a static queue");
        q.set_repeat(RepeatMode::All);
        assert!(!q.is_at_end(), "repeat all never ends");
        q.set_repeat(RepeatMode::One);
        assert!(!q.is_at_end());

        // A source with material left is not at its end even on the last entry.
        let mut s = Queue::new();
        s.set_window_size(10);
        s.set_source(SourceKind::Playlist, "P".into(), SourceMode::InOrder, ids(25), vec![], None);
        let last = s.len() - 1;
        s.jump_to(last);
        assert!(s.source_info().unwrap().3 > 0, "ids still undrawn after the window");
        assert!(!s.is_at_end(), "never at end while the source can still draw");
        while s.next().is_some() {}
        assert_eq!(s.current_id(), None, "exhausted: next() cleared current");
    }

    #[test]
    fn continue_with_source_keeps_current_and_history_and_plays_the_new_tracks_next() {
        let mut q = Queue::new();
        q.set_queue(ids(3), 2); // last track playing
        assert!(q.is_at_end());
        q.continue_with_source(SourceKind::Radio, "Autoplay · t2".into(), vec!["r1".into(), "r2".into()], vec![]);
        assert_eq!(q.current_id(), Some("t2"));
        assert!(!q.is_at_end());
        assert_eq!(q.source_info().unwrap().1, "Autoplay · t2");
        assert_eq!(q.next(), Some("r1"));
        assert_eq!(q.next(), Some("r2"));
        assert!(q.is_at_end());
        assert_eq!(q.previous(), Some("r1"));
    }

    #[test]
    fn continue_with_source_leaves_the_users_shuffle_setting_alone() {
        let mut q = Queue::new();
        q.set_shuffle(true);
        q.set_queue(ids(3), 0);
        let last = q.len() - 1;
        q.jump_to(last);
        let cur = q.current_id().unwrap().to_string();
        q.continue_with_source(SourceKind::Radio, "Autoplay".into(), vec!["r1".into(), "r2".into(), "r3".into()], vec![]);
        assert!(q.shuffle, "the global shuffle mode is not flipped by autoplay");
        assert_eq!(q.current_id(), Some(cur.as_str()));
        assert_eq!(q.source_info().unwrap().2, SourceMode::InOrder, "radio still plays in relevance order");
        assert_eq!(q.next(), Some("r1"));
        assert_eq!(q.next(), Some("r2"));
    }

    #[test]
    fn fill_artists_restores_spread_data_for_a_restored_source() {
        let mut q = Queue::new();
        q.spread_artists = true;
        q.set_source(SourceKind::Playlist, "P".into(), SourceMode::Shuffle, ids(30), vec![], None);
        let r = Queue::restore(q.snapshot_core(), q.snapshot_pools(), &all_exist, 20, true);
        let mut rq = r.queue;
        assert!(rq.artist_of.is_empty(), "not persisted");
        rq.fill_artists(&|id: &str| Some(format!("artist-of-{id}")));
        assert_eq!(rq.artist_of.len(), 30);
        // Static queues never carry artist data.
        let mut s = Queue::new();
        s.set_queue(ids(3), 0);
        s.fill_artists(&|_: &str| Some("x".into()));
        assert!(s.artist_of.is_empty());
    }
    // --- feat-flow ---------------------------------------------------------

    fn flow_features(n: usize) -> HashMap<String, FlowTrack> {
        (0..n)
            .map(|i| {
                let t = FlowTrack {
                    bpm: (i % 4 != 0).then_some(100.0 + (i % 50) as f32),
                    key: (i % 3 == 0).then(|| sequence::Camelot { num: (i % 12) as u8 + 1, minor: i % 2 == 0 }),
                    genre: Some(if i % 2 == 0 { "Techno".into() } else { "House".into() }),
                    artist: Some(format!("artist{}", i % 9).as_str().into()),
                    ..Default::default()
                };
                (format!("t{i}"), t)
            })
            .collect()
    }

    fn flow_queue(n: usize, window: usize, seed: u64) -> Queue {
        let mut q = Queue::new();
        q.window_size = window;
        q.set_source_flow_seeded(FlowArc::Steady, ids(n), flow_features(n), seed);
        q
    }

    fn play_out(q: &mut Queue) -> Vec<String> {
        let mut played = vec![q.current_id().unwrap().to_string()];
        while let Some(id) = q.next() {
            played.push(id.to_string());
        }
        played
    }

    #[test]
    fn flow_source_plays_each_track_once_per_round_through_refills() {
        let mut q = flow_queue(120, 10, 1);
        assert!(q.is_flow());
        assert_eq!(q.len(), 10, "only a window is materialized");
        let played = play_out(&mut q);
        assert_eq!(played.len(), 120);
        let set: std::collections::HashSet<_> = played.iter().collect();
        assert_eq!(set.len(), 120, "no repeats within the round");
        assert!(q.len() <= 2 * 10 + 2, "history pruning keeps the queue bounded: {}", q.len());
    }

    #[test]
    fn flow_is_deterministic_per_seed() {
        let a = flow_queue(60, 20, 5);
        let b = flow_queue(60, 20, 5);
        let c = flow_queue(60, 20, 6);
        assert_eq!(a.ordered_ids(), b.ordered_ids());
        assert_ne!(a.ordered_ids(), c.ordered_ids());
        assert_eq!(a.source_info().unwrap().2, SourceMode::Flow);
    }

    #[test]
    fn starting_flow_switches_the_global_shuffle_flag_off_like_in_order_does() {
        let mut q = Queue::new();
        q.shuffle = true;
        q.set_source_flow(SourceKind::Playlist, "P".into(), FlowArc::Steady, ids(30), flow_features(30), None);
        assert!(!q.shuffle, "Flow is its own mode: the Shuffle button must read off, and pressing it converts the source");
        assert!(q.is_flow());
    }

    #[test]
    fn flow_opens_with_the_requested_track_and_continues_from_it() {
        let mut q = Queue::new();
        q.set_source_flow(SourceKind::Playlist, "P".into(), FlowArc::Steady, ids(40), flow_features(40), Some("t17"));
        assert_eq!(q.current_id(), Some("t17"));
        assert_eq!(q.len(), q.window_size());
        let set: std::collections::HashSet<_> = q.ordered_ids().into_iter().collect();
        assert_eq!(set.len(), q.len());
    }

    #[test]
    fn flow_keeps_user_queued_items_ahead_of_the_source() {
        let mut q = flow_queue(50, 10, 2);
        q.enqueue_next(vec!["user-a".into()]);
        q.enqueue_end(vec!["user-b".into()]);
        assert_eq!(q.next(), Some("user-a"));
        assert_eq!(q.next(), Some("user-b"));
        for _ in 0..30 {
            q.next();
        }
        assert!(q.page(0, 100).iter().skip(q.current_position().unwrap() + 1).all(|(_, o)| *o == QueueItemOrigin::Source));
    }

    #[test]
    fn flow_repeat_all_draws_a_fresh_round_without_a_back_to_back_repeat_or_duplicates() {
        let mut pool = ids(8);
        pool.push("t3".into()); // a duplicate playlist ref
        let mut feats = flow_features(8);
        feats.remove("t5");
        for trial in 0..20u64 {
            let mut q = Queue::new();
            q.window_size = 10;
            q.set_source_flow_seeded(FlowArc::Peak, pool.clone(), feats.clone(), trial);
            q.set_repeat(RepeatMode::All);
            let mut played = vec![q.current_id().unwrap().to_string()];
            for _ in 0..23 {
                played.push(q.next().unwrap().to_string());
            }
            for round in played.chunks(8) {
                let set: std::collections::HashSet<_> = round.iter().collect();
                assert_eq!(set.len(), round.len(), "trial {trial}: a round repeats a track: {round:?}");
            }
            assert!(played.windows(2).all(|w| w[0] != w[1]), "trial {trial}: back-to-back repeat: {played:?}");
        }
    }

    #[test]
    fn flow_repeat_off_just_runs_out() {
        let mut q = flow_queue(5, 10, 3);
        assert_eq!(play_out(&mut q).len(), 5);
    }

    #[test]
    fn flow_reasons_use_real_values_for_source_entries_only() {
        let mut q = flow_queue(30, 20, 4);
        assert!(q.flow_reason(0).is_none(), "the first row follows nothing");
        let with_reason = (1..q.len()).filter(|&i| q.flow_reason(i).is_some()).count();
        assert!(with_reason > 5);
        for i in 1..q.len() {
            if let Some(r) = q.flow_reason(i) {
                let prev = &q.flow[&q.entries[i - 1].id];
                let cur = &q.flow[&q.entries[i].id];
                assert_eq!(r.bpm_from.is_some(), prev.bpm.is_some() && cur.bpm.is_some());
                assert_eq!(r.key_from.is_some(), prev.key.is_some() && cur.key.is_some());
            }
        }
        q.enqueue_next(vec!["u".into()]);
        let pos = q.current_position().unwrap() + 1;
        assert!(q.flow_reason(pos).is_none(), "user-queued rows have no flow reason");
        // Non-Flow sources never report one.
        let s = seeded_source(ids(30), SourceMode::Shuffle, 10, 1);
        assert!((0..s.len()).all(|i| s.flow_reason(i).is_none()));
        assert!(s.flow_info().is_none());
    }

    #[test]
    fn flow_coverage_counts_distinct_pool_tracks() {
        let mut pool = ids(20);
        pool.push("t1".into());
        let mut q = Queue::new();
        q.set_source_flow_seeded(FlowArc::Build, pool, flow_features(20), 1);
        let (arc, cov) = q.flow_info().unwrap();
        assert_eq!(arc, FlowArc::Build);
        assert_eq!(cov.total, 20);
        assert_eq!(cov.bpm, (0..20).filter(|i| i % 4 != 0).count() as u32);
        assert_eq!(cov.key, (0..20).filter(|i| i % 3 == 0).count() as u32);
    }

    #[test]
    fn flow_source_survives_a_session_round_trip_with_mode_arc_and_remaining_pool() {
        let mut q = flow_queue(100, 20, 9);
        q.next();
        q.next();
        q.set_source_id(Some("pl1".into()));
        let core = q.snapshot_core();
        let pools = q.snapshot_pools();
        let json = serde_json::to_string(&core).unwrap();
        assert!(json.contains("\"flow\""), "mode serializes as a plain string");
        let core2: QueueCore = serde_json::from_str(&json).unwrap();
        let r = Queue::restore(core2, pools, &all_exist, 20, false);
        let mut q2 = r.queue;
        assert!(q2.is_flow());
        assert_eq!(q2.source_info().map(|s| (s.2, s.3)), q.source_info().map(|s| (s.2, s.3)));
        assert_eq!(q2.ordered_ids(), q.ordered_ids());
        // Features are not persisted: until the engine supplies them the restored source has no reasons...
        assert!(q2.flow_reason(2).is_none());
        // ...and it asks for them again (`flow_wanted_ids` -> `set_flow_features`).
        assert!(q2.flow_wanted_ids().len() >= 100);
        q2.set_flow_features(flow_features(100));
        assert_eq!(q2.flow_info().unwrap().1.total, 100);
        let played = play_out(&mut q2);
        let set: std::collections::HashSet<_> = played.iter().collect();
        assert_eq!(set.len(), played.len());
        // The remaining round is the same set the original would have drawn.
        let mut rest: Vec<String> = play_out(&mut q);
        rest.sort();
        let mut got = played.clone();
        got.sort();
        assert_eq!(got, rest);
    }

    #[test]
    fn an_old_stamp_without_an_arc_still_parses_and_a_shuffle_toggle_overrides_flow() {
        let q = seeded_source(ids(40), SourceMode::InOrder, 10, 1);
        let json = serde_json::to_string(&q.source_stamp().unwrap()).unwrap();
        assert!(!json.contains("arc"));
        let _: SourceStamp = serde_json::from_str(&json).unwrap();
        // Documented behavior: an explicit shuffle toggle converts a Flow source.
        let mut f = flow_queue(40, 10, 2);
        f.set_shuffle(true);
        assert_eq!(f.source_info().unwrap().2, SourceMode::Shuffle);
        assert!(f.flow_info().is_none());
    }

    #[test]
    fn a_flow_to_shuffle_toggle_hands_artists_to_spread_and_drops_the_flow_data() {
        let mut f = flow_queue(40, 10, 3);
        assert_eq!(f.artists_known(), 0, "a Flow source keeps its artists in the flow data");
        f.set_shuffle(true);
        assert_eq!(f.artists_known(), 40, "Spread artists works for the converted source");
        assert!(f.flow.is_empty());
    }

    #[test]
    fn cloning_a_flow_queue_shares_the_flow_data() {
        // The engine clones the queue at every track start and EOF (prefetch,
        // gapless `queue_after`): the per-id data of a 50k pool must not be
        // deep-copied each time.
        let q = flow_queue(200, 20, 1);
        let c = q.clone();
        assert!(Arc::ptr_eq(&q.flow, &c.flow));
        assert_eq!(c.flow.len(), 200);
    }

    fn known_ids(q: &Queue, ids: &[String]) -> usize {
        ids.iter().filter(|id| q.flow.get(*id).is_some_and(sequence::energy_known)).count()
    }

    fn quota_of(q: &Queue) -> Quota {
        q.source.as_ref().unwrap().quota
    }

    #[test]
    fn the_flow_quota_counts_the_round_and_is_rebuilt_on_regenerate_and_restore() {
        // flow_features: bpm (so a known energy) on 3 of every 4 tracks.
        let mut q = Queue::new();
        q.window_size = 10;
        q.set_source_flow_seeded(FlowArc::Build, ids(40), flow_features(40), 4);
        let drawn: Vec<String> = q.ordered_ids();
        assert_eq!(quota_of(&q), Quota { known: 30, drawn: known_ids(&q, &drawn) });

        // Restore: the quota is not persisted, `set_flow_features` rebuilds it
        // from what is left in the round.
        q.next();
        let core = q.snapshot_core();
        let pools = q.snapshot_pools();
        let mut r = Queue::restore(core, pools, &all_exist, 10, false).queue;
        r.set_flow_features(flow_features(40));
        assert_eq!(quota_of(&r), quota_of(&q));

        // Repeat-all: a fresh round starts a fresh count.
        q.set_repeat(RepeatMode::All);
        for _ in 0..45 {
            q.next();
        }
        let src = q.source.as_ref().unwrap();
        let this_round: Vec<String> = q.flow.keys().filter(|id| !src.remaining.contains(id)).cloned().collect();
        assert_eq!(src.quota.known, 30);
        assert_eq!(src.quota.drawn, known_ids(&q, &this_round), "only this round's draws count");
    }

    #[test]
    fn build_flow_draws_known_energy_tracks_at_their_share() {
        // 1 in 4 tracks has a tempo, the rest nothing: Build must not push
        // the tagged ones to the end of the round (the old penalty-only arc did).
        let n = 400;
        let feats: HashMap<String, FlowTrack> = (0..n)
            .map(|i| {
                let t = FlowTrack {
                    bpm: (i % 4 == 0).then_some(80.0 + (i % 90) as f32),
                    artist: Some(format!("artist{}", i % 37).as_str().into()),
                    ..Default::default()
                };
                (format!("t{i}"), t)
            })
            .collect();
        let mut q = Queue::new();
        q.window_size = 20;
        q.set_source_flow_seeded(FlowArc::Build, ids(n), feats, 7);
        let played = play_out(&mut q);
        assert_eq!(played.len(), n);
        let first = known_ids(&q, &played[..100]);
        let last = known_ids(&q, &played[n - 100..]);
        assert!((15..=35).contains(&first) && (15..=35).contains(&last), "first 100: {first}, last 100: {last} (base 25)");
    }
}
