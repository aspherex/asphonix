//! Now Playing (`NowPlaying.svelte`, "artwork dominant" layout): header strip with the real
//! source/output line, dominant cover with the source-format tag, title block, a 28-band FFT
//! with peak hold, stereo peak meters, a 240-bucket waveform seek bar and the full transport.
//! Animates on vsync (`request_animation_frame`) only while visible, playing and not
//! minimized; the backend is told to compute the spectrum only while this view is up.
use crate::app::{self, PlayerEvent, PlayerModel};
use crate::art::{self, ArtHandle};
use crate::playerkeys::PLAYER_KEYS;
use crate::router::Route;
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{BODY_LG, BODY_MD, BODY_SM, HEADLINE_LG, HEADLINE_SM, LABEL_CAPS, MONO_CODE, MONO_DATA, MONO_METRIC, TypeSpec};
use crate::widgets::slider::{self, OnChange, SliderHandle, slider};
use crate::widgets::{hard_shadow, icon, icon_g};
use crate::widgets::tooltip::Tip;
use gpui::{
    AnyElement, Bounds, Context, CursorStyle, Div, ElementId, Entity, Hsla, IntoElement, MouseButton, Render, SharedString, Stateful, Subscription, Window, canvas, div, fill, point, prelude::*, px, size,
};
use sonix_api::format as fmt;
use sonix_api::*;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use crate::metrics::spectrum as sp;
use crate::probe::pid;


pub struct NowPlayingView {
    add_btn: Rc<Cell<Bounds<gpui::Pixels>>>,
    pub(super) backend: Arc<dyn Backend>,
    art: ArtHandle,
    player: Entity<PlayerModel>,
    visible: bool,
    suspended: bool,
    fft: Fft,
    wave_for: Option<String>,
    wave: Option<Vec<u8>>,
    wave_seek: SliderHandle,
    vol: SliderHandle,
    seek_preview: Option<f32>,
    cover_area_h: Rc<Cell<f32>>,
    device: String,
    wave_filled: usize,
    wave_live: bool,
    wave_gen: u64,
    wave_hover: Option<f32>,
    upnext_open: bool,
    source: Option<QueueSourceInfo>,
    upcoming: Vec<QueueItem>,
    next_start: usize,
    queue_total: usize,
    queue_key: (usize, String, bool),
    queue_seq: u64,
    pub(super) lyrics_open: bool,
    pub(super) ly: super::np_lyrics::LyricsPanel,
    np_scroll: gpui::ScrollHandle,
    last_audible: f64,
    _sub: Subscription,
}

impl NowPlayingView {
    pub fn new(backend: Arc<dyn Backend>, art: ArtHandle, player: &Entity<PlayerModel>, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe(player, |this: &mut Self, _, _: &PlayerEvent, cx| this.on_track_change(cx));
        let mut s = Self {
            backend,
            art,
            player: player.clone(),
            visible: false,
            suspended: false,
            fft: Fft::default(),
            wave_for: None,
            wave: None,
            wave_seek: slider::new_handle(),
            vol: slider::new_handle(),
            seek_preview: None,
            cover_area_h: Rc::new(Cell::new(0.0)),
            device: String::new(),
            add_btn: Rc::new(Cell::new(Bounds::default())),
            wave_filled: 0,
            wave_live: false,
            wave_gen: 0,
            wave_hover: None,
            upnext_open: false,
            source: None,
            upcoming: Vec::new(),
            next_start: 0,
            queue_total: 0,
            queue_key: (usize::MAX, String::new(), false),
            queue_seq: 0,
            lyrics_open: false,
            ly: Default::default(),
            np_scroll: gpui::ScrollHandle::new(),
            last_audible: 2.0 / 3.0,
            _sub: sub,
        };
        s.fetch_device(cx);
        s
    }

    fn fetch_device(&mut self, cx: &mut Context<Self>) {
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let st = cx.background_executor().spawn(async move { b.output_get_status() }).await;
            let _ = this.update(cx, |t, cx| {
                t.device = st.ok().and_then(|s| s.device_name).unwrap_or_default();
                cx.notify();
            });
        })
        .detach();
    }

    fn spectrum_wanted(&self) -> bool {
        self.visible && !self.suspended
    }

    /// The shell owns `player_set_spectrum_active` (Now Playing or the rail needs frames).
    fn sync_spectrum(&self, _cx: &mut Context<Self>) {}

    pub fn set_visible(&mut self, v: bool, cx: &mut Context<Self>) {
        if self.visible == v {
            return;
        }
        self.visible = v;
        self.fft.reset_clock();
        self.sync_spectrum(cx);
        if v {
            self.on_track_change(cx);
        }
        cx.notify();
    }

    pub fn set_suspended(&mut self, s: bool, cx: &mut Context<Self>) {
        self.suspended = s;
        if s {
            self.wave = None;
            self.wave_for = None;
        } else {
            self.fft.reset_clock();
            self.on_track_change(cx);
        }
        self.sync_spectrum(cx);
        cx.notify();
    }

    fn on_track_change(&mut self, cx: &mut Context<Self>) {
        if !self.visible || self.suspended {
            return;
        }
        let (id, live) = {
            let m = self.player.read(cx);
            (m.current_id().map(str::to_string), matches!(m.state.status, PlaybackStatus::Playing | PlaybackStatus::Paused))
        };
        // `Waveform.svelte`: the scan reads playback's own download, so a track asked about before it was live may have
        // answered "no waveform" (done, nothing filled): ask once more when it turns live while the bar is still empty.
        let became_live = live && !self.wave_live;
        self.wave_live = live;
        if id == self.wave_for {
            if let Some(id) = id.filter(|_| became_live) {
                self.poll_waveform(id, cx);
            }
            cx.notify();
            return;
        }
        self.wave_for = id.clone();
        self.wave = None;
        self.wave_filled = 0;
        if let Some(id) = id {
            self.poll_waveform(id, cx);
        }
        cx.notify();
    }
}

/// Port of `np/FftPanel.svelte`'s dynamics (constants in `metrics::spectrum`, copied from the web source):
/// the bars show the latest frame, a cap holds each band's peak and falls at 0.45 of full scale per second, the
/// stereo meters fall at 30 dB/s with a 1.2 s peak hold, and a stalled stream (> 120 ms without a frame) lets the
/// bars fall at 2.2 x full scale per second. Time is the shared clock; draws are at most 20 ms apart.
pub struct Fft {
    bands: Vec<f32>, // latest frame, 0..255
    caps: Vec<f32>,  // peak hold, 0..1
    level: [f32; 2], // smoothed L/R, dBFS
    hold: [f32; 2],
    hold_at: [f64; 2],
    last_frame_at: f64,
    last_draw: Option<f64>,
    seen_seq: u64,
}

impl Default for Fft {
    fn default() -> Self {
        Self { bands: Vec::new(), caps: Vec::new(), level: [sp::FLOOR_DB; 2], hold: [sp::FLOOR_DB; 2], hold_at: [0.0; 2], last_frame_at: 0.0, last_draw: None, seen_seq: 0 }
    }
}

impl Fft {
    fn reset_clock(&mut self) {
        self.last_draw = None;
    }

    /// `clearAll()`: nothing audible.
    fn clear(&mut self) {
        *self = Self { seen_seq: self.seen_seq, ..Self::default() };
    }

    /// `onFrame`: copy the bands, raise the meters, restart the peak hold.
    fn on_frame(&mut self, f: &SpectrumFrame, now: f64) {
        if self.bands.len() != f.bands.len() {
            self.bands = vec![0.0; f.bands.len()];
            self.caps = vec![0.0; f.bands.len()];
        }
        for (d, s) in self.bands.iter_mut().zip(&f.bands) {
            *d = *s as f32;
        }
        self.last_frame_at = now;
        for (c, peak) in [f.peak_l, f.peak_r].into_iter().enumerate() {
            let p = peak.max(sp::FLOOR_DB);
            if p > self.level[c] {
                self.level[c] = p;
            }
            if p >= self.hold[c] {
                self.hold[c] = p;
                self.hold_at[c] = now;
            }
        }
    }

