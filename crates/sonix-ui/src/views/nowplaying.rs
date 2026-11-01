//! Now Playing (`NowPlaying.svelte`, "artwork dominant" layout): header strip with the real
//! source/output line, dominant cover with the source-format tag, title block, a 28-band FFT
//! with peak hold, stereo peak meters, a 240-bucket waveform seek bar and the full transport.
//! Animates on vsync (`request_animation_frame`) only while visible, playing and not
//! minimized; the backend is told to compute the spectrum only while this view is up.
use crate::app::{self, PlayerEvent, PlayerModel};
use crate::art::{self, ArtHandle};
use crate::theme::Theme;
use crate::tracking::{BODY_SM, DISPLAY_XL, HEADLINE_LG, LABEL_CAPS, MONO_CODE, MONO_DATA, TypeSpec, tracked};
use crate::typo::{ellipsis, ts};
use crate::widgets::button::ghost_icon;
use crate::widgets::slider::{self, OnChange, SliderHandle, slider};
use crate::widgets::{hard_shadow, icon};
use gpui::{Bounds, Context, CursorStyle, Entity, IntoElement, Render, SharedString, Subscription, Window, canvas, div, fill, point, prelude::*, px, size};
use sonix_api::format as fmt;
use sonix_api::*;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use crate::metrics::spectrum as sp;
use crate::probe::pid;


pub struct NowPlayingView {
    backend: Arc<dyn Backend>,
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
        let id = self.player.read(cx).current_id().map(str::to_string);
        if id == self.wave_for {
            cx.notify();
            return;
        }
        self.wave_for = id.clone();
        self.wave = None;
        if let Some(id) = id {
            let b = self.backend.clone();
            let want = id.clone();
            cx.spawn(async move |this, cx| {
                let w = cx.background_executor().spawn(async move { b.player_get_waveform(&want) }).await;
                let _ = this.update(cx, |t, cx| {
                    if t.wave_for.as_deref() == Some(id.as_str()) {
                        t.wave = w.ok().map(|w| w.buckets);
                        cx.notify();
                    }
                });
            })
            .detach();
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

/// `FftPanel.drawMeters` (segments only; the L/R and value labels are separate elements here): 20 segments of 3 dB
/// from -60 to 0 dBFS, lit up to the smoothed level, one segment of peak hold, red above -3 dB.
fn meter_canvas(th: Theme, level: f32, hold: f32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |b, _, window, _| {
            let s = window.scale_factor();
            let (ox, oy) = (f32::from(b.origin.x) * s, f32::from(b.origin.y) * s);
            let (w, h) = ((f32::from(b.size.width) * s).round(), (f32::from(b.size.height) * s).round());
            let seg_gap = s.round().max(1.0);
            let seg_w = (w - seg_gap * (sp::SEGMENTS as f32 - 1.0)) / sp::SEGMENTS as f32;
            let lit = (((level - sp::FLOOR_DB) / -sp::FLOOR_DB) * sp::SEGMENTS as f32).round() as i32;
            let hold_seg = (((hold - sp::FLOOR_DB) / -sp::FLOOR_DB) * sp::SEGMENTS as f32).round() as i32 - 1;
            for i in 0..sp::SEGMENTS as i32 {
                let x = (i as f32 * (seg_w + seg_gap)).round();
                let seg_db = sp::FLOOR_DB + ((i + 1) as f32 / sp::SEGMENTS as f32) * -sp::FLOOR_DB;
                let hot = seg_db > sp::CLIP_FROM_DB;
                let col = if i < lit {
                    if hot { th.clip } else { th.meter }
                } else if i == hold_seg && hold > sp::FLOOR_DB {
                    if hot { th.clip } else { th.text }
                } else {
                    th.highest
                };
                window.paint_quad(fill(Bounds::new(point(px((ox + x) / s), px(oy / s)), size(px(seg_w.round().max(1.0) / s), px(h / s))), col));
            }
        },
    )
    .size_full()
}

fn wave_canvas(th: Theme, buckets: Vec<u8>, frac: f32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |b, _, window, _| {
            let n = buckets.len().max(1);
            let w = f32::from(b.size.width);
            let h = f32::from(b.size.height);
            let step = w / n as f32;
            let bw = (step - 1.0).max(1.0);
            // scale to the loudest bucket so quiet masters still read
            let max = buckets.iter().copied().max().unwrap_or(255).max(1) as f32;
            for (i, v) in buckets.iter().enumerate() {
                let bh = (*v as f32 / max * h).max(2.0);
                let played = (i as f32 + 0.5) / n as f32 <= frac;
                let col = if played { th.meter } else { th.muted.opacity(0.55) };
                window.paint_quad(fill(Bounds::new(point(b.origin.x + px(i as f32 * step), b.origin.y + px((h - bh) / 2.0)), size(px(bw), px(bh))), col));
            }
            let x = b.origin.x + px(w * frac);
            window.paint_quad(fill(Bounds::new(point(x, b.origin.y), size(px(2.0), b.size.height)), th.text));
        },
    )
    .size_full()
}

