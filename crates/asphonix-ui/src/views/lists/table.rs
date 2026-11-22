//! The shared dense track table of the list routes (rows in a virtual list inside a framed panel): header row,
//! fixed 36 px rows, hover / selected / active, the width-driven Year/Added hiding rule, optional drag handle /
//! add / remove buttons, day-header rows (History) and the shared track-list keyboard handling.
//!
//! One entity per list. Data lives here (a `Vec` for playlist-sized lists, a [`Pager`] for server-sized
//! ones); everything route-specific (what Play does, which menu opens) is reported as a [`TableEvent`].
use super::super::shell::{EnqueueSelected, OpenMenuKey, PlaySelected, SelDown, SelUp, Shell};
use super::super::tracks::{flex_cell, now_playing_bars};
use super::SelectAllRows;
use crate::anim;
use crate::app::{self, PlayerEvent, PlayerModel};
use crate::columns::{Columns, visible_columns};
use crate::metrics::{anim as m_anim, lists as lm, tracks as tr};
use crate::pager::{PAGE, Pager};
use crate::router::Route;
use crate::selection::Selection;
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{BODY_MD, BODY_SM, LABEL_CAPS, MONO_CODE, MONO_DATA};
use crate::widgets::scrollbar::{self, ScrollbarHandle};
use crate::widgets::{icon, icon_g};
use gpui::{
    AnyElement, Bounds, ClickEvent, Context, CursorStyle, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent, Pixels, Point, Render, ScrollStrategy,
    Subscription, UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use asphonix_api::format as fmt;
use asphonix_api::*;
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

/// One row of a table: a track (with an optional replacement for the index text, History's clock) or a day header.
#[derive(Clone)]
pub enum RowItem {
    Track { t: Box<Track>, label: Option<String> },
    Day { day: String, count: u32 },
}

pub type Fetch = Arc<dyn Fn(&dyn Backend, usize, usize) -> ApiResult<Paged<RowItem>> + Send + Sync>;

enum Data {
    Local { rows: Vec<RowItem> },
    Paged { pager: Pager<RowItem>, fetch: Fetch, loaded: bool },
}

/// What the arrow keys do (the playlist page moves only the cursor, the others also select the row).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Arrows {
    Select,
    CursorOnly,
}

#[derive(Clone)]
pub struct TableCfg {
    pub head: bool,
    pub album_column: bool,
    pub draggable: bool,
    pub add_btn: bool,
    pub remove_btn: bool,
    pub multi: bool,
    pub arrows: Arrows,
    /// trailing action buttons the head reserves
    pub action_count: usize,
    pub empty: &'static str,
}

pub enum TableEvent {
    Play(usize),
    Enqueue(usize),
    Star(usize),
    Remove(usize),
    /// right click / "..." / menu key: `ix` is the row, `pos` the anchor in window coordinates
    Menu { ix: usize, pos: Point<Pixels> },
    /// the row's "+": `pos` = the bottom-right corner of the button (the add-to-playlist menu anchors there)
    AddToPlaylist { ix: usize, pos: Point<Pixels> },
    Reorder { from: usize, to: usize },
    SelectionChanged,
}
impl EventEmitter<TableEvent> for Table {}

#[derive(Clone)]
struct DragRow(usize);
struct DragGhost(String);
impl Render for DragGhost {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = app::theme(cx);
        div().px(px(8.)).py(px(4.)).bg(th.high).border_1().border_color(th.focus).text_color(th.text).child(txt(&th, MONO_DATA, self.0.clone()))
    }
}

pub struct Table {
    pub cfg: TableCfg,
    data: Data,
    backend: Arc<dyn Backend>,
    current_id: Option<String>,
    playing: bool,
    pub sel: Selection,
    list: UniformListScrollHandle,
    sb: ScrollbarHandle,
    pub focus: FocusHandle,
    vis: Range<usize>,
    hover_row: Option<usize>,
    body_bounds: Rc<Cell<Bounds<Pixels>>>,
    last_width: f32,
    /// The owner's rows are still on their way (a playlist page before `set_local`), so skeleton rows show
    /// instead of the empty text of the initial empty local list. Cleared by every `set_*`.
    pub pending: bool,
    _subs: Vec<Subscription>,
}

