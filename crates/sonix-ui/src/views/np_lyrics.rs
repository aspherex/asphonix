//! The lyrics panel: synced lines highlight and auto-scroll with the playback position (centered, paused for 4 s
//! after the user scrolls or presses), click a synced line to seek, plain lyrics just scroll, honest loading / none states.
use super::nowplaying::NowPlayingView;
use crate::anim::cubic_bezier;
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{BODY_LG, HEADLINE_SM, LABEL_CAPS, MONO_CODE, MONO_DATA};
use crate::widgets::tooltip::Tip;
use gpui::{AnyElement, Context, CursorStyle, IntoElement, MouseButton, ScrollHandle, ScrollWheelEvent, Window, div, point, prelude::*, px};
use sonix_api::*;
use std::collections::HashMap;
use std::rc::Rc;

pub enum Load {
    Loading,
    None,
    Ok(Rc<Lyrics>),
}

/// How long the user's own scrolling wins over the auto-scroll.
const USER_SCROLL_HOLD: f64 = 4.0;
/// Duration of the smooth auto-scroll to the current line (s).
const SMOOTH: f64 = 0.3;

pub struct LyricsPanel {
    pub for_id: Option<String>,
    pub load: Load,
    memo: HashMap<String, Option<Rc<Lyrics>>>,
    pub scroll: ScrollHandle,
    last_user: f64,
    /// (from y, to y, start time) of a running smooth scroll
    anim: Option<(f32, f32, f64)>,
    last_active: i64,
}

impl Default for LyricsPanel {
    fn default() -> Self {
        Self { for_id: None, load: Load::Loading, memo: HashMap::new(), scroll: ScrollHandle::new(), last_user: f64::NEG_INFINITY, anim: None, last_active: -2 }
    }
}

