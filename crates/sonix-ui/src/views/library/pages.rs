//! The three virtual-grid routes: Albums (`Albums.svelte`), Artists (`Artists.svelte`) and one genre's albums
//! (`GenreDetail.svelte`). Each owns a paged [`GridState`] and renders through [`grid_element`].
use super::cards::{CardItem, card};
use super::grid::{Geo, GridHost, GridState, grid_element};
use super::parts::{checkbox, segmented};
use super::{Ctx, LibEvent, menus};
use crate::app;
use crate::metrics::library as m;
use crate::pager::PAGE;
use crate::probe::pid;
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{BODY_SM, HEADLINE_LG, HEADLINE_MD, LABEL_CAPS, MONO_CODE, MONO_DATA};
use crate::widgets::button::{Variant, button};
use gpui::{AnyElement, Context, IntoElement, Render, Subscription, Window, div, prelude::*, px};
use sonix_api::format::fmt_int;
use sonix_api::*;

/// The id of the n-th card of the web's rendered window (`.window > .cell:nth-child(n)`): the web renders from
/// `max(0, floor(scrollTop / rowHeight) - 2)` rows.
pub(crate) fn web_index<T>(g: &GridState<T>, ix: usize, geo: Geo) -> Option<usize> {
    let start = ((g.scroll_top() / geo.row_h()).floor() as i64 - 2).max(0) as usize * geo.cols;
    ix.checked_sub(start)
}

fn loading_cell(th: &Theme, geo: Geo) -> AnyElement {
    div().w(px(geo.cell_w)).h(px(geo.cell_h)).flex_none().bg(th.low).into_any_element()
}

fn page_shell(header: impl IntoElement, body: impl IntoElement) -> AnyElement {
    pid(
        "page",
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(m::PAGE_GAP))
            .p(px(m::PAGE_PAD))
            .child(header)
            .child(pid("lib.gridwrap", div().flex_1().min_h_0().child(body))),
    )
    .into_any_element()
}

fn header_row() -> gpui::Div {
    div().flex().flex_row().items_center().justify_between().flex_none()
}

// ---------------------------------------------------------------------------------------------- Albums
pub struct AlbumsView {
    ctx: Ctx,
    grid: GridState<Album>,
    sort: AlbumSort,
    starred_only: bool,
    _sub: Subscription,
}

impl AlbumsView {
    pub fn new(ctx: Ctx, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe(&ctx.nav.clone(), |this: &mut Self, _, ev: &LibEvent, cx| {
            if matches!(ev, LibEvent::Refresh) {
                for p in this.grid.visible_pages() {
                    this.fetch(p, cx);
                }
            }
        });
        // native-only play-button state: repaint when the loaded track / status / source changes
        cx.subscribe(&ctx.player.clone(), |_, _, _: &crate::app::PlayerEvent, cx| cx.notify()).detach();
        let mut this = Self { ctx, grid: GridState::new(String::new()), sort: AlbumSort::Alphabetical, starred_only: false, _sub: sub };
        this.reload(cx);
        this
    }


    fn req(&self, page: usize) -> GetAlbumsRequest {
        GetAlbumsRequest { offset: page * PAGE, limit: PAGE, sort: self.sort, filter: self.starred_only.then(|| AlbumFilter { starred_only: true, ..Default::default() }) }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.grid.reset(format!("albums:{:?}:{}", self.sort, self.starred_only));
        self.grid.pager.want(0);
        self.fetch(0, cx);
        cx.notify();
    }

