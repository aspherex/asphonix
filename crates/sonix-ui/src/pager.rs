//! Page cache for virtualized lists: pages of `PAGE` rows keyed by page number,
//! at most `MAX_PAGES` resident (~1,600 rows), the one farthest from the viewport
//! evicted first. Stale answers (older `epoch`) are dropped, so a changed
//! sort/filter never mixes rows. Mirrors the frontend `createPager`.
use std::collections::{HashMap, HashSet};

pub const PAGE: usize = 100;
pub const MAX_PAGES: usize = 16;

pub struct Pager<T> {
    pub total: usize,
    pub epoch: u64,
    pages: HashMap<usize, Vec<T>>,
    pending: HashSet<usize>,
}

impl<T> Default for Pager<T> {
    fn default() -> Self {
        Self { total: 0, epoch: 0, pages: HashMap::new(), pending: HashSet::new() }
    }
}

impl<T> Pager<T> {
    /// Invalidate everything (new sort/filter/query). Returns the new epoch.
    pub fn reset(&mut self) -> u64 {
        self.epoch += 1;
        self.pages.clear();
        self.pending.clear();
        self.epoch
    }

    pub fn get(&self, ix: usize) -> Option<&T> {
        self.pages.get(&(ix / PAGE))?.get(ix % PAGE)
    }

    pub fn get_mut(&mut self, ix: usize) -> Option<&mut T> {
        self.pages.get_mut(&(ix / PAGE))?.get_mut(ix % PAGE)
    }

    #[allow(dead_code)]
    pub fn has_page(&self, p: usize) -> bool {
        self.pages.contains_key(&p)
    }

    /// True if page `p` should be requested now (and marks it pending).
    pub fn want(&mut self, p: usize) -> bool {
        if p * PAGE >= self.total.max(PAGE) || self.pages.contains_key(&p) || self.pending.contains(&p) {
            return false;
        }
        self.pending.insert(p);
        true
    }

    /// Store a page; false (dropped) when the answer belongs to an older epoch.
    pub fn insert(&mut self, epoch: u64, p: usize, rows: Vec<T>, centre_row: usize) -> bool {
        if epoch != self.epoch {
            return false;
        }
        self.pending.remove(&p);
        self.pages.insert(p, rows);
        while self.pages.len() > MAX_PAGES {
            let centre = (centre_row / PAGE) as i64;
            // never the page just inserted: Tracks renders row 0 every frame (uniform_list measures item 0), so with the viewport far
            // from the top that page would be evicted at once and re-queried on every frame.
            let far = self.pages.keys().copied().filter(|k| *k != p).max_by_key(|k| (*k as i64 - centre).abs()).unwrap();
            self.pages.remove(&far);
        }
        true
    }

    /// Drop all rows but keep `total` (minimize: "drop caches and paged rows").
    pub fn release(&mut self) {
        self.pages.clear();
        self.pending.clear();
    }

    #[cfg(test)]
    pub fn resident_pages(&self) -> usize {
        self.pages.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(n: usize) -> Vec<u32> {
        (0..n as u32).collect()
    }

    #[test]
    fn want_dedupes_and_respects_total() {
        let mut p: Pager<u32> = Pager { total: 250, ..Pager::default() };
        assert!(p.want(0));
        assert!(!p.want(0), "pending");
        assert!(p.want(2));
        assert!(!p.want(3), "past the end");
        let e = p.epoch;
        p.insert(e, 0, rows(100), 0);
        assert!(!p.want(0), "resident");
        assert_eq!(p.get(5), Some(&5));
        assert_eq!(p.get(150), None);
    }

    #[test]
    fn stale_epoch_is_dropped() {
        let mut p: Pager<u32> = Pager { total: 1000, ..Pager::default() };
        let e = p.epoch;
        assert!(p.want(1));
        p.reset();
        assert!(!p.insert(e, 1, rows(100), 0));
        assert_eq!(p.resident_pages(), 0);
        assert!(p.want(1), "pending was cleared by the reset");
    }

    #[test]
    fn a_page_far_from_the_viewport_is_kept_once_inserted() {
        // Tracks renders row 0 every frame (the list measures item 0): with the viewport at page 19 and the cache full, that
        // insert used to evict page 0 itself, so the next frame fetched it again, forever.
        let mut p: Pager<u32> = Pager { total: 2_071, ..Pager::default() };
        for page in 4..=19 {
            let e = p.epoch;
            p.insert(e, page, rows(100), 19 * PAGE);
        }
        assert!(p.want(0));
        let e = p.epoch;
        p.insert(e, 0, rows(100), 19 * PAGE);
        assert!(p.has_page(0) && !p.want(0), "row 0 stays resident, no re-request");
        assert_eq!(p.resident_pages(), MAX_PAGES);
        assert!(p.has_page(19) && !p.has_page(4), "the farthest other page went");
    }

    #[test]
    fn never_more_than_16_pages_and_farthest_goes_first() {
        let mut p: Pager<u32> = Pager { total: 54_515, ..Pager::default() };
        for page in 0..40 {
            let e = p.epoch;
            p.insert(e, page, rows(100), 39 * PAGE);
        }
        assert_eq!(p.resident_pages(), MAX_PAGES);
        assert!(p.has_page(39) && p.has_page(24));
        assert!(!p.has_page(0));
        p.release();
        assert_eq!(p.resident_pages(), 0);
        assert_eq!(p.total, 54_515);
    }
}