    /// `frame(now)`: release step; returns whether anything is still moving (keep requesting frames).
    fn frame(&mut self, now: f64) -> bool {
        if let Some(last) = self.last_draw
            && (now - last) * 1000.0 < sp::MIN_DRAW_MS as f64
        {
            return true;
        }
        let dt = self.last_draw.map(|l| ((now - l) as f32).min(sp::MAX_DT)).unwrap_or(0.016);
        self.last_draw = Some(now);
        let mut busy = false;
        for (cap, band) in self.caps.iter_mut().zip(&self.bands) {
            let target = band / 255.0;
            *cap = if target >= *cap { target } else { target.max(*cap - sp::CAP_FALL_PER_S * dt) };
            busy |= *cap > 0.002;
        }
        for c in 0..2 {
            if self.level[c] > sp::FLOOR_DB {
                self.level[c] = (self.level[c] - sp::METER_FALL_DB_PER_S * dt).max(sp::FLOOR_DB);
                busy = true;
            }
            if self.hold[c] > sp::FLOOR_DB && (now - self.hold_at[c]) * 1000.0 > sp::PEAK_HOLD_MS as f64 {
                self.hold[c] = self.level[c].max(self.hold[c] - sp::METER_FALL_DB_PER_S * dt);
                busy = true;
            }
        }
        if busy {
            if (now - self.last_frame_at) * 1000.0 > sp::STALE_AFTER_MS as f64 {
                for b in &mut self.bands {
                    *b = (*b - 255.0 * sp::STALE_FALL_PER_S * dt).max(0.0);
                }
            }
        } else {
            self.last_draw = None;
        }
        busy
    }
}

/// `FftPanel.drawSpectrum` on a canvas of the element's device-pixel size: bar gap `max(1, round(dpr))` device px,
/// bar edges rounded to whole device px, cap line `round(2 * dpr)` px thick.
fn spectrum_canvas(th: Theme, bands: Vec<f32>, caps: Vec<f32>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |b, _, window, _| {
            let s = window.scale_factor();
            let (ox, oy) = (f32::from(b.origin.x) * s, f32::from(b.origin.y) * s);
            let (w, h) = ((f32::from(b.size.width) * s).round(), (f32::from(b.size.height) * s).round());
            let n = caps.len();
            if n == 0 {
                return;
            }
            let gap = s.round().max(1.0);
            let cap_h = (2.0 * s).round().max(1.0);
            let bar_w = w / n as f32;
            let dev = |x: f32, y: f32, ww: f32, hh: f32| Bounds::new(point(px((ox + x) / s), px((oy + y) / s)), size(px(ww / s), px(hh / s)));
            for (i, cap) in caps.iter().enumerate().take(n) {
                let x = (i as f32 * bar_w).round();
                let bw = ((i as f32 + 1.0) * bar_w).round() - x - gap;
                let v = (bands.get(i).copied().unwrap_or(0.0) / 255.0).clamp(0.0, 1.0);
                let bh = (v * h).round().max(1.0);
                window.paint_quad(fill(dev(x, h - bh, bw, bh), th.meter));
                let ch = (cap * h).round().max(1.0);
                window.paint_quad(fill(dev(x, (h - ch - (2.0 * s).round()).max(0.0), bw, cap_h), th.text));
            }
        },
    )
    .size_full()
}

// ===================================================================== Now Playing layout (web `NowPlaying.svelte`)

const LOSSLESS_FMT: [&str; 10] = ["flac", "alac", "wav", "aiff", "aif", "ape", "wv", "dsf", "dff", "tta"];

fn with_size(base: TypeSpec, size: f32, line: f32) -> TypeSpec {
    TypeSpec { size, line, ..base }
}

/// `spectrum.ts bandRangeHz`
pub fn band_range_hz(rate: f64) -> (f64, f64) {
    let nyq = rate / 2.0;
    ((40.0f64).min(nyq * 0.9), nyq * 0.98)
}
/// `spectrum.ts formatHz`: "50 HZ" / "1 KHZ" / "21.6 KHZ".
pub fn format_hz(hz: f64) -> String {
    if hz < 1000.0 {
        return format!("{} HZ", hz.round() as i64);
    }
    let k = hz / 1000.0;
    if k.fract() == 0.0 { format!("{} KHZ", k as i64) } else { format!("{:.1} KHZ", k) }
}

/// Text on a canvas (CSS px geometry): one shaped line painted at `x`, vertically centred on `mid_y`.
#[allow(clippy::too_many_arguments)]
fn canvas_text(window: &mut Window, cx: &mut gpui::App, s: &str, size: f32, weight: f32, color: Hsla, x: f32, mid_y: f32, right_align: bool) {
    let mut f = gpui::font(crate::theme::MONO);
    f.weight = gpui::FontWeight(weight);
    let run = gpui::TextRun { len: s.len(), font: f, color, background_color: None, underline: None, strikethrough: None };
    let line = window.text_system().shape_line(SharedString::from(s.to_string()), px(size), &[run], None);
    let w = f32::from(line.width);
    let ox = if right_align { x - w } else { x };
    let _ = line.paint(point(px(ox), px(mid_y - size * 0.6)), px(size * 1.2), window, cx);
}

/// `FftPanel.drawMeters` on the canvas' device-pixel box: `L`/`R` labels, 20 segments, held-peak value.
fn meter_canvas(th: Theme, level: [f32; 2], hold: [f32; 2]) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |b, _, window, cx| {
            let s = window.scale_factor();
            let (ox, oy) = (f32::from(b.origin.x) * s, f32::from(b.origin.y) * s);
            let (w, h) = ((f32::from(b.size.width) * s).round(), (f32::from(b.size.height) * s).round());
            let label_w = (12.0 * s).round();
            let value_w = (38.0 * s).round();
            let row_gap = (4.0 * s).round();
            let row_h = 2f32.max(((14.0 * s).round()).min(((h - row_gap) / 2.0).floor()));
            let top = ((h - (row_h * 2.0 + row_gap)) / 2.0).round();
            let bar_w = w - label_w - value_w;
            let seg_gap = s.round().max(1.0);
            let seg_w = (bar_w - seg_gap * (sp::SEGMENTS as f32 - 1.0)) / sp::SEGMENTS as f32;
            let dev = |x: f32, y: f32, ww: f32, hh: f32| Bounds::new(point(px((ox + x) / s), px((oy + y) / s)), size(px(ww / s), px(hh / s)));
            for ch in 0..2 {
                let y = top + ch as f32 * (row_h + row_gap);
                let mid = y + row_h / 2.0;
                canvas_text(window, cx, if ch == 0 { "L" } else { "R" }, 9.0, 400.0, th.muted, ox / s, (oy + mid) / s, false);
                let lit = (((level[ch] - sp::FLOOR_DB) / -sp::FLOOR_DB) * sp::SEGMENTS as f32).round() as i32;
                let hold_seg = (((hold[ch] - sp::FLOOR_DB) / -sp::FLOOR_DB) * sp::SEGMENTS as f32).round() as i32 - 1;
                for i in 0..sp::SEGMENTS as i32 {
                    let x = (label_w + i as f32 * (seg_w + seg_gap)).round();
                    let seg_db = sp::FLOOR_DB + ((i + 1) as f32 / sp::SEGMENTS as f32) * -sp::FLOOR_DB;
                    let hot = seg_db > sp::CLIP_FROM_DB;
                    let col = if i < lit {
                        if hot { th.clip } else { th.meter }
                    } else if i == hold_seg && hold[ch] > sp::FLOOR_DB {
                        if hot { th.clip } else { th.text }
                    } else {
                        th.highest
                    };
                    window.paint_quad(fill(dev(x, y, seg_w.round().max(1.0), row_h), col));
                }
                let val = if hold[ch] <= sp::FLOOR_DB { "-INF".to_string() } else { format!("{:.1}", hold[ch]) };
                canvas_text(window, cx, &val, 9.0, 400.0, if hold[ch] > sp::CLIP_FROM_DB { th.clip } else { th.muted }, (ox + w) / s, (oy + mid) / s, true);
            }
        },
    )
    .size_full()
}

/// Everything `Waveform.svelte draw()` needs, in one value so the canvas closure owns it.
#[derive(Clone)]
struct WaveDraw {
    buckets: Vec<u8>,
    filled: usize,
    ratio: f32,
    hover: Option<f32>,
    dragging: bool,
    loop_a: Option<f32>,
    loop_b: Option<f32>,
}

