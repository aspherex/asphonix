//! `AlbumDetail.svelte`: header (cover, name, artist link, stats, play/shuffle/star/menu), the 640 px "Track Vectors"
//! table (`TrackRow` rows, multi-select, keyboard, context menu) and the 280 px telemetry panel with the artist's other albums.
use super::cards::{anchor_cell, open_menu_at};
use super::entity_link::set_current;
use super::fmtx;
use super::parts::{PageScroll, art_slot, measure, text_link};
use super::rows::{self, RowCb, RowOpts, RowState};
use super::{Ctx, LibEvent, menus, navigate};
use crate::app::{self, PlayerEvent};
use crate::columns::visible_columns;
use crate::metrics::{library as m, tracks as tr};
use crate::probe::pid;
use crate::router::Route;
use crate::text::{natural_width, txt};
use crate::theme::{MONO, Theme};
use crate::tracking::{BODY_MD, BODY_SM, HEADLINE_LG, HEADLINE_SM, LABEL_CAPS, MONO_CODE, MONO_DATA};
use crate::views::shell::{EnqueueSelected, OpenMenuKey, PlaySelected, SelDown, SelUp};
use crate::widgets::context_menu::MenuItem;
use crate::widgets::scrollbar::{self, ScrollbarHandle};
use crate::widgets::{hard_shadow, icon};
use gpui::{
    AnyElement, Bounds, ClickEvent, Context, CursorStyle, FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent, Pixels, Point, Render, ScrollStrategy, Subscription,
    UniformListScrollHandle, Window, div, point, prelude::*, px, uniform_list,
};
use sonix_api::*;
use std::cell::Cell;
use std::collections::BTreeSet;
use std::ops::Range;
use std::rc::Rc;

/// `TrackSelection` (set + anchor) plus the keyboard cursor `selectedIndex` of `AlbumDetail.svelte` (two separate things there).
#[derive(Default)]
pub struct DetailSelection {
    set: BTreeSet<usize>,
    anchor: Option<usize>,
    pub cursor: Option<usize>,
}

impl DetailSelection {
    pub fn has(&self, i: usize) -> bool {
        self.set.contains(&i)
    }
    pub fn len(&self) -> usize {
        self.set.len()
    }
    /// `handleClick` + `selectRow`: shift extends from the anchor, ctrl toggles, plain selects just this row.
    pub fn click(&mut self, i: usize, ctrl: bool, shift: bool) {
        if shift {
            let from = self.anchor.unwrap_or(i);
            self.set.extend(from.min(i)..=from.max(i));
        } else if ctrl {
            if !self.set.remove(&i) {
                self.set.insert(i);
            }
            self.anchor = Some(i);
        } else {
            self.set = BTreeSet::from([i]);
            self.anchor = Some(i);
        }
        self.cursor = Some(i);
    }
    pub fn select_all(&mut self, n: usize) {
        self.set = (0..n).collect();
    }
    /// `forContextMenu`: keep the selection when `i` is in it, else just `i`.
    pub fn for_context_menu(&mut self, i: usize) -> Vec<usize> {
        if !self.set.contains(&i) {
            self.set = BTreeSet::from([i]);
            self.anchor = Some(i);
        }
        self.cursor = Some(i);
        self.set.iter().copied().collect()
    }
}

/// Ctrl/Cmd+A in a `[data-tracklist]` list (`trackListKeydown`'s `selectAll`): one action for every list.
use crate::views::shell::SelectAll as SelectAllRows;

pub struct AlbumDetailView {
    ctx: Ctx,
    id: String,
    detail: Option<AlbumDetail>,
    other: Vec<Album>,
    loading: bool,
    error: bool,
    sel: DetailSelection,
    list: UniformListScrollHandle,
    sb: ScrollbarHandle,
    page: PageScroll,
    panel_w: Rc<Cell<f32>>,
    row_anchor: Rc<Cell<Bounds<Pixels>>>,
    focus: FocusHandle,
    current: Option<String>,
    playing: bool,
    vis: Range<usize>,
    _subs: Vec<Subscription>,
}

