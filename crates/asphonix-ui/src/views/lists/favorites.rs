//! The favorites page ("Starred Crates"): starred albums in the library routes' `VirtualGrid` of album cards
//! (min cell width 140, text height 40, gap 16, pages of 60 via `browse_get_albums` with `starred_only`), with a link
//! to Liked Songs. Remounted on every visit (the shell drops the entity when the route changes).
use super::ListsEvent;
use crate::app;
use crate::metrics::{lists as lm, space};
use crate::pager::PAGE;
use crate::router::Route;
use crate::text::txt;
use crate::tracking::{BODY_SM, HEADLINE_MD, LABEL_CAPS, MONO_CODE};
use crate::views::library::Ctx;
use crate::views::library::cards::{CardItem, card};
use crate::views::library::grid::{Geo, GridHost, GridState, grid_element};
use gpui::{AnyElement, Context, CursorStyle, EventEmitter, IntoElement, Render, Window, div, prelude::*, px};
use asphonix_api::*;

impl EventEmitter<ListsEvent> for FavoritesView {}

pub struct FavoritesView {
    ctx: Ctx,
    grid: GridState<Album>,
}

impl FavoritesView {
    pub fn new(ctx: Ctx, cx: &mut Context<Self>) -> Self {
        let mut this = Self { ctx, grid: GridState::new("favorites".into()) };
        this.grid.pager.want(0);
        this.fetch(0, cx);
        this
    }

    fn fetch(&mut self, page: usize, cx: &mut Context<Self>) {
        let (b, epoch) = (self.ctx.backend.clone(), self.grid.pager.epoch);
        let req = GetAlbumsRequest { offset: page * PAGE, limit: PAGE, sort: AlbumSort::Alphabetical, filter: Some(AlbumFilter { starred_only: true, ..Default::default() }) };
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { b.browse_get_albums(req) }).await;
            let _ = this.update(cx, |t, cx| {
                match r {
                    Ok(p) => {
                        let centre = t.grid.vis.start * t.grid.cols;
                        if t.grid.pager.insert(epoch, page, p.items, centre) {
                            t.grid.pager.total = p.total;
                        }
                    }
                    Err(_) => t.grid.pager.total = 0,
                }
                t.grid.loaded = true;
                cx.notify();
            });
        })
        .detach();
    }
}

impl GridHost<Album> for FavoritesView {
    fn grid(&mut self) -> &mut GridState<Album> {
        &mut self.grid
    }
    fn cell(&mut self, item: Option<Album>, geo: Geo, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = app::theme(cx);
        // the page is still loading: a plain block
        let Some(a) = item else { return div().w(px(geo.cell_w)).h(px(geo.cell_h)).flex_none().bg(th.low).into_any_element() };
        card(&self.ctx.clone(), &th, CardItem::Album(&a), geo.cell_w, Some(geo.cell_h), window, cx)
    }
    fn request(&mut self, page: usize, cx: &mut Context<Self>) {
        if self.grid.pager.want(page) {
            self.fetch(page, cx);
        }
    }
}

impl Render for FavoritesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = app::theme(cx);
        let total = self.grid.pager.total;
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .flex_none()
            .child(txt(&th, HEADLINE_MD, "Starred Crates").color(th.text))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(space::MD))
                    .child(
                        div()
                            .id("fav-liked")
                            .cursor(CursorStyle::PointingHand)
                            .text_color(th.muted)
                            .hover(move |s| s.text_color(th.accent_fg))
                            .child(txt(&th, LABEL_CAPS, "Starred tracks → Liked Songs"))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ListsEvent::Navigate(Route::Liked)))),
                    )
                    .child(txt(&th, MONO_CODE, format!("{total} ALBUMS")).color(th.muted)),
            );
        let body: AnyElement = if total == 0 && self.grid.loaded {
            let m = "No starred albums yet — tap the star on any album or track.";
            div().child(txt(&th, BODY_SM, m).color(th.muted)).into_any_element()
        } else {
            div().flex_1().min_h_0().child(grid_element(self, th, window, cx)).into_any_element()
        };
        div().size_full().flex().flex_col().gap(px(space::MD)).p(px(lm::PAGE_PAD)).child(header).child(body)
    }
}
