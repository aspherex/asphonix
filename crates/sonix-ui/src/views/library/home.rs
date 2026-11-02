//! `Home.svelte`: the telemetry panel, the "Resume from server" bar, Jump Back In, Recently Played Tracks, Heavy Rotation,
//! Playlists // Vectors and the virtualized "Recently Ingested" list. A plain page inside the scrolling stage.
use super::cards::{CardItem, CardMenu, CardOpen, Tile, TileArt, TileOverlays, anchor_cell, card, open_menu_at, tile};
use super::entity_link::{Kind, set_current};
use super::fmtx;
use super::grid::list_overflows;
use super::parts::{PageScroll, auto_fill};
use super::{Ctx, LibEvent, menus, navigate};
use crate::views::lists::playlist_menu;
use crate::anim;
use crate::app;
use crate::metrics::library as m;
use crate::metrics::space as sp;
use crate::pager::{PAGE, Pager};
use crate::probe::pid;
use crate::router::Route;
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{BODY_MD, BODY_SM, HEADLINE_LG, HEADLINE_SM, LABEL_CAPS, MONO_CODE, MONO_DATA};
use crate::widgets::icon;
use crate::widgets::scrollbar::{self, ScrollbarHandle};
use gpui::{AnyElement, Context, CursorStyle, IntoElement, Render, Subscription, UniformListScrollHandle, Window, div, prelude::*, px, uniform_list};
use sonix_api::format::fmt_int;
use sonix_api::*;
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

/// A declined "Resume from server" offer stays declined for this app run (`dismissedOfferKey`).
static DISMISSED: Mutex<Option<String>> = Mutex::new(None);

fn offer_key(o: &ServerQueueOffer) -> String {
    o.changed_at.clone().unwrap_or_else(|| format!("track:{}", o.track.id))
}

pub struct HomeView {
    ctx: Ctx,
    status: Option<SessionStatus>,
    sections: Option<HomeSections>,
    sections_unavailable: bool,
    /// `pager.unavailable` of the Recently Ingested list
    list_unavailable: bool,
    lists: Vec<Playlist>,
    recent: Vec<HistoryEntry>,
    offer: Option<ServerQueueOffer>,
    pager: Pager<Album>,
    list: UniformListScrollHandle,
    sb: ScrollbarHandle,
    page: PageScroll,
    w: Rc<Cell<f32>>,
    _sub: Subscription,
}

impl HomeView {
    pub fn new(ctx: Ctx, cx: &mut Context<Self>) -> Self {
        set_current(&Route::Home);
        let sub = cx.subscribe(&ctx.nav.clone(), |this: &mut Self, _, ev: &LibEvent, cx| {
            if matches!(ev, LibEvent::Refresh) {
                this.load(cx);
            }
        });
        let mut this = Self {
            ctx,
            status: None,
            sections: None,
            sections_unavailable: false,
            list_unavailable: false,
            lists: Vec::new(),
            recent: Vec::new(),
            offer: None,
            pager: Pager::default(),
            list: UniformListScrollHandle::new(),
            sb: scrollbar::new_handle(),
            page: PageScroll::new(),
            w: Rc::new(Cell::new(super::parts::initial_width("home"))),
            _sub: sub,
        };
        this.load(cx);
        this.pager.want(0);
        this.fetch(0, cx);
        this
    }