impl AlbumDetailView {
    pub fn new(ctx: Ctx, id: String, cx: &mut Context<Self>) -> Self {
        set_current(&Route::Album(id.clone()));
        let mut subs = Vec::new();
        subs.push(cx.subscribe(&ctx.player.clone(), |this: &mut Self, p, _: &PlayerEvent, cx| {
            let p = p.read(cx);
            this.current = p.current_id().map(str::to_string);
            this.playing = p.playing();
            cx.notify();
        }));
        subs.push(cx.subscribe(&ctx.nav.clone(), |this: &mut Self, _, ev: &LibEvent, cx| {
            if matches!(ev, LibEvent::Refresh) {
                this.load(false, cx);
            }
        }));
        let p = ctx.player.read(cx);
        let (current, playing) = (p.current_id().map(str::to_string), p.playing());
        let mut this = Self {
            ctx,
            id,
            detail: None,
            other: Vec::new(),
            loading: true,
            error: false,
            sel: DetailSelection::default(),
            list: UniformListScrollHandle::new(),
            sb: scrollbar::new_handle(),
            page: PageScroll::new(),
            panel_w: Rc::new(Cell::new(1000.0)),
            row_anchor: anchor_cell(),
            focus: cx.focus_handle(),
            current,
            playing,
            vis: 0..0,
            _subs: subs,
        };
        this.load(true, cx);
        this
    }