impl Render for NowPlayingView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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

        let vw = f32::from(window.viewport_size().width);
        let vh = f32::from(window.viewport_size().height);
        let compact = vw < 1280.0;
        let track = ps.track.clone();
        let Some(t) = track else {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .child(tracked("Nothing playing", HEADLINE_LG, th.body_font, th.text, window))
                .child(ts(&th, BODY_SM).text_color(th.muted).child("Pick a track in Tracks and press Enter, or Shuffle All."))
                .into_any_element();
        };

        // ---- header strip
        let mode = if ps.bit_perfect {
            "BITPERFECT"
        } else if ps.output_mode == OutputMode::Exclusive {
            "EXCLUSIVE"
        } else {
            "SHARED MODE"
        };
        let line = format!("{} · {mode}{}", fmt::source_line(&t.format), if self.device.is_empty() { String::new() } else { format!(" · {}", self.device) });
        let strip = pid("np.strip", div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(16.))
            .h(px(36.))
            .px(px(16.))
            .flex_none()
            .border_b_1()
            .border_color(th.border)
            .child(tracked("NOW PLAYING", LABEL_CAPS, th.body_font, th.text, window))
            .child(ts(&th, MONO_CODE).text_color(th.muted).child("//"))
            .child(ellipsis(ts(&th, MONO_CODE).flex_1()).text_color(if ps.bit_perfect { th.accent_fg } else { th.muted }).child(SharedString::from(line)))
            .child(
                ghost_icon("np-close", th, "collapse", 14., 28., false, false).on_click(cx.listener(|_, _, _, cx| {
                    let _ = cx;
                    window_back(cx)
                })),
            ));

        // ---- cover (sized from the measured area, one-frame lag)
        let area_h = self.cover_area_h.get();
        let max_w = (vw * 0.5).max(120.0);
        let cover_px = if area_h > 0.0 { (area_h - 24.0).clamp(120.0, max_w) } else { (vh - 520.0).clamp(120.0, max_w) };
        let cover_px = cover_px.floor();
        let backend = self.backend.clone();
        let cover = art::cover(&self.art, &backend, t.cover_art.as_deref(), cover_px, window, cx);
        let cell = self.cover_area_h.clone();
        let tag = fmt::source_tag(&t.format);
        let cover_area = div()
            .flex_1()
            .min_h(px(120.))
            .w_full()
            .relative()
            .flex()
            .items_center()
            .justify_center()
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
            .child(
                div()
                    .relative()
                    .size(px(cover_px))
                    .flex_none()
                    .border_1()
                    .border_color(th.frame)
                    .shadow(hard_shadow(&th, 6.))
                    .child(cover)
                    .child(div().absolute().left(px(0.)).bottom(px(0.)).px(px(8.)).py(px(3.)).bg(th.accent).child(tracked(&tag, LABEL_CAPS, th.body_font, th.accent_text, window))),
            );

        // ---- title block
        let spec: TypeSpec = if compact { HEADLINE_LG } else { DISPLAY_XL };
        let sub = match (&t.album, t.year) {
            (Some(a), Some(y)) => format!("{} · {} ({y})", t.artist, a),
            (Some(a), None) => format!("{} · {a}", t.artist),
            _ => t.artist.clone(),
        };
        let title_block = pid("np.titleblock", div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.))
            .px(px(16.))
            .pt(px(8.))
            .flex_none()
            .child(div().max_w(px(vw - 80.)).overflow_hidden().child(tracked(&t.title, spec, th.body_font, th.text, window)))
            .child(ts(&th, MONO_DATA).text_color(th.muted).child(SharedString::from(sub))));

        // ---- FFT + meters
        let fft_h = (vh * 0.085).clamp(68.0, 84.0);
        let fft = pid("np.fft", div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(16.))
            .w_full()
            .max_w(px(1040.))
            .h(px(fft_h))
            .flex_none()
            .mt(px(8.))
            .child(div().flex_1().h_full().child(spectrum_canvas(th, self.fft.bands.clone(), self.fft.caps.clone())))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .w(px(160.))
                    .flex_none()
                    .child(div().flex().flex_row().items_center().gap(px(8.)).child(ts(&th, MONO_CODE).w(px(10.)).text_color(th.muted).child("L")).child(div().flex_1().h(px(10.)).child(meter_canvas(th, self.fft.level[0], self.fft.hold[0]))))
                    .child(div().flex().flex_row().items_center().gap(px(8.)).child(ts(&th, MONO_CODE).w(px(10.)).text_color(th.muted).child("R")).child(div().flex_1().h(px(10.)).child(meter_canvas(th, self.fft.level[1], self.fft.hold[1])))),
            ));

        // ---- deck: waveform seek + transport
        let dur = ps.duration_sec;
        let seek_ratio = self.seek_preview.unwrap_or(if dur > 0.0 { (ps.position_sec / dur) as f32 } else { 0.0 });
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
        let shown_pos = if self.seek_preview.is_some() { seek_ratio as f64 * dur } else { ps.position_sec };
        let wave = match &self.wave {
            Some(b) => div().size_full().child(wave_canvas(th, b.clone(), seek_ratio)).into_any_element(),
            None => div().size_full().flex().items_center().child(div().w_full().h(px(2.)).bg(th.highest)).into_any_element(),
        };
        let seek_row = pid("np.seek", div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(16.))
            .w_full()
            .px(px(16.))
            .child(div().flex().flex_col().w(px(88.)).flex_none().child(tracked("POS", LABEL_CAPS, th.body_font, th.muted, window)).child(ts(&th, MONO_DATA).text_color(th.text).child(SharedString::from(fmt::format_duration(Some(shown_pos))))))
            .child(div().flex_1().h(px(48.)).child(slider::track("np-wave", &self.wave_seek, seek_cb).h(px(48.)).child(wave)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_end()
                    .w(px(88.))
                    .flex_none()
                    .child(tracked("REMAINING", LABEL_CAPS, th.body_font, th.muted, window))
                    .child(ts(&th, MONO_DATA).text_color(th.text).child(SharedString::from(format!("-{}", fmt::format_duration(Some((dur - shown_pos).max(0.0))))))),
            ));
        let vol_cb: OnChange = {
            let this = cx.entity().downgrade();
            Rc::new(move |f, _c, _w, cx| {
                if let Some(e) = this.upgrade() {
                    e.update(cx, |_, cx| {
                        app::call(cx, move |b| {
                            let _ = b.player_set_volume(SetVolumeRequest { volume: f as f64 });
                        })
                    });
                }
            })
        };
        let play_btn = div()
            .id("np-play")
            .size(px(44.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .bg(th.accent)
            .text_color(th.accent_text)
            .shadow(hard_shadow(&th, 3.))
            .cursor(CursorStyle::PointingHand)
            .hover(move |s| s.bg(th.accent_hover))
            .child(icon(if playing { "pause" } else { "play" }, 20., th.accent_text))
            .on_click(cx.listener(|_, _, _, cx| {
                app::call(cx, |b| {
                    let _ = b.player_play_pause();
                })
            }));
        let transport = pid("np.transport", div()
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .gap(px(16.))
            .pb(px(12.))
            .child(ghost_icon("np-shuffle", th, "shuffle", 16., 32., ps.shuffle, false).on_click(cx.listener(|this, _, _, cx| {
                let on = !this.player.read(cx).state.shuffle;
                app::call(cx, move |b| {
                    let _ = b.player_set_shuffle(on);
                });
            })))
            .child(ghost_icon("np-prev", th, "prev", 18., 32., false, false).on_click(cx.listener(|_, _, _, cx| {
                app::call(cx, |b| {
                    let _ = b.player_previous();
                })
            })))
            .child(play_btn)
            .child(ghost_icon("np-next", th, "next", 18., 32., false, false).on_click(cx.listener(|_, _, _, cx| {
                app::call(cx, |b| {
                    let _ = b.player_next();
                })
            })))
            .child(ghost_icon("np-repeat", th, if ps.repeat == RepeatMode::One { "repeat-one" } else { "repeat" }, 16., 32., ps.repeat != RepeatMode::Off, false).on_click(cx.listener(|this, _, _, cx| {
                let next = match this.player.read(cx).state.repeat {
                    RepeatMode::Off => RepeatMode::All,
                    RepeatMode::All => RepeatMode::One,
                    RepeatMode::One => RepeatMode::Off,
                };
                app::call(cx, move |b| {
                    let _ = b.player_set_repeat(next);
                });
            })))
            .child(div().w(px(24.)))
            .child(div().flex().flex_row().items_center().gap(px(8.)).text_color(th.muted).child(icon("volume", 15., th.muted)).child(div().w(px(120.)).child(slider("np-vol", th, &self.vol, ps.volume as f32, None, vol_cb))).child(ts(&th, MONO_CODE).w(px(52.)).text_right().whitespace_nowrap().text_color(th.muted).child(SharedString::from(fmt::volume_label(&ps))))));
        let deck = pid("np.deck", div().flex().flex_col().gap(px(8.)).w_full().flex_none().pt(px(12.)).bg(th.chrome).border_t_1().border_color(th.frame).child(seek_row).child(transport));
        let legend = div()
            .h(px(24.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(16.))
            .text_color(th.muted)
            .children(["SPACE PLAY/PAUSE", "N CLOSE", "ESC CLOSE", "L LIKE"].iter().map(|s| tracked(s, LABEL_CAPS, th.body_font, th.muted, window)).collect::<Vec<_>>())
            .into_any_element();

        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .child(div().w_full().child(strip))
            .child(cover_area)
            .child(title_block)
            .child(fft)
            .child(deck)
            .when(vh >= 820.0, |d| d.child(legend))
            .into_any_element()
    }
}

fn window_back(cx: &mut Context<NowPlayingView>) {
    cx.emit(NpEvent::Close);
}

pub enum NpEvent {
    Close,
}
impl gpui::EventEmitter<NpEvent> for NowPlayingView {}

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
