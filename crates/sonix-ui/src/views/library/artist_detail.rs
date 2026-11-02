//! `ArtistDetail.svelte`: header (cover, name, stats, play discography / flow / star / menu), the network-backed
//! extras (Popular or Most played tracks, About with the 4-line bio clamp), the Core Releases grid and Fans also like.
//! The local part renders at once; top tracks and info arrive later (the mock waits 450 / 700 ms) behind skeletons.
use super::cards::{CardItem, anchor_cell, card, open_menu_at};
use super::entity_link::set_current;
use super::fmtx;
use super::parts::{PageScroll, art_slot, auto_fill, measure, text_link};
use super::rows::{self, RowCb, RowOpts, RowState};
use super::{Ctx, LibEvent, menus};
use crate::app::{self, PlayerEvent};
use crate::columns::visible_columns;
use crate::metrics::library as m;
use crate::probe::pid;
use crate::router::Route;
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{BODY_MD, HEADLINE_LG, HEADLINE_SM, LABEL_CAPS, MONO_CODE, MONO_DATA};
use crate::views::shell::{EnqueueSelected, OpenMenuKey, PlaySelected, SelDown, SelUp};
use crate::widgets::{hard_shadow, icon};
use gpui::{
    AnyElement, Bounds, ClickEvent, Context, CursorStyle, FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent, Pixels, Point, Render, SharedString, Subscription, Window, div,
    point, prelude::*, px,
};
use sonix_api::*;
use std::cell::Cell;
use std::rc::Rc;

pub struct ArtistDetailView {
    ctx: Ctx,
    id: String,
    detail: Option<ArtistDetail>,
    loading: bool,
    error: bool,
    top: Option<ArtistTopTracks>,
    top_pending: bool,
    info: Option<ArtistInfo>,
    info_pending: bool,
    bio_expanded: bool,
    selected_top: Option<usize>,
    /// the FLOW button's bounds (its menu anchors under it)
    flow_btn: Rc<Cell<Bounds<Pixels>>>,
    page: PageScroll,
    page_w: Rc<Cell<f32>>,
    top_w: Rc<Cell<f32>>,
    bio_w: Rc<Cell<f32>>,
    row_anchor: Rc<Cell<Bounds<Pixels>>>,
    focus: FocusHandle,
    current: Option<String>,
    playing: bool,
    _subs: Vec<Subscription>,
}

impl ArtistDetailView {
    pub fn new(ctx: Ctx, id: String, cx: &mut Context<Self>) -> Self {
        set_current(&Route::Artist(id.clone()));
        let mut subs = Vec::new();
        subs.push(cx.subscribe(&ctx.player.clone(), |this: &mut Self, p, _: &PlayerEvent, cx| {
            let p = p.read(cx);
            this.current = p.current_id().map(str::to_string);
            this.playing = p.playing();
            cx.notify();
        }));
        subs.push(cx.subscribe(&ctx.nav.clone(), |this: &mut Self, _, ev: &LibEvent, cx| {
            if matches!(ev, LibEvent::Refresh) {
                this.load_local(cx);
            }
        }));
        let p = ctx.player.read(cx);
        let (current, playing) = (p.current_id().map(str::to_string), p.playing());
        let mut this = Self {
            ctx,
            id,
            detail: None,
            loading: true,
            error: false,
            top: None,
            top_pending: true,
            info: None,
            info_pending: true,
            bio_expanded: false,
            selected_top: None,
            flow_btn: Rc::new(Cell::new(Bounds::default())),
            page: PageScroll::new(),
            page_w: Rc::new(Cell::new(0.0)),
            top_w: Rc::new(Cell::new(1000.0)),
            bio_w: Rc::new(Cell::new(0.0)),
            row_anchor: anchor_cell(),
            focus: cx.focus_handle().tab_stop(true),
            current,
            playing,
            _subs: subs,
        };
        this.load_local(cx);
        this.load_extras(cx);
        this
    }

    fn load_local(&mut self, cx: &mut Context<Self>) {
        let (b, id) = (self.ctx.backend.clone(), self.id.clone());
        cx.spawn(async move |this, cx| {
            let d = cx.background_executor().spawn(async move { b.browse_get_artist(&id) }).await;
            let _ = this.update(cx, |t, cx| {
                match d {
                    Ok(d) => {
                        t.detail = Some(d);
                        t.error = false;
                    }
                    Err(_) => t.error = true,
                }
                t.loading = false;
                cx.notify();
            });
        })
        .detach();
    }

