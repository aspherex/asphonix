//! Minimal queue rail (`QueueRail.svelte`, phase 1 subset): the deck (current track), the
//! "NEXT FROM <label> · SHUFFLE" window and the real "+ N more" footer. Docked at >= 1440 px,
//! a drawer below. Not yet ported: drag-reorder, multi-select, remove, user-queue section
//! (docs/PORTING.md).
use crate::app::{self, PlayerEvent, PlayerModel};
use crate::art::{self, ArtHandle};
use crate::theme::Theme;
use crate::tracking::{BODY_SM, LABEL_CAPS, MONO_CODE, MONO_DATA, tracked};
use crate::typo::{ellipsis_line, ts};
use crate::widgets::icon;
use gpui::{ClickEvent, Context, CursorStyle, Entity, IntoElement, Render, SharedString, Subscription, Window, div, prelude::*, px};
use sonix_api::format as fmt;
use sonix_api::*;
use std::sync::Arc;

pub struct QueueRail {
    backend: Arc<dyn Backend>,
    art: ArtHandle,
    player: Entity<PlayerModel>,
    state: Option<QueueState>,
    _sub: Subscription,
}

impl QueueRail {
    pub fn new(backend: Arc<dyn Backend>, art: ArtHandle, player: &Entity<PlayerModel>, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe(player, |this: &mut Self, _, _: &PlayerEvent, cx| this.refresh(cx));
        let mut s = Self { backend, art, player: player.clone(), state: None, _sub: sub };
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

impl Render for QueueRail {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th: Theme = app::theme(cx);
        let ps = self.player.read(cx).state.clone();
        let backend = self.backend.clone();
        let vw = f32::from(window.viewport_size().width);
        let drawer = vw < 1440.0;
        let q = self.state.clone();
        let cur = q.as_ref().map(|q| q.current_index).unwrap_or(-1);
        let readout = fmt::output_readout(&ps, "");

        let deck = match &ps.track {
            Some(t) => {
                let cover = art::cover(&self.art, &backend, t.cover_art.as_deref(), 56.0, window, cx);
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.))
                    .p(px(12.))
                    .border_b_1()
                    .border_color(th.border)
                    .child(div().border_1().border_color(th.border).child(cover))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
                            .child(tracked("NOW PLAYING", LABEL_CAPS, th.body_font, th.accent_fg, window))
                            .child(ellipsis_line(ts(&th, crate::tracking::BODY_MD).text_color(th.text)).child(SharedString::from(t.title.clone())))
                            .child(ellipsis_line(ts(&th, MONO_DATA).text_color(th.muted)).child(SharedString::from(t.artist.clone())))
                            .child(ellipsis_line(ts(&th, MONO_CODE).text_color(if ps.bit_perfect { th.accent_fg } else { th.muted })).child(SharedString::from(readout))),
                    )
                    .into_any_element()
            }
            None => div().p(px(12.)).border_b_1().border_color(th.border).child(ts(&th, BODY_SM).text_color(th.muted).child("Nothing in the queue yet.")).into_any_element(),
        };

        let header_label = match q.as_ref().and_then(|q| q.source.as_ref()) {
            Some(s) => format!("Next from {}{}", s.label, if s.mode == SourceMode::Shuffle { " · Shuffle" } else { "" }),
            None => "Next in buffer".to_string(),
        };

        let mut list = div().id("queue-list").flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        if let Some(q) = &q {
            for (i, it) in q.items.iter().enumerate() {
                if (i as i64) <= cur {
                    continue;
                }
                let t = &it.track;
                let cover = art::cover(&self.art, &backend, t.cover_art.as_deref(), 32.0, window, cx);
                let idx = i;
                list = list.child(
                    div()
                        .id(("qrow", i))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.))
                        .h(px(44.))
                        .px(px(12.))
                        .flex_none()
                        .border_b_1()
                        .border_color(th.mid)
                        .cursor(CursorStyle::PointingHand)
                        .hover(move |s| s.bg(th.mid))
                        .child(cover)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(ellipsis_line(ts(&th, crate::tracking::BODY_SM).text_color(th.text)).child(SharedString::from(t.title.clone())))
                                .child(ellipsis_line(ts(&th, MONO_CODE).text_color(th.muted)).child(SharedString::from(t.artist.clone()))),
                        )
                        .child(ts(&th, MONO_CODE).flex_none().text_color(th.muted).child(SharedString::from(fmt::format_duration(Some(t.duration)))))
                        .when(it.origin == QueueItemOrigin::User, |d| d.child(div().text_color(th.accent_fg).child(icon("add", 11., th.accent_fg))))
                        .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                            app::call(cx, move |b| {
                                let _ = b.player_jump_to(idx);
                            });
                        })),
                );
            }
        }
        let footer = q.as_ref().and_then(|q| q.source.as_ref()).filter(|s| s.remaining > 0).map(|s| {
            ts(&th, BODY_SM).p(px(12.)).flex_none().border_t_1().border_color(th.border).text_color(th.muted).child(SharedString::from(format!("+ {} more from {}", fmt::fmt_int(s.remaining as u64), s.label)))
        });
        let total = q.as_ref().map(|q| q.total).unwrap_or(0);

        div()
            .w(px(340.))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .min_h_0()
            .bg(if drawer { th.low } else { th.chrome })
            .border_l_1()
            .border_color(th.frame)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px(px(12.))
                    .h(px(36.))
                    .flex_none()
                    .border_b_1()
                    .border_color(th.border)
                    .child(tracked("QUEUE", LABEL_CAPS, th.body_font, th.text, window))
                    .child(ts(&th, MONO_CODE).text_color(th.muted).child(SharedString::from(format!("{total} QUEUED")))),
            )
            .child(deck)
            .child(div().px(px(12.)).pt(px(10.)).pb(px(6.)).flex_none().child(tracked(&header_label, LABEL_CAPS, th.body_font, th.muted, window)))
            .child(list)
            .children(footer)
    }
}
