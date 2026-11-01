//! Multi-select over row *indices* (`trackSelection.svelte.ts`): Ctrl/Cmd+click toggles,
//! Shift+click selects a range from the anchor, Ctrl+A selects everything, arrows move
//! the cursor (Shift extends). Holds indices only, never rows, so it works with paged
//! 50k-row lists.
use std::collections::BTreeSet;

#[derive(Default, Debug, Clone)]
pub struct Selection {
    set: BTreeSet<usize>,
    anchor: Option<usize>,
    cursor: Option<usize>,
}

impl Selection {
    pub fn clear(&mut self) {
        self.set.clear();
        self.anchor = None;
        self.cursor = None;
    }
    pub fn len(&self) -> usize {
        self.set.len()
    }
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
    pub fn contains(&self, i: usize) -> bool {
        self.set.contains(&i)
    }
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }
    pub fn indices(&self) -> Vec<usize> {
        self.set.iter().copied().collect()
    }

    /// Plain / Ctrl / Shift click on row `i`.
    pub fn click(&mut self, i: usize, ctrl: bool, shift: bool) {
        if shift {
            let a = self.anchor.unwrap_or(i);
            let (lo, hi) = (a.min(i), a.max(i));
            if !ctrl {
                self.set.clear();
            }
            self.set.extend(lo..=hi);
            self.cursor = Some(i);
        } else if ctrl {
            if !self.set.remove(&i) {
                self.set.insert(i);
            }
            self.anchor = Some(i);
            self.cursor = Some(i);
        } else {
            self.set.clear();
            self.set.insert(i);
            self.anchor = Some(i);
            self.cursor = Some(i);
        }
    }

    pub fn select_all(&mut self, total: usize) {
        self.set = (0..total).collect();
        if total > 0 {
            self.anchor.get_or_insert(0);
            self.cursor.get_or_insert(0);
        }
    }

    /// Keyboard: move the cursor by `delta` rows (clamped); with `extend` select the range
    /// from the anchor, otherwise collapse onto the new row. Returns the new cursor.
    pub fn move_cursor(&mut self, delta: isize, extend: bool, total: usize) -> Option<usize> {
        if total == 0 {
            return None;
        }
        let cur = self.cursor.map(|c| c as isize).unwrap_or(if delta >= 0 { -1 } else { total as isize });
        let n = (cur + delta).clamp(0, total as isize - 1) as usize;
        self.move_to(n, extend);
        Some(n)
    }

    pub fn move_to(&mut self, n: usize, extend: bool) {
        if extend {
            let a = *self.anchor.get_or_insert(self.cursor.unwrap_or(n));
            self.set = (a.min(n)..=a.max(n)).collect();
        } else {
            self.set.clear();
            self.set.insert(n);
            self.anchor = Some(n);
        }
        self.cursor = Some(n);
    }

    /// Right-click / "..." on row `i`: keep the selection when `i` is part of it, else
    /// collapse to just `i`. This is the only place a menu open may touch the selection.
    pub fn for_context_menu(&mut self, i: usize) -> Vec<usize> {
        if !self.set.contains(&i) {
            self.set.clear();
            self.set.insert(i);
            self.anchor = Some(i);
            self.cursor = Some(i);
        }
        self.indices()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_ctrl_and_shift_clicks() {
        let mut s = Selection::default();
        s.click(5, false, false);
        assert_eq!(s.indices(), vec![5]);
        s.click(8, true, false);
        assert_eq!(s.indices(), vec![5, 8]);
        s.click(5, true, false);
        assert_eq!(s.indices(), vec![8], "ctrl toggles off");
        s.click(10, false, true);
        assert_eq!(s.indices(), (5..=10).collect::<Vec<_>>(), "range from the last clicked row (the anchor)");
        s.click(2, false, true);
        assert_eq!(s.indices(), (2..=5).collect::<Vec<_>>(), "shift replaces the previous range");
        s.click(20, true, true);
        assert!(s.contains(20) && s.contains(2) && s.contains(8), "ctrl+shift adds the range");
    }

    #[test]
    fn keyboard_moves_and_extends() {
        let mut s = Selection::default();
        assert_eq!(s.move_cursor(1, false, 100), Some(0), "first down lands on row 0");
        assert_eq!(s.move_cursor(1, false, 100), Some(1));
        assert_eq!(s.move_cursor(3, true, 100), Some(4));
        assert_eq!(s.indices(), vec![1, 2, 3, 4]);
        assert_eq!(s.move_cursor(-2, true, 100), Some(2));
        assert_eq!(s.indices(), vec![1, 2]);
        assert_eq!(s.move_cursor(-999, false, 100), Some(0));
        assert_eq!(s.indices(), vec![0]);
        assert_eq!(s.move_cursor(999, false, 100), Some(99));
        assert_eq!(s.move_cursor(1, false, 0), None);
        let mut t = Selection::default();
        assert_eq!(t.move_cursor(-1, false, 10), Some(9), "up from nothing starts at the end");
    }

    #[test]
    fn select_all_and_context_menu_rule() {
        let mut s = Selection::default();
        s.select_all(50_000);
        assert_eq!(s.len(), 50_000);
        // right-click inside the selection keeps it
        assert_eq!(s.for_context_menu(7).len(), 50_000);
        s.click(3, false, false);
        s.click(4, true, false);
        // right-click outside collapses to that row
        assert_eq!(s.for_context_menu(9), vec![9]);
        assert_eq!(s.cursor(), Some(9));
        // right-click inside a 2-row selection keeps both
        s.click(1, false, false);
        s.click(2, true, false);
        assert_eq!(s.for_context_menu(2), vec![1, 2]);
    }
}
