//! The Tracks route (`Tracks.svelte` + `TrackRow.svelte`): a 54k-row paged, virtualized
//! table with sortable headers, the width-driven Year/Added hiding rule, hover / selected /
//! active rows, Ctrl/Shift multi-select, keyboard navigation, double-click or Enter to play,
//! a context menu and a draggable styled scrollbar.
use super::shell::{EnqueueSelected, OpenMenuKey, PlaySelected, SelDown, SelUp, Shell};
use crate::app::{self, PlayerEvent, PlayerModel};
use crate::art::ArtHandle;
use crate::columns::{Columns, visible_columns};
use crate::pager::{PAGE, Pager};
use crate::probe::pid;
use crate::router::Route;
use crate::selection::Selection;
use crate::anim;
use crate::metrics::{anim as m_anim, controls as ctl, tracks as tr};
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{BODY_MD, BODY_SM, HEADLINE_MD, LABEL_CAPS, MONO_CODE, MONO_DATA, TypeSpec};
use crate::widgets::button::{Variant, button};
use crate::widgets::context_menu::{ContextMenu, MenuItem};
use crate::widgets::scrollbar::{self, ScrollbarHandle};
use crate::widgets::text_input::{InputEvent, TextInput};
use crate::widgets::{icon, icon_g};
use gpui::{
    AnyElement, ClickEvent, Context, CursorStyle, Div, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton,
    MouseDownEvent, Point, Pixels, Render, ScrollStrategy, SharedString, Stateful, Subscription, Timer, UniformListScrollHandle, Window, div,
    canvas, prelude::*, px, uniform_list,
};
use sonix_api::format as fmt;
use sonix_api::*;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

pub const ROW_HEIGHT: f32 = tr::ROW_H;

pub enum TracksEvent {
    Navigate(Route),
}
impl EventEmitter<TracksEvent> for TracksView {}

pub struct TracksView {
    th: Theme,
    backend: Arc<dyn Backend>,
    _art: ArtHandle,
    menu: Entity<ContextMenu>,
    current_id: Option<String>,
    playing: bool,
    sort: TrackSortField,
    dir: SortDir,
    filter: String,
    filter_gen: u64,
    filter_input: Entity<TextInput>,
    pager: Pager<Track>,
    list: UniformListScrollHandle,
    sb: ScrollbarHandle,
    sel: Selection,
    focus: FocusHandle,
    vis: Range<usize>,
    hover_row: Option<usize>,
    playlists: Vec<Playlist>,
    flow_btn: std::rc::Rc<std::cell::Cell<gpui::Bounds<Pixels>>>,
    _subs: Vec<Subscription>,
}

impl TracksView {
    pub fn new(backend: Arc<dyn Backend>, art: ArtHandle, player: &Entity<PlayerModel>, menu: &Entity<ContextMenu>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let th = app::theme(cx);
        let filter_input = cx.new(|cx| TextInput::new(cx, "", &th, crate::theme::MONO, MONO_DATA.size, MONO_DATA.line, MONO_DATA.weight));
        let mut subs = Vec::new();
        subs.push(cx.subscribe(&filter_input, |this: &mut Self, input, ev: &InputEvent, cx| {
            if *ev == InputEvent::Changed {
                let q = input.read(cx).content.to_string();
                this.set_filter_debounced(q, cx);
            }
        }));
        subs.push(cx.subscribe(player, |this: &mut Self, p, _: &PlayerEvent, cx| {
            let p = p.read(cx);
            this.current_id = p.current_id().map(str::to_string);
            this.playing = p.playing();
            cx.notify();
        }));
        let mut this = Self {
            th,
            backend,
            _art: art,
            menu: menu.clone(),
            current_id: None,
            playing: false,
            sort: TrackSortField::Title,
            dir: SortDir::Asc,
            filter: String::new(),
            filter_gen: 0,
            filter_input,
            pager: Pager::default(),
            list: UniformListScrollHandle::new(),
            sb: scrollbar::new_handle(),
            sel: Selection::default(),
            focus: cx.focus_handle(),
            vis: 0..0,
            hover_row: None,
            playlists: Vec::new(),
            flow_btn: std::rc::Rc::new(std::cell::Cell::new(gpui::Bounds::default())),
            _subs: subs,
        };
        let _ = window;
        this.reload(cx);
        let b = this.backend.clone();
        cx.spawn(async move |this, cx| {
            let pl = cx.background_executor().spawn(async move { b.playlists_list() }).await;
            let _ = this.update(cx, |t, _| t.playlists = pl.unwrap_or_default());
        })
        .detach();
        this
    }

