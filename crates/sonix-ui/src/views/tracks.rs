//! The Tracks route (`Tracks.svelte` + `TrackRow.svelte`): a 54k-row paged, virtualized
//! table with sortable headers, the width-driven Year/Added hiding rule, hover / selected /
//! active rows, Ctrl/Shift multi-select, keyboard navigation, double-click or Enter to play,
//! a context menu and a draggable styled scrollbar.
use super::shell::{ListEnd, ListHome, OpenMenuKey, PgDown, PgUp, PlaySelected, SelDown, SelUp, SelectAllRows, Shell};
use crate::app::{self, PlayerEvent, PlayerModel};
use crate::art::ArtHandle;
use crate::columns::{Columns, visible_columns};
use crate::pager::{PAGE, Pager};
use crate::router::Route;
use crate::selection::Selection;
use crate::theme::{MONO, Theme};
use crate::tracking::{BODY_MD, HEADLINE_MD, LABEL_CAPS, MONO_CODE, tracked};
use crate::typo::{ellipsis, ts};
use crate::widgets::button::{Variant, button};
use crate::widgets::context_menu::{ContextMenu, MenuItem};
use crate::widgets::scrollbar::{self, ScrollbarHandle};
use crate::widgets::text_input::{InputEvent, TextInput};
use crate::widgets::{icon, icon_g};
use gpui::{
    AnyElement, ClickEvent, ClipboardItem, Context, CursorStyle, Div, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, MouseButton,
    MouseDownEvent, Point, Pixels, Render, ScrollStrategy, SharedString, Stateful, Subscription, Timer, UniformListScrollHandle, Window, div,
    prelude::*, px, uniform_list,
};
use sonix_api::format as fmt;
use sonix_api::*;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

pub const ROW_HEIGHT: f32 = 36.0;

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
    playlists: Vec<Playlist>,
    _subs: Vec<Subscription>,
}