impl Table {
    pub fn new(backend: Arc<dyn Backend>, player: &Entity<PlayerModel>, cfg: TableCfg, cx: &mut Context<Self>) -> Self {
        let mut subs = Vec::new();
        subs.push(cx.subscribe(player, |this: &mut Self, p, _: &PlayerEvent, cx| {
            let p = p.read(cx);
            this.current_id = p.current_id().map(str::to_string);
            this.playing = p.playing();
            cx.notify();
        }));
        let (cur, playing) = {
            let p = player.read(cx);
            (p.current_id().map(str::to_string), p.playing())
        };
        Self {
            cfg,
            data: Data::Local { rows: Vec::new() },
            backend,
            current_id: cur,
            playing,
            sel: Selection::default(),
            list: UniformListScrollHandle::new(),
            sb: scrollbar::new_handle(),
            focus: cx.focus_handle().tab_stop(true),
            vis: 0..0,
            hover_row: None,
            body_bounds: Rc::new(Cell::new(Bounds::default())),
            last_width: 1440.0,
            pending: false,
            _subs: subs,
        }
    }

    // ------------------------------------------------------------ data
    pub fn set_local(&mut self, rows: Vec<RowItem>, cx: &mut Context<Self>) {
        self.data = Data::Local { rows };
        self.pending = false;
        self.vis = 0..0;
        cx.notify();
    }
    pub fn set_local_keep_scroll(&mut self, rows: Vec<RowItem>, cx: &mut Context<Self>) {
        self.data = Data::Local { rows };
        self.pending = false;
        cx.notify();
    }

    pub fn set_paged(&mut self, fetch: Fetch, cx: &mut Context<Self>) {
        self.data = Data::Paged { pager: Pager::default(), fetch, loaded: false };
        self.pending = false;
        self.sel.clear();
        self.list.scroll_to_item(0, ScrollStrategy::Top);
        self.vis = 0..0;
        self.load_page(0, true, cx);
        cx.notify();
    }

    /// Re-read from page 0 (a like/unlike elsewhere, a clear): drops rows and selection, keeps nothing stale.
    fn load_page(&mut self, page: usize, first: bool, cx: &mut Context<Self>) {
        let Data::Paged { pager, fetch, .. } = &mut self.data else { return };
        let epoch = if first { pager.reset() } else { pager.epoch };
        if first {
            pager.want(0);
        }
        let (b, fetch) = (self.backend.clone(), fetch.clone());
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { fetch(&*b, page * PAGE, PAGE) }).await;
            let _ = this.update(cx, |t, cx| {
                let centre = t.vis.start;
                let Data::Paged { pager, loaded, .. } = &mut t.data else { return };
                match r {
                    Ok(p) => {
                        if pager.insert(epoch, page, p.items, centre) {
                            pager.total = p.total;
                            *loaded = true;
                            cx.notify();
                        }
                    }
                    // a failed current query still settles the first load: the empty text, not skeleton rows forever
                    Err(_) if epoch == pager.epoch && !*loaded => {
                        *loaded = true;
                        cx.notify();
                    }
                    Err(_) => {}
                }
            });
        })
        .detach();
    }

    pub fn total(&self) -> usize {
        match &self.data {
            Data::Local { rows } => rows.len(),
            Data::Paged { pager, .. } => pager.total,
        }
    }
    pub fn loaded(&self) -> bool {
        !self.pending
            && match &self.data {
                Data::Local { .. } => true,
                Data::Paged { loaded, .. } => *loaded,
            }
    }
    pub fn item(&self, ix: usize) -> Option<&RowItem> {
        match &self.data {
            Data::Local { rows } => rows.get(ix),
            Data::Paged { pager, .. } => pager.get(ix),
        }
    }
    pub fn track_at(&self, ix: usize) -> Option<&Track> {
        match self.item(ix)? {
            RowItem::Track { t, .. } => Some(t.as_ref()),
            RowItem::Day { .. } => None,
        }
    }
    /// Tracks of the selection indices that are resident (paged lists only hold ~1,600 rows).
    pub fn tracks_at(&self, indices: &[usize]) -> Vec<Track> {
        indices.iter().filter_map(|i| self.track_at(*i).cloned()).collect()
    }
    /// Right click / "..." rule: keep the selection when the row is part of it, else collapse to the row.
    pub fn menu_indices(&mut self, ix: usize) -> Vec<usize> {
        self.sel.for_context_menu(ix)
    }
    /// Patch one resident track in place (a heart flipped on a playlist page).
    pub fn patch_track(&mut self, ix: usize, f: impl FnOnce(&mut Track), cx: &mut Context<Self>) {
        let item = match &mut self.data {
            Data::Local { rows } => rows.get_mut(ix),
            Data::Paged { pager, .. } => pager.get_mut(ix),
        };
        if let Some(RowItem::Track { t, .. }) = item {
            f(t);
            cx.notify();
        }
    }
    pub fn release(&mut self) {
        if let Data::Paged { pager, .. } = &mut self.data {
            pager.release();
        }
    }