    pub fn set_theme(&mut self, th: &Theme) {
        self.th = *th;
    }

    /// Parity harness: selection, scroll offset and which of this view's controls has focus.
    #[cfg(feature = "parity")]
    pub(crate) fn parity_state(&self, window: &Window, cx: &gpui::App) -> serde_json::Value {
        let focus = if self.focus.is_focused(window) {
            "tracklist"
        } else if self.filter_input.read(cx).focus_handle.is_focused(window) {
            "filter"
        } else {
            "body"
        };
        let sel = self.sel.indices();
        let scroll = f32::from(-self.list.0.borrow().base_handle.offset().y);
        let titles: Vec<String> = if scroll < 36.0 { (0..3).filter_map(|i| self.pager.get(i).map(|t| t.title.clone())).collect() } else { vec![] };
        serde_json::json!({
            "focus": focus,
            "selectionCount": if sel.len() > 1 { sel.len() } else { 0 }, // the header only shows "N SELECTED" for more than one row
            "selection": sel.iter().filter(|i| **i < 12).collect::<Vec<_>>(),
            "scrollTop": scroll,
            "sort": {"field": format!("{:?}", self.sort), "dir": format!("{:?}", self.dir)},
            "trackCount": self.pager.total,
            "titles": titles,
        })
    }

    // ------------------------------------------------------------ data
    fn req(&self, page: usize) -> GetTracksRequest {
        GetTracksRequest {
            offset: page * PAGE,
            limit: PAGE,
            sort: self.sort,
            dir: self.dir,
            filter: if self.filter.trim().is_empty() { None } else { Some(self.filter.clone()) },
        }
    }

