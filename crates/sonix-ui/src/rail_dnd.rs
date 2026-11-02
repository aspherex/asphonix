//! Pure maths of the queue rail's drag and drop (`QueueRail.svelte`: `gapAt`, `moveBlock`, the "single row dropped
//! next to itself" no-op) plus the placeholder bookkeeping of the native-only grey gap.

/// `gapAt`: the insertion gap (before play-order position N; `heights.len()` = the end) under content-space `y`, the
/// pointer's distance from the top of the list's first row. Upper half of a row = before it, lower half = after it.
pub fn gap_at(y: f32, heights: &[f32]) -> usize {
    let mut top = 0.0;
    for (i, h) in heights.iter().enumerate() {
        if y < top + h / 2.0 {
            return i;
        }
        top += h;
    }
    heights.len()
}

/// `gap_at` while a `ph` px placeholder sits at gap `cur` (the rows below it are pushed down by `ph`): the pointer
/// over the placeholder keeps its gap, below it the placeholder's height is taken out first.
pub fn gap_with_placeholder(y: f32, heights: &[f32], cur: Option<usize>, ph: f32) -> usize {
    match cur {
        Some(g) if ph > 0.0 => {
            let top: f32 = heights.iter().take(g).sum();
            if y >= top + ph {
                gap_at(y - ph, heights)
            } else if y >= top {
                g.min(heights.len())
            } else {
                gap_at(y, heights)
            }
        }
        _ => gap_at(y, heights),
    }
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

    #[test]
    fn gap_is_before_the_row_in_its_upper_half_and_after_it_in_the_lower() {
        let h = [44.0, 44.0, 56.0];
        assert_eq!(gap_at(0.0, &h), 0);
        assert_eq!(gap_at(21.0, &h), 0);
        assert_eq!(gap_at(23.0, &h), 1);
        assert_eq!(gap_at(44.0 + 21.0, &h), 1);
        assert_eq!(gap_at(88.0 + 27.0, &h), 2);
        assert_eq!(gap_at(88.0 + 29.0, &h), 3);
        assert_eq!(gap_at(900.0, &h), 3, "empty space below the last row = the end");
    }

    #[test]
    fn the_placeholder_does_not_make_the_gap_jump() {
        let h = [44.0; 4];
        // placeholder (44 px) open at gap 2: it spans y 88..132; over it the gap stays
        assert_eq!(gap_with_placeholder(100.0, &h, Some(2), 44.0), 2);
        // rows below are pushed down: the old row 2 now starts at 132, its upper half is still gap 2
        assert_eq!(gap_with_placeholder(132.0 + 10.0, &h, Some(2), 44.0), 2);
        assert_eq!(gap_with_placeholder(132.0 + 30.0, &h, Some(2), 44.0), 3);
        // above it nothing moved
        assert_eq!(gap_with_placeholder(30.0, &h, Some(2), 44.0), 1);
        assert_eq!(gap_with_placeholder(30.0, &h, None, 0.0), 1);
    }

    #[test]
    fn single_row_next_to_itself_is_a_noop() {
        assert!(is_noop(&[3], 3) && is_noop(&[3], 4));
        assert!(!is_noop(&[3], 5) && !is_noop(&[3, 4], 3));
    }

    #[test]
    fn move_block_matches_the_web_simulation() {
        // one row down to the end / to the front / up
        assert_eq!(apply(6, &move_block(6, &[2], 6)), [0, 1, 3, 4, 5, 2]);
        assert_eq!(apply(6, &move_block(6, &[4], 0)), [4, 0, 1, 2, 3, 5]);
        assert_eq!(apply(6, &move_block(6, &[4], 2)), [0, 1, 4, 2, 3, 5]);
        assert_eq!(apply(6, &move_block(6, &[1], 4)), [0, 2, 3, 1, 4, 5]);
        // blocks keep their relative order, also when the gap sits inside the block
        assert_eq!(apply(7, &move_block(7, &[1, 4], 6)), [0, 2, 3, 5, 1, 4, 6]);
        assert_eq!(apply(7, &move_block(7, &[5, 2], 0)), [2, 5, 0, 1, 3, 4, 6]);
        assert_eq!(apply(4, &move_block(4, &[0, 1], 0)), [0, 1, 2, 3], "a block dropped on itself ends where it was");
    }
}