    /// Popular tracks and the biography are independent calls that never block the local page.
    fn load_extras(&mut self, cx: &mut Context<Self>) {
        let (b, id) = (self.ctx.backend.clone(), self.id.clone());
        let (b2, id2) = (b.clone(), id.clone());
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { b.browse_get_artist_top_tracks(&id) }).await;
            let _ = this.update(cx, |t, cx| {
                t.top = r.ok();
                t.top_pending = false;
                cx.notify();
            });
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { b2.browse_get_artist_info(&id2) }).await;
            let _ = this.update(cx, |t, cx| {
                t.info = r.ok();
                t.info_pending = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn top_items(&self) -> &[Track] {
        self.top.as_ref().map(|t| t.items.as_slice()).unwrap_or(&[])
    }

    fn play_top(&self, i: usize, cx: &mut Context<Self>) {
        menus::play_tracks(cx, self.top_items().iter().map(|t| t.id.clone()).collect(), i);
    }

    fn top_menu(&mut self, i: usize, pos: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.selected_top = Some(i);
        let Some(t) = self.top_items().get(i).cloned() else { return };
        let ids: Vec<String> = self.top_items().iter().map(|t| t.id.clone()).collect();
        let items = menus::track_menu(&[t], pos, move |cx| menus::play_tracks(cx, ids.clone(), i));
        open_menu_at(&self.ctx, items, pos, window, cx);
        cx.notify();
    }

    fn top_row(&mut self, i: usize, o: RowOpts, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = app::theme(cx);
        let t = self.top_items()[i].clone();
        let active = self.current.as_deref() == Some(t.id.as_str());
        let st = RowState { ix: i, active, playing: self.playing && active, selected: self.selected_top == Some(i), anchor: (self.selected_top == Some(i)).then(|| self.row_anchor.clone()) };
        let this = cx.entity().downgrade();
        let (t1, t2, t3) = (this.clone(), this.clone(), this.clone());
        let cb = RowCb {
            star: Rc::new(move |_, cx| {
                let _ = t1.update(cx, |t, cx| {
                    if let Some(tr) = t.top_items().get(i) {
                        menus::toggle_star(cx, tr.id.clone(), StarTargetType::Track, tr.starred.is_some());
                    }
                });
            }),
            add: None,
            more: Rc::new(move |pos, window, cx| {
                let _ = t2.update(cx, |t, cx| t.top_menu(i, pos, window, cx));
            }),
            play: Rc::new(move |_, cx| {
                let _ = t3.update(cx, |t, cx| t.play_top(i, cx));
            }),
        };
        let r = rows::row(&th, &t, &st, o, m::TOP_ROW_H, Some(format!("artist.top.{i}")), cb, window)
            .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                if ev.click_count() >= 2 {
                    this.play_top(i, cx);
                } else {
                    this.selected_top = Some(i);
                }
                window.focus(&this.focus);
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus);
                    this.top_menu(i, ev.position, window, cx);
                    cx.stop_propagation();
                }),
            );
        pid(format!("artist.top.{i}"), r).into_any_element()
    }

    fn header(&mut self, th: &Theme, d: &ArtistDetail, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = *th;
        let artist = &d.artist;
        let total_tracks: u32 = d.albums.iter().map(|a| a.song_count).sum();
        let total_duration: f64 = d.albums.iter().map(|a| a.duration).sum();
        let art = pid("artist.art", art_slot(&th, &self.ctx.art, &self.ctx.backend, artist.cover_art.as_deref(), m::ARTIST_ART, true, 160, "artist", 36.0, window, cx));
        let sep = || txt(&th, MONO_DATA, "·").color(th.muted);
        let stats = div()
            .flex()
            .flex_row()
            .gap(px(crate::metrics::space::XS))
            .child(txt(&th, MONO_DATA, format!("{} ALBUMS", artist.album_count)).color(th.muted))
            .child(sep())
            .child(txt(&th, MONO_DATA, format!("{total_tracks} TRACKS")).color(th.muted))
            .child(sep())
            .child(txt(&th, MONO_DATA, fmtx::total_duration(total_duration)).color(th.muted));
        let has_albums = !d.albums.is_empty();
        let (id, id2, id3) = (artist.id.clone(), artist.id.clone(), artist.id.clone());
        let starred = artist.starred.is_some();
        let play = div()
            .id("artist-play")
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
            .child(txt(&th, HEADLINE_SM, "PLAY DISCOGRAPHY").color(th.accent_text))
            .on_click(move |_, _, cx| {
                if has_albums {
                    menus::play_artist(cx, id.clone(), false);
                }
            });
        // `openFlowMenu(button, { kind: "artist", id }, "artist", flow => playArtist(id, { flow }))`
        let (flow_cell, flow_menu, flow_id) = (self.flow_btn.clone(), self.ctx.menu.clone(), artist.id.clone());
        let flow = div()
            .id("artist-flow")
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
            .child(icon("sliders", 15.0, th.text))
            .child(txt(&th, LABEL_CAPS, "FLOW").color(th.text))
            .relative()
            .child(crate::flow::bounds_probe(flow_cell.clone()))
            .on_click(move |_, window, cx| {
                let id = flow_id.clone();
                let play: crate::flow::Play = Rc::new(move |arc, cx| crate::playback::play_artist(cx, id.clone(), false, Some(arc)));
                crate::flow::open_menu(flow_menu.clone(), flow_cell.get(), crate::flow::pool(sonix_api::FlowPoolKind::Artist, Some(flow_id.clone()), None), "artist", play, window, cx);
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
        let star = icon_btn("artist-star", if starred { "star-filled" } else { "star" }, starred).on_click(move |_, _, cx| menus::toggle_star(cx, id2.clone(), StarTargetType::Artist, starred));
        let ctx = self.ctx.clone();
        let artist_c = artist.clone();
        let _ = id3;
        let more = icon_btn("artist-more", "more", false).on_click(move |ev: &ClickEvent, window, cx| {
            open_menu_at(&ctx, menus::artist_menu(&artist_c), ev.position(), window, cx);
        });
        let actions = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(crate::metrics::space::SM))
            .mt(px(crate::metrics::space::SM))
            .child(pid("artist.btn.play", play))
            .child(pid("artist.btn.flow", flow))
            .child(pid("artist.btn.star", star))
            .child(pid("artist.btn.more", more));
        let info = div()
            .flex()
            .flex_col()
            .gap(px(crate::metrics::space::XS))
            .justify_end()
            .min_w_0()
            .child(pid("artist.name", div().flex().child(txt(&th, HEADLINE_LG, artist.name.clone()).color(th.text))))
            .child(pid("artist.stats", stats))
            .child(pid("artist.actions", actions));
        pid("artist.header", div().flex().flex_row().gap(px(m::PAGE_PAD)).flex_none().child(art).child(info)).into_any_element()
    }

    fn section_title(th: &Theme, text: &str) -> AnyElement {
        div().flex().child(txt(th, LABEL_CAPS, text.to_string()).color(th.muted)).into_any_element()
    }

    fn popular(&mut self, th: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = *th;
        let most = self.top.as_ref().map(|t| t.source == TopSource::MostPlayed).unwrap_or(false);
        let mut title = div().flex().flex_row().items_center().child(txt(&th, LABEL_CAPS, if most { "Most played" } else { "Popular" }).color(th.muted));
        if most {
            title = title.child(div().ml(px(crate::metrics::space::SM)).opacity(0.8).child(txt(&th, MONO_CODE, "FROM YOUR LISTENING HISTORY").color(th.muted)));
        }
        let mut sec = div().flex().flex_col().gap(px(crate::metrics::space::SM)).min_w_0().child(pid("artist.top.title", title));
        if self.top_pending {
            let mut rows_v = div().flex().flex_col().border_1().border_color(th.border).bg(th.low);
            for _ in 0..10 {
                rows_v = rows_v.child(div().relative().h(px(m::TOP_ROW_H)).border_b_1().border_color(th.mid).opacity(0.5).child(div().absolute().left_0().top_0().bottom_0().w(gpui::relative(0.4)).bg(th.mid)));
            }
            sec = sec.child(pid("artist.top.list", rows_v));
        } else {
            let o = RowOpts { show_artist: false, cols: visible_columns(self.top_w.get()), actions: 2 };
            let n = self.top_items().len();
            let mut list = div()
                .id("artist-top")
                .key_context("TrackList")
                .track_focus(&self.focus)
                .on_action(cx.listener(|this, _: &SelDown, _, cx| this.move_top(1, cx)))
                .on_action(cx.listener(|this, _: &SelUp, _, cx| this.move_top(-1, cx)))
                .on_action(cx.listener(|this, _: &PlaySelected, _, cx| {
                    if let Some(i) = this.selected_top {
                        this.play_top(i, cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &EnqueueSelected, _, cx| {
                    if let Some(t) = this.selected_top.and_then(|i| this.top_items().get(i)) {
                        menus::enqueue(cx, vec![t.id.clone()], EnqueuePosition::End);
                    }
                }))
                .on_action(cx.listener(|this, _: &OpenMenuKey, window, cx| {
                    if let Some(i) = this.selected_top {
                        let b = this.row_anchor.get();
                        this.top_menu(i, point(b.origin.x, b.origin.y + b.size.height + px(4.)), window, cx);
                    }
                }))
                .relative()
                .flex()
                .flex_col()
                .border_1()
                .border_color(th.border)
                .bg(th.low)
                .child(measure(&self.top_w));
            for i in 0..n {
                list = list.child(div().h(px(m::TOP_ROW_H)).child(self.top_row(i, o, window, cx)));
            }
            sec = sec.child(pid("artist.top.list", list));
        }
        pid("artist.top", sec).into_any_element()
    }

    fn move_top(&mut self, delta: isize, cx: &mut Context<Self>) {
        let n = self.top_items().len();
        if n == 0 {
            return;
        }
        let from = self.selected_top.map(|c| c as isize).unwrap_or(if delta > 0 { -1 } else { n as isize });
        self.selected_top = Some((from + delta).clamp(0, n as isize - 1) as usize);
        cx.notify();
    }

    fn about(&mut self, th: &Theme, window: &mut Window, _cx: &mut Context<Self>) -> AnyElement {
        let th = *th;
        let mut sec = div().relative().flex().flex_col().gap(px(crate::metrics::space::SM)).min_w_0().child(Self::section_title(&th, "About")).child(measure(&self.bio_w));
        if self.info_pending {
            sec = sec.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(crate::metrics::space::XS))
                    .child(div().h(px(14.)).bg(th.mid).opacity(0.6))
                    .child(div().h(px(14.)).bg(th.mid).opacity(0.6))
                    .child(div().h(px(14.)).w(gpui::relative(0.6)).bg(th.mid).opacity(0.6)),
            );
        } else if let Some(bio) = self.info.as_ref().and_then(|i| i.biography.clone()) {
            // `line-height: 1.5` of the 13 px body font, clamped to 4 lines until expanded
            let lh = 13.0 * 1.5;
            let all = bio_lines(window, &bio, self.bio_w.get(), &th);
            let overflows = all.len() > 4;
            let expanded = self.bio_expanded;
            let spec = crate::tracking::TypeSpec { line: lh, ..BODY_MD };
            // -webkit-line-clamp: 4: the fourth line is cut to make room for "…"
            let shown: Vec<String> = if expanded || !overflows {
                all
            } else {
                let mut v: Vec<String> = all.into_iter().take(4).collect();
                let last = v.pop().unwrap_or_default();
                v.push(ellipsize(window, &last, self.bio_w.get(), &th));
                v
            };
            let mut p = div().w_full().flex().flex_col();
            for l in shown {
                p = p.child(div().flex().child(txt(&th, spec, l).color(th.muted)));
            }
            sec = sec.child(pid("artist.bio", p));
            if overflows || expanded {
                let this = _cx.entity().downgrade();
                sec = sec.child(
                    div().flex().child(text_link(&th, "bio-toggle", if expanded { "Show less" } else { "Show more" }.to_uppercase(), LABEL_CAPS, (th.accent_fg, th.text), true, move |_, cx| {
                        let _ = this.update(cx, |t, cx| {
                            t.bio_expanded = !t.bio_expanded;
                            cx.notify();
                        });
                    })),
                );
            }
        }
        pid("artist.about", sec).into_any_element()
    }
}

/// The visual lines `text` wraps to at `width` in the 13 px body font (`white-space: pre-line`: newlines break, spaces collapse at wraps).
fn bio_lines(window: &Window, text: &str, width: f32, th: &Theme) -> Vec<String> {
    if width <= 0.0 {
        return Vec::new();
    }
    let f = crate::text::css_font(th.body_font, 400.0);
    let run = gpui::TextRun { len: text.len(), font: f, color: gpui::black(), background_color: None, underline: None, strikethrough: None };
    let Ok(shaped) = window.text_system().shape_text(SharedString::from(text.to_string()), px(13.), &[run], Some(px(width)), None) else { return Vec::new() };
    let mut out = Vec::new();
    let mut para_start = 0usize;
    for line in shaped.iter() {
        let layout = &line.unwrapped_layout;
        let mut cuts: Vec<usize> = line.wrap_boundaries().iter().map(|b| layout.runs[b.run_ix].glyphs[b.glyph_ix].index).collect();
        cuts.push(layout.len);
        let mut start = 0usize;
        for c in cuts {
            let seg = &text[para_start + start..(para_start + c).min(text.len())];
            out.push(seg.trim_end().to_string());
            start = c;
        }
        para_start += layout.len + 1; // the newline between paragraphs
    }
    out
}

/// `line` cut at a glyph boundary so that it plus "…" fits `width`.
fn ellipsize(window: &Window, line: &str, width: f32, th: &Theme) -> String {
    let nat = |s: &str| crate::text::natural_width(window, s, th.body_font, 13.0, 400.0, 0.0);
    let mut chars: Vec<char> = line.chars().collect();
    while !chars.is_empty() && nat(&format!("{}…", chars.iter().collect::<String>())) > width {
        chars.pop();
    }
    format!("{}…", chars.into_iter().collect::<String>())
}

impl Focusable for ArtistDetailView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ArtistDetailView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = app::theme(cx);
        let content: AnyElement = if self.loading {
            div().p(px(crate::metrics::space::XL)).child(txt(&th, MONO_CODE, "LOADING ARTIST…").color(th.muted)).into_any_element()
        } else if let Some(d) = self.detail.clone() {
            let vw = f32::from(window.viewport_size().width);
            let header = self.header(&th, &d, window, cx);
            let mut page = div().relative().w_full().flex_none().flex().flex_col().p(px(m::PAGE_PAD)).gap(px(crate::metrics::space::XL)).child(measure(&self.page_w)).child(header);
            let content_w = (self.page_w.get() - 2.0 * m::PAGE_PAD).max(1.0);
            let show_top = self.top_pending || !self.top_items().is_empty();
            let show_about = self.info_pending || self.info.as_ref().is_some_and(|i| i.biography.is_some());
            if show_top || show_about {
                // `.extras { grid-template-columns: minmax(0, 1.4fr) minmax(0, 1fr); gap: lg }`, one column below 1100 px
                let stacked = vw < m::EXTRAS_STACK_BELOW;
                let mut ex = div().flex().items_start().gap(px(m::PAGE_PAD));
                ex = if stacked { ex.flex_col() } else { ex.flex_row() };
                let col = |el: AnyElement, grow: f32| {
                    let mut d = div().min_w_0();
                    if stacked {
                        d = d.w_full();
                    } else {
                        let s = d.style();
                        s.flex_grow = Some(grow);
                        s.flex_shrink = Some(1.0);
                        s.flex_basis = Some(px(0.).into());
                    }
                    d.child(el)
                };
                if show_top {
                    let top = self.popular(&th, window, cx);
                    ex = ex.child(col(top, 1.4));
                }
                if show_about {
                    let about = self.about(&th, window, cx);
                    ex = ex.child(col(about, 1.0));
                }
                page = page.child(pid("artist.extras", ex));
            }
            // Core Releases
            let (cols, cw) = auto_fill(content_w, 160.0, crate::metrics::space::MD);
            let _ = cols;
            let mut grid = div().flex().flex_row().flex_wrap().gap(px(crate::metrics::space::MD));
            for (i, a) in d.albums.iter().enumerate() {
                let ids = (i < 8).then(|| format!("artist.album.{i}"));
                grid = grid.child(card(&self.ctx.clone(), &th, CardItem::Album(a), cw, None, ids, window, cx));
            }
            page = page.child(pid(
                "artist.albums",
                div().flex().flex_col().gap(px(crate::metrics::space::SM)).child(Self::section_title(&th, "Core Releases")).child(grid),
            ));
            // Fans also like (below the albums so the network-loaded row never shifts them)
            if let Some(info) = self.info.clone().filter(|i| !i.similar.is_empty()) {
                let mut row = div().flex().flex_row().flex_wrap().gap(px(crate::metrics::space::MD));
                for (i, s) in info.similar.iter().enumerate() {
                    let ids = (i < 8).then(|| format!("artist.similar.{i}"));
                    row = row.child(card(&self.ctx.clone(), &th, CardItem::Artist(s), cw, None, ids, window, cx));
                }
                page = page.child(pid("artist.similar", div().flex().flex_col().gap(px(crate::metrics::space::SM)).child(Self::section_title(&th, "Fans also like")).child(row)));
            }
            page.into_any_element()
        } else {
            div().p(px(crate::metrics::space::XL)).child(txt(&th, MONO_CODE, "ARTIST UNAVAILABLE.").color(th.clip)).into_any_element()
        };
        let _ = BODY_MD;
        self.page.wrap(window, &th, "artist-scroll", content)
    }
}