    /// New sort/filter: drop everything, fetch page 0 (which also yields the total).
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.sel.clear();
        let epoch = self.pager.reset();
        self.list.scroll_to_item(0, ScrollStrategy::Top);
        self.vis = 0..0;
        let b = self.backend.clone();
        let req = self.req(0);
        self.pager.want(0);
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { b.browse_get_tracks(req) }).await;
            let _ = this.update(cx, |t, cx| {
                if t.pager.epoch != epoch {
                    return;
                }
                if let Ok(p) = r {
                    t.pager.total = p.total;
                    t.pager.insert(epoch, 0, p.items, 0);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn request_page(&mut self, page: usize, cx: &mut Context<Self>) {
        if !self.pager.want(page) {
            return;
        }
        self.fetch_page(page, cx);
    }

    fn fetch_page(&mut self, page: usize, cx: &mut Context<Self>) {
        let (b, epoch, req) = (self.backend.clone(), self.pager.epoch, self.req(page));
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { b.browse_get_tracks(req) }).await;
            let _ = this.update(cx, |t, cx| {
                if let Ok(p) = r {
                    let centre = t.vis.start;
                    if t.pager.insert(epoch, page, p.items, centre) {
                        t.pager.total = p.total;
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Stars/ratings changed: re-fetch the visible pages in place (no blank flash, scroll kept).
    pub fn refresh_rows(&mut self, cx: &mut Context<Self>) {
        let (a, z) = (self.vis.start / PAGE, self.vis.end.saturating_sub(1) / PAGE);
        for p in a..=z {
            self.fetch_page(p, cx);
        }
    }

    /// Screenshot hook: open the context menu on row `ix` at a fixed spot.
    pub fn debug_menu(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.sel.click(ix, false, false);
        self.open_menu(ix, gpui::point(px(520.), px(150. + ix as f32 * ROW_HEIGHT)), window, cx);
    }

    pub fn script_scroll(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.list.scroll_to_item(ix, ScrollStrategy::Top);
        cx.notify();
    }

    /// Minimize: drop the paged rows (keep total, scroll offset and selection indices).
    pub fn release(&mut self) {
        self.pager.release();
    }

    pub fn restore(&mut self, cx: &mut Context<Self>) {
        cx.notify(); // visible pages are re-requested by the next render
    }

    fn set_filter_debounced(&mut self, q: String, cx: &mut Context<Self>) {
        self.filter_gen += 1;
        let g = self.filter_gen;
        cx.spawn(async move |this, cx| {
            Timer::after(Duration::from_millis(200)).await;
            let _ = this.update(cx, |t, cx| {
                if t.filter_gen == g && t.filter != q {
                    t.filter = q;
                    t.reload(cx);
                }
            });
        })
        .detach();
    }

    fn on_sort(&mut self, f: TrackSortField, cx: &mut Context<Self>) {
        if self.sort == f {
            self.dir = if self.dir == SortDir::Asc { SortDir::Desc } else { SortDir::Asc };
        } else {
            self.sort = f;
            self.dir = if f == TrackSortField::DateAdded { SortDir::Desc } else { SortDir::Asc };
        }
        self.reload(cx);
    }

    // ------------------------------------------------------------ actions
    fn play_from(&mut self, id: String, cx: &mut Context<Self>) {
        let (field, dir) = (self.sort, self.dir);
        let filter = if self.filter.trim().is_empty() { None } else { Some(self.filter.clone()) };
        app::call(cx, move |b| {
            let _ = b.player_play_all(PlayAllRequest { order: TrackOrder::Sort { field, dir }, filter, start_track_id: Some(id), flow: None });
        });
    }

    fn play_all(&mut self, shuffle: bool, cx: &mut Context<Self>) {
        let (field, dir) = (self.sort, self.dir);
        let filter = if self.filter.trim().is_empty() { None } else { Some(self.filter.clone()) };
        app::call(cx, move |b| {
            let order = if shuffle { TrackOrder::Shuffle } else { TrackOrder::Sort { field, dir } };
            let _ = b.player_play_all(PlayAllRequest { order, filter, start_track_id: None, flow: None });
        });
    }

    /// `flow.ts openFlowMenu`: the pool's real coverage hint (disabled entry + rule), then Steady / Build / Peak under the button.
    fn open_flow_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let filter = if self.filter.trim().is_empty() { None } else { Some(self.filter.clone()) };
        let at = {
            let b = self.flow_btn.get();
            gpui::point(b.left(), b.bottom() + px(4.))
        };
        let b = self.backend.clone();
        let f2 = filter.clone();
        cx.spawn_in(window, async move |this, cx| {
            let cov = cx.background_executor().spawn(async move { b.flow_get_coverage(FlowCoverageRequest { kind: FlowPoolKind::AllTracks, id: None, filter: f2, seed: None }) }).await;
            let hint = cov.ok().map(|c| fmt::coverage_hint(&c, "view")).unwrap_or_default();
            let _ = this.update_in(cx, |t, window, cx| {
                let mut items = Vec::new();
                if !hint.is_empty() {
                    items.push(MenuItem::action(hint, |_, _| {}).disabled(true));
                    items.push(MenuItem::separator());
                }
                for (arc, label, h) in [(FlowArc::Steady, "Steady", "Smooth transitions, even energy"), (FlowArc::Build, "Build", "Energy rises over each ~40 tracks"), (FlowArc::Peak, "Peak", "Rise, then cool down")] {
                    let filter = filter.clone();
                    items.push(
                        MenuItem::action(label, move |_, cx| {
                            let filter = filter.clone();
                            app::call(cx, move |b| {
                                let _ = b.player_play_all(PlayAllRequest { order: TrackOrder::Shuffle, filter, start_track_id: None, flow: Some(arc) });
                            });
                        })
                        .hint(h),
                    );
                }
                t.menu.update(cx, |m, cx| m.show(items, at, window, cx));
            });
        })
        .detach();
    }

    fn toggle_star(&mut self, t: &Track, cx: &mut Context<Self>) {
        let (id, starred) = (t.id.clone(), t.starred.is_some());
        app::call(cx, move |b| {
            let req = StarRequest { id, kind: StarTargetType::Track };
            let _ = if starred { b.star_remove(req) } else { b.star_add(req) };
        });
    }

    fn selected_tracks(&self, indices: &[usize]) -> Vec<Track> {
        indices.iter().filter_map(|i| self.pager.get(*i).cloned()).collect()
    }

    fn open_menu(&mut self, ix: usize, pos: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let indices = self.sel.for_context_menu(ix);
        let tracks = self.selected_tracks(&indices);
        if tracks.is_empty() {
            return;
        }
        let items = self.menu_items(&tracks, pos, cx);
        self.menu.update(cx, |m, cx| m.show(items, pos, window, cx));
        cx.notify();
    }

    fn menu_items(&self, tracks: &[Track], at: Point<Pixels>, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let this = cx.entity().downgrade();
        let ids: Vec<String> = tracks.iter().map(|t| t.id.clone()).collect();
        let this_play = this.clone();
        let ids_play = ids.clone();
        let this_nav = this.clone();
        crate::menus::track_menu_items(crate::menus::TrackMenu {
            tracks: tracks.to_vec(),
            at,
            on_play_now: std::rc::Rc::new(move |cx| {
                let ids = ids_play.clone();
                if let Some(t) = this_play.upgrade() {
                    t.update(cx, |t, cx| {
                        if ids.len() == 1 {
                            t.play_from(ids[0].clone(), cx);
                        } else {
                            app::call(cx, move |b| {
                                let _ = b.player_play_queue(PlayQueueRequest { track_ids: ids, start_index: 0 });
                            });
                        }
                    });
                }
            }),
            on_play_next: None,
            on_remove_from_queue: None,
            on_navigate: std::rc::Rc::new(move |r, cx| {
                if let Some(t) = this_nav.upgrade() {
                    t.update(cx, |_, cx| cx.emit(TracksEvent::Navigate(r)));
                }
            }),
        })
    }

    // ------------------------------------------------------------ keyboard
    fn scroll_to_cursor(&mut self, n: usize) {
        if n < self.vis.start {
            self.list.scroll_to_item(n, ScrollStrategy::Top);
        } else if n + 1 >= self.vis.end {
            self.list.scroll_to_item(n, ScrollStrategy::Bottom);
        }
    }

    fn key_move(&mut self, delta: isize, extend: bool, cx: &mut Context<Self>) {
        if let Some(n) = self.sel.move_cursor(delta, extend, self.pager.total) {
            self.scroll_to_cursor(n);
        }
        cx.notify();
    }

    // ------------------------------------------------------------ render pieces
    fn cols(&self, window: &Window) -> Columns {
        let vw = f32::from(window.viewport_size().width);
        let rail = if Shell::rail_docked(vw, &Route::Tracks) { crate::metrics::layout::RAIL_W } else { 0.0 };
        // container = table panel content box: viewport - sidebar - rail - 2*16 page padding - 2*1 panel border
        visible_columns(vw - Shell::sidebar_width(vw) - rail - 2.0 * tr::PAGE_PAD - 2.0)
    }

    /// Sortable column header: a `<button class="sort-btn">` inside `.type-label-caps`. UA buttons reset
    /// `text-transform`, so the labels stay mixed case ("Title ↑"); colour is the head's muted, stark on hover.
    fn header_cell(&self, id: &'static str, label: &'static str, field: TrackSortField, cx: &mut Context<Self>) -> Stateful<Div> {
        let th = self.th;
        let arrow = if self.sort == field { if self.dir == SortDir::Desc { " ↓" } else { " ↑" } } else { "" };
        let spec = TypeSpec { caps: false, ..LABEL_CAPS };
        div()
            .id(id)
            .flex()
            .flex_row()
            .cursor(CursorStyle::PointingHand)
            .text_color(th.muted)
            .hover(move |s| s.text_color(th.text))
            .child(txt(&th, spec, format!("{label}{arrow}")).no_tracking())
            .on_click(cx.listener(move |this, _, _, cx| this.on_sort(field, cx)))
    }

    fn head_row(&self, cols: Columns, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let th = self.th;
        let spacer = || div().w(px(tr::ICON_BTN)).h(px(tr::ICON_BTN)).flex_none();
        pid(
            "table.head",
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(tr::ROW_GAP))
                .pl(px(tr::ROW_PAD_LEFT))
                .pr(px(tr::ROW_PAD_RIGHT))
                .py(px(tr::HEAD_PAD_Y))
                .flex_none()
                .border_b_1()
                .border_color(th.border)
                .child(div().w(px(tr::IDX_W)).flex_none())
                .child(pid("table.head.title", flex_cell(tr::TITLE_GROW).flex().child(pid("table.head.btn.title", self.header_cell("h-title", "Title", TrackSortField::Title, cx)))))
                .child(pid("table.head.artist", flex_cell(tr::ARTIST_GROW).flex().child(pid("table.head.btn.artist", self.header_cell("h-artist", "Artist", TrackSortField::Artist, cx)))))
                .child(pid("table.head.album", flex_cell(tr::ALBUM_GROW).flex().child(pid("table.head.btn.album", self.header_cell("h-album", "Album", TrackSortField::Album, cx)))))
                .when(cols.year, |d| d.child(pid("table.head.year", div().w(px(tr::YEAR_W)).flex_none().flex().justify_end().child(pid("table.head.btn.year", self.header_cell("h-year", "Year", TrackSortField::Year, cx))))))
                .when(cols.added, |d| d.child(pid("table.head.added", div().w(px(tr::ADDED_W)).flex_none().flex().justify_end().child(pid("table.head.btn.added", self.header_cell("h-added", "Added", TrackSortField::DateAdded, cx))))))
                .child(pid("table.head.format", div().w(px(tr::FORMAT_W)).flex_none().flex().justify_end().child(pid("table.head.btn.format", self.header_cell("h-format", "Format", TrackSortField::Format, cx)))))
                .child(pid("table.head.time", div().w(px(tr::TIME_W)).flex_none().flex().justify_end().child(pid("table.head.btn.time", self.header_cell("h-time", "Time", TrackSortField::Duration, cx)))))
                .child(spacer())
                .child(spacer()),
        )
    }

    fn row(&mut self, ix: usize, cols: Columns, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = self.th;
        let Some(t) = self.pager.get(ix).cloned() else {
            self.request_page(ix / PAGE, cx);
            return div().id(("trow", ix)).h(px(tr::ROW_H)).w_full().bg(th.mid).into_any_element();
        };
        let active = self.current_id.as_deref() == Some(t.id.as_str());
        let selected = self.sel.contains(ix);
        let starred = t.starred.is_some();
        let id_play = t.id.clone();
        let hovered = self.hover_row == Some(ix);
        // `TrackRow dragIds`: dragging a selected row drags the whole (loaded part of the) selection
        let drag_ids: Vec<String> = if selected && self.sel.len() > 1 && self.sel.len() <= 500 { self.sel.indices().into_iter().filter_map(|i| self.pager.get(i).map(|t| t.id.clone())).collect() } else { vec![t.id.clone()] };
        let drag = crate::dnd::SonixDrag(crate::dnd::DragPayload::Tracks(drag_ids));
        // `.idx { transition: opacity 0.1s }`; the play glyph appears at once on :hover.
        let idx_opacity = 1.0 - anim::transition(window, ("idx-fade", ix), if hovered { 1.0 } else { 0.0 }, m_anim::IDX_FADE);
        let idx_cell = pid(
            format!("track.row.{ix}.idx"),
            div()
                .relative()
                .w(px(tr::IDX_W))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(2.))
                .text_color(th.muted)
                .child(if active {
                    now_playing_bars(&th, self.playing).into_any_element()
                } else {
                    div()
                        .relative()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(div().opacity(idx_opacity).child(txt(&th, MONO_CODE, format!("{:02}", ix + 1))))
                        .when(hovered, |d| d.child(div().absolute().child(icon("play", tr::ROW_PLAY_ICON, th.muted))))
                        .into_any_element()
                }),
        );
        let cell = |name: &str, grow: f32, text: String| {
            pid(format!("track.row.{ix}.{name}"), flex_cell(grow).flex().child(txt(&th, MONO_DATA, text).color(th.muted).no_tracking().ellipsis()))
        };
        let right = |name: &str, w: f32, s: String| pid(format!("track.row.{ix}.{name}"), div().w(px(w)).flex_none().flex().justify_end().child(txt(&th, MONO_DATA, s).color(th.muted)));

        pid(
            format!("track.row.{ix}"),
            div()
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
                .on_drag(drag, move |d: &crate::dnd::SonixDrag, _, _, cx| {
                    let label: SharedString = crate::dnd::ghost_label(&d.0).into();
                    cx.new(|_| crate::dnd::Ghost { label, th })
                })
                .when(active || selected, |d| d.bg(th.mid))
                .when(!active && !selected, |d| d.hover(move |s| s.bg(th.mid)))
                .child(idx_cell)
                .child(flex_cell(tr::TITLE_GROW).flex().child(pid(
                    format!("track.row.{ix}.title"),
                    div().flex().min_w_0().child(txt(&th, BODY_MD, t.title.clone()).color(if active { th.accent_fg } else { th.text }).ellipsis()),
                )))
                .child(cell("artist", tr::ARTIST_GROW, t.artist.clone()))
                .child(cell("album", tr::ALBUM_GROW, t.album.clone().unwrap_or_else(|| "—".into())))
                .when(cols.year, |d| d.child(right("year", tr::YEAR_W, fmt::track_year(&t))))
                .when(cols.added, |d| d.child(right("added", tr::ADDED_W, t.date_added.as_deref().map(fmt::date_part).unwrap_or("—").to_string())))
                .child(right("format", tr::FORMAT_W, fmt::format_track_format(&t.format)))
                .child(right("time", tr::TIME_W, fmt::format_duration(Some(t.duration))))
                .child(pid(format!("track.row.{ix}.star"), self.star_button(ix, &t, starred, cx)))
                .child(pid(format!("track.row.{ix}.more"), self.more_button(ix, cx)))
                .when(active, |d| d.child(div().absolute().left_0().top_0().bottom_0().w(px(tr::ACTIVE_BAR_W)).bg(th.accent_fg)))
                .when(selected, |d| d.child(div().absolute().top_0().left_0().size_full().border_1().border_color(th.accent_fg)))
                .on_hover(cx.listener(move |this, h: &bool, _, cx| {
                    if *h {
                        this.hover_row = Some(ix);
                    } else if this.hover_row == Some(ix) {
                        this.hover_row = None;
                    }
                    cx.notify();
                }))
                .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                    if ev.click_count() >= 2 {
                        this.play_from(id_play.clone(), cx);
                    } else {
                        let m = ev.modifiers();
                        this.sel.click(ix, m.control || m.platform, m.shift);
                    }
                    window.focus(&this.focus);
                    cx.notify();
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                        window.focus(&this.focus);
                        this.open_menu(ix, ev.position, window, cx);
                        // the list body (a focusable ancestor) must not take the focus back from the menu
                        cx.stop_propagation();
                    }),
                ),
        )
        .into_any_element()
    }

    fn star_button(&self, ix: usize, t: &Track, starred: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let th = self.th;
        let t = t.clone();
        div()
            .id(("star", ix))
            .group("hv")
            .size(px(24.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .cursor(CursorStyle::PointingHand)
            .text_color(if starred { th.accent_fg } else { th.muted })
            .hover(move |s| s.text_color(if starred { th.accent_fg } else { th.text }))
            .child(icon_g(if starred { "heart-filled" } else { "heart" }, tr::ROW_ICON, if starred { th.accent_fg } else { th.muted }, if starred { th.accent_fg } else { th.text }))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_star(&t, cx);
            }))
    }

    fn more_button(&self, ix: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let th = self.th;
        div()
            .id(("more", ix))
            .group("hv")
            .size(px(24.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .cursor(CursorStyle::PointingHand)
            .text_color(th.muted)
            .hover(move |s| s.text_color(th.text))
            .child(icon_g("more", tr::ROW_ICON, th.muted, th.text))
            .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                cx.stop_propagation();
                let p = ev.position();
                this.open_menu(ix, gpui::point(p.x, p.y + px(8.)), window, cx);
            }))
    }
}