    /// `Home.svelte: load()` / `loadRecent()`: each piece fills in as its own answer arrives (the web awaits them one
    /// after the other and renders after each; native runs them side by side), nothing waits for the slowest call.
    fn load(&mut self, cx: &mut Context<Self>) {
        macro_rules! piece {
            ($call:expr, $apply:expr) => {{
                let b = self.ctx.backend.clone();
                cx.spawn(async move |this, cx| {
                    let r = cx.background_executor().spawn(async move { $call(b) }).await;
                    let _ = this.update(cx, |t, cx| {
                        $apply(t, r);
                        cx.notify();
                    });
                })
                .detach();
            }};
        }
        piece!(|b: Arc<dyn Backend>| b.auth_session_status(), |t: &mut Self, r: ApiResult<SessionStatus>| t.status = r.ok());
        piece!(|b: Arc<dyn Backend>| b.browse_get_home_sections(), |t: &mut Self, r: ApiResult<HomeSections>| match r {
            Ok(s) => t.sections = Some(s),
            Err(_) => t.sections_unavailable = true,
        });
        piece!(|b: Arc<dyn Backend>| b.playlists_list(), |t: &mut Self, r: ApiResult<Vec<Playlist>>| t.lists = r.map(|l| l.into_iter().take(6).collect()).unwrap_or_default());
        piece!(|b: Arc<dyn Backend>| b.history_recent(HistoryRecentRequest { limit: 8 }), |t: &mut Self, r: ApiResult<Paged<HistoryEntry>>| t.recent = r.map(|p| p.items).unwrap_or_default());
        piece!(|b: Arc<dyn Backend>| b.session_check_server_queue(), |t: &mut Self, r: ApiResult<Option<ServerQueueOffer>>| {
            let dismissed = DISMISSED.lock().ok().and_then(|d| d.clone());
            t.offer = r.ok().flatten().filter(|o| Some(offer_key(o)) != dismissed);
        });
    }