pub fn refresh_visible(&mut self, cx: &mut Context<Self>) {
        if let Data::Paged { .. } = &self.data {
            let (a, z) = (self.vis.start / PAGE, self.vis.end.saturating_sub(1) / PAGE);
            for p in a..=z {
                self.load_page(p, false, cx);
            }
        }
    }
    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.sel.clear();
        cx.emit(TableEvent::SelectionChanged);
        cx.notify();
    }

    pub fn selected_count(&self) -> usize {
        self.sel.len()
    }


    // ------------------------------------------------------------ keyboard
    fn scroll_to_cursor(&mut self, n: usize) {
        if n < self.vis.start {
            self.list.scroll_to_item(n, ScrollStrategy::Top);
        } else if n + 1 >= self.vis.end {
            self.list.scroll_to_item(n, ScrollStrategy::Bottom);
        }
    }

    /// ArrowUp/Down; History additionally skips day headers.
    fn key_move(&mut self, dir: isize, cx: &mut Context<Self>) {
        let total = self.total();
        if total == 0 {
            return;
        }
        let mut i = match self.sel.cursor() {
            Some(c) => c as isize,
            None => {
                if dir > 0 {
                    -1
                } else {
                    total as isize
                }
            }
        };
        loop {
            i += dir;
            if i < 0 || i >= total as isize {
                break; // at the boundary the cursor stays
            }
            let n = i as usize;
            match self.item(n) {
                Some(RowItem::Day { .. }) => continue,
                _ => {
                    self.set_cursor(n);
                    break;
                }
            }
        }
        cx.emit(TableEvent::SelectionChanged);
        cx.notify();
    }

    fn set_cursor(&mut self, n: usize) {
        match self.cfg.arrows {
            Arrows::Select => self.sel.move_to(n, false),
            Arrows::CursorOnly => self.sel.set_cursor(n),
        }
        self.scroll_to_cursor(n);
    }

    fn cols(&mut self, window: &Window) -> Columns {
        let vw = f32::from(window.viewport_size().width);
        let rail = if Shell::rail_docked(vw, &Route::Tracks) { crate::metrics::layout::RAIL_W } else { 0.0 };
        let w = vw - Shell::sidebar_width(vw) - rail - 2.0 * lm::PAGE_PAD - 2.0;
        self.last_width = w;
        visible_columns(w)
    }

    fn open_menu_at_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        let b = self.body_bounds.get();
        let scroll = f32::from(self.list.0.borrow().base_handle.offset().y);
        let y = f32::from(b.origin.y) + scroll + (ix as f32 + 1.0) * tr::ROW_H + 4.0;
        cx.emit(TableEvent::Menu { ix, pos: gpui::point(b.origin.x, px(y)) });
    }

    // ------------------------------------------------------------ render pieces
    fn head_row(&self, th: Theme, cols: Columns) -> impl IntoElement + use<> {
        let spacer = || div().w(px(tr::ICON_BTN)).h(px(tr::ICON_BTN)).flex_none();
        let plain = |label: &'static str| txt(&th, LABEL_CAPS, label);
        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(tr::ROW_GAP))
            .pl(px(tr::ROW_PAD_LEFT))
            .pr(px(tr::ROW_PAD_RIGHT))
            .py(px(tr::HEAD_PAD_Y))
            .flex_none()
            .text_color(th.muted)
            .border_b_1()
            .border_color(th.border)
            .child(div().w(px(tr::IDX_W)).flex_none());
        if self.cfg.draggable {
            row = row.child(div().w(px(14.)).flex_none());
        }
        row = row
            .child(flex_cell(tr::TITLE_GROW).flex().child(plain("Title")))
            .child(flex_cell(tr::ARTIST_GROW).flex().child(plain("Artist")));
        if self.cfg.album_column {
            row = row.child(flex_cell(tr::ALBUM_GROW).flex().child(plain("Album")));
        }
        if cols.year {
            row = row.child(div().w(px(tr::YEAR_W)).flex_none().flex().justify_end().child(plain("Year")));
        }
        if cols.added {
            row = row.child(div().w(px(tr::ADDED_W)).flex_none().flex().justify_end().child(plain("Added")));
        }
        row = row
            .child(div().w(px(tr::FORMAT_W)).flex_none().flex().justify_end().child(plain("Format")))
            .child(div().w(px(tr::TIME_W)).flex_none().flex().justify_end().child(plain("Time")));
        for _ in 0..self.cfg.action_count {
            row = row.child(spacer());
        }
        row
    }

    fn day_row(&self, th: Theme, ix: usize, day: &str, count: u32, now: i64) -> AnyElement {
        let label = fmt::format_day_header(day, now);
        let plays = format!("{count} {}", if count == 1 { "PLAY" } else { "PLAYS" });
        div()
            .id(("day", ix))
            .flex()
            .flex_row()
            .items_end()
            .justify_between()
            .h(px(tr::ROW_H))
            .w_full()
            .pl(px(lm::DAY_PAD_X))
            .pr(px(lm::DAY_PAD_X))
            .pb(px(lm::DAY_PAD_BOTTOM))
            .bg(th.base)
            .border_b_1()
            .border_color(th.border)
            .text_color(th.accent_fg)
            .child(txt(&th, LABEL_CAPS, label).color(th.accent_fg))
            .child(txt(&th, MONO_CODE, plays).color(th.muted))
            .into_any_element()
    }

    fn row(&mut self, ix: usize, cols: Columns, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = app::theme(cx);
        let Some(item) = self.item(ix).cloned() else {
            if let Data::Paged { .. } = &self.data {
                let p = ix / PAGE;
                if let Data::Paged { pager, .. } = &mut self.data
                    && pager.want(p)
                {
                    self.load_page(p, false, cx);
                }
            }
            return crate::widgets::skeleton::row(&th, tr::ROW_H).w_full().into_any_element();
        };
        let (t, label) = match item {
            RowItem::Day { day, count } => return self.day_row(th, ix, &day, count, self.backend.now_ms()),
            RowItem::Track { t, label } => (*t, label),
        };
        let active = self.current_id.as_deref() == Some(t.id.as_str());
        let selected = self.sel.contains(ix);
        let starred = t.starred.is_some();
        let hovered = self.hover_row == Some(ix);
        let idx_opacity = 1.0 - anim::transition(window, ("lidx-fade", ix), if hovered { 1.0 } else { 0.0 }, m_anim::IDX_FADE);
        let idx_text = label.unwrap_or_else(|| format!("{:02}", ix + 1));
        let idx_cell = div()
                .id(("lidxc", ix))
                .relative()
                .w(px(tr::IDX_W))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(2.))
                .text_color(th.muted)
                // the play-glyph hover tracks only the index cell, not the whole row (see views/tracks.rs)
                .on_hover(cx.listener(move |this, h: &bool, _, cx| {
                    if *h {
                        this.hover_row = Some(ix);
                    } else if this.hover_row == Some(ix) {
                        this.hover_row = None;
                    }
                    cx.notify();
                }))
                .child(if active {
                    now_playing_bars(&th, self.playing).into_any_element()
                } else {
                    div()
                        .relative()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(div().opacity(idx_opacity).child(txt(&th, MONO_CODE, idx_text)))
                        .when(hovered, |d| d.child(div().absolute().child(icon("play", tr::ROW_PLAY_ICON, th.muted))))
                        .into_any_element()
                });
        let cell = |grow: f32, text: String| {
            flex_cell(grow).flex().child(txt(&th, MONO_DATA, text).color(th.muted).no_tracking().ellipsis())
        };
        let right = |w: f32, s: String| div().w(px(w)).flex_none().flex().justify_end().child(txt(&th, MONO_DATA, s).color(th.muted));
        let icon_btn = |id: &'static str, name: &'static str, color: gpui::Hsla, hover: gpui::Hsla| {
            div()
                .id((id, ix))
                .group("hv")
                .size(px(tr::ICON_BTN))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .cursor(CursorStyle::PointingHand)
                .text_color(color)
                .hover(move |s| s.text_color(hover))
                .child(icon_g(name, tr::ROW_ICON, color, hover))
        };

        let star = icon_btn("star", if starred { "heart-filled" } else { "heart" }, if starred { th.accent_fg } else { th.muted }, if starred { th.accent_fg } else { th.text })
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.stop_propagation();
                cx.emit(TableEvent::Star(ix));
            }));
        let more = icon_btn("more", "more", th.muted, th.text).on_click(cx.listener(move |_, ev: &ClickEvent, _, cx| {
            cx.stop_propagation();
            let p = ev.position();
            cx.emit(TableEvent::Menu { ix, pos: gpui::point(p.x, p.y + px(8.)) });
        }));
        let ghost_title = t.title.clone();

        let mut row = div()
            .id(("trow", ix))
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(tr::ROW_GAP))
            .pl(px(tr::ROW_PAD_LEFT))
            .pr(px(tr::ROW_PAD_RIGHT))
            .h(px(tr::ROW_H))
            .w_full()
            .border_b_1()
            .border_color(th.mid)
            .cursor(CursorStyle::Arrow)
            .when(active || selected, |d| d.bg(th.mid))
            .when(!active && !selected, |d| d.hover(move |s| s.bg(th.mid)))
            .child(idx_cell);
        if self.cfg.draggable {
            row = row.child(div().w(px(14.)).flex_none().text_color(th.muted).cursor(CursorStyle::OpenHand).child(icon("drag", 14., th.muted)));
        }
        row = row
            .child(flex_cell(tr::TITLE_GROW).flex().child(div().flex().min_w_0().child(txt(&th, BODY_MD, t.title.clone()).color(if active { th.accent_fg } else { th.text }).ellipsis())))
            .child(cell(tr::ARTIST_GROW, t.artist.clone()));
        if self.cfg.album_column {
            row = row.child(cell(tr::ALBUM_GROW, t.album.clone().unwrap_or_else(|| "—".into())));
        }
        row = row
            .when(cols.year, |d| d.child(right(tr::YEAR_W, fmt::track_year(&t))))
            .when(cols.added, |d| d.child(right(tr::ADDED_W, t.date_added.as_deref().map(fmt::date_part).unwrap_or("—").to_string())))
            .child(right(tr::FORMAT_W, fmt::format_track_format(&t.format)))
            .child(right(tr::TIME_W, fmt::format_duration(Some(t.duration))))
            .child(star);
        if self.cfg.add_btn {
            let rect = std::rc::Rc::new(std::cell::Cell::new(gpui::Bounds::<Pixels>::default()));
            let probe = crate::flow::bounds_probe(rect.clone());
            row = row.child(icon_btn("add", "add", th.muted, th.text).relative().child(probe).on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                let b = rect.get();
                cx.emit(TableEvent::AddToPlaylist { ix, pos: gpui::point(b.right(), b.bottom()) });
            })));
        }
        if self.cfg.remove_btn {
            row = row.child(icon_btn("remove", "trash", th.muted, th.text).on_click(cx.listener(move |_, _, _, cx| {
                cx.stop_propagation();
                cx.emit(TableEvent::Remove(ix));
            })));
        }
        row = row
            .child(more)
            .when(active, |d| d.child(div().absolute().left_0().top_0().bottom_0().w(px(tr::ACTIVE_BAR_W)).bg(th.accent_fg)))
            .when(selected, |d| d.child(div().absolute().top_0().left_0().size_full().border_1().border_color(th.accent_fg)))
            .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                if ev.click_count() >= 2 {
                    cx.emit(TableEvent::Play(ix));
                } else {
                    let m = ev.modifiers();
                    if this.cfg.multi {
                        // TrackSelection.range keeps the existing selection and the anchor
                        this.sel.click(ix, m.control || m.platform || m.shift, m.shift);
                    } else {
                        this.sel.move_to(ix, false);
                    }
                    cx.emit(TableEvent::SelectionChanged);
                }
                window.focus(&this.focus);
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus);
                    cx.emit(TableEvent::Menu { ix, pos: ev.position });
                    cx.stop_propagation();
                }),
            );
        if self.cfg.draggable {
            row = row
                .on_drag(DragRow(ix), move |_, _, _, cx| {
                    let t = ghost_title.clone();
                    cx.new(|_| DragGhost(t))
                })
                .drag_over::<DragRow>(move |s, _, _, _| s.border_t_2().border_color(th.accent_fg))
                .on_drop(cx.listener(move |_, d: &DragRow, _, cx| cx.emit(TableEvent::Reorder { from: d.0, to: ix })));
        }
        row.into_any_element()
    }
}