/// `flex: <grow> 1 0` (basis 0), the columns' proportional widths.
fn flex_cell(grow: f32) -> Div {
    let mut d = div().min_w_0();
    let s = d.style();
    s.flex_grow = Some(grow);
    s.flex_shrink = Some(1.0);
    s.flex_basis = Some(px(0.).into());
    d
}

/// The three "now playing" bars of an active row (`TrackRow.svelte:.now-playing`): `scaleY` keyframes on the
/// shared clock (0.9 s ease-in-out infinite, delays -0.6/-0.3 s) while playing, static `scaleY(.33)` when paused.
/// Repaints every frame only while playing, visible and not suspended (the row exists only while on screen).
fn now_playing_bars(th: &Theme, playing: bool) -> impl IntoElement {
    let color = th.accent;
    gpui::canvas(
        |_, _, _| (),
        move |b, _, window, _| {
            let heights = anim::bar_heights(playing, crate::clock::now() as f32);
            anim::paint_bars(window, b.origin, color, heights);
            if playing && !crate::clock::frozen() {
                window.request_animation_frame();
            }
        },
    )
    .w(px(tr::BAR_W * 3.0 + tr::BARS_GAP * 2.0))
    .h(px(tr::BARS_H))
}

impl Focusable for TracksView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TracksView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = self.th;
        let cols = self.cols(window);
        let total = self.pager.total;
        let selecting = self.sel.len() > 1;
        let count: SharedString =
            if selecting { format!("{} SELECTED", fmt::fmt_int(self.sel.len() as u64)) } else { format!("{} TRACKS", fmt::fmt_int(total as u64)) }.into();
        let filter_focused = self.filter_input.read(cx).focus_handle.is_focused(window);

        let field_w = if th.light { ctl::FIELD_W_LIGHT } else { ctl::FIELD_W_DARK };
        let filter_empty = self.filter_input.read(cx).content.is_empty();
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(tr::HEADER_GAP))
            .flex_none()
            .child(pid("page.title", txt(&th, HEADLINE_MD, "Tracks").color(th.text)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(tr::HEADER_RIGHT_GAP))
                    .child(pid("page.count", txt(&th, MONO_CODE, count).color(th.muted)))
                    .child(pid(
                        "page.filter",
                        div()
                            .relative()
                            .w(px(field_w))
                            .h(px(ctl::FIELD_H))
                            .flex_none()
                            .px(px(ctl::FIELD_PAD_X))
                            .flex()
                            .items_center()
                            .bg(th.low)
                            .border_1()
                            .border_color(if filter_focused { th.focus } else { th.border })
                            .when(filter_focused, |d| d.shadow(crate::widgets::hard_shadow(&th, ctl::BTN_SHADOW)))
                            .child(pid("page.filter.input", div().flex_1().min_w_0().h_full().flex().items_center().child(self.filter_input.clone())))
                            .when(filter_empty, |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .left(px(ctl::FIELD_PAD_X))
                                        .right(px(ctl::FIELD_PAD_X))
                                        .top_0()
                                        .bottom_0()
                                        .overflow_hidden()
                                        .flex()
                                        .items_center()
                                        .child(div().flex_none().child(txt(&th, MONO_DATA, "Filter by title, artist, album…").color(th.muted))),
                                )
                            }),
                    ))
                    .child(pid("page.btn.0", button("play-all", th, Variant::Secondary, "Play All", Some("play"), total == 0, window).on_click(cx.listener(|this, _, _, cx| this.play_all(false, cx)))))
                    .child(pid("page.btn.1", button("shuffle-all", th, Variant::Primary, "Shuffle All", Some("shuffle"), total == 0, window).on_click(cx.listener(|this, _, _, cx| this.play_all(true, cx)))))
                    // Flow (rule-based radio) is not ported yet: the control is present for layout parity, inert.
                    .child(pid("page.btn.2", {
                        let cell = self.flow_btn.clone();
                        button("flow", th, Variant::Secondary, "Flow", Some("sliders"), total == 0, window)
                            .relative()
                            .child(canvas(move |b, _, _| cell.set(b), |_, _, _, _| ()).absolute().size_full())
                            .on_click(cx.listener(|this, _, window, cx| this.open_flow_menu(window, cx)))
                    })),
            );

        let list = uniform_list(
            "tracks-list",
            total,
            cx.processor(move |this, range: Range<usize>, window, cx| {
                this.vis = range.clone();
                range.map(|ix| this.row(ix, cols, window, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.list.clone())
        .size_full();

        let body: AnyElement = if total == 0 && self.pager.has_page(0) {
            div()
                .p(px(crate::metrics::space::MD))
                .child(txt(&th, BODY_SM, if self.filter.trim().is_empty() { "No tracks in your library yet." } else { "No tracks match your filter." }).color(th.muted))
                .into_any_element()
        } else {
            div()
                .id("tracks-body")
                .key_context("TrackList")
                .track_focus(&self.focus)
                .on_action(cx.listener(|this, _: &SelDown, _, cx| this.key_move(1, false, cx)))
                .on_action(cx.listener(|this, _: &SelUp, _, cx| this.key_move(-1, false, cx)))
                .on_action(cx.listener(|this, _: &EnqueueSelected, _, cx| {
                    if let Some(id) = this.sel.cursor().and_then(|i| this.pager.get(i)).map(|t| t.id.clone()) {
                        app::call(cx, move |b| {
                            let _ = b.player_enqueue(EnqueueRequest { track_ids: vec![id], position: EnqueuePosition::End });
                        });
                    }
                }))
                .on_action(cx.listener(|this, _: &PlaySelected, _, cx| {
                    if let Some(t) = this.sel.cursor().and_then(|i| this.pager.get(i)).map(|t| t.id.clone()) {
                        this.play_from(t, cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &OpenMenuKey, window, cx| {
                    if let Some(c) = this.sel.cursor() {
                        let y = (c.saturating_sub(this.vis.start)) as f32 * ROW_HEIGHT + 200.0;
                        this.open_menu(c, gpui::point(px(420.), px(y)), window, cx);
                    }
                }))
                .relative()
                .size_full()
                .child(div().size_full().pr(px(tr::SCROLLBAR_W)).child(list))
                .child(scrollbar::scrollbar("tracks-scrollbar", th, &self.list, &self.sb))
                .into_any_element()
        };

        pid("page", div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(tr::PAGE_GAP))
            .p(px(tr::PAGE_PAD))
            .child(header)
            .child(
                pid("table.panel", div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .border_1()
                    .border_color(th.border)
                    .bg(th.low)
                    .child(self.head_row(cols, cx))
                    .child(div().flex_1().min_h_0().child(body))),
            ))
    }
}
