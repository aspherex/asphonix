//! `Favorites.svelte` ("Starred Crates"): starred albums with a link to Liked Songs.
//! TODO(albums agent): the grid of `AlbumCard`s (`VirtualGrid`, minCellWidth 140, gap 16) belongs to the Albums route
//! port; until it lands a non-empty result shows the count only. The mock has no starred albums by default, so the
//! empty state is what both apps show.
use super::ListsEvent;
use crate::app;
use crate::metrics::{lists as lm, space};
use crate::probe::pid;
use crate::router::Route;
use crate::text::txt;
use crate::tracking::{BODY_SM, HEADLINE_MD, LABEL_CAPS, MONO_CODE};
use gpui::{Context, CursorStyle, EventEmitter, IntoElement, Render, Window, div, prelude::*, px};
use sonix_api::*;
use std::sync::Arc;

impl EventEmitter<ListsEvent> for FavoritesView {}

pub struct FavoritesView {
    total: usize,
    loaded: bool,
}

impl FavoritesView {
    pub fn new(backend: Arc<dyn Backend>, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move { backend.browse_get_albums(GetAlbumsRequest { offset: 0, limit: 60, sort: AlbumSort::Alphabetical, filter: Some(AlbumFilter { genre: None, year: None, starred_only: true }) }) })
                .await;
            let _ = this.update(cx, |t, cx| {
                t.total = r.map(|p| p.total).unwrap_or(0);
                t.loaded = true;
                cx.notify();
            });
        })
        .detach();
        Self { total: 0, loaded: false }
    }
}

impl Render for FavoritesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = app::theme(cx);
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .flex_none()
            .child(pid("page.title", txt(&th, HEADLINE_MD, "Starred Crates").color(th.text)))
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
                    .child(pid("page.count", txt(&th, MONO_CODE, format!("{} ALBUMS", self.total)).color(th.muted))),
            );
        let body = if self.total == 0 {
            let m = if self.loaded { "No starred albums yet — tap the star on any album or track." } else { "Loading…" };
            div().child(txt(&th, BODY_SM, m).color(th.muted))
        } else {
            div().child(txt(&th, BODY_SM, format!("{} starred albums", self.total)).color(th.muted))
        };
        pid("page", div().size_full().flex().flex_col().gap(px(space::MD)).p(px(lm::PAGE_PAD)).child(header).child(body))
    }
}