impl Focusable for Table {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Table {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::rcount::hit("table");
        crate::live::begin_view(window);
        let th = app::theme(cx);
        let cols = self.cols(window);
        let total = self.total();
        let bounds = self.body_bounds.clone();
        // the 10 px scrollbar shows only when the rows overflow the viewport
        let bh = f32::from(self.body_bounds.get().size.height);
        let overflow = scrollbar::overflow_or_last("lists-table", (bh > 0.0 && total > 0).then_some(total as f32 * tr::ROW_H > bh + 0.5));
        let bounds_for_canvas = bounds.clone();

        let list = uniform_list(
            "lists-table",
            total,
            cx.processor(move |this, range: Range<usize>, window, cx| {
                this.vis = range.clone();
                range.map(|ix| this.row(ix, cols, window, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.list.clone())
        .size_full();

        let body: AnyElement = if total == 0 && !self.loaded() {
            // skeleton rows at the real row height instead of a "Loading…" line
            let mut rows_v = div().flex().flex_col().size_full().overflow_hidden();
            for _ in 0..40 {
                rows_v = rows_v.child(crate::widgets::skeleton::row(&th, tr::ROW_H));
            }
            rows_v.into_any_element()
        } else if total == 0 {
            div().p(px(lm::EMPTY_PAD)).child(txt(&th, BODY_SM, self.cfg.empty).color(th.muted)).into_any_element()
        } else {
            div()
                .id("lists-body")
                .key_context("TrackList")
                .track_focus(&self.focus)
                .on_action(cx.listener(|this, _: &SelDown, _, cx| this.key_move(1, cx)))
                .on_action(cx.listener(|this, _: &SelUp, _, cx| this.key_move(-1, cx)))
                .on_action(cx.listener(|this, _: &SelectAllRows, _, cx| {
                    if this.cfg.multi {
                        let n = this.total();
                        this.sel.select_all(n);
                        cx.emit(TableEvent::SelectionChanged);
                        cx.notify();
                    }
                }))
                .on_action(cx.listener(|this, _: &EnqueueSelected, _, cx| {
                    if let Some(i) = this.sel.cursor()
                        && this.track_at(i).is_some()
                    {
                        cx.emit(TableEvent::Enqueue(i));
                    }
                }))
                .on_action(cx.listener(|this, _: &PlaySelected, _, cx| {
                    if let Some(i) = this.sel.cursor()
                        && this.track_at(i).is_some()
                    {
                        cx.emit(TableEvent::Play(i));
                    }
                }))
                .on_action(cx.listener(|this, _: &OpenMenuKey, _, cx| {
                    // the menu key opens the menu only when a row is selected
                    if let Some(c) = this.sel.cursor().filter(|c| this.sel.contains(*c)) {
                        this.open_menu_at_row(c, cx);
                    }
                }))
                .relative()
                .size_full()
                .child(
                    gpui::canvas(
                        move |b, window, _| {
                            let old = bounds_for_canvas.get();
                            bounds_for_canvas.set(b);
                            if (f32::from(old.size.height) - f32::from(b.size.height)).abs() > 0.5 {
                                window.request_animation_frame(); // prepaint: `refresh()` is a no-op while drawing
                            }
                        },
                        |_, _, _, _| (),
                    )
                    .absolute()
                    .size_full(),
                )
                .child(div().size_full().when(overflow, |d| d.pr(px(tr::SCROLLBAR_W))).child(list))
                .children(overflow.then(|| scrollbar::scrollbar("lists-scrollbar", th, &self.list, &self.sb)))
                .child(crate::widgets::smooth::drive(window, crate::widgets::smooth::Scrollable::list(&self.list)))
                .into_any_element()
        };

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .border_1()
            .border_color(th.border)
            .bg(th.low)
            .children(self.cfg.head.then(|| self.head_row(th, cols)))
            .child(div().flex_1().min_h_0().child(body))
    }
}
