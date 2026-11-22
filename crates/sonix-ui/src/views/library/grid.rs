//! `VirtualGrid.svelte`: a windowed grid of square-art cards. Only the rows on screen exist; every row is one uniform
//! list item of `cellHeight + gap` px. Columns = as many `minCellWidth` cells (+ gap) as fit the viewport width
//! (`clientWidth`, i.e. without the scrollbar gutter); `cellHeight = round(cellWidth + textHeight)`.
//! The scroll offset is remembered per key for the session (`savedScroll`).
use crate::metrics::library as m;
use crate::pager::{PAGE, Pager};
use crate::theme::Theme;
use crate::widgets::scrollbar::{self, ScrollbarHandle};
use gpui::{AnyElement, Context, IntoElement, UniformListScrollHandle, Window, div, point, prelude::*, px, uniform_list};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

thread_local! {
    static SAVED_SCROLL: RefCell<HashMap<String, f32>> = RefCell::new(HashMap::new());
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geo {
    pub cols: usize,
    pub cell_w: f32,
    pub cell_h: f32,
    pub gap: f32,
}

impl Geo {
    pub fn row_h(&self) -> f32 {
        self.cell_h + self.gap
    }
}

/// `columns`, `cellWidth`, `cellHeight` of `VirtualGrid` for a viewport of `vw` CSS px.
pub fn geometry(vw: f32, min_cell: f32, text_h: f32, gap: f32) -> Geo {
    let cols = (((vw + gap) / (min_cell + gap)).floor() as usize).max(1);
    let cell_w = ((vw - gap * (cols as f32 - 1.0)) / cols as f32).max(1.0);
    Geo { cols, cell_w, cell_h: (cell_w + text_h).round(), gap }
}

pub struct GridState<T> {
    pub pager: Pager<T>,
    pub list: UniformListScrollHandle,
    pub sb: ScrollbarHandle,
    pub width: Rc<Cell<f32>>,
    /// `scrollKey` of the Svelte grid.
    pub key: String,
    restored: bool,
    /// Rows currently rendered and the column count of the last frame (to re-fetch the visible pages in place).
    pub vis: Range<usize>,
    pub cols: usize,
    /// The first page arrived (`pager.loaded` in the Svelte pager).
    pub loaded: bool,
}

impl<T> GridState<T> {
    pub fn new(key: String) -> Self {
        Self { pager: Pager::default(), list: UniformListScrollHandle::new(), sb: scrollbar::new_handle(), width: Rc::new(Cell::new(super::parts::initial_width("grid"))), key, restored: false, vis: 0..0, cols: 1, loaded: false }
    }
    /// New sort/filter: drop everything (the scroll offset of the *new* key is restored once its first page arrives).
    pub fn reset(&mut self, key: String) -> u64 {
        self.key = key;
        self.restored = false;
        self.list.0.borrow().base_handle.set_offset(point(px(0.), px(0.)));
        self.pager.reset()
    }
    /// Scroll offset of the grid viewport in CSS px.
    pub fn scroll_top(&self) -> f32 {
        -f32::from(self.list.0.borrow().base_handle.offset().y)
    }
    /// Pages covering the rows on screen (stars changed: re-fetch them in place).
    pub fn visible_pages(&self) -> std::ops::RangeInclusive<usize> {
        let (a, z) = (self.vis.start * self.cols / PAGE, (self.vis.end * self.cols).saturating_sub(1) / PAGE);
        a..=z.max(a)
    }
}

pub trait GridHost<T: Clone + 'static>: Sized + 'static {
    fn grid(&mut self) -> &mut GridState<T>;
    /// One cell (`item == None`: the page is still loading, draw the `.loading` block).
    fn cell(&mut self, ix: usize, item: Option<T>, geo: Geo, window: &mut Window, cx: &mut Context<Self>) -> AnyElement;
    /// Start fetching page `page` (the host checks `pager.want`).
    fn request(&mut self, page: usize, cx: &mut Context<Self>);
}

pub fn list_overflows(list: &UniformListScrollHandle) -> Option<bool> {
    let s = list.0.borrow();
    let item = s.last_item_size?;
    Some(f32::from(item.contents.height) > f32::from(item.item.height) + 0.5)
}

/// The grid inside its `.grid-wrap` (flex: 1; min-height: 0).
pub fn grid_element<T: Clone + 'static, V: GridHost<T>>(this: &mut V, th: Theme, window: &mut Window, cx: &mut Context<V>) -> AnyElement {
    let g = this.grid();
    let total = g.pager.total;
    let wrap_w = g.width.get();
    // the 10 px scrollbar gutter is part of the layout only while the content overflows (assume it does until measured)
    let overflow = list_overflows(&g.list).unwrap_or(total > 0 || !g.pager.has_page(0));
    let vw = if wrap_w > 0.0 {
        wrap_w - if overflow { scrollbar::WIDTH } else { 0.0 }
    } else {
        f32::from(window.viewport_size().width) - crate::views::shell::Shell::sidebar_width(f32::from(window.viewport_size().width)) - 2.0 * m::PAGE_PAD - scrollbar::WIDTH
    };
    let geo = geometry(vw, m::GRID_MIN_CELL, m::GRID_TEXT_H, m::GRID_GAP);
    // native-only: before the first page arrives, placeholder cards of the loaded cell size fill the viewport
    if total == 0 && !g.pager.has_page(0) {
        let width = g.width.clone();
        let mut wrap = div().flex().flex_row().flex_wrap().items_start().content_start().gap(px(geo.gap)).size_full().overflow_hidden().pr(px(scrollbar::WIDTH));
        for _ in 0..geo.cols * 4 {
            wrap = wrap.child(div().w(px(geo.cell_w)).h(px(geo.cell_h)).flex_none().overflow_hidden().child(super::cards::card_skeleton(&th, geo.cell_w)));
        }
        return div().relative().size_full().child(super::parts::measure_keyed("grid", &width)).child(wrap).into_any_element();
    }
    let rows = total.div_ceil(geo.cols);
    // scroll memory (session only): restore once there is content, then keep the map current
    if total > 0 {
        if !g.restored {
            g.restored = true;
            let saved = SAVED_SCROLL.with(|s| s.borrow().get(&g.key).copied()).unwrap_or(0.0);
            g.list.0.borrow().base_handle.set_offset(point(px(0.), px(-saved)));
        } else {
            let top = g.scroll_top();
            SAVED_SCROLL.with(|s| s.borrow_mut().insert(g.key.clone(), top));
        }
    }
    g.cols = geo.cols;
    let (list, sb, width) = (g.list.clone(), g.sb.clone(), g.width.clone());
    let row_h = geo.row_h();
    let body = uniform_list(
        "vgrid",
        rows,
        cx.processor(move |this: &mut V, range: Range<usize>, window, cx| {
            let mut out = Vec::with_capacity(range.len());
            this.grid().vis = range.clone();
            for r in range {
                let mut row = div().id(("vgrow", r)).flex().flex_row().items_start().gap(px(geo.gap)).w_full().h(px(row_h)).flex_none();
                for ix in r * geo.cols..((r + 1) * geo.cols).min(this.grid().pager.total) {
                    let item = this.grid().pager.get(ix).cloned();
                    if item.is_none() {
                        this.request(ix / PAGE, cx);
                    }
                    row = row.child(this.cell(ix, item, geo, window, cx));
                }
                out.push(row);
            }
            out
        }),
    )
    .track_scroll(list.clone())
    .size_full();
    div()
        .relative()
        .size_full()
        .child(super::parts::measure_keyed("grid", &width))
        .child(div().size_full().when(overflow, |d| d.pr(px(scrollbar::WIDTH))).child(body))
        .child(scrollbar::scrollbar("vgrid-scrollbar", th, &list, &sb))
        .child(crate::widgets::smooth::drive(window, crate::widgets::smooth::Scrollable::list(&list)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_and_cell_height_follow_the_svelte_grid() {
        // Albums at 1440 x 900: stage 840 - 32 padding - 10 scrollbar = 798 wide
        let g = geometry(798.0, 140.0, 40.0, 16.0);
        assert_eq!(g.cols, 5);
        assert!((g.cell_w - 146.8).abs() < 1e-3);
        assert_eq!(g.cell_h, 187.0);
        assert_eq!(g.row_h(), 203.0);
        // 1280 (no docked rail): stage 1064 - 32 - 10 = 1022
        let g = geometry(1022.0, 140.0, 40.0, 16.0);
        assert_eq!((g.cols, g.cell_h), (6, 197.0));
        // always at least one column
        assert_eq!(geometry(50.0, 140.0, 40.0, 16.0).cols, 1);
    }
}