/// `Waveform.svelte draw()`, line for line, in device pixels of the canvas box.
fn wave_canvas(th: Theme, d: WaveDraw) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |b, _, window, cx| {
            let s = window.scale_factor();
            // Chromium snaps the canvas to whole device pixels (277 css px * 1.5 = 415.5 -> 416); fractional quads blur
            let (ox, oy) = ((f32::from(b.origin.x) * s).round(), (f32::from(b.origin.y) * s).round());
            let (w, h) = ((f32::from(b.size.width) * s).round(), (f32::from(b.size.height) * s).round());
            let dev = |x: f32, y: f32, ww: f32, hh: f32| Bounds::new(point(px((ox + x) / s), px((oy + y) / s)), size(px(ww / s), px(hh / s)));
            let n = if d.buckets.is_empty() { 240 } else { d.buckets.len() };
            let bar_w = w / n as f32;
            let gap = s.round().max(1.0);
            let mid = h / 2.0;
            let played_x = d.ratio * w;
            let mut top = 64.0f32;
            for i in 0..d.filled.min(d.buckets.len()) {
                top = top.max(d.buckets[i] as f32);
            }
            if let Some(a) = d.loop_a {
                let ax = a * w;
                let bx = d.loop_b.map(|b| b * w).unwrap_or(ax);
                window.paint_quad(fill(dev(ax, 0.0, (bx - ax).max(0.0), h), th.meter.opacity(0.14)));
            }
            for i in 0..n {
                let x = (i as f32 * bar_w).round();
                let bw = (((i as f32 + 1.0) * bar_w).round() - x - gap).max(1.0);
                let known = i < d.filled;
                let bh = if known { (2.0 * s).max(d.buckets.get(i).copied().unwrap_or(0) as f32 / top * (h - 4.0 * s)) } else { s.round().max(1.0) };
                let played = x + bw / 2.0 <= played_x;
                let col = if played {
                    th.meter.opacity(if known { 1.0 } else { 0.5 })
                } else {
                    th.muted.opacity(if known { 0.38 } else { 0.5 })
                };
                window.paint_quad(fill(dev(x, (mid - bh / 2.0).round(), bw, bh.round()), col));
            }
            let ph_w = (2.0 * s).round().max(1.0);
            window.paint_quad(fill(dev(played_x.min(w - 2.0 * s).round(), 0.0, ph_w, h), th.text));
            if let (Some(hv), false) = (d.hover, d.dragging) {
                window.paint_quad(fill(dev((hv * w).round(), 0.0, s.round().max(1.0), h), th.text.opacity(0.5)));
            }
            for (t, label) in [(d.loop_a, "A"), (d.loop_b, "B")] {
                let Some(t) = t else { continue };
                let x = (t * w).round();
                window.paint_quad(fill(dev(x, 0.0, ph_w, h), th.meter));
                let fw = (11.0 * s).round();
                let fx = if label == "A" { x } else { x - fw + ph_w };
                let fx = fx.max(0.0);
                window.paint_quad(fill(dev(fx, 0.0, fw, (12.0 * s).round()), th.meter));
                let mut f = gpui::font(crate::theme::MONO);
                f.weight = gpui::FontWeight(700.0);
                let run = gpui::TextRun { len: 1, font: f, color: th.base, background_color: None, underline: None, strikethrough: None };
                let line = window.text_system().shape_line(SharedString::from(label), px(9.0), &[run], None);
                let lw = f32::from(line.width);
                let _ = line.paint(point(px((ox + fx + fw / 2.0) / s - lw / 2.0), px((oy + (2.0 * s).round()) / s)), px(11.0), window, cx);
            }
        },
    )
    .size_full()
}

/// A bordered chip button (`.hbtn`, `.abtn`, `.t`): hover or `on` = accent text + border (instant, like the CSS).
fn chip(id: impl Into<ElementId>, th: Theme, h: f32, pad_x: f32, bg: Hsla, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .group("hv")
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .h(px(h))
        .px(px(pad_x))
        .flex_none()
        .bg(bg)
        .border_1()
        .border_color(if on { th.accent_fg } else { th.border })
        .text_color(if on { th.accent_fg } else { th.text })
        .cursor(CursorStyle::PointingHand)
        .hover(move |s| s.text_color(th.accent_fg).border_color(th.accent_fg))
}
fn chip_icon(th: Theme, name: &str, size: f32, on: bool) -> gpui::Svg {
    icon_g(name, size, if on { th.accent_fg } else { th.text }, th.accent_fg)
}

