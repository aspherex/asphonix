//! Pure maths of the queue rail's drag and drop (`QueueRail.svelte`: `gapAt`, `moveBlock`, the "single row dropped
//! next to itself" no-op) plus the placement of the native-only grey gap.
//!
//! Everything is in absolute play-order indices (the web's `data-qabs`). A [`Gap`] is "before play-order position `at`"
//! (`total` = the end of the queue); `user` says which list shows it when `at` is the boundary between the "Next in
//! queue" run and the source rows (`at == user_start + user_count`).

/// Where a drop would insert: before play-order position `at`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap {
    pub at: usize,
    /// The pointer is over the "Next in queue" section (decides the list at the user-run boundary).
    pub user: bool,
}

/// A rendered row's vertical extent (window space) as painted last frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowBox {
    pub abs: usize,
    pub top: f32,
    pub bottom: f32,
    pub user: bool,
}

/// What is under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// Over the open gap itself: keep the current one (its rows were pushed away by it).
    Keep,
    Gap(Gap),
}

/// `gapAt`: the row under `y` decides the gap (upper half = before it, lower half = after it); `ph` = the open
/// placeholder's `(top, bottom)`. `None` = no row there (the caller falls back to the list / section background).
pub fn hit(y: f32, rows: &[RowBox], ph: Option<(f32, f32)>) -> Option<Hit> {
    if ph.is_some_and(|(t, b)| y >= t && y < b) {
        return Some(Hit::Keep);
    }
    rows.iter().find(|r| y >= r.top && y < r.bottom).map(|r| Hit::Gap(Gap { at: if y > r.top + (r.bottom - r.top) / 2.0 { r.abs + 1 } else { r.abs }, user: r.user }))
}

/// Does the gap show inside the "Next in queue" section (`ustart..ustart + ucount`)?
pub fn in_user_section(g: Gap, ustart: usize, ucount: usize) -> bool {
    ucount > 0 && g.at >= ustart && (g.at < ustart + ucount || (g.at == ustart + ucount && g.user))
}

/// Where an external drop really lands (`queue.rs enqueue_next` / `enqueue_end`): Play next = right after the current
/// track, i.e. the front of the "Next in queue" run; Add to queue with a source = the end of that run, still ahead of
/// every source row; without a source (a static queue) = the literal end.
pub fn external_gap(next: bool, has_source: bool, ustart: usize, ucount: usize, total: usize) -> Gap {
    if next {
        Gap { at: ustart, user: ucount > 0 }
    } else if has_source {
        Gap { at: ustart + ucount, user: ucount > 0 }
    } else {
        Gap { at: total, user: false }
    }
}

/// The contiguous run of user-queued rows right after the current one (`QueueRail.svelte userRunStart/userRunEnd`):
/// `(start, count)` over `origins` (`true` = user-queued) that start at play-order index `base`.
pub fn user_run(base: usize, origins: &[bool], current: i64) -> (usize, usize) {
    let start = (current + 1).max(0) as usize;
    let count = (start..base + origins.len()).take_while(|i| i.checked_sub(base).and_then(|k| origins.get(k)).copied().unwrap_or(false)).count();
    (start, count)
}

/// `onWindowPointerUp`: a single row dropped right before or after itself moves nothing.
pub fn is_noop(moving: &[usize], gap: usize) -> bool {
    moving.len() == 1 && (gap == moving[0] || gap == moving[0] + 1)
}

