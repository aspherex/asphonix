//! `Genres.svelte`: every genre with real album/track counts in one plain CSS grid (`minmax(220px, 1fr)`), sortable by
//! track count or A-Z, each card with a relative-size bar.
use super::parts::{PageScroll, auto_fill, segmented};
use super::{Ctx, navigate};
use crate::app;
use crate::metrics::library as m;
use crate::metrics::space as sp;
use crate::probe::pid;
use crate::router::Route;
use crate::text::txt;
use crate::tracking::{BODY_SM, HEADLINE_MD, HEADLINE_SM, MONO_CODE, MONO_DATA};
use gpui::{AnyElement, Context, CursorStyle, IntoElement, Render, Window, div, prelude::*, px};
use sonix_api::format::fmt_int;
use sonix_api::*;
use std::cell::Cell;
use std::rc::Rc;

pub struct GenresView {
    ctx: Ctx,
    sort: GenreSort,
    items: Vec<Genre>,
    loaded: bool,
    error: bool,
    seq: u64,
    page: PageScroll,
    w: Rc<Cell<f32>>,
}

impl GenresView {
    pub fn new(ctx: Ctx, cx: &mut Context<Self>) -> Self {
        let mut this = Self { ctx, sort: GenreSort::Tracks, items: Vec::new(), loaded: false, error: false, seq: 0, page: PageScroll::new(), w: Rc::new(Cell::new(super::parts::initial_width("genres"))) };
        this.load(cx);
        this
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.seq += 1;
        let (my, b, sort) = (self.seq, self.ctx.backend.clone(), self.sort);
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { b.browse_get_genres(GetGenresRequest { offset: 0, limit: 1000, sort }) }).await;
            let _ = this.update(cx, |t, cx| {
                if t.seq != my {
                    return;
                }
                match r {
                    Ok(p) => {
                        t.items = p.items;
                        t.error = false;
                    }
                    Err(_) => t.error = true,
                }
                t.loaded = true;
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for GenresView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = app::theme(cx);
        let this = cx.entity().downgrade();
        let options: [(GenreSort, &'static str); 2] = [(GenreSort::Tracks, "Most tracks"), (GenreSort::Alphabetical, "A-Z")];
        let header = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(px(sp::MD))
            .flex_none()
            .child(pid("page.title", txt(&th, HEADLINE_MD, "Genres").color(th.text)))
            .child(pid("lib.controls",
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(m::CONTROLS_GAP))
                    .child(pid("genres.count", txt(&th, MONO_CODE, format!("{} GENRES", fmt_int(self.items.len() as u64))).color(th.muted)))
                    .child(pid(
                        "lib.seg",
                        segmented(&th, "lib.seg", &options, self.sort, move |v, _, cx| {
                            let _ = this.update(cx, |t, cx| {
                                t.sort = v;
                                t.load(cx);
                            });
                        }),
                    )),
            ));
        let body: AnyElement = if !self.loaded {
            // native-only: placeholder genre cards (same box as the loaded ones) until the list arrives
            let content_w = (self.w.get() - 2.0 * m::PAGE_PAD).max(1.0);
            let (cols, cw) = auto_fill(content_w, m::GENRE_MIN_CELL, sp::MD);
            let mut grid = div().flex().flex_row().flex_wrap().items_start().gap(px(sp::MD));
            for _ in 0..cols * 3 {
                grid = grid.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(sp::XS))
                        .w(px(cw))
                        .flex_none()
                        .p(px(sp::MD))
                        .bg(th.low)
                        .border_1()
                        .border_color(th.border)
                        .child(crate::widgets::skeleton::text_slot(&th, HEADLINE_SM.line, 14.0, 0.6))
                        .child(crate::widgets::skeleton::text_slot(&th, MONO_DATA.line, 10.0, 0.8))
                        .child(div().mt(px(sp::XS)).h(px(m::GENRE_BAR_H)).bg(th.high)),
                );
            }
            grid.into_any_element()
        } else if self.error {
            div().flex().child(txt(&th, MONO_CODE, "GENRES UNAVAILABLE.").color(th.muted)).into_any_element()
        } else if self.items.is_empty() {
            div().flex().child(txt(&th, BODY_SM, "No genre tags in your library yet.").color(th.muted)).into_any_element()
        } else {
            let content_w = (self.w.get() - 2.0 * m::PAGE_PAD).max(1.0);
            let (_, cw) = auto_fill(content_w, m::GENRE_MIN_CELL, sp::MD);
            let max_tracks = self.items.iter().map(|g| g.track_count).max().unwrap_or(1).max(1) as f32;
            let mut grid = div().flex().flex_row().flex_wrap().items_start().gap(px(sp::MD));
            for (i, g) in self.items.iter().enumerate() {
                let name = g.name.clone();
                let pct = ((g.track_count as f32 / max_tracks) * 100.0).max(2.0);
                // a `<button>`: Tab stop, Enter/Space open, `:focus-visible` = focus border + the UA ring
                let fh = super::cards::focus_handle(cx, super::cards::key_of("genre", &g.name));
                let focused = fh.contains_focused(window, cx);
                let open = name.clone();
                let c = div()
                    .id(("genre-card", i))
                    .track_focus(&fh)
                    .tab_stop(true)
                    .key_context("Card")
                    .on_action(move |_: &super::cards::CardOpen, _, cx| navigate(cx, Route::Genre(open.clone())))
                    .relative()
                    .flex()
                    .flex_col()
                    .gap(px(sp::XS))
                    .w(px(cw))
                    .min_w_0()
                    .flex_none()
                    .p(px(sp::MD))
                    .bg(th.low)
                    .border_1()
                    .border_color(if focused { th.focus } else { th.border })
                    .cursor(CursorStyle::PointingHand)
                    .hover(move |s| s.border_color(th.focus))
                    .when(focused, |d| d.child(super::cards::focus_ring()))
                    .child(div().flex().min_w_0().child(txt(&th, HEADLINE_SM, g.name.clone()).color(th.text).ellipsis()))
                    .child(div().flex().min_w_0().child(txt(&th, MONO_DATA, format!("{} ALBUMS · {} TRACKS", fmt_int(g.album_count as u64), fmt_int(g.track_count as u64))).color(th.muted)))
                    .child(div().mt(px(sp::XS)).h(px(m::GENRE_BAR_H)).bg(th.high).child(div().h_full().w(gpui::relative(pct / 100.0)).bg(th.accent)))
                    .on_click(move |_, _, cx| navigate(cx, Route::Genre(name.clone())));
                grid = grid.child(if i < 8 { pid(format!("genres.card.{i}"), c).into_any_element() } else { c.into_any_element() });
            }
            grid.into_any_element()
        };
        let page = div().relative().w_full().flex_none().flex().flex_col().gap(px(m::PAGE_GAP)).p(px(m::PAGE_PAD)).child(super::parts::measure_keyed("genres", &self.w)).child(pid("lib.header", header)).child(pid("genres.body", body));
        self.page.wrap(window, &th, "genres-scroll", pid("page", page))
    }
}