    fn load(&mut self, first: bool, cx: &mut Context<Self>) {
        let (b, id) = (self.ctx.backend.clone(), self.id.clone());
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    let d = b.browse_get_album(&id);
                    let other = d.as_ref().ok().and_then(|d| d.album.artist_id.clone()).and_then(|a| b.browse_get_artist(&a).ok()).map(|a| a.albums.into_iter().filter(|x| x.id != id).collect::<Vec<_>>());
                    (d, other)
                })
                .await;
            let _ = this.update(cx, |t, cx| {
                match r.0 {
                    Ok(d) => {
                        t.detail = Some(d);
                        t.error = false;
                    }
                    Err(_) => t.error = true,
                }
                t.other = r.1.unwrap_or_default();
                t.loading = false;
                let _ = first;
                cx.notify();
            });
        })
        .detach();
    }

    fn tracks(&self) -> &[Track] {
        self.detail.as_ref().map(|d| d.tracks.as_slice()).unwrap_or(&[])
    }

    fn play_from(&self, ix: usize, cx: &mut Context<Self>) {
        let ids: Vec<String> = self.tracks().iter().map(|t| t.id.clone()).collect();
        menus::play_tracks(cx, ids, ix);
    }

    fn toggle_album_star(&mut self, cx: &mut Context<Self>) {
        if let Some(d) = &self.detail {
            menus::toggle_star(cx, d.album.id.clone(), StarTargetType::Album, d.album.starred.is_some());
        }
    }

    fn track_menu(&mut self, ix: usize, at: Point<Pixels>) -> Vec<MenuItem> {
        let indices = self.sel.for_context_menu(ix);
        let tracks: Vec<Track> = indices.iter().filter_map(|i| self.tracks().get(*i).cloned()).collect();
        if tracks.is_empty() {
            return Vec::new();
        }
        let ids: Vec<String> = self.tracks().iter().map(|t| t.id.clone()).collect();
        let start = indices[0];
        menus::track_menu(&tracks, at, move |cx| menus::play_tracks(cx, ids.clone(), start))
    }

    fn open_track_menu(&mut self, ix: usize, pos: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let items = self.track_menu(ix, pos);
        if !items.is_empty() {
            open_menu_at(&self.ctx, items, pos, window, cx);
        }
        cx.notify();
    }

    /// The "+" button: `openAddToPlaylistMenu(anchor, t.id)` (`pos` = the button's bottom-right corner).
    fn open_add_menu(&mut self, ix: usize, pos: Point<Pixels>, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.tracks().get(ix) else { return };
        crate::overlays::add_to_playlist(cx, vec![t.id.clone()], pos);
    }

    fn select_row(&mut self, ix: usize) {
        self.sel.cursor = Some(ix);
        // `VirtualList.scrollToIndex`: the minimum scroll that brings the row fully into view
        if ix < self.vis.start {
            self.list.scroll_to_item(ix, ScrollStrategy::Top);
        } else if ix + 1 >= self.vis.end {
            self.list.scroll_to_item(ix, ScrollStrategy::Bottom);
        }
    }

    fn key_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let n = self.tracks().len();
        if n == 0 {
            return;
        }
        let from = self.sel.cursor.map(|c| c as isize).unwrap_or(if delta > 0 { -1 } else { n as isize });
        self.select_row((from + delta).clamp(0, n as isize - 1) as usize);
        cx.notify();
    }


    fn row_el(&mut self, ix: usize, o: RowOpts, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = app::theme(cx);
        let Some(t) = self.tracks().get(ix).cloned() else { return div().into_any_element() };
        let active = self.current.as_deref() == Some(t.id.as_str());
        let st = RowState { ix, active, playing: self.playing && active, selected: self.sel.has(ix), anchor: (self.sel.cursor == Some(ix)).then(|| self.row_anchor.clone()) };
        let this = cx.entity().downgrade();
        let (t_star, t_add, t_more, t_play) = (this.clone(), this.clone(), this.clone(), this.clone());
        let cb = RowCb {
            star: Rc::new(move |_, cx| {
                let _ = t_star.update(cx, |t, cx| {
                    if let Some(tr) = t.tracks().get(ix) {
                        menus::toggle_star(cx, tr.id.clone(), StarTargetType::Track, tr.starred.is_some());
                    }
                });
            }),
            add: Some(Rc::new(move |pos, window, cx| {
                let _ = t_add.update(cx, |t, cx| t.open_add_menu(ix, pos, window, cx));
            })),
            more: Rc::new(move |pos, window, cx| {
                let _ = t_more.update(cx, |t, cx| t.open_track_menu(ix, pos, window, cx));
            }),
            play: Rc::new(move |_, cx| {
                let _ = t_play.update(cx, |t, cx| t.play_from(ix, cx));
            }),
        };
        let r = rows::row(&th, &t, &st, o, tr::ROW_H, Some(format!("track.row.{}", self.web_row(ix))), cb, window)
            .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                if ev.click_count() >= 2 {
                    this.play_from(ix, cx);
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
                    this.open_track_menu(ix, ev.position, window, cx);
                    cx.stop_propagation();
                }),
            );
        pid(format!("track.row.{}", self.web_row(ix)), r).into_any_element()
    }

    /// Index of the row among the web's rendered window (`VirtualList` renders from `floor(scrollTop / 36) - 6`).
    fn web_row(&self, ix: usize) -> usize {
        let top = -f32::from(self.list.0.borrow().base_handle.offset().y);
        let start = ((top / tr::ROW_H).floor() as i64 - 6).max(0) as usize;
        ix.saturating_sub(start)
    }

    fn header(&mut self, th: &Theme, d: &AlbumDetail, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = *th;
        let album = &d.album;
        let fsum = fmtx::summarize_formats(&d.tracks);
        let id = self.id.clone();
        let (id2, artist_id, genre) = (self.id.clone(), album.artist_id.clone(), album.genre.clone());
        let art = pid("album.art", art_slot(&th, &self.ctx.art, &self.ctx.backend, album.cover_art.as_deref(), m::ALBUM_ART, true, 320, "album", 40.0, window, cx));
        let sep = |th: &Theme| txt(th, MONO_DATA, "·").color(th.muted);
        let mut stats = div().flex().flex_row().gap(px(crate::metrics::space::XS)).child(txt(&th, MONO_DATA, album.year.map(|y| y.to_string()).unwrap_or_else(|| "—".into())).color(th.muted));
        stats = stats.child(sep(&th)).child(txt(&th, MONO_DATA, format!("{} TRACKS", album.song_count)).color(th.muted)).child(sep(&th)).child(txt(&th, MONO_DATA, fmtx::total_duration(album.duration)).color(th.muted));
        if let Some(g) = genre.clone() {
            let g2 = g.clone();
            stats = stats.child(sep(&th)).child(text_link(&th, "stat-genre", g, MONO_DATA, (th.muted, th.accent_fg), true, move |_, cx| navigate(cx, Route::Genre(g2.clone()))));
        }
        stats = stats.child(sep(&th)).child(txt(&th, MONO_DATA, fsum).color(th.muted));

        let this = cx.entity().downgrade();
        let (t_shuf, t_star) = (this.clone(), this.clone());
        let anchor = anchor_cell();
        let anchor2 = anchor.clone();
        let ctx = self.ctx.clone();
        let album_c = album.clone();
        let play_btn = div()
            .id("album-play")
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(crate::metrics::space::SM))
            .h(px(m::BTN_H))
            .px(px(crate::metrics::space::LG))
            .flex_none()
            .bg(th.accent)
            .text_color(th.accent_text)
            .shadow(hard_shadow(&th, 2.0))
            .cursor(CursorStyle::PointingHand)
            .hover(move |s| s.bg(th.accent_hover))
            .active(|s| s.top(px(2.)).left(px(2.)).shadow(vec![]))
            .child(icon("play", 16.0, th.accent_text))
            .child(txt(&th, HEADLINE_SM, "PLAY ALBUM").color(th.accent_text))
            .on_click(move |_, _, cx| menus::play_album(cx, id.clone(), false));
        let secondary = div()
            .id("album-shuffle")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(crate::metrics::space::SM))
            .h(px(m::BTN_H))
            .px(px(crate::metrics::space::MD))
            .flex_none()
            .bg(th.mid)
            .border_1()
            .border_color(th.border)
            .cursor(CursorStyle::PointingHand)
            .hover(move |s| s.border_color(th.focus))
            .child(icon("shuffle", 15.0, th.text))
            .child(txt(&th, BODY_MD, "SHUFFLE").color(th.text))
            .on_click(move |_, _, cx| {
                let _ = t_shuf.update(cx, |t, cx| menus::play_album(cx, t.id.clone(), true));
            });
        let icon_btn = |id: &'static str, name: &'static str, on: bool| {
            div()
                .id(id)
                .relative()
                .size(px(m::BTN_H))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .bg(th.mid)
                .border_1()
                .border_color(th.border)
                .cursor(CursorStyle::PointingHand)
                .hover(move |s| s.border_color(th.focus))
                .child(icon(name, 15.0, if on { th.accent_fg } else { th.text }))
        };
        let starred = album.starred.is_some();
        let star_btn = icon_btn("album-star", if starred { "star-filled" } else { "star" }, starred).on_click(move |_, _, cx| {
            let _ = t_star.update(cx, |t, cx| t.toggle_album_star(cx));
        });
        let more_btn = icon_btn("album-more", "more", false)
            .child(gpui::canvas(move |b, _, _| anchor.set(b), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
            .on_click(move |_: &ClickEvent, window, cx| {
                let b = anchor2.get();
                let pos = point(b.origin.x, b.origin.y + b.size.height + px(4.));
                open_menu_at(&ctx, menus::album_menu(&album_c, pos), pos, window, cx);
            });
        let _ = id2;
        let actions = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(crate::metrics::space::SM))
            .mt(px(crate::metrics::space::SM))
            .child(pid("album.btn.play", play_btn))
            .child(pid("album.btn.shuffle", secondary))
            .child(pid("album.btn.star", star_btn))
            .child(pid("album.btn.more", more_btn));
        let info = div()
            .flex()
            .flex_col()
            .gap(px(crate::metrics::space::XS))
            .justify_end()
            .min_w_0()
            .child(pid("album.name", div().flex().child(txt(&th, HEADLINE_LG, album.name.clone()).color(th.text))))
            .child(div().flex().flex_row().child(pid(
                "album.artist",
                text_link(&th, "artist-link", album.artist.clone(), HEADLINE_SM, (th.accent_fg, th.accent_fg), false, move |_, cx| {
                    if let Some(a) = artist_id.clone() {
                        navigate(cx, Route::Artist(a));
                    }
                }),
            )))
            .child(pid("album.stats", stats))
            .child(pid("album.actions", actions));
        pid("album.header", div().flex().flex_row().gap(px(m::PAGE_PAD)).flex_none().child(art).child(pid("album.info", info))).into_any_element()
    }

    fn tracks_panel(&mut self, th: &Theme, o: RowOpts, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = *th;
        let total = self.tracks().len();
        let selecting = self.sel.len() > 1;
        let count = if selecting { format!("{} SELECTED", self.sel.len()) } else { format!("{total} TOTAL") };
        let head = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .flex_none()
            .px(px(crate::metrics::space::MD))
            .py(px(crate::metrics::space::SM))
            .bg(th.mid)
            .border_b_1()
            .border_color(th.border)
            .child(txt(&th, LABEL_CAPS, "Track Vectors").color(th.muted))
            .child(txt(&th, MONO_CODE, count).color(th.muted));
        let overflow = super::grid::list_overflows(&self.list).unwrap_or(false);
        let list = uniform_list(
            "album-tracks",
            total,
            cx.processor(move |this: &mut Self, range: Range<usize>, window, cx| {
                this.vis = range.clone();
                range.map(|ix| this.row_el(ix, o, window, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.list.clone())
        .size_full();
        let body = div()
            .id("album-body")
            .key_context("TrackList")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelDown, _, cx| this.key_move(1, cx)))
            .on_action(cx.listener(|this, _: &SelUp, _, cx| this.key_move(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectAllRows, _, cx| {
                let n = this.tracks().len();
                this.sel.select_all(n);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &PlaySelected, _, cx| {
                if let Some(c) = this.sel.cursor.filter(|c| *c < this.tracks().len()) {
                    this.play_from(c, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &EnqueueSelected, _, cx| {
                if let Some(t) = this.sel.cursor.and_then(|c| this.tracks().get(c)) {
                    menus::enqueue(cx, vec![t.id.clone()], EnqueuePosition::End);
                }
            }))
            .on_action(cx.listener(|this, _: &OpenMenuKey, window, cx| {
                if let Some(c) = this.sel.cursor {
                    let b = this.row_anchor.get();
                    let pos = point(b.origin.x, b.origin.y + b.size.height + px(4.));
                    this.open_track_menu(c, pos, window, cx);
                }
            }))
            .relative()
            .size_full()
            .child(div().size_full().when(overflow, |d| d.pr(px(scrollbar::WIDTH))).child(list))
            .child(scrollbar::scrollbar("album-scrollbar", th, &self.list, &self.sb))
            .child(crate::widgets::smooth::drive(window, crate::widgets::smooth::Scrollable::list(&self.list)));
        let _ = window;
        pid(
            "album.panel",
            div()
                .relative()
                .flex_1()
                .min_w_0()
                .h(px(m::TRACKS_PANEL_H))
                .flex()
                .flex_col()
                .border_1()
                .border_color(th.border)
                .bg(th.low)
                .child(measure(&self.panel_w))
                .child(pid("album.panel.header", head))
                .child(rows::head(&th, o, "album.panel"))
                .child(div().flex_1().min_h_0().child(body)),
        )
        .into_any_element()
    }

    fn meta_panel(&mut self, th: &Theme, d: &AlbumDetail, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = *th;
        let album = &d.album;
        let vw = f32::from(window.viewport_size().width);
        let w = if vw < crate::metrics::bp::COMPACT_BELOW { m::META_PANEL_W_COMPACT } else { m::META_PANEL_W };
        let fsum = fmtx::summarize_formats(&d.tracks);
        let mut rows_v: Vec<(&'static str, AnyElement)> = Vec::new();
        // `grid-template-columns: auto 1fr`: the label column is as wide as the widest label; a value too long for the rest wraps
        let mut labels = vec!["Format", "Genre", "Year", "Tracks", "Duration"];
        if album.play_count.is_some() {
            labels.push("Plays");
        }
        if album.created.is_some() {
            labels.push("Added");
        }
        let dt_w = labels.iter().map(|l| natural_width(window, l, MONO, MONO_DATA.size, MONO_DATA.weight, MONO_DATA.tracking_px())).fold(0.0, f32::max);
        let avail = w - 2.0 - 2.0 * crate::metrics::space::MD - dt_w - crate::metrics::space::MD;
        let family = MONO;
        let val = |s: String| -> AnyElement {
            if natural_width(window, &s, family, MONO_DATA.size, MONO_DATA.weight, MONO_DATA.tracking_px()) <= avail {
                txt(&th, MONO_DATA, s).color(th.text).into_any_element()
            } else {
                div().w_full().text_right().font_family(MONO).font_weight(gpui::FontWeight(MONO_DATA.weight)).text_size(px(MONO_DATA.size)).line_height(px(MONO_DATA.line)).text_color(th.text).child(gpui::SharedString::from(s)).into_any_element()
            }
        };
        rows_v.push(("Format", val(fsum)));
        rows_v.push((
            "Genre",
            match album.genre.clone() {
                Some(g) => {
                    let g2 = g.clone();
                    text_link(&th, "meta-genre", g, MONO_DATA, (th.text, th.accent_fg), true, move |_, cx| navigate(cx, Route::Genre(g2.clone()))).into_any_element()
                }
                None => val("—".into()),
            },
        ));
        rows_v.push(("Year", val(album.year.map(|y| y.to_string()).unwrap_or_else(|| "—".into()))));
        rows_v.push(("Tracks", val(album.song_count.to_string())));
        rows_v.push(("Duration", val(fmtx::total_duration(album.duration))));
        if let Some(p) = album.play_count {
            rows_v.push(("Plays", val(p.to_string())));
        }
        if let Some(c) = &album.created {
            rows_v.push(("Added", val(fmtx::locale_date(c))));
        }
        let mut dl = div().flex().flex_col().gap(px(crate::metrics::space::XS));
        for (label, v) in rows_v {
            dl = dl.child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(crate::metrics::space::MD))
                    .child(div().w(px(dt_w)).flex_none().child(txt(&th, MONO_DATA, label).color(th.muted)))
                    .child(div().flex_1().min_w_0().flex().justify_end().child(v)),
            );
        }
        let block = |children: Vec<AnyElement>| {
            let mut b = div().flex().flex_col().gap(px(crate::metrics::space::SM)).p(px(crate::metrics::space::MD)).bg(th.low).border_1().border_color(th.border);
            for c in children {
                b = b.child(c);
            }
            b
        };
        let mut panel = div().w(px(w)).flex_none().flex().flex_col().gap(px(crate::metrics::space::MD)).child(pid(
            "album.meta.block.0",
            block(vec![div().flex().child(txt(&th, LABEL_CAPS, "Album Telemetry").color(th.muted)).into_any_element(), dl.into_any_element()]),
        ));
        if !self.other.is_empty() {
            let mut list = div().flex().flex_col().gap(px(crate::metrics::space::XS));
            let others: Vec<Album> = self.other.iter().take(6).cloned().collect();
            for (i, o) in others.into_iter().enumerate() {
                let thumb = art_slot(&th, &self.ctx.art, &self.ctx.backend, o.cover_art.as_deref(), 40.0, false, 64, "album", 14.0, window, cx);
                let oid = o.id.clone();
                list = list.child(
                    div()
                        .id(("other-album", i))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(crate::metrics::space::SM))
                        .w_full()
                        .p(px(crate::metrics::space::XS))
                        .cursor(CursorStyle::PointingHand)
                        .hover(move |s| s.bg(th.mid))
                        .child(thumb)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .child(div().flex().min_w_0().child(txt(&th, BODY_SM, o.name.clone()).color(th.text).ellipsis()))
                                .child(div().flex().child(txt(&th, MONO_CODE, format!("{} · {} TRACKS", o.year.map(|y| y.to_string()).unwrap_or_else(|| "—".into()), o.song_count)).color(th.muted))),
                        )
                        .on_click(move |_, _, cx| navigate(cx, Route::Album(oid.clone()))),
                );
            }
            panel = panel.child(pid(
                "album.meta.block.1",
                block(vec![div().flex().child(txt(&th, LABEL_CAPS, format!("{} in Vault", album.artist)).color(th.muted)).into_any_element(), list.into_any_element()]),
            ));
        }
        pid("album.meta", panel).into_any_element()
    }
}

impl Focusable for AlbumDetailView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AlbumDetailView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = app::theme(cx);
        let content: AnyElement = if self.loading {
            div().p(px(crate::metrics::space::XL)).child(txt(&th, MONO_CODE, "LOADING ALBUM…").color(th.muted)).into_any_element()
        } else if let Some(d) = self.detail.clone() {
            let o = RowOpts { show_artist: true, cols: visible_columns(self.panel_w.get()), actions: 3 };
            let header = self.header(&th, &d, window, cx);
            let panel = self.tracks_panel(&th, o, window, cx);
            let meta = self.meta_panel(&th, &d, window, cx);
            div()
                .w_full()
                .flex_none()
                .flex()
                .flex_col()
                .p(px(m::PAGE_PAD))
                .gap(px(m::PAGE_PAD))
                .child(header)
                .child(div().flex().flex_row().gap(px(m::PAGE_PAD)).child(panel).child(meta))
                .into_any_element()
        } else {
            div().p(px(crate::metrics::space::XL)).child(txt(&th, MONO_CODE, "ALBUM UNAVAILABLE.").color(th.clip)).into_any_element()
        };
        self.page.wrap(window, &th, "album-scroll", content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_follows_track_selection_svelte() {
        let mut s = DetailSelection::default();
        s.click(2, false, false);
        s.click(4, true, false);
        assert!(s.has(2) && s.has(4) && s.len() == 2);
        s.click(6, false, true); // shift extends from the anchor (4) and keeps the rest
        assert!(s.has(2) && !s.has(3) && (4..=6).all(|i| s.has(i)));
        s.click(5, true, false); // ctrl toggles
        assert!(!s.has(5));
        assert_eq!(s.for_context_menu(9), vec![9]);
        s.select_all(3);
        assert_eq!(s.for_context_menu(1), vec![0, 1, 2], "inside the selection: keep it");
    }
}