impl TracksView {
    pub fn new(backend: Arc<dyn Backend>, art: ArtHandle, player: &Entity<PlayerModel>, menu: &Entity<ContextMenu>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let th = app::theme(cx);
        let filter_input = cx.new(|cx| TextInput::new(cx, "Filter by title, artist, album…", &th, th.body_font, 13.0, 18.0, 400.0));
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
            playlists: Vec::new(),
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
            let _ = b.player_play_all(PlayAllRequest { order: TrackOrder::Sort { field, dir }, filter, start_track_id: Some(id) });
        });
    }

    fn play_all(&mut self, shuffle: bool, cx: &mut Context<Self>) {
        let (field, dir) = (self.sort, self.dir);
        let filter = if self.filter.trim().is_empty() { None } else { Some(self.filter.clone()) };
        app::call(cx, move |b| {
            let order = if shuffle { TrackOrder::Shuffle } else { TrackOrder::Sort { field, dir } };
            let _ = b.player_play_all(PlayAllRequest { order, filter, start_track_id: None });
        });
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
        let items = self.menu_items(&tracks, cx);
        self.menu.update(cx, |m, cx| m.show(items, pos, window, cx));
        cx.notify();
    }

    fn menu_items(&self, tracks: &[Track], cx: &mut Context<Self>) -> Vec<MenuItem> {
        let n = tracks.len();
        let suffix = if n > 1 { format!(" ({n} tracks)") } else { String::new() };
        let ids: Vec<String> = tracks.iter().map(|t| t.id.clone()).collect();
        let first = tracks[0].clone();
        let this = cx.entity().downgrade();
        let all_starred = tracks.iter().all(|t| t.starred.is_some());

        let ids_a = ids.clone();
        let this_a = this.clone();
        let play_now = MenuItem::action("Play now", move |_, cx| {
            let ids = ids_a.clone();
            if let Some(t) = this_a.upgrade() {
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
        })
        .icon("play");
        let enqueue = |label: String, pos: EnqueuePosition, ids: Vec<String>, icon: &'static str| {
            MenuItem::action(label, move |_, cx| {
                let ids = ids.clone();
                app::call(cx, move |b| {
                    let _ = b.player_enqueue(EnqueueRequest { track_ids: ids, position: pos });
                });
            })
            .icon(icon)
        };
        let playlists: Vec<MenuItem> = self
            .playlists
            .iter()
            .filter(|p| !p.read_only)
            .map(|p| {
                let (pid, ids) = (p.id.clone(), ids.clone());
                MenuItem::action(p.name.clone(), move |_, cx| {
                    let (pid, ids) = (pid.clone(), ids.clone());
                    app::call(cx, move |b| {
                        let _ = b.playlists_add_tracks(AddToPlaylistRequest { id: pid, track_ids: ids });
                    });
                })
            })
            .collect();
        let rate = |stars: u8, ids: Vec<String>| {
            let label = if stars == 0 { "Clear rating".to_string() } else { format!("{}★", stars) };
            let mut it = MenuItem::action(label, move |_, cx| {
                let ids = ids.clone();
                app::call(cx, move |b| {
                    for id in ids {
                        let _ = b.rating_set(SetRatingRequest { id, kind: StarTargetType::Track, rating: stars });
                    }
                });
            });
            if stars == 1 {
                it = it.danger().hint("1★ tracks are purged by the NAS nightly.");
            }
            it
        };
        let ids_l = ids.clone();
        let like = MenuItem::action(format!("{}{suffix}", if all_starred { "Unlike" } else { "Like" }), move |_, cx| {
            let ids = ids_l.clone();
            app::call(cx, move |b| {
                for id in ids {
                    let req = StarRequest { id, kind: StarTargetType::Track };
                    let _ = if all_starred { b.star_remove(req) } else { b.star_add(req) };
                }
            });
        })
        .icon(if all_starred { "heart" } else { "heart-filled" });

        let mut items = vec![
            play_now,
            enqueue(format!("Play next{suffix}"), EnqueuePosition::Next, ids.clone(), "skip-fwd"),
            enqueue(format!("Add to queue{suffix}"), EnqueuePosition::End, ids.clone(), "queue"),
            MenuItem::separator(),
            MenuItem::submenu(format!("Add to playlist{suffix}"), playlists).icon("playlist"),
            like,
            MenuItem::submenu(format!("Rate{suffix}"), vec![rate(0, ids.clone()), rate(5, ids.clone()), rate(4, ids.clone()), rate(3, ids.clone()), rate(2, ids.clone()), rate(1, ids.clone())]),
        ];
        if n == 1 {
            items.push(MenuItem::separator());
            if let Some(al) = first.album_id.clone() {
                let this = this.clone();
                items.push(
                    MenuItem::action("Go to album", move |_, cx| {
                        if let Some(t) = this.upgrade() {
                            let al = al.clone();
                            t.update(cx, |_, cx| cx.emit(TracksEvent::Navigate(Route::Album(al))));
                        }
                    })
                    .icon("album"),
                );
            }
            if let Some(ar) = first.artist_id.clone() {
                let this = this.clone();
                items.push(
                    MenuItem::action("Go to artist", move |_, cx| {
                        if let Some(t) = this.upgrade() {
                            let ar = ar.clone();
                            t.update(cx, |_, cx| cx.emit(TracksEvent::Navigate(Route::Artist(ar))));
                        }
                    })
                    .icon("artist"),
                );
            }
            let text = format!("{} – {}", first.artist, first.title);
            items.push(MenuItem::action("Copy \"Artist – Title\"", move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))));
        }
        items
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

    fn page_rows(&self) -> isize {
        (self.vis.end.saturating_sub(self.vis.start)).saturating_sub(1).max(1) as isize
    }

    // ------------------------------------------------------------ render pieces
    fn cols(&self, window: &Window) -> Columns {
        let vw = f32::from(window.viewport_size().width);
        let rail = if Shell::rail_docked(vw, &Route::Tracks) { 340.0 } else { 0.0 };
        // container = table panel content box: viewport - sidebar - rail - 2*16 page padding - 2*1 panel border
        visible_columns(vw - Shell::sidebar_width(vw) - rail - 32.0 - 2.0)
    }

    fn header_cell(&self, id: &'static str, label: &'static str, field: TrackSortField, right: bool, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let th = self.th;
        let arrow = if self.sort == field { if self.dir == SortDir::Desc { " ↓" } else { " ↑" } } else { "" };
        let text = format!("{label}{arrow}");
        let active = self.sort == field;
        div()
            .id(id)
            .flex()
            .flex_row()
            .when(right, |d| d.justify_end())
            .cursor(CursorStyle::PointingHand)
            .text_color(if active { th.text } else { th.muted })
            .hover(move |s| s.text_color(th.text))
            .child(tracked(&text, LABEL_CAPS, th.body_font, if active { th.text } else { th.muted }, window))
            .on_click(cx.listener(move |this, _, _, cx| this.on_sort(field, cx)))
    }

    fn head_row(&self, cols: Columns, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let th = self.th;
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .pl(px(4.))
            .pr(px(8.))
            .py(px(4.))
            .flex_none()
            .border_b_1()
            .border_color(th.border)
            .child(div().w(px(36.)).flex_none())
            .child(flex_cell(3.0).child(self.header_cell("h-title", "Title", TrackSortField::Title, false, window, cx)))
            .child(flex_cell(2.0).child(self.header_cell("h-artist", "Artist", TrackSortField::Artist, false, window, cx)))
            .child(flex_cell(2.0).child(self.header_cell("h-album", "Album", TrackSortField::Album, false, window, cx)))
            .when(cols.year, |d| d.child(div().w(px(56.)).flex_none().child(self.header_cell("h-year", "Year", TrackSortField::Year, true, window, cx))))
            .when(cols.added, |d| d.child(div().w(px(88.)).flex_none().child(self.header_cell("h-added", "Added", TrackSortField::DateAdded, true, window, cx))))
            .child(div().w(px(96.)).flex_none().child(self.header_cell("h-format", "Format", TrackSortField::Format, true, window, cx)))
            .child(div().w(px(48.)).flex_none().child(self.header_cell("h-time", "Time", TrackSortField::Duration, true, window, cx)))
            .child(div().w(px(24.)).flex_none())
            .child(div().w(px(24.)).flex_none())
    }

    fn row(&mut self, ix: usize, cols: Columns, cx: &mut Context<Self>) -> AnyElement {
        let th = self.th;
        let Some(t) = self.pager.get(ix).cloned() else {
            self.request_page(ix / PAGE, cx);
            return div().id(("trow", ix)).h(px(ROW_HEIGHT)).w_full().bg(th.mid).into_any_element();
        };
        let active = self.current_id.as_deref() == Some(t.id.as_str());
        let selected = self.sel.contains(ix);
        let starred = t.starred.is_some();
        let id_play = t.id.clone();
        let idx_cell = div().w(px(36.)).flex_none().flex().items_center().justify_center().gap(px(2.)).text_color(th.muted).child(if active {
            now_playing_bars(&th, self.playing).into_any_element()
        } else {
            ts(&th, MONO_CODE).child(SharedString::from(format!("{:02}", ix + 1))).into_any_element()
        });
        let mut cell = |d: Div| ellipsis(d.text_color(th.muted));
        let artist = cell(flex_cell(2.0).font_family(MONO).text_size(px(12.)).line_height(px(16.)).font_weight(FontWeight::MEDIUM)).child(SharedString::from(t.artist.clone()));
        let album = cell(flex_cell(2.0).font_family(MONO).text_size(px(12.)).line_height(px(16.)).font_weight(FontWeight::MEDIUM)).child(SharedString::from(t.album.clone().unwrap_or_else(|| "—".into())));
        let right = |w: f32, s: String| div().w(px(w)).flex_none().text_right().font_family(MONO).text_size(px(12.)).line_height(px(16.)).font_weight(FontWeight::MEDIUM).text_color(th.muted).whitespace_nowrap().child(SharedString::from(s));

        div()
            .id(("trow", ix))
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .pl(px(4.))
            .pr(px(8.))
            .h(px(ROW_HEIGHT))
            .w_full()
            .border_b_1()
            .border_color(th.mid)
            .cursor(CursorStyle::Arrow)
            .when(active || selected, |d| d.bg(th.mid))
            .when(!active && !selected, |d| d.hover(move |s| s.bg(th.mid)))
            .child(idx_cell)
            .child(ellipsis(flex_cell(3.0).text_size(px(13.)).line_height(px(18.)).text_color(if active { th.accent_fg } else { th.text })).child(SharedString::from(t.title.clone())))
            .child(artist)
            .child(album)
            .when(cols.year, |d| d.child(right(56., fmt::track_year(&t))))
            .when(cols.added, |d| d.child(right(88., t.date_added.as_deref().map(fmt::date_part).unwrap_or("—").to_string())))
            .child(right(96., fmt::format_track_format(&t.format)))
            .child(right(48., fmt::format_duration(Some(t.duration))))
            .child(self.star_button(ix, &t, starred, cx))
            .child(self.more_button(ix, cx))
            .when(active, |d| d.child(div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(th.accent_fg)))
            .when(selected, |d| d.child(div().absolute().top_0().left_0().size_full().border_1().border_color(th.accent_fg)))
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
                }),
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
            .child(icon_g(if starred { "heart-filled" } else { "heart" }, 14., if starred { th.accent_fg } else { th.muted }, if starred { th.accent_fg } else { th.text }))
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
            .child(icon_g("more", 14., th.muted, th.text))
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

