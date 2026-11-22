//! Property tests of the play queue (`audio::queue::Queue`) through its public API: random operation sequences against a
//! model, rounds of a windowed source, shuffle, "spread artists" and Flow. A failure prints the minimal operation list;
//! proptest stores its seed under `proptest-regressions/` (commit that file) and replays it first on the next run.
use proptest::prelude::*;
use asphonix_core::audio::queue::Queue;
use asphonix_core::audio::sequence::{self, Camelot, FlowTrack, Lean, Slot};
use asphonix_core::ipc::{FlowArc, RepeatMode, SourceKind, SourceMode};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
enum Op {
    Next,
    Prev,
    Jump(usize),
    Reorder(usize, usize),
    Remove(usize),
    EnqNext(usize),
    EnqEnd(usize),
    Repeat(RepeatMode),
}

fn op() -> impl Strategy<Value = Op> {
    // indices are taken modulo `len + 2` when applied, so some are always out of range
    prop_oneof![
        4 => Just(Op::Next),
        2 => Just(Op::Prev),
        1 => any::<usize>().prop_map(Op::Jump),
        2 => (any::<usize>(), any::<usize>()).prop_map(|(a, b)| Op::Reorder(a, b)),
        2 => any::<usize>().prop_map(Op::Remove),
        1 => (0usize..3).prop_map(Op::EnqNext),
        1 => (0usize..3).prop_map(Op::EnqEnd),
        1 => prop_oneof![Just(RepeatMode::Off), Just(RepeatMode::All), Just(RepeatMode::One)].prop_map(Op::Repeat),
    ]
}

/// What a static (no source, shuffle off) queue must do, written as plainly as possible.
#[derive(Debug, Default)]
struct Model {
    ids: Vec<String>,
    cur: Option<usize>,
    repeat: Option<RepeatMode>,
    fresh: usize,
}

impl Model {
    fn new_ids(&mut self, n: usize) -> Vec<String> {
        (0..n)
            .map(|_| {
                self.fresh += 1;
                format!("u{}", self.fresh)
            })
            .collect()
    }