impl NowPlayingView {
    /// `refreshQueue()`: source label + the next ten rows, only when the identity key changed.
    fn refresh_queue(&mut self, key: (usize, String, bool), cx: &mut Context<Self>) {
        if key == self.queue_key {
            return;
        }
        self.queue_key = key;
        self.queue_seq += 1;
        let mine = self.queue_seq;
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    let head = b.player_get_queue(PageRequest { offset: 0, limit: 1 }).ok()?;
                    let start = (head.current_index + 1).max(0) as usize;
                    let page = if head.current_index >= 0 { b.player_get_queue(PageRequest { offset: start, limit: 10 }).ok()?.items } else { Vec::new() };
                    Some((head.source, head.total, start, page))
                })
                .await;
            let _ = this.update(cx, |t, cx| {
                if t.queue_seq == mine
                    && let Some((source, total, start, items)) = r
                {
                    t.source = source;
                    t.queue_total = total;
                    t.next_start = start;
                    t.upcoming = items;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn poll_waveform(&mut self, id: String, cx: &mut Context<Self>) {
        self.wave_gen += 1;
        self.poll_waveform_gen(id, self.wave_gen, 0, cx);
    }

    /// `Waveform.svelte`'s `poll`: again in 400 ms while not done; a failed request is retried at 1.5 s, at most
    /// 3 tries per ask (`failures` counts them for this generation and, like the web, is never reset).
    fn poll_waveform_gen(&mut self, id: String, gen_: u64, failures: u32, cx: &mut Context<Self>) {
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let want = id.clone();
            let w = cx.background_executor().spawn(async move { b.player_get_waveform(&want) }).await;
            let _ = this.update(cx, |t, cx| {
                if t.wave_for.as_deref() != Some(id.as_str()) || t.wave_gen != gen_ {
                    return;
                }
                let (again, failures) = match w {
                    Ok(w) => {
                        let done = w.done;
                        t.wave_filled = w.filled;
                        t.wave = Some(w.buckets);
                        ((!done).then_some(400), failures)
                    }
                    Err(_) => ((failures + 1 < 3).then_some(1500), failures + 1),
                };
                if let Some(ms) = again {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(std::time::Duration::from_millis(ms)).await;
                        let _ = this.update(cx, |t, cx| {
                            if t.wave_for.as_deref() == Some(id.as_str()) && t.wave_gen == gen_ {
                                t.poll_waveform_gen(id, gen_, failures, cx);
                            }
                        });
                    })
                    .detach();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Plain-key shortcuts of the view (`playerKeys.ts`, scope "nowplaying"). True when consumed.
    pub fn key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let ps = self.player.read(cx).state.clone();
        match key {
            "t" => {
                self.lyrics_open = !self.lyrics_open;
                cx.notify();
            }
            "m" => self.toggle_mute(cx),
            "a" => {
                if ps.track.is_some() {
                    app::call(cx, |b| {
                        let _ = b.player_loop_cycle();
                    });
                }
            }
            "j" => self.skip_by(-10.0, cx),
            "k" => self.skip_by(10.0, cx),
            "z" => app::call(cx, |b| {
                let _ = b.player_previous();
            }),
            "b" => app::call(cx, |b| {
                let _ = b.player_next();
            }),
            _ => return false,
        }
        true
    }

    fn skip_by(&self, delta: f64, cx: &mut Context<Self>) {
        let ps = self.player.read(cx).state.clone();
        if ps.track.is_none() || ps.status == PlaybackStatus::Stopped {
            return;
        }
        let max = if ps.duration_sec > 0.0 { (ps.duration_sec - 0.5).max(0.0) } else { f64::INFINITY };
        let pos = (ps.position_sec + delta).max(0.0).min(max);
        app::call(cx, move |b| {
            let _ = b.player_seek(SeekRequest { position_sec: pos });
        });
    }

    fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        let v = self.player.read(cx).state.volume;
        let target = if v > 0.0 {
            self.last_audible = v;
            0.0
        } else {
            self.last_audible
        };
        app::call(cx, move |b| {
            let _ = b.player_set_volume(SetVolumeRequest { volume: target });
        });
    }
}

fn tag_text(t: &Track) -> String {
    let f = &t.format;
    let suffix = f.suffix.to_uppercase();
    if LOSSLESS_FMT.contains(&f.suffix.to_lowercase().as_str()) {
        let khz = if f.sampling_rate > 0 { format!("{}", ((f.sampling_rate as f64 / 1000.0) * 10.0).round() / 10.0) } else { String::new() };
        let spec = match (f.bit_depth, khz.is_empty()) {
            (Some(d), false) if d > 0 => format!("{d}/{khz} "),
            (_, false) => format!("{khz} KHZ "),
            _ => String::new(),
        };
        format!("{spec}LOSSLESS {suffix}")
    } else if let Some(b) = f.bit_rate {
        format!("{suffix} {b}K")
    } else {
        suffix
    }
}

/// One space glyph of the mono faces (JetBrains Mono advance 0.6 em + the tracking step), between inline spans.
const MONO_DATA_SPACE: f32 = 12.0 * 0.6 + 12.0 * 0.02;
const MONO_CODE_SPACE: f32 = 11.0 * 0.6 + 11.0 * 0.04;

pub enum NpEvent {
    Close,
    Navigate(Route),
    AddToPlaylist { id: String, at: gpui::Point<gpui::Pixels> },
    TrackInfo(Box<Track>),
    Rate { id: String, title: SharedString, rating: u8 },
}
impl gpui::EventEmitter<NpEvent> for NowPlayingView {}

impl Render for NowPlayingView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::rcount::hit("np");
        let th: Theme = app::theme(cx);
        let (ps, frame) = {
            let m = self.player.read(cx);
            (m.state.clone(), m.spectrum.clone())
        };
        let playing = ps.status == PlaybackStatus::Playing;
        let now = crate::clock::now();
        let seq = self.player.read(cx).spectrum_seq;
        let idle = !playing || ps.track.is_none();
        let mut moving = false;
        if self.spectrum_wanted() {
            if idle && !self.fft.bands.is_empty() && ps.status == PlaybackStatus::Stopped {
                self.fft.clear();
            }
            if seq != self.fft.seen_seq {
                self.fft.seen_seq = seq;
                if let Some(f) = frame.as_ref() {
                    self.fft.on_frame(f, now);
                }
            }
            moving = self.fft.frame(now);
        }
        if self.spectrum_wanted() && (playing || moving) && !crate::clock::frozen() {
            window.request_animation_frame();
        }
        if ps.volume > 0.0 {
            self.last_audible = ps.volume;
        }

        let vw = f32::from(window.viewport_size().width);
        let vh = f32::from(window.viewport_size().height);
        let narrow = vw < 1440.0; // @media (max-width: 1439px)
        let compact = vw < 1280.0; // @media (max-width: 1279px)
        let short = vh <= 820.0; // @media (max-height: 820px)
        let strip_h = if vh <= 900.0 { 40.0 } else { 44.0 };
        let Some(t) = ps.track.clone() else {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .child(pid("np.strip", self.strip(th, strip_h, None, &ps, compact, cx)))
                .child(
                    div().flex_1().m(px(16.)).border_1().border_color(th.border).flex().flex_col().items_center().justify_center().gap(px(8.)).child(icon("equalizer", 32., th.muted)).child(txt(&th, HEADLINE_SM, "Nothing playing").color(th.text)).child(txt(&th, BODY_SM, "Pick an album, playlist or track to start the signal chain.").color(th.muted)),
                )
                .into_any_element();
        };
        self.refresh_queue((ps.queue_length, t.id.clone(), ps.shuffle), cx);

        let strip = pid("np.strip", self.strip(th, strip_h, Some(&t), &ps, compact, cx));

        // ---- cover (square, the whole height of the 1fr row; measured, one-frame lag) [+ lyrics column]
        let area_h = self.cover_area_h.get();
        let content_w = (vw - super::shell::Shell::sidebar_width(vw) - 32.0).max(120.0);
        let lyrics_on = self.lyrics_open;
        let lyrics_beside = lyrics_on && vw >= 1280.0; // < 1280: the lyrics replace the cover
        let col_w = if lyrics_beside { ((content_w - 16.0) / 2.0).floor() } else { content_w };
        let max_w = col_w.max(120.0);
        let cover_px = if area_h > 0.0 { (area_h - 4.0).clamp(120.0, max_w) } else { (vh - 520.0).clamp(120.0, max_w) };
        let cover_px = cover_px.floor();
        let backend = self.backend.clone();
        let cover = art::cover(&self.art, &backend, t.cover_art.as_deref(), cover_px, window, cx);
        let cell = self.cover_area_h.clone();
        let tag = tag_text(&t);
        let cover_el = pid(
            "np.cover",
            div()
                .relative()
                .size(px(cover_px))
                .flex_none()
                .border_1()
                .border_color(th.border)
                .bg(th.high)
                .shadow(hard_shadow(&th, 4.))
                .overflow_hidden()
                .child(cover)
                .children((!tag.is_empty()).then(|| {
                    pid("np.cover.tag", div().absolute().top_0().left_0().px(px(8.)).py(px(2.)).bg(th.accent).child(txt(&th, MONO_DATA, tag).weight(700.0).color(th.accent_text)))
                })),
        );
        if lyrics_on {
            self.lyrics_ensure(&t.id, cx);
        }
        let lyrics_el = lyrics_on.then(|| self.lyrics_panel(th, ps.position_sec, if lyrics_beside { col_w } else { content_w }, window, cx));
        let cover_area = div()
            .flex_1()
            .min_h(px(140.))
            .w_full()
            .relative()
            .pt(px(4.))
            .px(px(16.))
            .flex()
            .flex_row()
            .gap(px(16.))
            .when(!lyrics_on, |d| d.justify_center())
            .child(
                canvas(
                    move |b, _, _| f32::from(b.size.height),
                    move |_, h, window, _| {
                        if (cell.get() - h).abs() > 1.0 {
                            cell.set(h);
                            window.refresh();
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            .children(if !lyrics_on {
                Some(cover_el.into_any_element())
            } else if lyrics_beside {
                Some(div().flex_1().min_w_0().flex().flex_row().justify_end().child(cover_el).into_any_element())
            } else {
                None
            })
            .children(lyrics_el.map(|l| div().flex_1().min_w_0().min_h_0().flex().flex_col().child(l)));

        // ---- title block
        let title_row = pid(
            "np.title",
            div()
                .flex()
                .flex_row()
                .items_end()
                .gap(px(12.))
                .max_w_full()
                .min_w_0()
                .child(pid("np.title.text", div().id("np-title").tip(t.title.clone(), th).flex().min_w_0().child(txt(&th, HEADLINE_LG, t.title.clone()).color(th.text).ellipsis())))
                .child(div().flex_none().pb(px(3.)).child(pid("np.title.dur", div().child(txt(&th, MONO_METRIC, fmt::format_duration(Some(ps.duration_sec))).color(th.accent_fg))))),
        );
        let link = |id: &'static str, label: String, strong: bool, nav: Option<Route>| {
            let mut d = div().id(id).flex_none().child({
                let x = txt(&th, BODY_LG, label).no_tracking();
                if strong { x.color(th.text).weight(600.0) } else { x.color(th.muted) }
            });
            if let Some(r) = nav {
                d = d.cursor(CursorStyle::PointingHand).on_click(cx.listener(move |_, _, _, cx| cx.emit(NpEvent::Navigate(r.clone()))));
            }
            d
        };
        let artline = pid(
            "np.artline",
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .justify_center()
                .items_center()
                .gap(px(8.))
                .max_w_full()
                .child(link("np-artist", t.artist.clone(), true, t.artist_id.clone().map(Route::Artist)))
                .children(t.album.clone().map(|a| {
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(8.))
                        .child(txt(&th, BODY_LG, "·").color(th.muted.opacity(0.6)))
                        .child(link("np-album", a, false, t.album_id.clone().map(Route::Album)))
                }))
                .children(t.year.map(|y| txt(&th, BODY_LG, format!("({y})")).color(th.muted))),
        );
        let key_lbl = |k: &str| PLAYER_KEYS.iter().find(|p| p.keys[0] == k).map(|p| p.legend).unwrap_or("");
        let tid = t.id.clone();
        let add_cell = self.add_btn.clone();
        let add_cell2 = self.add_btn.clone();
        let t_for_info = t.clone();
        let rating = t.rating.unwrap_or(0);
        let lyr_on = self.lyrics_open;
        let actions = pid(
            "np.actions",
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .justify_center()
                .items_center()
                .gap(px(8.))
                .mt(px(if short { 0.0 } else { 4.0 }))
                .child(pid(
                    "np.abtn.add",
                    chip("np-add", th, 30.0, 12.0, th.low, false)
                        .child(chip_icon(th, "add", 14., false))
                        .child(txt(&th, MONO_DATA, "ADD TO PLAYLIST"))
                        .child(canvas(move |b, _, _| add_cell.set(b), |_, _, _, _| ()).absolute().size_full())
                        .relative()
                        .on_click(cx.listener(move |_, _: &gpui::ClickEvent, _, cx| cx.emit(NpEvent::AddToPlaylist { id: tid.clone(), at: add_cell2.get().bottom_right() }))),
                ))
                .child(pid(
                    "np.abtn.lyrics",
                    chip("np-lyrics2", th, 30.0, 12.0, th.low, lyr_on)
                        .child(chip_icon(th, "lyrics", 14., lyr_on))
                        .child(txt(&th, MONO_DATA, format!("[{}] LYRICS", key_lbl("t"))))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.lyrics_open = !this.lyrics_open;
                            cx.notify();
                        })),
                ))
                .child(pid(
                    "np.abtn.info",
                    chip("np-info", th, 30.0, 12.0, th.low, false)
                        .child(txt(&th, MONO_DATA, "TRACK INFO"))
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(NpEvent::TrackInfo(Box::new(t_for_info.clone()))))),
                ))
                .child(pid(
                    "np.stars",
                    div().flex().flex_row().ml(px(8.)).children((1..=5u8).map(|n| {
                        let on = rating >= n;
                        let hover = if n == 1 { th.clip } else { th.text };
                        let id = t.id.clone();
                        let title: SharedString = t.title.clone().into();
                        pid(format!("np.star.{n}"), div()
                            .id(("np-rate", n as usize))
                            .tip(if n == 1 { "1 star: purged from the library nightly".to_string() } else { format!("{n} stars") }, th)
                            .group("hv")
                            .w(px(22.))
                            .h(px(30.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor(CursorStyle::PointingHand)
                            .child(icon_g(if on { "star-filled" } else { "star" }, 14., if on { th.accent_fg } else { th.muted }, hover))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                let next = if rating == n { 0 } else { n };
                                cx.emit(NpEvent::Rate { id: id.clone(), title: title.clone(), rating: next });
                            })))
                    })),
                )),
        );
        let title_block = div().px(px(16.)).flex_none().min_w_0().child(pid("np.titleblock", div().flex().flex_col().items_center().gap(px(4.)).min_w_0().child(title_row).child(artline).child(actions)));

        // ---- FFT panel
        let (lo, hi) = band_range_hz(ps.sample_rate.map(|r| r as f64).unwrap_or(44100.0));
        let ticks: Vec<(f64, f32)> = [50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0]
            .into_iter()
            .filter(|f| *f > lo * 1.15 && *f < hi * 0.8)
            .map(|f| (f, ((f / lo).ln() / (hi / lo).ln() * 100.0) as f32))
            .collect();
        let fft_h = (vh * 0.085).clamp(68.0, 84.0);
        let axis = pid(
            "np.fft.axis",
            div().relative().h(px(13.)).flex_none().children(ticks.into_iter().map(|(f, left)| {
                // `span { position: absolute; transform: translateX(-50%) }` at `left: N%`
                let label = format_hz(f);
                let w = crate::text::natural_width(window, &label, crate::theme::MONO, 11.0, 400.0, MONO_CODE.tracking_px());
                div().absolute().top_0().left(gpui::relative(left / 100.0)).ml(px(-w / 2.0)).child(txt(&th, MONO_CODE, label).color(th.muted))
            })),
        );
        let fft = pid(
            "np.fft",
            div()
                .flex()
                .flex_col()
                .w_full()
                .max_w(px(1040.))
                .h(px(fft_h))
                .flex_none()
                .border_1()
                .border_color(th.border)
                .bg(th.low)
                .child(pid(
                    "np.fft.head",
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap(px(12.))
                        .px(px(12.))
                        .h(px(22.))
                        .flex_none()
                        .bg(th.mid)
                        .border_b_1()
                        .border_color(th.border)
                        .child(txt(&th, LABEL_CAPS, format!("FAST FOURIER TRANSFORM // {}-BAND FFT", sp::BANDS)).color(th.accent_fg))
                        .child(txt(&th, MONO_CODE, format!("{}-PT · {}\u{2013}{} · PEAK HOLD", sp::FFT_SIZE, format_hz(lo), format_hz(hi))).color(th.muted)),
                ))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_row()
                        .gap(px(12.))
                        .pt(px(4.))
                        .px(px(12.))
                        .pb(px(8.))
                        .child(
                            div().flex_1().min_w_0().flex().flex_col().child(axis).child(pid(
                                "np.fft.spec",
                                div().flex_1().min_h_0().w_full().bg(th.base).border_1().border_color(th.border).child(spectrum_canvas(th, self.fft.bands.clone(), self.fft.caps.clone())),
                            )),
                        )
                        .child(pid(
                            "np.fft.meters",
                            div()
                                .w(px(176.))
                                .flex_none()
                                .flex()
                                .flex_col()
                                .border_l_1()
                                .border_color(th.border)
                                .pl(px(12.))
                                .children((!short).then(|| {
                                    div()
                                        .flex()
                                        .flex_row()
                                        .justify_between()
                                        .h(px(13.))
                                        .flex_none()
                                        .child(txt(&th, LABEL_CAPS, "STEREO").color(th.muted))
                                        .child(txt(&th, LABEL_CAPS, "PEAK").color(th.accent_fg))
                                }))
                                .child(div().flex_1().min_h_0().w_full().child(meter_canvas(th, self.fft.level, self.fft.hold))),
                        )),
                ),
        );
        let fft_row = div().w_full().px(px(16.)).flex_none().flex().justify_center().child(fft);

        // ---- deck: times + waveform seek, transport
        let dur = ps.duration_sec;
        let seek_ratio = self.seek_preview.unwrap_or(if dur > 0.0 { (ps.position_sec / dur).min(1.0) as f32 } else { 0.0 });
        let seek_cb: OnChange = {
            let this = cx.entity().downgrade();
            Rc::new(move |f, committed, _w, cx| {
                if let Some(e) = this.upgrade() {
                    e.update(cx, |d, cx| {
                        if committed {
                            d.seek_preview = None;
                            let pos = f as f64 * dur;
                            app::call(cx, move |b| {
                                let _ = b.player_seek(SeekRequest { position_sec: pos });
                            });
                        } else {
                            d.seek_preview = Some(f);
                        }
                        cx.notify();
                    });
                }
            })
        };
        let remaining = if dur > 0.0 { Some((dur - ps.position_sec).max(0.0)) } else { None };
        let dragging = self.wave_seek.dragging.get();
        let wd = WaveDraw {
            buckets: self.wave.clone().unwrap_or_default(),
            filled: if self.wave.is_some() { self.wave_filled } else { 0 },
            ratio: seek_ratio,
            hover: self.wave_hover,
            dragging,
            loop_a: ps.loop_a.filter(|_| dur > 0.0).map(|a| (a / dur) as f32),
            loop_b: ps.loop_b.filter(|_| dur > 0.0).map(|b| (b / dur) as f32),
        };
        let hover_label = self.wave_hover.filter(|_| dur > 0.0).map(|h| fmt::format_duration(Some(h as f64 * dur)));
        let hover_x = self.wave_hover.unwrap_or(0.0);
        let wavebox_h = (vh * 0.05).clamp(34.0, 46.0);
        let wave_hover_cb = {
            let this = cx.entity().downgrade();
            move |h: Option<f32>, cx: &mut gpui::App| {
                if let Some(e) = this.upgrade() {
                    e.update(cx, |d, cx| {
                        if d.wave_hover != h {
                            d.wave_hover = h;
                            cx.notify();
                        }
                    });
                }
            }
        };
        let wave_state = self.wave_seek.clone();
        let wave_inner = slider::track("np-wave", &self.wave_seek, seek_cb)
            .h_full()
            .cursor(CursorStyle::PointingHand)
            .on_mouse_move({
                let cb = wave_hover_cb.clone();
                move |ev: &gpui::MouseMoveEvent, _w, cx| {
                    let b = wave_state.bounds.get();
                    cb(Some(slider::frac_at(f32::from(ev.position.x), f32::from(b.left()), f32::from(b.size.width))), cx);
                }
            })
            .on_hover({
                let cb = wave_hover_cb;
                move |h, _w, cx| {
                    if !*h {
                        cb(None, cx)
                    }
                }
            })
            .child(div().absolute().size_full().child(wave_canvas(th, wd)));
        let wavebox = div().h(px(wavebox_h)).w_full().child(pid(
            "np.wavebox",
            div()
                .relative()
                .h_full()
                .min_h(px(40.))
                .w_full()
                .bg(th.base)
                .border_1()
                .border_color(th.border)
                .child(wave_inner)
                .children(hover_label.map(|l| {
                    // `.tip { left: x%; transform: translateX(-50%) }`: a zero-width centring box (flex centre overflows both ways)
                    div().absolute().bottom(gpui::relative(1.0)).mb(px(2.)).left(gpui::relative(hover_x)).w(px(0.)).flex().flex_row().justify_center().child(div().flex_none().bg(th.high).border_1().border_color(th.border).px(px(4.)).child(txt(&th, MONO_CODE, l).color(th.text)))
                })),
        ));
        let shown_pos = ps.position_sec; // NowPlaying: POS never follows the drag preview
        let times = pid(
            "np.times",
            div()
                .flex()
                .flex_row()
                .justify_between()
                .gap(px(12.))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .child(txt(&th, MONO_DATA, "POS:").color(th.muted))
                        .child(div().w(px(MONO_DATA_SPACE)))
                        .child(txt(&th, MONO_DATA, fmt::format_duration(Some(shown_pos))).weight(700.0).color(th.accent_fg))
                        .child(div().w(px(MONO_DATA_SPACE)))
                        .child(txt(&th, MONO_DATA, format!("/ {}", fmt::format_duration(Some(dur)))).color(th.muted)),
                )
                .child(txt(&th, MONO_DATA, remaining.map(|r| format!("-{} REMAINING", fmt::format_duration(Some(r)))).unwrap_or_default()).color(th.clip)),
        );
        let seek = pid("np.seek", div().flex().flex_col().gap(px(4.)).child(times).child(wavebox));

        // transport
        let tbtn = |id: &'static str, on: bool| chip(id, th, 40.0, 8.0, th.low, on).min_w(px(40.)).relative();
        let hint = |k: &str| div().absolute().right(px(3.)).bottom(px(1.)).child(txt(&th, with_size(LABEL_CAPS, 8.0, 10.0), k.to_string()).color(th.muted));
        let c1 = pid(
            "np.cluster.0",
            div()
                .flex()
                .flex_row()
                
                .gap(px(4.))
                .child(tbtn("np-prev", false).child(chip_icon(th, "prev", 16., false)).child(hint(key_lbl("z"))).on_click(cx.listener(|_, _, _, cx| {
                    app::call(cx, |b| drop(b.player_previous()))
                })))
                .child(tbtn("np-back", false).child(chip_icon(th, "skip-back", 18., false)).child(hint(key_lbl("j"))).on_click(cx.listener(|this, _, _, cx| this.skip_by(-10.0, cx))))
                .child({
                    let play_w = if narrow { 104.0 } else { 130.0 };
                    div()
                        .id("np-play")
                        .group("hv")
                        .min_w(px(play_w))
                        .h(px(40.))
                        .px(px(12.))
                        .flex_none()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_center()
                        .gap(px(8.))
                        .bg(th.accent)
                        .border_1()
                        .border_color(th.accent_fg)
                        .text_color(th.accent_text)
                        .shadow(hard_shadow(&th, 2.))
                        .cursor(CursorStyle::PointingHand)
                        .child(icon(if playing { "pause" } else { "play" }, 18., th.accent_text))
                        .child(txt(&th, with_size(LABEL_CAPS, 13.0, 12.0), if playing { "PAUSE" } else { "PLAY" }).color(th.accent_text).weight(700.0))
                        .children((!narrow).then(|| div().px(px(3.)).py(px(1.)).child(txt(&th, with_size(LABEL_CAPS, 8.0, 10.0), "[SPACE]").color(th.accent_text.opacity(0.75)))))
                        .on_click(cx.listener(|_, _, _, cx| app::call(cx, |b| drop(b.player_play_pause()))))
                })
                .child(tbtn("np-fwd", false).child(chip_icon(th, "skip-fwd", 18., false)).child(hint(key_lbl("k"))).on_click(cx.listener(|this, _, _, cx| this.skip_by(10.0, cx))))
                .child(tbtn("np-next", false).child(chip_icon(th, "next", 16., false)).child(hint(key_lbl("b"))).on_click(cx.listener(|_, _, _, cx| app::call(cx, |b| drop(b.player_next()))))),
        );
        let loop_label = if ps.loop_b.is_some() {
            "A↔B ACTIVE"
        } else if ps.loop_a.is_some() {
            "SET B"
        } else {
            "A↔B LOOP"
        };
        let c2 = pid(
            "np.cluster.1",
            div()
                .flex()
                .flex_row()
                
                .gap(px(4.))
                .child(tbtn("np-shuffle", ps.shuffle).child(chip_icon(th, "shuffle", 15., ps.shuffle)).on_click(cx.listener(|this, _, _, cx| {
                    let on = !this.player.read(cx).state.shuffle;
                    app::call(cx, move |b| drop(b.player_set_shuffle(on)));
                })))
                .child(
                    tbtn("np-repeat", ps.repeat != RepeatMode::Off)
                        .child(chip_icon(th, if ps.repeat == RepeatMode::One { "repeat-one" } else { "repeat" }, 15., ps.repeat != RepeatMode::Off))
                        .on_click(cx.listener(|this, _, _, cx| {
                            let next = match this.player.read(cx).state.repeat {
                                RepeatMode::Off => RepeatMode::All,
                                RepeatMode::All => RepeatMode::One,
                                RepeatMode::One => RepeatMode::Off,
                            };
                            app::call(cx, move |b| drop(b.player_set_repeat(next)));
                        })),
                )
                .child({
                    let on = ps.loop_a.is_some();
                    chip("np-loop", th, 40.0, 12.0, th.low, on)
                        .tip("A to B loop: press to set A, again to set B, again to clear (A)", th)
                        .child(chip_icon(th, "loop-ab", 15., on))
                        .child(txt(&th, MONO_DATA, loop_label))
                        .on_click(cx.listener(|_, _, _, cx| app::call(cx, |b| drop(b.player_loop_cycle()))))
                }),
        );
        let vol_cb: OnChange = {
            let this = cx.entity().downgrade();
            Rc::new(move |f, _c, _w, cx| {
                if let Some(e) = this.upgrade() {
                    e.update(cx, |_, cx| app::call(cx, move |b| drop(b.player_set_volume(SetVolumeRequest { volume: f as f64 }))));
                }
            })
        };
        let muted = ps.volume <= 0.0;
        let fader_w = if narrow { (vw * 0.08).clamp(70.0, 150.0) } else { (vw * 0.10).clamp(90.0, 150.0) };
        let c3 = pid(
            "np.cluster.2",
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .px(px(8.))
                .flex_none()
                .border_1()
                .border_color(th.border)
                .bg(th.low)
                .text_color(th.muted)
                .child(icon(if muted { "volume-off" } else { "volume" }, 15., th.muted))
                .child(
                    div()
                        .w(px(fader_w))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|_, ev: &gpui::MouseDownEvent, _, cx| {
                                if ev.click_count == 2 {
                                    app::call(cx, |b| drop(b.player_set_volume(SetVolumeRequest { volume: 1.0 })));
                                }
                            }),
                        )
                        .on_scroll_wheel(cx.listener(|this, ev: &gpui::ScrollWheelEvent, _, cx| {
                            let dy = f32::from(ev.delta.pixel_delta(px(16.)).y);
                            if dy != 0.0 {
                                let v = (this.player.read(cx).state.volume + if dy > 0.0 { 1.0 } else { -1.0 } / 60.0).clamp(0.0, 1.0);
                                app::call(cx, move |b| drop(b.player_set_volume(SetVolumeRequest { volume: v })));
                            }
                        }))
                        .child(slider("np-vol", th, &self.vol, ps.volume as f32, None, vol_cb)),
                )
                .child(div().min_w(px(56.)).flex().justify_end().child(txt(&th, MONO_CODE, fmt::volume_label(&ps)).color(if muted { th.muted } else { th.accent_fg })))
                .child(chip("np-mute", th, 32.0, 12.0, th.low, muted).tip("Mute (M)", th).child(txt(&th, MONO_DATA, format!("MUTE [{}]", key_lbl("m")))).on_click(cx.listener(|this, _, _, cx| this.toggle_mute(cx)))),
        );
        // up next
        let more = self.queue_total.saturating_sub(self.next_start);
        let un_title = self.upcoming.first().map(|u| format!("{} — {}", u.track.title, u.track.artist)).unwrap_or_else(|| "Nothing queued".into());
        let un_head = format!("UP NEXT{}", if more > 1 { format!(" · +{}", more - 1) } else { String::new() });
        let open = self.upnext_open;
        let un_w: f32 = if narrow { 180.0 } else { 360.0 };
        let upnext = pid(
            "np.upnext",
            div()
                .relative()
                .flex_basis(px(un_w.min(220.0)))
                .flex_grow()
                .when(!compact, |d| d.max_w(px(un_w)))
                .min_w(px(if narrow { 180.0 } else { 220.0 }))
                .flex_shrink()
                .flex()
                .flex_row()
                
                .border_1()
                .border_color(th.border)
                .bg(th.low)
                .child(
                    div()
                        .id("np-un-main")
                        .tip("Show the next tracks", th)
                        .group("hv")
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(12.))
                        .px(px(12.))
                        .text_color(th.muted)
                        .cursor(CursorStyle::PointingHand)
                        .hover(move |s| s.text_color(th.text))
                        .child(icon_g("queue", 16., th.muted, th.text))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .child(txt(&th, LABEL_CAPS, un_head).color(th.accent_fg))
                                .child(div().flex().min_w_0().child(txt(&th, BODY_MD, un_title).color(th.text).ellipsis())),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.upnext_open = !this.upnext_open;
                            cx.notify();
                        })),
                )
                .children((!narrow).then(|| {
                    let dis = self.upcoming.is_empty();
                    let d = div().id("np-skip").group("hv").m(px(8.)).px(px(12.)).flex().items_center().bg(th.highest).border_1().border_color(th.border).text_color(th.text).child(txt(&th, LABEL_CAPS, "SKIP"));
                    if dis {
                        d.opacity(0.5).cursor(CursorStyle::OperationNotAllowed)
                    } else {
                        d.cursor(CursorStyle::PointingHand).hover(move |s| s.text_color(th.accent_fg).border_color(th.accent_fg)).on_click(cx.listener(|_, _, _, cx| app::call(cx, |b| drop(b.player_next()))))
                    }
                }))
                .children(open.then(|| {
                    let start = self.next_start;
                    div()
                        .absolute()
                        .right_0()
                        .bottom(gpui::relative(1.0))
                        .mb(px(4.))
                        .w(px((520.0f32).min(vw * 0.9)))
                        .bg(th.low)
                        .border_1()
                        .border_color(th.accent_fg)
                        .occlude()
                        .when(self.upcoming.is_empty(), |d| d.p(px(12.)).child(txt(&th, BODY_SM, "Nothing queued after this track.").color(th.muted)))
                        .children(self.upcoming.iter().enumerate().map(|(k, u)| {
                            let idx = start + k;
                            div()
                                .id(("np-qrow", k))
                                .tip("Play this track", th)
                                .w_full()
                                .min_h(px(30.))
                                .px(px(12.))
                                .flex()
                                .flex_col()
                                .justify_center()
                                .border_b_1()
                                .border_color(th.mid)
                                .cursor(CursorStyle::PointingHand)
                                .hover(move |s| s.bg(th.mid))
                                .child({
                                    let mut title = div().flex().flex_basis(px(0.)).min_w_0().child(txt(&th, BODY_MD, u.track.title.clone()).color(th.text).ellipsis());
                                    title.style().flex_grow = Some(3.0);
                                    let mut artist = div().flex().flex_basis(px(0.)).min_w_0().child(txt(&th, MONO_DATA, u.track.artist.clone()).color(th.muted).ellipsis());
                                    artist.style().flex_grow = Some(2.0);
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(px(12.))
                                        .min_h(px(30.))
                                        .child(div().w(px(20.)).flex_none().child(txt(&th, MONO_CODE, format!("{:02}", k + 1)).color(th.muted)))
                                        .child(title)
                                        .child(artist)
                                        .child(div().w(px(44.)).flex_none().flex().justify_end().child(txt(&th, MONO_CODE, fmt::format_duration(Some(u.track.duration))).color(th.muted)))
                                })
                                // `.q-flow { margin-top: -6px; padding: 0 0 5px 32px }`
                                .children({
                                    let line = fmt::flow_line(u.flow.as_ref());
                                    (!line.is_empty()).then(|| div().id(("np-qflow", k)).tip("Why this track is next", th).flex().min_w_0().mt(px(-6.)).pb(px(5.)).pl(px(32.)).child(txt(&th, MONO_CODE, line).color(th.accent_fg).ellipsis()))
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.upnext_open = false;
                                    cx.notify();
                                    app::call(cx, move |b| drop(b.player_jump_to(idx)));
                                }))
                        }))
                })),
        );
        let transport = pid("np.transport", div().flex().flex_row().flex_wrap().gap(px(if narrow { 8.0 } else { 12.0 })).child(c1).child(c2).child(c3).child(upnext));
        let deck = pid("np.deck", div().flex().flex_col().gap(px(8.)).w_full().flex_none().px(px(16.)).py(px(8.)).bg(th.low).border_t_1().border_b_1().border_color(th.border).child(seek).child(transport));

        let legend_wrap = |inner: AnyElement| div().px(px(16.)).flex_none().child(pid("np.legend", div().pb(px(8.)).child(inner)));
        let legend = pid(
            "np.legend.row",
            div().flex().flex_row().flex_wrap().gap_x(px(16.)).gap_y(px(4.)).flex_none().children(PLAYER_KEYS.iter().filter(|k| k.in_legend).map(|k| {
                div()
                    .flex()
                    .flex_row()
                    .child(txt(&th, MONO_CODE, format!("[{}]", k.legend)).weight(500.0).color(th.accent_fg))
                    .child(div().w(px(MONO_CODE_SPACE)))
                    .child(txt(&th, MONO_CODE, k.label).color(th.muted))
            })),
        );

        let page = div().size_full().min_h(px(600.)).flex().flex_col().gap(px(8.)).child(strip).child(cover_area).child(title_block).child(fft_row).child(deck).when(!short, |d| d.child(legend_wrap(legend.into_any_element())))
            .when(short, |d| d.child(div().h(px(0.)).flex_none()));
        // `.np { min-height: 600px }`: below that the stage scrolls (classic 10 px scrollbar eats the width, like the web)
        let (sbar, overflow) = crate::widgets::scrollbar::passive(th, &self.np_scroll);
        div()
            .relative()
            .size_full()
            .child(div().id("np-scroll").track_scroll(&self.np_scroll).size_full().overflow_y_scroll().when(overflow, |d| d.pr(px(crate::metrics::tracks::SCROLLBAR_W))).child(page))
            .child(sbar)
            .child(crate::widgets::smooth::drive(window, crate::widgets::smooth::Scrollable::div(std::ptr::addr_of!(self.np_scroll) as usize, &self.np_scroll)))
            .into_any_element()
    }
}

