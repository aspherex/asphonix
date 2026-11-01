//! Queue rail (`QueueRail.svelte`, phase-1 subset): "Live Transport Deck" (current track, spectrum,
//! position/output row), the "Next from <label> · Shuffle" window with the real "+ N more" footer.
//! Docked at >= 1440 px, an overlay drawer below. Not yet ported: drag-reorder, multi-select, remove,
//! the "Next in queue" user section, flow rows (docs/PORTING.md). Sizes: `crate::metrics::rail`.
use crate::app::{self, PlayerEvent, PlayerModel};
use crate::art::{self, ArtHandle};
use crate::metrics::{layout, rail as rl, space, tracks as tr};
use crate::probe::pid;
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{BODY_LG, BODY_MD, BODY_SM, LABEL_CAPS, MONO_CODE, MONO_DATA};
use crate::widgets::icon;
use gpui::{Bounds, ClickEvent, Context, CursorStyle, Entity, IntoElement, Render, Subscription, Window, canvas, div, fill, point, prelude::*, px, size};
use sonix_api::format as fmt;
use sonix_api::*;
use std::sync::Arc;

pub struct QueueRail {
    backend: Arc<dyn Backend>,
    art: ArtHandle,
    player: Entity<PlayerModel>,
    state: Option<QueueState>,
    scroll: gpui::ScrollHandle,
    _sub: Subscription,
}

impl QueueRail {
    pub fn new(backend: Arc<dyn Backend>, art: ArtHandle, player: &Entity<PlayerModel>, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe(player, |this: &mut Self, _, _: &PlayerEvent, cx| this.refresh(cx));
        let mut s = Self { backend, art, player: player.clone(), state: None, scroll: gpui::ScrollHandle::new(), _sub: sub };
        s.refresh(cx);
        s
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let q = cx.background_executor().spawn(async move { b.player_get_queue(PageRequest { offset: 0, limit: 100 }) }).await;
            let _ = this.update(cx, |t, cx| {
                t.state = q.ok();
                cx.notify();
            });
        })
        .detach();
    }

    pub fn release(&mut self) {
        self.state = None;
    }
}

/// `deck-header`: 8/12 padding, `surface-mid`, bottom rule, label caps (+ right-aligned count).
fn header(th: &Theme, id: &'static str, label: String, right: Option<String>) -> impl IntoElement {
    pid(
        id,
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .justify_between()
            .px(px(rl::HEADER_PAD_X))
            .py(px(rl::HEADER_PAD_Y))
            .bg(th.mid)
            .border_b_1()
            .border_color(th.border)
            .child(txt(th, LABEL_CAPS, label).color(th.text))
            .children(right.map(|r| txt(th, MONO_CODE, r).color(th.muted))),
    )
}

impl Render for QueueRail {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th: Theme = app::theme(cx);
        let (ps, frame) = {
            let m = self.player.read(cx);
            (m.state.clone(), m.spectrum.clone())
        };
        let backend = self.backend.clone();
        let vw = f32::from(window.viewport_size().width);
        let drawer = vw < crate::metrics::bp::RAIL_DOCKED_FROM;
        let q = self.state.clone();
        let cur = q.as_ref().map(|q| q.current_index).unwrap_or(-1);
        let total = q.as_ref().map(|q| q.total).unwrap_or(0);
        let readout = if ps.track.is_some() {
            format!("{}{}", fmt::output_readout(&ps, "NO OUTPUT"), if ps.bit_perfect { " · BIT-PERFECT" } else { "" })
        } else {
            String::new()
        };