impl NowPlayingView {
    /// Fetches lyrics when the track changes (memoized in a small map).
    pub(super) fn lyrics_ensure(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.ly.for_id.as_deref() == Some(id) {
            return;
        }
        self.ly.for_id = Some(id.to_string());
        self.ly.last_active = -2;
        self.ly.anim = None;
        self.ly.scroll.set_offset(point(px(0.), px(0.)));
        if let Some(m) = self.ly.memo.get(id) {
            self.ly.load = m.clone().map(Load::Ok).unwrap_or(Load::None);
            return;
        }
        self.ly.load = Load::Loading;
        let (b, want) = (self.backend.clone(), id.to_string());
        cx.spawn(async move |this, cx| {
            let w = want.clone();
            let r = cx.background_executor().spawn(async move { b.lyrics_get(&w) }).await;
            let _ = this.update(cx, |t, cx| {
                let l = r.ok().flatten().map(Rc::new);
                if t.ly.memo.len() > 20 {
                    t.ly.memo.clear();
                }
                t.ly.memo.insert(want.clone(), l.clone());
                if t.ly.for_id.as_deref() == Some(want.as_str()) {
                    t.ly.load = l.map(Load::Ok).unwrap_or(Load::None);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The lyrics section for a column `panel_w` wide.
    pub(super) fn lyrics_panel(&mut self, th: Theme, position_sec: f64, panel_w: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let now = crate::clock::now();
        // a wheel notch the smooth-scroll host consumed counts as the user scrolling (pauses the auto-scroll)
        let key = std::ptr::addr_of!(self.ly.scroll) as usize;
        if let Some(t) = crate::widgets::smooth::last_wheel(key)
            && t > self.ly.last_user
        {
            self.ly.last_user = t;
            self.ly.anim = None;
        }
        let lyrics = match &self.ly.load {
            Load::Ok(l) => Some(l.clone()),
            _ => None,
        };
        // index of the line being sung: the last one that started at or before the position
        let active: i64 = match &lyrics {
            Some(l) if l.synced => {
                let ms = position_sec * 1000.0;
                l.lines.iter().rposition(|x| x.start_ms.unwrap_or(0) as f64 <= ms).map(|i| i as i64).unwrap_or(-1)
            }
            _ => -1,
        };
        // keep the current line centered, unless the user scrolled on their own recently
        if active >= 0 && active != self.ly.last_active && now - self.ly.last_user >= USER_SCROLL_HOLD {
            if let Some(ib) = self.ly.scroll.bounds_for_item(active as usize + 1) {
                let vb = self.ly.scroll.bounds();
                let off = self.ly.scroll.offset();
                let delta = f32::from(ib.center().y) - f32::from(vb.center().y);
                let max = f32::from(self.ly.scroll.max_offset().height);
                let to = (f32::from(off.y) - delta).clamp(-max, 0.0);
                self.ly.anim = Some((f32::from(off.y), to, now));
                self.ly.last_active = active;
            }
        } else if active < 0 {
            self.ly.last_active = active;
        }
        if let Some((from, to, start)) = self.ly.anim {
            let u = (((now - start) / SMOOTH) as f32).clamp(0.0, 1.0);
            let y = from + (to - from) * cubic_bezier(0.42, 0.0, 0.58, 1.0, u);
            self.ly.scroll.set_offset(point(px(0.), px(y)));
            if u >= 1.0 {
                self.ly.anim = None;
            } else {
                window.request_animation_frame();
            }
        }

        let (sbar, overflow) = crate::widgets::scrollbar::passive(th, &self.ly.scroll);
        let content_w = (panel_w - 2.0 - 48.0 - if overflow { crate::metrics::tracks::SCROLLBAR_W } else { 0.0 }).max(0.0);
        let pad_block = 0.3 * content_w; // 30 % of the list width above the first and below the last line
        let head_right: Option<String> = lyrics.as_ref().map(|l| format!("{} · {}", if l.synced { "SYNCED" } else { "PLAIN" }, if l.source == LyricsSource::Opensubsonic { "OPENSUBSONIC" } else { "SUBSONIC" }));
        let head = div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .h(px(26.))
                .flex_none()
                .px(px(12.))
                .bg(th.mid)
                .border_b_1()
                .border_color(th.border)
                .child(txt(&th, LABEL_CAPS, "LYRICS").color(th.accent_fg))
                .children(head_right.map(|r| txt(&th, MONO_CODE, r).color(th.muted)));
        let mut scroll = div()
            .id("np-lyrics-scroll")
            .track_scroll(&self.ly.scroll)
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .pl(px(24.))
            .pr(px(24.0 + if overflow { crate::metrics::tracks::SCROLLBAR_W } else { 0.0 }))
            .py(px(16.))
            .size_full()
            .on_scroll_wheel(cx.listener(|this, _: &ScrollWheelEvent, _, _| {
                this.ly.last_user = crate::clock::now();
                this.ly.anim = None;
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, _| this.ly.last_user = crate::clock::now()));
        match (&self.ly.load, &lyrics) {
            (Load::Loading, _) => scroll = scroll.child(div().flex().justify_center().pt(px(24.)).child(txt(&th, MONO_DATA, "LOADING…").color(th.muted))),
            (Load::None, _) | (Load::Ok(_), None) => scroll = scroll.child(div().flex().justify_center().pt(px(24.)).child(txt(&th, MONO_DATA, "No lyrics available").color(th.muted))),
            (Load::Ok(_), Some(l)) => {
                // child 0 = top spacer, 1.. = the lines (so `bounds_for_item(i + 1)` is line i), last = bottom spacer
                scroll = scroll.child(div().h(px(pad_block)).flex_none());
                for (i, line) in l.lines.iter().enumerate() {
                    let text = if line.text.is_empty() { String::new() } else { line.text.clone() };
                    let cur = i as i64 == active;
                    let past = active >= 0 && (i as i64) < active;
                    let mut row = div().id(("ly", i)).flex_none().w_full().when(i > 0, |d| d.mt(px(4.))).when(past, |d| d.opacity(0.6));
                    if let Some(ms) = line.start_ms {
                        let col = if cur { th.accent_fg } else { th.muted };
                        let spec = crate::tracking::TypeSpec { caps: false, ..HEADLINE_SM };
                        row = row
                            .cursor(CursorStyle::PointingHand)
                            .tip("Seek to this line", th)
                            .text_color(col)
                            .hover(move |s| if cur { s } else { s.text_color(th.text) })
                            .child(crate::typo::ts(&th, spec).child(text))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                let pos = ms as f64 / 1000.0;
                                crate::app::call(cx, move |b| drop(b.player_seek(SeekRequest { position_sec: pos })));
                            }));
                    } else {
                        row = row.child(crate::typo::ts(&th, BODY_LG).text_color(th.text).child(text));
                    }
                    scroll = scroll.child(row);
                }
                scroll = scroll.child(div().h(px(pad_block)).flex_none());
            }
        }
        div().flex().flex_col().min_h_0().min_w_0().h_full().flex_1().border_1().border_color(th.border).bg(th.low).child(head).child(div().relative().flex_1().min_h_0().child(scroll).child(sbar).child(crate::widgets::smooth::drive(window, crate::widgets::smooth::Scrollable::div(key, &self.ly.scroll))))
        .into_any_element()
    }
}