impl NowPlayingView {
    /// The secondary header strip (`.strip`).
    fn strip(&self, th: Theme, h: f32, t: Option<&Track>, ps: &PlayerState, compact: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let loading = ps.status == PlaybackStatus::Loading;
        let mode = if ps.bit_perfect {
            "BITPERFECT"
        } else if ps.output_mode == OutputMode::Exclusive {
            "EXCLUSIVE"
        } else {
            "SHARED MODE"
        };
        let mut parts: Vec<String> = Vec::new();
        if let (Some(rate), Some(_)) = (ps.sample_rate, t) {
            let khz = format!("{:.1}", rate as f64 / 1000.0);
            parts.push(if ps.float_mix == Some(true) {
                format!("{}F MIX / {khz} KHZ", ps.bit_depth.unwrap_or(32))
            } else {
                format!("{} {}{khz} KHZ", ps.format_label.clone().unwrap_or_default(), ps.bit_depth.map(|b| format!("{b}-BIT / ")).unwrap_or_default()).trim().to_string()
            });
        }
        parts.push(mode.into());
        if !self.device.is_empty() {
            parts.push(self.device.to_uppercase());
        }
        let line = parts.join(" · ");
        let single = t.map(|t| t.album_id.is_some() && self.upcoming.iter().all(|u| u.track.album_id == t.album_id)).unwrap_or(false);
        let src_text = match &self.source {
            Some(s) => format!("{}{}", s.label, if s.mode == SourceMode::Shuffle { " · Shuffle" } else { "" }),
            None => match t.and_then(|t| t.album.clone()) {
                Some(a) if single => a,
                _ => "Queue".into(),
            },
        };
        let source_route: Option<Route> = match (&self.source, t) {
            (Some(s), _) => match (s.kind, &s.id) {
                (SourceKind::Playlist, Some(id)) => Some(Route::Playlist(id.clone())),
                (SourceKind::Artist, Some(id)) => Some(Route::Artist(id.clone())),
                (SourceKind::Genre, Some(id)) => Some(Route::Genre(id.clone())),
                (SourceKind::Liked, _) => Some(Route::Liked),
                _ => None,
            },
            (None, Some(t)) if single => t.album_id.clone().map(Route::Album),
            _ => None,
        };
        let lyr_on = self.lyrics_open;
        let key_t = PLAYER_KEYS.iter().find(|p| p.keys[0] == "t").map(|p| p.legend).unwrap_or("T");
        let starred = t.map(|t| t.starred.is_some()).unwrap_or(false);
        let track_id = t.map(|t| t.id.clone());
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .h(px(h))
            .px(px(16.))
            .flex_none()
            .border_b_1()
            .border_color(th.border)
            .child(div().flex().flex_row().items_center().gap(px(8.)).flex_none().child(div().size(px(8.)).bg(th.accent)).child(txt(&th, LABEL_CAPS, "NOW PLAYING").color(th.accent_fg)))
            .children(t.map(|_| {
                let mut d = div()
                    .id("np-source")
                    .tip_opt(source_route.as_ref().map(|_| "Go to source"), th)
                    .flex()
                    .flex_row()
                    .min_w_0()
                    .max_w(gpui::relative(0.22))
                    .flex_shrink()
                    .child(txt(&th, MONO_DATA, "// ").color(th.muted))
                    .child(pid("np.strip.source", div().flex().min_w_0().child(txt(&th, MONO_DATA, src_text).weight(500.0).color(th.text).ellipsis())));
                if let Some(r) = source_route {
                    d = d.cursor(CursorStyle::PointingHand).on_click(cx.listener(move |_, _, _, cx| cx.emit(NpEvent::Navigate(r.clone()))));
                }
                d
            }))
            .children(t.map(|_| div().w(px(1.)).h(px(16.)).flex_none().bg(th.border)))
            .children(t.map(|_| {
                let ft = line.clone();
                let inner: AnyElement = if let Some(e) = &ps.error {
                    txt(&th, MONO_DATA, e.clone()).color(th.clip).ellipsis().into_any_element()
                } else if loading {
                    txt(&th, MONO_DATA, "LOADING…").color(th.muted).ellipsis().into_any_element()
                } else {
                    txt(&th, MONO_DATA, line).color(th.muted).ellipsis().into_any_element()
                };
                pid("np.strip.fmt", div().id("np-fmt").tip(ft, th).flex().flex_1().min_w_0().child(inner))
            }))
            .child(div().when(t.is_none(), |d| d.flex_1().min_w_0()).when(t.is_some(), |d| d.flex_none().w(px(0.))))
            .child(pid(
                "np.hbtn.lyrics",
                chip("np-lyrics", th, 28.0, 12.0, th.mid, lyr_on)
                    .tip("Lyrics (T)", th)
                    .child(chip_icon(th, "lyrics", 14., lyr_on))
                    .children((!compact).then(|| txt(&th, MONO_DATA, format!("LYRICS [{key_t}]"))))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.lyrics_open = !this.lyrics_open;
                        cx.notify();
                    })),
            ))
            .children(track_id.map(|id| {
                pid(
                    "np.hbtn.like",
                    chip("np-like", th, 28.0, 8.0, th.mid, starred).tip(if starred { "Liked - L to unlike" } else { "Like (L)" }, th).child(chip_icon(th, if starred { "heart-filled" } else { "heart" }, 15., starred)).on_click(cx.listener(move |_, _, _, cx| {
                        let id = id.clone();
                        app::call(cx, move |b| {
                            let req = StarRequest { id, kind: StarTargetType::Track };
                            let _ = if starred { b.star_remove(req) } else { b.star_add(req) };
                        });
                    })),
                )
            }))
            .child(pid(
                "np.hbtn.min",
                chip("np-close", th, 28.0, 12.0, th.mid, false)
                    .tip("Close (Esc)", th)
                    .child(chip_icon(th, "collapse", 14., false))
                    .children((!compact).then(|| txt(&th, MONO_DATA, "MINIMIZE [ESC]")))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(NpEvent::Close))),
            ))
    }
}