/// `moveBlock`: the `(from, to)` `player_reorder_queue` calls that move the entries at `moving` (kept in their relative
/// order) into the gap before play-order position `gap` (`total` = the end). Each `to` is a position after the removal
/// (the engine's remove-then-insert semantics), simulated on a local index array.
pub fn move_block(total: usize, moving: &[usize], gap: usize) -> Vec<(usize, usize)> {
    let mut moving: Vec<usize> = moving.to_vec();
    moving.sort_unstable();
    moving.dedup();
    let mut order: Vec<usize> = (0..total).collect();
    // first entry that ends up right after the moved block (None = the end)
    let anchor = (gap..total).find(|i| !moving.contains(i));
    let mut prev: Option<usize> = None;
    let mut ops = Vec::new();
    for idx in moving {
        let Some(from) = order.iter().position(|x| *x == idx) else { continue };
        order.remove(from);
        let to = match (prev, anchor) {
            (Some(p), _) => order.iter().position(|x| *x == p).map_or(order.len(), |i| i + 1),
            (None, None) => order.len(),
            (None, Some(a)) => order.iter().position(|x| *x == a).unwrap_or(order.len()),
        };
        order.insert(to, idx);
        if from != to {
            ops.push((from, to));
        }
        prev = Some(idx);
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(total: usize, ops: &[(usize, usize)]) -> Vec<usize> {
        let mut v: Vec<usize> = (0..total).collect();
        for &(f, t) in ops {
            let x = v.remove(f);
            v.insert(t, x);
        }
        v
    }

    fn rows(n: usize, h: f32, user: std::ops::Range<usize>) -> Vec<RowBox> {
        (0..n).map(|i| RowBox { abs: i, top: i as f32 * h, bottom: (i + 1) as f32 * h, user: user.contains(&i) }).collect()
    }

    #[test]
    fn gap_is_before_the_row_in_its_upper_half_and_after_it_in_the_lower() {
        let r = rows(3, 44.0, 0..0);
        assert_eq!(hit(1.0, &r, None), Some(Hit::Gap(Gap { at: 0, user: false })));
        assert_eq!(hit(21.0, &r, None), Some(Hit::Gap(Gap { at: 0, user: false })));
        assert_eq!(hit(23.0, &r, None), Some(Hit::Gap(Gap { at: 1, user: false })));
        assert_eq!(hit(44.0 + 30.0, &r, None), Some(Hit::Gap(Gap { at: 2, user: false })));
        assert_eq!(hit(500.0, &r, None), None, "below the last row: the caller decides (end of the queue)");
    }

    #[test]
    fn the_open_gap_keeps_itself_and_rows_report_their_painted_position() {
        // rows 0,1 above, the 44 px placeholder at 88..132, rows 2,3 pushed below it
        let r = vec![
            RowBox { abs: 0, top: 0.0, bottom: 44.0, user: false },
            RowBox { abs: 1, top: 44.0, bottom: 88.0, user: false },
            RowBox { abs: 2, top: 132.0, bottom: 176.0, user: false },
            RowBox { abs: 3, top: 176.0, bottom: 220.0, user: false },
        ];
        let ph = Some((88.0, 132.0));
        assert_eq!(hit(100.0, &r, ph), Some(Hit::Keep));
        assert_eq!(hit(140.0, &r, ph), Some(Hit::Gap(Gap { at: 2, user: false })));
        assert_eq!(hit(170.0, &r, ph), Some(Hit::Gap(Gap { at: 3, user: false })));
    }

    #[test]
    fn user_rows_are_hit_by_absolute_index_and_flag_their_section() {
        let r = rows(5, 44.0, 1..3);
        assert_eq!(hit(44.0 + 40.0, &r, None), Some(Hit::Gap(Gap { at: 2, user: true })));
        let g = Gap { at: 3, user: true }; // lower half of the last user row = end of the run
        assert!(in_user_section(g, 1, 2));
        assert!(!in_user_section(Gap { at: 3, user: false }, 1, 2), "upper half of the first source row shows in the main list");
        assert!(in_user_section(Gap { at: 1, user: false }, 1, 2), "front of the run");
        assert!(!in_user_section(Gap { at: 1, user: false }, 1, 0), "no run, no section");
    }

    #[test]
    fn external_drops_show_where_the_queue_would_put_them() {
        // current = 4: the user run is 5..7, source rows follow
        assert_eq!(external_gap(true, true, 5, 2, 30), Gap { at: 5, user: true }, "Play next: front of Next in queue");
        assert_eq!(external_gap(false, true, 5, 2, 30), Gap { at: 7, user: true }, "Add to queue (source): end of the run, ahead of source rows");
        assert_eq!(external_gap(false, true, 5, 0, 30), Gap { at: 5, user: false }, "no run yet: right after the playing row");
        assert_eq!(external_gap(false, false, 5, 0, 30), Gap { at: 30, user: false }, "static queue: the literal end");
        assert_eq!(external_gap(true, false, 5, 0, 30), Gap { at: 5, user: false });
    }

    #[test]
    fn the_user_run_is_the_contiguous_user_rows_after_the_current_one() {
        // current 1: rows 2,3 user, 4 source, 5 user again (a later "Add to queue" past the run does not join it)
        let o = [false, false, true, true, false, true];
        assert_eq!(user_run(0, &o, 1), (2, 2));
        assert_eq!(user_run(0, &o, 3), (4, 0));
        // a window that starts at 10
        assert_eq!(user_run(10, &o[2..], 11), (12, 2));
        assert_eq!(user_run(0, &o, -1), (0, 0));
    }

    #[test]
    fn single_row_next_to_itself_is_a_noop() {
        assert!(is_noop(&[3], 3) && is_noop(&[3], 4));
        assert!(!is_noop(&[3], 5) && !is_noop(&[3, 4], 3));
    }

    #[test]
    fn move_block_matches_the_web_simulation() {
        assert_eq!(apply(6, &move_block(6, &[2], 6)), [0, 1, 3, 4, 5, 2]);
        assert_eq!(apply(6, &move_block(6, &[4], 0)), [4, 0, 1, 2, 3, 5]);
        assert_eq!(apply(6, &move_block(6, &[4], 2)), [0, 1, 4, 2, 3, 5]);
        assert_eq!(apply(6, &move_block(6, &[1], 4)), [0, 2, 3, 1, 4, 5]);
        assert_eq!(apply(7, &move_block(7, &[1, 4], 6)), [0, 2, 3, 5, 1, 4, 6]);
        assert_eq!(apply(7, &move_block(7, &[5, 2], 0)), [2, 5, 0, 1, 3, 4, 6]);
        assert_eq!(apply(4, &move_block(4, &[0, 1], 0)), [0, 1, 2, 3], "a block dropped on itself ends where it was");
    }
}