/// The three "now playing" bars of an active row. Static on purpose (a CSS infinite
/// animation would cost one frame per vsync on this route): full when playing, a third
/// when paused, drawn at fixed heights; the state tick (4 Hz) is the only thing that changes them.
fn now_playing_bars(th: &Theme, playing: bool) -> Div {
    let hs: [f32; 3] = if playing { [5.0, 12.0, 8.0] } else { [4.0, 4.0, 4.0] };
    div().flex().flex_row().items_end().gap(px(2.)).h(px(12.)).children(hs.iter().map(|h| div().w(px(2.)).h(px(*h)).bg(th.accent)))
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

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .flex_none()
            .child(tracked("Tracks", HEADLINE_MD, th.body_font, th.text, window))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .child(ts(&th, MONO_CODE).text_color(th.muted).child(count))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.))
                            .w(px(240.))
                            .h(px(32.))
                            .px(px(8.))
                            .bg(th.low)
                            .border_1()
                            .border_color(if filter_focused { th.focus } else { th.border })
                            .child(icon("search", 14., th.muted))
                            .child(div().flex_1().min_w_0().child(self.filter_input.clone())),
                    )
                    .child(button("play-all", th, Variant::Secondary, "Play All", Some("play"), total == 0).on_click(cx.listener(|this, _, _, cx| this.play_all(false, cx))))
                    .child(button("shuffle-all", th, Variant::Primary, "Shuffle All", Some("shuffle"), total == 0).on_click(cx.listener(|this, _, _, cx| this.play_all(true, cx)))),
            );

        let list = uniform_list(
            "tracks-list",
            total,
            cx.processor(move |this, range: Range<usize>, _window, cx| {
                this.vis = range.clone();
                range.map(|ix| this.row(ix, cols, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.list.clone())
        .size_full();

        let body: AnyElement = if total == 0 && self.pager.has_page(0) {
            ts(&th, BODY_MD)
                .p(px(12.))
                .text_color(th.muted)
                .child(if self.filter.trim().is_empty() { "No tracks in your library yet." } else { "No tracks match your filter." })
                .into_any_element()
        } else {
            div()
                .id("tracks-body")
                .key_context("TrackList")
                .track_focus(&self.focus)
                .on_action(cx.listener(|this, _: &SelDown, _, cx| this.key_move(1, false, cx)))
                .on_action(cx.listener(|this, _: &SelUp, _, cx| this.key_move(-1, false, cx)))
                .on_action(cx.listener(|this, _: &crate::views::shell::SelDownExtend, _, cx| this.key_move(1, true, cx)))
                .on_action(cx.listener(|this, _: &crate::views::shell::SelUpExtend, _, cx| this.key_move(-1, true, cx)))
                .on_action(cx.listener(|this, _: &PgDown, _, cx| {
                    let n = this.page_rows();
                    this.key_move(n, false, cx)
                }))
                .on_action(cx.listener(|this, _: &PgUp, _, cx| {
                    let n = this.page_rows();
                    this.key_move(-n, false, cx)
                }))
                .on_action(cx.listener(|this, _: &ListHome, _, cx| this.key_move(isize::MIN / 2, false, cx)))
                .on_action(cx.listener(|this, _: &ListEnd, _, cx| this.key_move(isize::MAX / 2, false, cx)))
                .on_action(cx.listener(|this, _: &SelectAllRows, _, cx| {
                    let total = this.pager.total;
                    this.sel.select_all(total);
                    cx.notify();
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
                .child(list)
                .child(scrollbar::scrollbar("tracks-scrollbar", th, &self.list, &self.sb))
                .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(16.))
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .border_1()
                    .border_color(th.border)
                    .bg(th.low)
                    .child(self.head_row(cols, window, cx))
                    .child(div().flex_1().min_h_0().child(body)),
            )
    }
}