    /// Applies `op` to the model and the queue alike.
    fn apply(&mut self, q: &mut Queue, op: &Op) {
        let len = self.ids.len();
        let ix = |i: usize| i % (len + 2);
        let repeat = self.repeat.unwrap_or(RepeatMode::Off);
        match *op {
            Op::Next => {
                q.next();
                if len > 0 && repeat != RepeatMode::One {
                    self.cur = match self.cur {
                        None => Some(0),
                        Some(p) if p + 1 < len => Some(p + 1),
                        Some(_) if repeat == RepeatMode::All => Some(0),
                        Some(_) => None,
                    };
                }
            }
            Op::Prev => {
                q.previous();
                match self.cur {
                    Some(p) if p > 0 => self.cur = Some(p - 1),
                    Some(_) if repeat == RepeatMode::All => self.cur = Some(len - 1),
                    _ => {}
                }
            }
            Op::Jump(p) => {
                let p = ix(p);
                q.jump_to(p);
                if p < len {
                    self.cur = Some(p);
                }
            }
            Op::Reorder(f, t) => {
                let (f, t) = (ix(f), ix(t));
                q.reorder(f, t);
                if f < len && t < len {
                    let moved = self.ids.remove(f);
                    self.ids.insert(t, moved);
                    self.cur = self.cur.map(|c| {
                        if c == f {
                            t
                        } else if f < c && c <= t {
                            c - 1
                        } else if t <= c && c < f {
                            c + 1
                        } else {
                            c
                        }
                    });
                }
            }
            Op::Remove(i) => {
                let i = ix(i);
                q.remove(i);
                if i < len {
                    self.ids.remove(i);
                    self.cur = match self.cur {
                        Some(c) if c == i => {
                            if i < self.ids.len() {
                                Some(i)
                            } else {
                                self.ids.len().checked_sub(1)
                            }
                        }
                        Some(c) if c > i => Some(c - 1),
                        c => c,
                    };
                }
            }
            Op::EnqNext(n) => {
                let new = self.new_ids(n);
                q.enqueue_next(new.clone());
                let at = self.cur.map(|c| c + 1).unwrap_or(0).min(len);
                self.ids.splice(at..at, new);
            }
            Op::EnqEnd(n) => {
                let new = self.new_ids(n);
                q.enqueue_end(new.clone());
                self.ids.extend(new);
            }
            Op::Repeat(m) => {
                q.set_repeat(m);
                self.repeat = Some(m);
            }
        }
    }
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

fn distinct_pool(max: usize) -> impl Strategy<Value = Vec<String>> {
    (0..max).prop_map(|n| (0..n).map(|i| format!("p{i}")).collect())
}

/// Plays a windowed source to its end (repeat off): the current id plus every id `next` moves to.
fn drain(q: &mut Queue) -> Vec<String> {
    let mut played: Vec<String> = q.current_id().map(str::to_string).into_iter().collect();
    while let Some(id) = q.next() {
        played.push(id.to_string());
        assert!(played.len() < 100_000, "a repeat-off source must end");
    }
    played
}

proptest! {
    /// A static queue (an explicit "play these" list, duplicates allowed) under any sequence of transport and edit
    /// operations: the rows are exactly the model's (nothing lost, duplicated or reordered beyond the operation), and
    /// `current` always points at a row (or nothing).
    #[test]
    fn static_queue_matches_the_model(
        init in prop::collection::vec(prop::sample::select(vec!["a", "b", "c", "d"]), 0..12),
        start in any::<usize>(),
        ops in prop::collection::vec(op(), 0..80),
    ) {
        let ids: Vec<String> = init.iter().map(|s| s.to_string()).collect();
        let mut q = Queue::new();
        q.set_queue(ids.clone(), start % ids.len().max(1));
        let mut m = Model { cur: (!ids.is_empty()).then(|| (start % ids.len().max(1)).min(ids.len() - 1)), ids, ..Model::default() };
        for op in &ops {
            m.apply(&mut q, op);
            prop_assert_eq!(q.ordered_ids(), m.ids.clone(), "rows after {:?}", op);
            prop_assert_eq!(q.current_position(), m.cur, "current after {:?}", op);
            prop_assert!(q.current_position().is_none_or(|c| c < q.len()));
            prop_assert_eq!(q.current_id().map(str::to_string), m.cur.map(|c| m.ids[c].clone()));
        }
    }

    /// Shuffling a static queue is a permutation that starts on the chosen row; turning shuffle off restores the
    /// natural order and keeps the playing track.
    #[test]
    fn static_shuffle_is_a_permutation_and_keeps_the_playing_track(
        init in prop::collection::vec(prop::sample::select(vec!["a", "b", "c", "d", "e", "f"]), 1..30),
        start in any::<usize>(),
    ) {
        let ids: Vec<String> = init.iter().map(|s| s.to_string()).collect();
        let start = start % ids.len();
        let mut q = Queue::new();
        q.set_shuffle(true);
        q.set_queue(ids.clone(), start);
        prop_assert_eq!(sorted(q.ordered_ids()), sorted(ids.clone()));
        prop_assert_eq!(q.current_position(), Some(0));
        prop_assert_eq!(q.current_id(), Some(ids[start].as_str()));
        q.set_shuffle(false);
        prop_assert_eq!(q.ordered_ids(), ids.clone());
        prop_assert_eq!(q.current_id(), Some(ids[start].as_str()));
    }

    /// A windowed source played to its end, while the user edits what is still ahead (reorder, remove, enqueue):
    /// every pool track is played or removed exactly once (no repeats within a round, none lost), user-queued tracks
    /// play exactly once too, and the materialized window stays bounded.
    #[test]
    fn a_source_round_plays_every_track_once(
        pool in distinct_pool(70),
        shuffle in any::<bool>(),
        window in 10usize..25,
        edits in prop::collection::vec((0u8..4, any::<usize>(), any::<usize>()), 0..60),
    ) {
        let mut q = Queue::new();
        q.set_window_size(window);
        let mode = if shuffle { SourceMode::Shuffle } else { SourceMode::InOrder };
        q.set_source(SourceKind::Playlist, "P".into(), mode, pool.clone(), Vec::new(), None);
        let mut played: Vec<String> = q.current_id().map(str::to_string).into_iter().collect();
        let (mut removed, mut queued) = (Vec::new(), Vec::new());
        for (n, (kind, a, b)) in edits.into_iter().enumerate() {
            let Some(cur) = q.current_position() else { break };
            let ahead = q.len() - cur - 1;
            match kind {
                0 => {
                    if let Some(id) = q.next() {
                        played.push(id.to_string());
                    }
                }
                1 if ahead >= 2 => q.reorder(cur + 1 + a % ahead, cur + 1 + b % ahead),
                2 if ahead >= 1 => {
                    let i = cur + 1 + a % ahead;
                    removed.push(q.page(i, 1)[0].0.clone());
                    q.remove(i);
                }
                3 => {
                    let id = format!("u{n}");
                    queued.push(id.clone());
                    if a % 2 == 0 { q.enqueue_next(vec![id]) } else { q.enqueue_end(vec![id]) }
                }
                _ => {}
            }
            let user = queued.len();
            prop_assert!(q.len() <= 2 * window + 1 + user, "window bounded: {} rows", q.len());
        }
        if q.current_position().is_some() {
            played.extend(drain(&mut q).into_iter().skip(1));
        }
        let (user_played, source_played): (Vec<String>, Vec<String>) = played.into_iter().partition(|id| id.starts_with('u'));
        let (user_removed, source_removed): (Vec<String>, Vec<String>) = removed.into_iter().partition(|id| id.starts_with('u'));
        let mut seen = source_played.clone();
        seen.extend(source_removed);
        prop_assert_eq!(sorted(seen), sorted(pool.clone()), "every pool track exactly once");
        let mut user_seen = user_played;
        user_seen.extend(user_removed);
        prop_assert_eq!(sorted(user_seen), sorted(queued));
    }

    /// In order, untouched: a source plays its pool from the start track to the end, in pool order.
    #[test]
    fn an_in_order_source_plays_the_pool_from_the_start_track(pool in distinct_pool(80), start in any::<usize>(), window in 10usize..30) {
        let mut q = Queue::new();
        q.set_window_size(window);
        let start_ix = start % pool.len().max(1);
        let start_id = pool.get(start_ix).cloned();
        q.set_source(SourceKind::Playlist, "P".into(), SourceMode::InOrder, pool.clone(), Vec::new(), start_id.as_deref());
        let played = drain(&mut q);
        let want: Vec<String> = if pool.is_empty() { Vec::new() } else { pool[start_ix..].to_vec() };
        prop_assert_eq!(played, want);
    }

    /// "Spread artists" on a shuffled source: still a permutation of the (deduplicated) pool, and a same-artist
    /// neighbour pair only remains where nothing else is left to swap in: from such a pair on, every later track is
    /// by that artist (the greedy pass cannot separate a run with no other artist behind it).
    #[test]
    fn spread_artists_separates_neighbours_wherever_possible(
        artists in prop::collection::vec(prop::option::weighted(0.9, prop::sample::select(vec!["A", "B", "C"])), 0..20),
    ) {
        let pool: Vec<String> = (0..artists.len()).map(|i| format!("p{i}")).collect();
        let by_id: HashMap<String, Option<&str>> = pool.iter().cloned().zip(artists.iter().copied()).collect();
        let mut q = Queue::new();
        q.spread_artists = true;
        q.set_source(SourceKind::Playlist, "P".into(), SourceMode::Shuffle, pool.clone(), artists.iter().map(|a| a.map(str::to_string)).collect(), None);
        let order = q.ordered_ids();
        prop_assert_eq!(sorted(order.clone()), sorted(pool));
        let art: Vec<Option<&str>> = order.iter().map(|id| by_id[id]).collect();
        for i in 1..art.len() {
            if let (Some(a), Some(b)) = (art[i - 1], art[i]) && a == b {
                prop_assert!(art[i..].iter().all(|x| *x == Some(a)), "pair at {} not a tail run: {:?}", i, art);
            }
        }
    }

    /// Flow: whatever the sequencing data, a round plays every track of the pool exactly once, opener first.
    #[test]
    fn a_flow_round_is_a_permutation_of_the_pool(
        feats in prop::collection::vec((prop::option::of(60f32..180.), prop::option::of(1u8..13), prop::option::of(-14f32..-4.)), 1..60),
        arc in prop_oneof![Just(FlowArc::Steady), Just(FlowArc::Build), Just(FlowArc::Peak)],
        start in any::<usize>(),
    ) {
        let pool: Vec<String> = (0..feats.len()).map(|i| format!("p{i}")).collect();
        let features: HashMap<String, FlowTrack> = pool
            .iter()
            .zip(&feats)
            .map(|(id, (bpm, key, loud))| {
                let key = key.and_then(|k| Camelot::parse(&format!("{k}A")));
                (id.clone(), FlowTrack { bpm: *bpm, key, loudness: *loud, artist: Some(Arc::from("X")), ..FlowTrack::default() })
            })
            .collect();
        let opener = pool[start % pool.len()].clone();
        let mut q = Queue::new();
        q.set_window_size(10);
        q.set_source_flow(SourceKind::Playlist, "F".into(), arc, pool.clone(), features, Some(&opener));
        let played = drain(&mut q);
        prop_assert_eq!(played.first(), Some(&opener));
        prop_assert_eq!(sorted(played), sorted(pool));
    }

    /// `sequence::pick` always returns a candidate index and is a pure function of its inputs.
    #[test]
    fn flow_pick_is_in_range_and_deterministic(
        bpms in prop::collection::vec(prop::option::of(60f32..180.), 1..40),
        prev in any::<usize>(),
        arc in prop_oneof![Just(FlowArc::Steady), Just(FlowArc::Build), Just(FlowArc::Peak)],
        pos in 0f32..=1.,
        lean in prop_oneof![Just(Lean::Neutral), Just(Lean::Known), Just(Lean::Unknown)],
    ) {
        let ids: Vec<String> = (0..bpms.len()).map(|i| format!("p{i}")).collect();
        let feats: HashMap<String, FlowTrack> = ids.iter().cloned().zip(bpms.iter().map(|b| FlowTrack { bpm: *b, ..FlowTrack::default() })).collect();
        let prev_id = ids[prev % ids.len()].as_str();
        let cands: Vec<&str> = ids.iter().map(String::as_str).filter(|id| *id != prev_id).collect();
        prop_assume!(!cands.is_empty());
        let recent = [prev_id];
        let slot = Slot { prev: Some(prev_id), recent: &recent, arc, arc_pos: pos, lean };
        let a = sequence::pick(&feats, &cands, &slot);
        prop_assert!(a < cands.len());
        prop_assert_eq!(a, sequence::pick(&feats, &cands, &slot));
    }
}