        // ---- deck
        let deck_body = match &ps.track {
            Some(t) => {
                let cover = art::cover(&self.art, &backend, t.cover_art.as_deref(), 56.0, window, cx);
                let bands: Option<Vec<u8>> = if ps.status == PlaybackStatus::Stopped { None } else { frame.map(|f| f.bands) };
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(space::SM))
                            .p(px(space::MD))
                            .child(pid("rail.deck.art", div().size(px(56.)).flex_none().border_1().border_color(th.border).bg(th.high).overflow_hidden().child(cover)))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .justify_center()
                                    .min_w_0()
                                    .gap(px(2.))
                                    .child(pid("rail.deck.title", div().flex().min_w_0().child(txt(&th, BODY_LG, t.title.clone()).color(th.text).ellipsis())))
                                    .child(pid("rail.deck.artist", div().flex().child(txt(&th, MONO_DATA, t.artist.clone()).color(th.muted).no_tracking().ellipsis()))),
                            ),
                    )
                    .child(pid("rail.deck.spectrum", div().h(px(64.)).px(px(space::MD)).child(rail_spectrum(th, bands))))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap(px(space::SM))
                            .pt(px(space::XS))
                            .px(px(space::MD))
                            .pb(px(space::MD))
                            .child(txt(&th, MONO_CODE, fmt::format_duration(Some(ps.position_sec))).color(th.muted))
                            .child(div().flex().min_w_0().child(txt(&th, MONO_CODE, readout).color(th.muted).ellipsis()))
                            .child(txt(&th, MONO_CODE, fmt::format_duration(Some(ps.duration_sec))).color(th.muted)),
                    )
                    .into_any_element()
            }
            None => pid("rail.deck.empty", div().p(px(space::MD)).child(txt(&th, BODY_SM, "Nothing playing.").color(th.muted))).into_any_element(),
        };
        let deck = div().flex_none().border_b_1().border_color(th.border).child(header(&th, "rail.deckheader", "Live Transport Deck".into(), Some(format!("{total} QUEUED")))).child(deck_body);

        // ---- queue section
        let label = match q.as_ref().and_then(|q| q.source.as_ref()) {
            Some(s) => format!("Next from {}{}", s.label, if s.mode == SourceMode::Shuffle { " · Shuffle" } else { "" }),
            None => "Next in buffer".to_string(),
        };
        let queue_section = if total == 0 {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(header(&th, "rail.queue.header", "Next in buffer".into(), None))
                .child(pid("rail.queue.empty", div().p(px(space::MD)).child(txt(&th, BODY_SM, "Queue is empty.").color(th.muted))))
        } else {
            let (bar, overflow) = crate::widgets::scrollbar::passive(th, &self.scroll);
            let mut list = div().id("queue-list").track_scroll(&self.scroll).size_full().overflow_y_scroll().flex().flex_col().when(overflow, |d| d.pr(px(tr::SCROLLBAR_W)));
            if let Some(q) = &q {
                for (i, it) in q.items.iter().enumerate() {
                    if (i as i64) < cur {
                        continue;
                    }
                    let t = &it.track;
                    let active = (i as i64) == cur;
                    let idx = i;
                    let n = i - cur.max(0) as usize;
                    list = list.child(pid(
                        format!("rail.row.{n}"),
                        div()
                            .id(("qrow", i))
                            .relative()
                            .flex()
                            .flex_row()
                            .flex_none()
                            .items_center()
                            .gap(px(space::XS))
                            .h(px(44.))
                            .px(px(space::SM))
                            .border_b_1()
                            .border_color(th.mid)
                            .cursor(CursorStyle::PointingHand)
                            .when(active, |d| d.bg(th.mid))
                            .when(!active, |d| d.hover(move |s| s.bg(th.mid)))
                            .child(icon("drag", 13., th.muted))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(div().flex().min_w_0().child(txt(&th, BODY_MD, t.title.clone()).color(th.text).ellipsis()))
                                    .child(div().flex().min_w_0().child(txt(&th, MONO_CODE, t.artist.clone()).color(th.muted).no_tracking().ellipsis())),
                            )
                            .child(div().flex_none().child(txt(&th, MONO_CODE, fmt::format_duration(Some(t.duration))).color(th.muted)))
                            .child(div().size(px(20.)).flex_none().flex().items_center().justify_center().child(icon("more", 11., th.muted)))
                            .child(div().size(px(20.)).flex_none().flex().items_center().justify_center().child(icon("close", 11., th.muted)))
                            .when(active, |d| d.child(div().absolute().left_0().top_0().bottom_0().w(px(tr::ACTIVE_BAR_W)).bg(th.accent_fg)))
                            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                app::call(cx, move |b| {
                                    let _ = b.player_jump_to(idx);
                                });
                            })),
                    ));
                }
            }
            let footer = q.as_ref().and_then(|q| q.source.as_ref()).filter(|s| s.remaining > 0).map(|s| {
                div()
                    .flex_none()
                    .py(px(space::XS))
                    .px(px(space::MD))
                    .border_t_1()
                    .border_color(th.border)
                    .child(txt(&th, BODY_SM, format!("+ {} more from {}", fmt::fmt_int(s.remaining as u64), s.label)).color(th.muted))
            });
            div().flex_1().min_h_0().flex().flex_col().child(header(&th, "rail.queue.header", label, None)).child(div().relative().flex_1().min_h_0().child(list).child(bar)).children(footer)
        };

        pid(
            "rail",
            div()
                .w(px(layout::RAIL_W))
                .h_full()
                .flex_none()
                .flex()
                .flex_col()
                .min_h_0()
                .bg(if drawer { th.low } else { th.chrome })
                .border_l_1()
                .border_color(th.frame)
                .child(deck)
                .child(queue_section),
        )
    }
}

/// `QueueRail.svelte: draw()`: bars of `barW = w / n` with a 1 px gap, `max(1, v * h)` tall, on a canvas whose bitmap
/// is the CSS size (so Chromium upsamples it by the device scale factor).
fn rail_spectrum(th: Theme, bands: Option<Vec<u8>>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |b, _, window, _| {
            let Some(bands) = &bands else { return };
            let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
            let n = bands.len().max(1);
            let bar_w = w / n as f32;
            for (i, v) in bands.iter().enumerate() {
                let bh = (*v as f32 / 255.0 * h).max(1.0);
                window.paint_quad(fill(Bounds::new(point(b.origin.x + px(i as f32 * bar_w), b.origin.y + px(h - bh)), size(px(bar_w - 1.0), px(bh))), th.meter));
            }
        },
    )
    .size_full()
}