    fn fetch(&mut self, page: usize, cx: &mut Context<Self>) {
        let (b, epoch, req) = (self.ctx.backend.clone(), self.grid.pager.epoch, self.req(page));
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { b.browse_get_albums(req) }).await;
            let _ = this.update(cx, |t, cx| {
                if let Ok(p) = r {
                    let centre = t.grid.vis.start * t.grid.cols;
                    if t.grid.pager.insert(epoch, page, p.items, centre) {
                        t.grid.pager.total = p.total;
                        cx.notify();
                    }
                }
                // native-only: any current answer (a failure too) ends the first-load skeleton
                if !t.grid.loaded && epoch == t.grid.pager.epoch {
                    t.grid.loaded = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

impl GridHost<Album> for AlbumsView {
    fn grid(&mut self) -> &mut GridState<Album> {
        &mut self.grid
    }
    fn cell(&mut self, ix: usize, item: Option<Album>, geo: Geo, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = app::theme(cx);
        let Some(a) = item else { return loading_cell(&th, geo) };
        let ids = web_index(&self.grid, ix, geo).filter(|n| *n < 8).map(|n| format!("lib.card.{n}"));
        card(&self.ctx.clone(), &th, CardItem::Album(&a), geo.cell_w, Some(geo.cell_h), ids, window, cx)
    }
    fn request(&mut self, page: usize, cx: &mut Context<Self>) {
        if self.grid.pager.want(page) {
            self.fetch(page, cx);
        }
    }
}

impl Render for AlbumsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = app::theme(cx);
        let this = cx.entity().downgrade();
        let this2 = this.clone();
        let options: [(AlbumSort, &'static str); 5] =
            [(AlbumSort::Alphabetical, "A-Z"), (AlbumSort::Artist, "Artist"), (AlbumSort::Newest, "Newest"), (AlbumSort::Year, "Year"), (AlbumSort::Random, "Random")];
        let controls = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(m::CONTROLS_GAP))
            .child(pid(
                "lib.check",
                checkbox(&th, "starred-only", self.starred_only, "Starred only", move |v, _, cx| {
                    let _ = this.update(cx, |t, cx| {
                        t.starred_only = v;
                        t.reload(cx);
                    });
                }),
            ))
            .child(pid(
                "lib.seg",
                segmented(&th, "lib.seg", &options, self.sort, move |v, _, cx| {
                    let _ = this2.update(cx, |t, cx| {
                        t.sort = v;
                        t.reload(cx);
                    });
                }),
            ));
        let header = pid("lib.header", header_row().child(pid("page.title", txt(&th, HEADLINE_MD, "Albums").color(th.text))).child(pid("lib.controls", controls)));
        let body = grid_element(self, th, window, cx);
        page_shell(header, body)
    }
}

// ---------------------------------------------------------------------------------------------- Artists
pub struct ArtistsView {
    ctx: Ctx,
    grid: GridState<Artist>,
    sort: ArtistSort,
    _sub: Subscription,
}

impl ArtistsView {
    pub fn new(ctx: Ctx, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe(&ctx.nav.clone(), |this: &mut Self, _, ev: &LibEvent, cx| {
            if matches!(ev, LibEvent::Refresh) {
                for p in this.grid.visible_pages() {
                    this.fetch(p, cx);
                }
            }
        });
        // native-only play-button state: repaint when the loaded track / status / source changes
        cx.subscribe(&ctx.player.clone(), |_, _, _: &crate::app::PlayerEvent, cx| cx.notify()).detach();
        let mut this = Self { ctx, grid: GridState::new(String::new()), sort: ArtistSort::Alphabetical, _sub: sub };
        this.reload(cx);
        this
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.grid.reset(format!("artists:{:?}", self.sort));
        self.grid.pager.want(0);
        self.fetch(0, cx);
        cx.notify();
    }

    fn fetch(&mut self, page: usize, cx: &mut Context<Self>) {
        let (b, epoch, sort) = (self.ctx.backend.clone(), self.grid.pager.epoch, self.sort);
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { b.browse_get_artists(GetArtistsRequest { offset: page * PAGE, limit: PAGE, sort: Some(sort) }) }).await;
            let _ = this.update(cx, |t, cx| {
                if let Ok(p) = r {
                    let centre = t.grid.vis.start * t.grid.cols;
                    if t.grid.pager.insert(epoch, page, p.items, centre) {
                        t.grid.pager.total = p.total;
                        cx.notify();
                    }
                }
                // native-only: any current answer (a failure too) ends the first-load skeleton
                if !t.grid.loaded && epoch == t.grid.pager.epoch {
                    t.grid.loaded = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

impl GridHost<Artist> for ArtistsView {
    fn grid(&mut self) -> &mut GridState<Artist> {
        &mut self.grid
    }
    fn cell(&mut self, ix: usize, item: Option<Artist>, geo: Geo, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = app::theme(cx);
        let Some(a) = item else { return loading_cell(&th, geo) };
        let ids = web_index(&self.grid, ix, geo).filter(|n| *n < 8).map(|n| format!("lib.card.{n}"));
        card(&self.ctx.clone(), &th, CardItem::Artist(&a), geo.cell_w, Some(geo.cell_h), ids, window, cx)
    }
    fn request(&mut self, page: usize, cx: &mut Context<Self>) {
        if self.grid.pager.want(page) {
            self.fetch(page, cx);
        }
    }
}

impl Render for ArtistsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = app::theme(cx);
        let this = cx.entity().downgrade();
        let options: [(ArtistSort, &'static str); 2] = [(ArtistSort::Alphabetical, "A-Z"), (ArtistSort::Random, "Random")];
        let header = pid(
            "lib.header",
            header_row().child(pid("page.title", txt(&th, HEADLINE_MD, "Artists").color(th.text))).child(pid(
                "lib.seg",
                segmented(&th, "lib.seg", &options, self.sort, move |v, _, cx| {
                    let _ = this.update(cx, |t, cx| {
                        t.sort = v;
                        t.reload(cx);
                    });
                }),
            )),
        );
        let body = grid_element(self, th, window, cx);
        page_shell(header, body)
    }
}

// ---------------------------------------------------------------------------------------------- GenreDetail
pub struct GenreDetailView {
    ctx: Ctx,
    name: String,
    grid: GridState<Album>,
    info: Option<Genre>,
    missing: bool,
    /// the Flow button's bounds (its menu anchors under it)
    flow_btn: std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>,
    _sub: Subscription,
}

impl GenreDetailView {
    pub fn new(ctx: Ctx, name: String, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe(&ctx.nav.clone(), |this: &mut Self, _, ev: &LibEvent, cx| {
            if matches!(ev, LibEvent::Refresh) {
                for p in this.grid.visible_pages() {
                    this.fetch(p, cx);
                }
            }
        });
        // native-only play-button state: repaint when the loaded track / status / source changes
        cx.subscribe(&ctx.player.clone(), |_, _, _: &crate::app::PlayerEvent, cx| cx.notify()).detach();
        let mut this = Self { ctx, grid: GridState::new(format!("genre:{name}")), name, info: None, missing: false, flow_btn: std::rc::Rc::new(std::cell::Cell::new(gpui::Bounds::default())), _sub: sub };
        this.grid.pager.want(0);
        this.fetch(0, cx);
        let (b, n) = (this.ctx.backend.clone(), this.name.clone());
        cx.spawn(async move |this, cx| {
            let g = cx.background_executor().spawn(async move { b.browse_get_genre(&n) }).await;
            let _ = this.update(cx, |t, cx| {
                match g {
                    Ok(g) => t.info = Some(g),
                    Err(_) => t.missing = true,
                }
                cx.notify();
            });
        })
        .detach();
        this
    }

    fn fetch(&mut self, page: usize, cx: &mut Context<Self>) {
        let (b, epoch, name) = (self.ctx.backend.clone(), self.grid.pager.epoch, self.name.clone());
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    b.browse_get_albums(GetAlbumsRequest { offset: page * PAGE, limit: PAGE, sort: AlbumSort::Alphabetical, filter: Some(AlbumFilter { genre: Some(name), ..Default::default() }) })
                })
                .await;
            let _ = this.update(cx, |t, cx| {
                if let Ok(p) = r {
                    let centre = t.grid.vis.start * t.grid.cols;
                    if t.grid.pager.insert(epoch, page, p.items, centre) {
                        t.grid.pager.total = p.total;
                        t.grid.loaded = true;
                        cx.notify();
                    }
                } else if !t.grid.loaded && epoch == t.grid.pager.epoch {
                    // native-only: a failed first page ends the skeleton (the empty note shows instead)
                    t.grid.loaded = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

impl GridHost<Album> for GenreDetailView {
    fn grid(&mut self) -> &mut GridState<Album> {
        &mut self.grid
    }
    fn cell(&mut self, ix: usize, item: Option<Album>, geo: Geo, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = app::theme(cx);
        let Some(a) = item else { return loading_cell(&th, geo) };
        let ids = web_index(&self.grid, ix, geo).filter(|n| *n < 8).map(|n| format!("lib.card.{n}"));
        card(&self.ctx.clone(), &th, CardItem::Album(&a), geo.cell_w, Some(geo.cell_h), ids, window, cx)
    }
    fn request(&mut self, page: usize, cx: &mut Context<Self>) {
        if self.grid.pager.want(page) {
            self.fetch(page, cx);
        }
    }
}

impl Render for GenreDetailView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = app::theme(cx);
        let ready = self.info.is_some();
        let (n1, n2) = (self.name.clone(), self.name.clone());
        // native-only: the loaded genre source's Play button shows the active state
        let pst = self.ctx.player.read(cx).play_state(&crate::app::PlayTarget::Genre(self.name.clone()));
        let title = div()
            .flex()
            .flex_col()
            .gap(px(crate::metrics::space::XS))
            .min_w_0()
            .child(pid("genre.crumb", div().flex().child(txt(&th, LABEL_CAPS, "Genre").color(th.muted))))
            .child(pid("page.title", div().flex().child(txt(&th, HEADLINE_LG, self.name.clone()).color(th.text))))
            .when_some(self.info.clone(), |d, g| {
                d.child(pid(
                    "genre.stats",
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(crate::metrics::space::XS))
                        .child(txt(&th, MONO_DATA, format!("{} ALBUMS", fmt_int(g.album_count as u64))).color(th.muted))
                        .child(txt(&th, MONO_DATA, "·").color(th.muted))
                        .child(txt(&th, MONO_DATA, format!("{} TRACKS", fmt_int(g.track_count as u64))).color(th.muted)),
                ))
            });
        let actions = div()
            .flex()
            .flex_row()
            .gap(px(crate::metrics::space::SM))
            .child(pid("genre.btn.0", button("genre-play", th, Variant::Secondary, "Play", Some(pst.icon()), !ready, window).when(pst.active(), |b| b.border_color(th.accent)).on_click(move |_, _, cx| {
                if pst.active() {
                    crate::playback::toggle(cx);
                } else {
                    menus::play_genre(cx, n1.clone(), false)
                }
            })))
            .child(pid("genre.btn.1", button("genre-shuffle", th, Variant::Primary, "Shuffle", Some("shuffle"), !ready, window).on_click(move |_, _, cx| menus::play_genre(cx, n2.clone(), true))))
            .child(pid("genre.btn.2", {
                let (cell, menu, n3) = (self.flow_btn.clone(), self.ctx.menu.clone(), self.name.clone());
                button("genre-flow", th, Variant::Secondary, "Flow", Some("sliders"), !ready, window).relative().child(crate::flow::bounds_probe(cell.clone())).on_click(move |_, window, cx| {
                    let name = n3.clone();
                    let play: crate::flow::Play = std::rc::Rc::new(move |arc, cx| crate::playback::play_genre(cx, name.clone(), false, Some(arc)));
                    crate::flow::open_menu(menu.clone(), cell.get(), crate::flow::pool(sonix_api::FlowPoolKind::Genre, Some(n3.clone()), None), "genre", play, window, cx);
                })
            }));
        let header = pid("lib.header", div().flex().flex_row().items_end().justify_between().gap(px(crate::metrics::space::MD)).flex_wrap().flex_none().child(title).child(actions));
        let body: AnyElement = if self.missing {
            txt(&th, MONO_CODE, format!("NO TRACKS TAGGED “{}”.", self.name.to_uppercase())).color(th.muted).into_any_element_wrapped()
        } else if self.grid.loaded && self.grid.pager.total == 0 {
            txt(&th, BODY_SM, "No albums are tagged with this genre (tracks may still be).").color(th.muted).into_any_element_wrapped()
        } else {
            grid_element(self, th, window, cx)
        };
        page_shell(header, body)
    }
}

/// A `Txt` as a block child.
trait Wrapped {
    fn into_any_element_wrapped(self) -> AnyElement;
}
impl Wrapped for crate::text::Txt {
    fn into_any_element_wrapped(self) -> AnyElement {
        div().flex().child(self).into_any_element()
    }
}
