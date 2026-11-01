//! Track-change preview card (`overlay/Overlay.svelte` + `overlay.rs`): a second, tiny, always-on-top window shown without
//! taking the focus, 4.5 s, then faded out and **destroyed** (nothing of it stays resident after it closes).
//!
//! Timings are the web's: the page paints first (cover preloaded, capped at 400 ms), then `opacity/transform .2s ease-out`
//! (`translateY(8px)` -> 0); `SHOW_FOR` 4500 ms; the fade-out is the same transition in reverse (`FADE` 220 ms) and the
//! window is destroyed after it. A generation counter keeps a stale timer from closing a card that got a newer track.
use crate::anim::cubic_bezier;
use crate::app;
use crate::art::{self, ArtHandle};
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{BODY_SM, HEADLINE_SM, LABEL_CAPS, MONO_DATA};
use crate::widgets::icon;
use gpui::{Context, CursorStyle, EventEmitter, IntoElement, MouseButton, Render, Window, div, prelude::*, px};
use sonix_api::{Backend, Track, format as fmt};
use std::sync::Arc;

pub const WIDTH: f32 = 360.0;
pub const HEIGHT: f32 = 96.0;
pub const MARGIN: f32 = 16.0;
/// How long the card stays up after it was shown or last updated.
pub const SHOW_FOR_MS: u64 = 4500;
/// The page's fade-out before the window is destroyed.
pub const FADE_MS: u64 = 220;
const TRANSITION: f64 = 0.2;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Corner {
    #[default]
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    /// `OverlayCorner::from_u8` (the persisted setting).
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Corner::TopRight,
            2 => Corner::BottomLeft,
            3 => Corner::BottomRight,
            _ => Corner::TopLeft,
        }
    }

    /// Top-left of a `w`x`h` card `m` px inside the work area (`overlay.rs OverlayCorner::place`, physical px).
    pub fn place(self, (ax, ay, aw, ah): (i32, i32, i32, i32), w: i32, h: i32, m: i32) -> (i32, i32) {
        let (left, right, top, bottom) = (ax + m, ax + aw - w - m, ay + m, ay + ah - h - m);
        match self {
            Corner::TopLeft => (left, top),
            Corner::TopRight => (right, top),
            Corner::BottomLeft => (left, bottom),
            Corner::BottomRight => (right, bottom),
        }
    }
}

/// `overlay.rs should_show`: only on an actual track change into a track that starts playing, never while the app is focused.
pub fn should_show(prev_track_id: Option<&str>, st: &sonix_api::PlayerState, enabled: bool, main_focused: bool) -> bool {
    let Some(t) = st.track.as_ref() else { return false };
    enabled && !main_focused && prev_track_id != Some(t.id.as_str()) && matches!(st.status, sonix_api::PlaybackStatus::Loading | sonix_api::PlaybackStatus::Playing)
}

pub enum CardEvent {
    /// The card was clicked: bring the main window up (and close the card).
    OpenMain,
}
impl EventEmitter<CardEvent> for CardView {}

pub struct CardView {
    backend: Arc<dyn Backend>,
    art: ArtHandle,
    pub track: Track,
    /// Clock time the card became `shown` (fade in), or started hiding.
    since: f64,
    pub shown: bool,
    /// Physical rect (x, y, w, h) the native window is placed at when it is first made visible.
    pub frames: u64,
}

impl CardView {
    pub fn new(backend: Arc<dyn Backend>, art: ArtHandle, track: Track, _place: (i32, i32, i32, i32)) -> Self {
        Self { backend, art, track, since: crate::clock::now(), shown: true, frames: 0 }
    }
    pub fn set_track(&mut self, t: Track, cx: &mut Context<Self>) {
        self.track = t;
        if !self.shown {
            self.shown = true;
            self.since = crate::clock::now();
        }
        cx.notify();
    }
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        if self.shown {
            self.shown = false;
            self.since = crate::clock::now();
            cx.notify();
        }
    }
}

fn ease_out(u: f32) -> f32 {
    cubic_bezier(0.0, 0.0, 0.58, 1.0, u)
}

impl Render for CardView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th: Theme = app::theme(cx);
        self.frames += 1;
        let now = crate::clock::now();
        let u = (((now - self.since) / TRANSITION) as f32).clamp(0.0, 1.0);
        let e = ease_out(u);
        let k = if self.shown { e } else { 1.0 - e };
        if u < 1.0 && !crate::clock::frozen() {
            window.request_animation_frame();
        }
        let hover = crate::anim::transition(window, ("card-hover", 0), if crate::anim::hovered(0xCA2D) { 1.0 } else { 0.0 }, 0.12);
        let border = crate::anim::mix(th.border, th.accent_fg, hover);
        let t = &self.track;
        let cover = art::cover(&self.art, &self.backend, t.cover_art.as_deref(), 78.0, window, cx);
        let sub = [Some(t.artist.clone()), t.album.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        div()
            .size_full()
            .bg(th.base)
            .child(
                div()
                    .id("card")
                    .relative()
                    .top(px(8.0 * (1.0 - k)))
                    .opacity(k)
                    .w_full()
                    .h_full()
                    .flex()
                    .flex_row()
                    .gap(px(12.))
                    .pt(px(8.))
                    .pb(px(8.))
                    .pr(px(12.))
                    .pl(px(8.))
                    .bg(th.low)
                    .border_1()
                    .border_l(px(4.))
                    .border_color(border)
                    .cursor(CursorStyle::PointingHand)
                    .on_hover(|h, window, _| {
                        crate::anim::set_hovered(0xCA2D, *h);
                        window.refresh();
                    })
                    // the 4 px left rule is always the accent (`border-left: 4px solid var(--color-accent-fg)`)
                    .child(div().absolute().left(px(-4.)).top(px(-1.)).w(px(4.)).h(px(HEIGHT)).bg(th.accent_fg))
                    .child(div().size(px(78.)).flex_none().bg(th.high).overflow_hidden().child(cover))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .justify_center()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(4.))
                                    .child(icon("next", 10., th.accent_fg))
                                    .child(txt(&th, LABEL_CAPS, "Now playing").color(th.accent_fg))
                                    .child(div().flex_1())
                                    .child(div().flex().flex_row().child(txt(&th, LABEL_CAPS, "SONIX").color(th.muted)).child(txt(&th, LABEL_CAPS, "//").color(th.accent_fg))),
                            )
                            .child(div().flex().min_w_0().child(txt(&th, HEADLINE_SM, t.title.clone()).color(th.text).ellipsis()))
                            .child(div().flex().min_w_0().child(txt(&th, BODY_SM, sub).color(th.muted).ellipsis()))
                            .child(txt(&th, MONO_DATA, fmt::format_track_format(&t.format)).color(th.muted)),
                    )
                    .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, cx| cx.emit(CardEvent::OpenMain))),
            )
    }
}