#[cfg(test)]
mod fft_tests {
    use super::*;

    fn frame(v: u8, l: f32, r: f32) -> SpectrumFrame {
        SpectrumFrame { bands: vec![v; sp::BANDS], peak_l: l, peak_r: r }
    }

    /// `FftPanel.svelte`: meters fall 30 dB/s, the 1.2 s peak hold only starts falling after that, caps fall 0.45/s.
    #[test]
    fn dynamics_follow_the_web_panel() {
        let mut f = Fft::default();
        f.on_frame(&frame(255, -3.0, -6.0), 0.0);
        assert_eq!((f.level, f.hold), ([-3.0, -6.0], [-3.0, -6.0]));
        let mut t = 0.0;
        for _ in 0..10 {
            t += 0.1;
            f.frame(t); // dt = 0.1 each (the first draw uses the 16 ms default)
        }
        // first frame dt = 0.016, then nine of 0.1: 0.916 s of falling at 30 dB/s
        assert!((f.level[0] - (-3.0 - 30.0 * 0.916)).abs() < 0.05, "{}", f.level[0]);
        assert_eq!(f.hold[0], -3.0, "peak hold lasts 1.2 s");
        // no frame for > 120 ms: the bars fall (2.2 x full scale / s) and the cap follows at most 0.45 / s
        assert!(f.bands[0] < 255.0 && f.caps[0] < 1.0 && f.caps[0] > f.bands[0] / 255.0, "{} {}", f.bands[0], f.caps[0]);
        for _ in 0..4 {
            t += 0.1;
            f.frame(t);
        }
        assert!(f.hold[0] < -3.0, "hold released after 1.2 s");
        assert_eq!(f.bands[0], 0.0, "fully fallen after ~1.4 s of stall");
    }

    #[test]
    fn draws_are_at_least_20ms_apart_and_idle_stops_the_loop() {
        let mut f = Fft::default();
        assert!(!f.frame(0.0), "nothing to animate");
        f.on_frame(&frame(10, -60.0, -60.0), 0.0);
        f.frame(1.0);
        let caps = f.caps[0];
        f.frame(1.005); // < 20 ms later: skipped
        assert_eq!(f.caps[0], caps);
    }
}
