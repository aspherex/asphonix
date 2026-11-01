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
use std::time::Instant;

const BANDS: usize = SPECTRUM_BANDS;

pub struct NowPlayingView {
    backend: Arc<dyn Backend>,
    art: ArtHandle,
    player: Entity<PlayerModel>,
    visible: bool,
    suspended: bool,
    bands: [f32; BANDS],
    hold: [f32; BANDS],
    hold_age: [f32; BANDS],
    peaks: [f32; 2],
    last: Instant,
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
            bands: [0.0; BANDS],
            hold: [0.0; BANDS],
            hold_age: [0.0; BANDS],
            peaks: [0.0; 2],
            last: Instant::now(),
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

    fn sync_spectrum(&self, cx: &mut Context<Self>) {
        let on = self.spectrum_wanted();
        app::call(cx, move |b| {
            let _ = b.player_set_spectrum_active(on);
        });
    }

    pub fn set_visible(&mut self, v: bool, cx: &mut Context<Self>) {
        if self.visible == v {
            return;
        }
        self.visible = v;
        self.last = Instant::now();
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
            self.last = Instant::now();
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

    /// Advance bar smoothing by `dt` seconds toward the latest frame; returns whether anything
    /// is still moving (so animation can stop once the meters have decayed).
    fn step(&mut self, target: Option<&SpectrumFrame>, playing: bool, dt: f32) -> bool {
        let mut moving = false;
        for i in 0..BANDS {
            let t = if playing { target.and_then(|f| f.bands.get(i)).map(|v| *v as f32 / 255.0).unwrap_or(0.0) } else { 0.0 };
            // fast attack, ~1.6/s release
            self.bands[i] = if t > self.bands[i] { t } else { (self.bands[i] - 1.6 * dt).max(t) };
            if self.bands[i] >= self.hold[i] {
                self.hold[i] = self.bands[i];
                self.hold_age[i] = 0.0;
            } else {
                self.hold_age[i] += dt;
                if self.hold_age[i] > 0.45 {
                    self.hold[i] = (self.hold[i] - 0.45 * dt).max(self.bands[i]);
                }
            }
            moving |= self.bands[i] > 0.002 || self.hold[i] > 0.002;
        }
        let db = |d: f32| ((d + 60.0) / 60.0).clamp(0.0, 1.0);
        let (l, r) = if playing { target.map(|f| (db(f.peak_l), db(f.peak_r))).unwrap_or((0.0, 0.0)) } else { (0.0, 0.0) };
        for (p, t) in self.peaks.iter_mut().zip([l, r]) {
            *p = if t > *p { t } else { (*p - 1.2 * dt).max(t) };
            moving |= *p > 0.002;
        }
        moving
    }
}

fn spectrum_canvas(th: Theme, bands: [f32; BANDS], hold: [f32; BANDS]) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |b, _, window, _| {
            let gap = 3.0f32;
            let w = f32::from(b.size.width);
            let h = f32::from(b.size.height);
            let bw = ((w - gap * (BANDS as f32 - 1.0)) / BANDS as f32).max(1.0);
            for i in 0..BANDS {
                let x = f32::from(b.origin.x) + i as f32 * (bw + gap);
                let bh = (bands[i] * h).max(1.0);
                window.paint_quad(fill(Bounds::new(point(px(x), b.origin.y + px(h - bh)), size(px(bw), px(bh))), th.meter));
                let hy = (hold[i] * h).max(2.0);
                window.paint_quad(fill(Bounds::new(point(px(x), b.origin.y + px(h - hy - 2.0)), size(px(bw), px(2.0))), th.text));
            }
        },
    )
    .size_full()
}

fn meter_canvas(th: Theme, level: f32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |b, _, window, _| {
            let segs = 20usize;
            let gap = 2.0f32;
            let w = f32::from(b.size.width);
            let sw = ((w - gap * (segs as f32 - 1.0)) / segs as f32).max(1.0);
            for i in 0..segs {
                let on = (i as f32 + 0.5) / segs as f32 <= level;
                let col = if i >= segs - 2 { th.clip } else if on { th.meter } else { th.highest };
                let col = if on || i >= segs - 2 && level >= 0.9 { col } else { th.highest };
                window.paint_quad(fill(Bounds::new(point(b.origin.x + px(i as f32 * (sw + gap)), b.origin.y), size(px(sw), b.size.height)), col));
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
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f32().min(0.1);
        self.last = now;
        let moving = if self.spectrum_wanted() { self.step(frame.as_ref(), playing, dt) } else { false };
        if self.spectrum_wanted() && (playing || moving) {
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
        let strip = div()
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
            );

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
        let title_block = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.))
            .px(px(16.))
            .pt(px(8.))
            .flex_none()
            .child(div().max_w(px(vw - 80.)).overflow_hidden().child(tracked(&t.title, spec, th.body_font, th.text, window)))
            .child(ts(&th, MONO_DATA).text_color(th.muted).child(SharedString::from(sub)));

        // ---- FFT + meters
        let fft_h = (vh * 0.085).clamp(68.0, 84.0);
        let fft = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(16.))
            .w_full()
            .max_w(px(1040.))
            .h(px(fft_h))
            .flex_none()
            .mt(px(8.))
            .child(div().flex_1().h_full().child(spectrum_canvas(th, self.bands, self.hold)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .w(px(160.))
                    .flex_none()
                    .child(div().flex().flex_row().items_center().gap(px(8.)).child(ts(&th, MONO_CODE).w(px(10.)).text_color(th.muted).child("L")).child(div().flex_1().h(px(10.)).child(meter_canvas(th, self.peaks[0]))))
                    .child(div().flex().flex_row().items_center().gap(px(8.)).child(ts(&th, MONO_CODE).w(px(10.)).text_color(th.muted).child("R")).child(div().flex_1().h(px(10.)).child(meter_canvas(th, self.peaks[1])))),
            );

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
        let seek_row = div()
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
            );
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
        let transport = div()
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
            .child(div().flex().flex_row().items_center().gap(px(8.)).text_color(th.muted).child(icon("volume", 15., th.muted)).child(div().w(px(120.)).child(slider("np-vol", th, &self.vol, ps.volume as f32, None, vol_cb))).child(ts(&th, MONO_CODE).w(px(52.)).text_right().whitespace_nowrap().text_color(th.muted).child(SharedString::from(fmt::volume_label(&ps)))));
        let deck = div().flex().flex_col().gap(px(8.)).w_full().flex_none().pt(px(12.)).bg(th.chrome).border_t_1().border_color(th.frame).child(seek_row).child(transport);
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