    fn fetch(&mut self, page: usize, cx: &mut Context<Self>) {
        let (b, epoch) = (self.ctx.backend.clone(), self.pager.epoch);
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { b.browse_get_albums(GetAlbumsRequest { offset: page * PAGE, limit: PAGE, sort: AlbumSort::Newest, filter: None }) }).await;
            let _ = this.update(cx, |t, cx| {
                match r {
                    Ok(p) => {
                        if t.pager.insert(epoch, page, p.items, 0) {
                            t.pager.total = p.total;
                        }
                    }
                    Err(_) => t.list_unavailable = true,
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn block_title(th: &Theme, text: &str) -> gpui::Div {
        div().flex().flex_row().items_center().mb(px(sp::SM)).child(txt(th, HEADLINE_SM, text.to_string()).color(th.text))
    }

    fn telemetry(&self, th: &Theme) -> AnyElement {
        let th = *th;
        let st = self.status.as_ref();
        let connected = st.map(|s| s.state == ConnectionState::Connected).unwrap_or(false);
        let state_label = match st.map(|s| s.state) {
            Some(ConnectionState::Connected) => "CONNECTED",
            Some(ConnectionState::Connecting) => "CONNECTING",
            Some(ConnectionState::Error) => "ERROR",
            _ => "DISCONNECTED",
        };
        let mut session = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(sp::SM))
            .child(div().size(px(8.)).flex_none().bg(if connected { th.accent } else { th.muted }))
            .child(txt(&th, MONO_CODE, state_label).color(th.text));
        if let Some(p) = st.and_then(|s| s.ping_ms) {
            session = session.child(txt(&th, MONO_CODE, format!("{p}ms")).color(th.muted));
        }
        let name = st.and_then(|s| s.username.clone());
        let title = format!("Welcome back{}", name.map(|n| format!(", {n}")).unwrap_or_default());
        let pill = |children: Vec<AnyElement>| {
            let mut p = div().flex().flex_row().items_center().gap(px(sp::XS)).px(px(sp::SM)).py(px(sp::XS)).bg(th.mid);
            for c in children {
                p = p.child(c);
            }
            p
        };
        let mut pills = div().flex().flex_row().flex_wrap().gap(px(sp::SM)).mt(px(sp::XS));
        if let Some(n) = st.and_then(|s| s.track_count) {
            pills = pills.child(pill(vec![icon("library", 14.0, th.text).into_any_element(), txt(&th, MONO_DATA, format!("{} TRACKS INDEXED", fmt_int(n))).color(th.text).into_any_element()]));
        }
        if let Some(at) = st.and_then(|s| s.last_sync_at.clone()) {
            pills = pills.child(pill(vec![txt(&th, MONO_DATA, format!("SYNCED {}", fmtx::relative_time(Some(&at), fmtx::now_ms()))).color(th.text).into_any_element()]));
        }
        match st.map(|s| s.state) {
            Some(ConnectionState::Disconnected) => {
                pills = pills.child(pill(vec![txt(&th, MONO_DATA, "NOT CONNECTED — HEAD TO SETTINGS TO ADD A SERVER").color(th.muted).into_any_element()]));
            }
            Some(ConnectionState::Error) => {
                pills = pills.child(pill(vec![txt(&th, MONO_DATA, "SERVER UNREACHABLE — BROWSING LOCAL LIBRARY").color(th.muted).into_any_element()]));
            }
            _ => {}
        }
        let key = |k: &'static str, rest: &'static str| {
            div().flex().flex_row().child(txt(&th, MONO_CODE, k).weight(700.0).color(th.text)).child(txt(&th, MONO_CODE, format!(" {rest}")).color(th.muted))
        };
        let hotkeys = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(sp::LG))
            .mt(px(sp::SM))
            .pt(px(sp::SM))
            .border_t_1()
            .border_color(th.border)
            .child(pid("home.hotkey.0", key("SPACE", "Play/Pause")))
            .child(key("J / K", "Scroll"))
            .child(div().flex().flex_row().child(txt(&th, MONO_CODE, "/").weight(700.0).color(th.text)).child(txt(&th, MONO_CODE, " or ").color(th.muted)).child(txt(&th, MONO_CODE, "CTRL+K").weight(700.0).color(th.text)).child(txt(&th, MONO_CODE, " Search").color(th.muted)))
            .child(key("ENTER", "Play"));
        div()
            .flex()
            .flex_col()
            .gap(px(sp::XS))
            .p(px(sp::MD))
            .bg(th.low)
            .border_1()
            .border_color(th.border)
            .child(pid("home.session", session))
            .child(pid("home.title", div().flex().child(txt(&th, HEADLINE_LG, title).color(th.text))))
            .child(pid("home.pills", pills))
            .child(pid("home.hotkeys", hotkeys))
            .into_any_element()
    }

    fn offer_bar(&self, th: &Theme, o: &ServerQueueOffer, cx: &mut Context<Self>) -> AnyElement {
        let th = *th;
        let prefix = match &o.changed_by {
            Some(n) => format!("LAST PLAYED ON {}", n.to_uppercase()),
            None => "QUEUE ON SERVER".into(),
        };
        let mono = |s: String, weight: Option<f32>, c| {
            let t = txt(&th, MONO_DATA, s).color(c);
            match weight {
                Some(w) => t.weight(w),
                None => t,
            }
        };
        let text = div()
            .flex()
            .flex_row()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .child(mono(format!("{prefix}: "), None, th.muted))
            .child(mono(o.track.title.clone(), Some(600.0), th.text))
            .child(mono(format!(" — {} @ {}", o.track.artist, sonix_api::format::format_duration(Some(o.position_sec))), None, th.muted));
        let this = cx.entity().downgrade();
        let this2 = this.clone();
        let key = offer_key(o);
        let hot = div()
            .id("offer-resume")
            .px(px(sp::MD))
            .py(px(sp::XS))
            .flex_none()
            .bg(th.accent)
            .border_1()
            .border_color(th.accent_fg)
            .cursor(CursorStyle::PointingHand)
            .hover(move |s| s.border_color(th.focus))
            .child(txt(&th, LABEL_CAPS, "Resume from server").color(th.accent_text))
            .on_click(move |_, _, cx| {
                let _ = this.update(cx, |t, cx| {
                    t.offer = None;
                    app::call(cx, |b| {
                        let _ = b.session_resume_from_server();
                    });
                    cx.notify();
                });
            });
        let dismiss = div()
            .id("offer-dismiss")
            .px(px(sp::MD))
            .py(px(sp::XS))
            .flex_none()
            .bg(th.mid)
            .border_1()
            .border_color(th.border)
            .cursor(CursorStyle::PointingHand)
            .hover(move |s| s.border_color(th.focus))
            .child(txt(&th, LABEL_CAPS, "Dismiss").color(th.text))
            .on_click(move |_, _, cx| {
                if let Ok(mut d) = DISMISSED.lock() {
                    *d = Some(key.clone());
                }
                let _ = this2.update(cx, |t, cx| {
                    t.offer = None;
                    cx.notify();
                });
            });
        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_wrap()
            .gap(px(sp::MD))
            .px(px(sp::MD))
            .py(px(sp::SM))
            .bg(th.low)
            .border_1()
            .border_color(th.border)
            .border_l(px(3.))
            .border_color(th.border)
            .child(icon("queue", 16.0, th.text))
            .child(text)
            .child(pid("home.offer.resume", hot))
            .child(pid("home.offer.dismiss", dismiss))
            .into_any_element()
    }

    /// Wraps `children` into a CSS-grid-like set of rows of `cols` items.
    fn grid(children: Vec<AnyElement>, gap: f32) -> AnyElement {
        let mut g = div().flex().flex_row().flex_wrap().items_start().gap(px(gap));
        for c in children {
            g = g.child(c);
        }
        g.into_any_element()
    }

    fn list_block(&mut self, th: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = *th;
        let overflow = list_overflows(&self.list).unwrap_or(self.pager.total > 0);
        let total = self.pager.total;
        let list = uniform_list(
            "home-list",
            total,
            cx.processor(move |this: &mut Self, range: Range<usize>, _window, cx| {
                let th = app::theme(cx);
                range
                    .map(|ix| {
                        let Some(a) = this.pager.get(ix).cloned() else {
                            if this.pager.want(ix / PAGE) {
                                this.fetch(ix / PAGE, cx);
                            }
                            return div().id(("hrow", ix)).h(px(m::HOME_ROW_H)).w_full().border_b_1().border_color(th.base).into_any_element();
                        };
                        let id = a.id.clone();
                        let r = div()
                            .id(("hrow", ix))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(sp::MD))
                            .px(px(sp::MD))
                            .h(px(m::HOME_ROW_H))
                            .w_full()
                            .border_b_1()
                            .border_color(th.base)
                            .cursor(CursorStyle::PointingHand)
                            .hover(move |s| s.bg(th.mid))
                            .child(div().w(px(28.)).flex_none().child(txt(&th, MONO_CODE, format!("{:03}", ix + 1)).color(th.muted)))
                            .child(div().flex_1().min_w_0().flex().child(txt(&th, BODY_MD, a.name.clone()).color(th.text).ellipsis()))
                            .child(div().max_w(px(240.)).min_w_0().flex().child(txt(&th, MONO_DATA, a.artist.clone()).color(th.muted).ellipsis()))
                            .child(div().w(px(40.)).flex_none().flex().justify_end().child(txt(&th, MONO_CODE, a.year.map(|y| y.to_string()).unwrap_or_else(|| "—".into())).color(th.muted)))
                            .on_action({
                    let id = id.clone();
                    move |_: &CardOpen, _, cx| navigate(cx, Route::Album(id.clone()))
                })
                .on_click(move |_, _, cx| navigate(cx, Route::Album(id.clone())));
                        if ix < 8 { pid(format!("home.list.row.{ix}"), r).into_any_element() } else { r.into_any_element() }
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(self.list.clone())
        .size_full();
        let wrap = div()
            .relative()
            .flex_1()
            .min_h_0()
            .border_1()
            .border_color(th.border)
            .bg(th.low)
            .child(div().size_full().when(overflow, |d| d.pr(px(scrollbar::WIDTH))).child(list))
            .child(scrollbar::scrollbar("home-scrollbar", th, &self.list, &self.sb))
            .child(crate::widgets::smooth::drive(window, crate::widgets::smooth::Scrollable::list(&self.list)));
        div()
            .flex()
            .flex_col()
            .h(px(m::HOME_LIST_H))
            .flex_none()
            .child(Self::block_title(&th, "Recently Ingested // Server Pipeline"))
            .child(if self.list_unavailable { div().flex().child(txt(&th, BODY_SM, "Unavailable until a server is connected.").color(th.muted)).into_any_element() } else { pid("home.list", wrap).into_any_element() })
            .into_any_element()
    }
}

impl Render for HomeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = app::theme(cx);
        let content_w = (self.w.get() - 2.0 * m::HOME_PAD).max(1.0);
        let mut children: Vec<AnyElement> = vec![self.telemetry(&th)];
        if let Some(o) = self.offer.clone() {
            children.push(self.offer_bar(&th, &o, cx));
        }
        let ctx = self.ctx.clone();
        let (_, tile_w) = auto_fill(content_w, m::TILE_GRID_MIN, sp::MD);
        let (_, card_w) = auto_fill(content_w, m::CARD_GRID_MIN, sp::MD);

        // Jump Back In // Sessions
        if let Some(s) = self.sections.clone().filter(|s| !s.recently_played.is_empty()) {
            let mut tiles = Vec::new();
            for (i, a) in s.recently_played.iter().enumerate() {
                let id = a.id.clone();
                let t = tile(
                    &ctx,
                    &th,
                    Tile {
                        id: &a.id,
                        art: TileArt::Cover(a.cover_art.clone(), "album"),
                        genre: a.genre.as_deref(),
                        title: &a.name,
                        link: Some((Kind::Artist, a.artist_id.as_deref(), &a.artist)),
                        sub_text: String::new(),
                        overlays: None,
                    },
                    tile_w,
                    window,
                    cx,
                )
                .on_action({
                    let id = id.clone();
                    move |_: &CardOpen, _, cx| navigate(cx, Route::Album(id.clone()))
                })
                .on_click(move |_, _, cx| navigate(cx, Route::Album(id.clone())));
                tiles.push(pid(format!("home.tile.{i}"), t).into_any_element());
            }
            children.push(
                div()
                    .flex()
                    .flex_col()
                    .child(Self::block_title(&th, "Jump Back In // Sessions"))
                    .child(Self::grid(tiles, sp::MD))
                    .into_any_element(),
            );
        }

        // Recently Played Tracks
        if !self.recent.is_empty() {
            let mut tiles = Vec::new();
            let ids: Vec<String> = self.recent.iter().map(|e| e.track.id.clone()).collect();
            for (i, e) in self.recent.clone().iter().enumerate() {
                let ids = ids.clone();
                let t = tile(
                    &ctx,
                    &th,
                    Tile {
                        id: &e.track.id,
                        art: TileArt::Cover(e.track.cover_art.clone(), "track"),
                        genre: None,
                        title: &e.track.title,
                        link: None,
                        sub_text: e.track.artist.clone(),
                        overlays: None,
                    },
                    tile_w,
                    window,
                    cx,
                )
                .on_action({
                    let ids = ids.clone();
                    move |_: &CardOpen, _, cx| menus::play_tracks(cx, ids.clone(), i)
                })
                .on_click(move |_, _, cx| menus::play_tracks(cx, ids.clone(), i));
                tiles.push(pid(format!("home.recent.{i}"), t).into_any_element());
            }
            let view_all = div()
                .id("view-history")
                .ml(px(sp::MD))
                .cursor(CursorStyle::PointingHand)
                .text_color(th.muted)
                .hover(move |s| s.text_color(th.accent_fg))
                .child(txt(&th, LABEL_CAPS, "View history"))
                .on_click(move |_, _, cx| navigate(cx, Route::History));
            children.push(
                div()
                    .flex()
                    .flex_col()
                    .child(Self::block_title(&th, "Recently Played Tracks").child(view_all))
                    .child(Self::grid(tiles, sp::MD))
                    .into_any_element(),
            );
        }

        // Heavy Rotation
        if let Some(s) = self.sections.clone().filter(|s| !s.frequent.is_empty()) {
            let cards: Vec<AnyElement> = s.frequent.iter().enumerate().map(|(i, a)| card(&ctx, &th, CardItem::Album(a), card_w, None, Some(format!("home.card.{i}")), window, cx)).collect();
            children.push(div().flex().flex_col().child(Self::block_title(&th, "Heavy Rotation")).child(Self::grid(cards, sp::MD)).into_any_element());
        }
        if self.sections.is_none() && self.sections_unavailable {
            children.push(div().child(txt(&th, BODY_SM, "Unavailable until a server is connected.").color(th.muted)).into_any_element());
        }

        // Playlists // Vectors
        if !self.lists.is_empty() {
            let mut tiles = Vec::new();
            for (i, p) in self.lists.clone().iter().enumerate() {
                let (id_open, id_play) = (p.id.clone(), p.id.clone());
                let (mctx, pl) = (ctx.clone(), p.clone());
                let overlays = TileOverlays {
                    play: Rc::new(move |cx| menus::play_playlist(cx, id_play.clone(), false)),
                    menu: Rc::new(move |anchor, window, cx| {
                        let b = anchor.get();
                        let pos = gpui::point(b.origin.x, b.origin.y + b.size.height + px(4.));
                        open_menu_at(&mctx, playlist_menu(&pl, cx), pos, window, cx);
                    }),
                };
                let (mctx2, pl2) = (ctx.clone(), p.clone());
                let card_anchor = anchor_cell();
                let t = tile(
                    &ctx,
                    &th,
                    Tile { id: &p.id, art: TileArt::Mosaic((p.song_count > 0).then(|| crate::art::mosaic_key(p))), genre: None, title: &p.name, link: None, sub_text: format!("{} TRACKS", p.song_count), overlays: Some(overlays) },
                    card_w,
                    window,
                    cx,
                )
                .on_action({
                    let id = id_open.clone();
                    move |_: &CardOpen, _, cx| navigate(cx, Route::Playlist(id.clone()))
                })
                .child(crate::flow::bounds_probe(card_anchor.clone()))
                .on_action({
                    let (mctx3, pl3, a) = (ctx.clone(), p.clone(), card_anchor.clone());
                    move |_: &CardMenu, window, cx| {
                        // `isContextMenuKey` on the card: `openContextMenuFor(card)` (left edge, 4 px below)
                        let b = a.get();
                        let pos = gpui::point(b.left(), b.bottom() + px(4.));
                        open_menu_at(&mctx3, playlist_menu(&pl3, cx), pos, window, cx);
                    }
                })
                .on_click(move |_, _, cx| navigate(cx, Route::Playlist(id_open.clone())))
                .on_mouse_down(gpui::MouseButton::Right, move |ev, window, cx| {
                    cx.stop_propagation();
                    open_menu_at(&mctx2, playlist_menu(&pl2, cx), ev.position, window, cx);
                });
                tiles.push(pid(format!("home.pl.{i}"), t).into_any_element());
            }
            children.push(div().flex().flex_col().child(Self::block_title(&th, "Playlists // Vectors")).child(Self::grid(tiles, sp::MD)).into_any_element());
        }

        // Recently Ingested // Server Pipeline
        children.push(self.list_block(&th, window, cx));

        let mut home = div().relative().w_full().flex_none().flex().flex_col().gap(px(m::HOME_GAP)).p(px(m::HOME_PAD)).child(super::parts::measure_keyed("home", &self.w));
        for (i, c) in children.into_iter().enumerate() {
            home = home.child(pid(format!("home.child.{i}"), c));
        }
        let _ = (anchor_cell, anim::clear as fn());
        self.page.wrap(window, &th, "home-scroll", home)
    }
}
