//! `BootIntro.svelte`: the one-shot launch animation (1.1 s). Everything is painted by one canvas from the shared clock
//! (`crate::clock::now`), so the parity runner can freeze/seek it like the web's paused CSS/WAAPI animations.
//!
//! Timings, easings and keyframes are the web's (see the constants). Two deliberate approximations, both because GPUI has
//! no radial gradients / element transforms: (1) the rings and the bloom are drawn as stacks of thin concentric annuli
//! (rounded quads with a border) whose alpha follows the CSS gradient stops; (2) the letters fade in without the 0.9 -> 1 scale.
use crate::anim::cubic_bezier;
use crate::theme::{DISPLAY, MONO, Theme};
use gpui::{
    App, Bounds, BorderStyle, Context, ContentMask, EventEmitter, Hsla, IntoElement, Render, Rgba, SharedString, TextRun, Window, canvas, div, point, prelude::*, px, quad, size,
};
use std::sync::Arc;

pub enum BootEvent {
    Done,
}
impl EventEmitter<BootEvent> for BootIntro {}

/// `introWave.ts` WAVE_ENVELOPE / WAVE_BANDS (a real track's loudness envelope and spectrum snapshot).
const ENV: [f32; 50] = [
    0., 0.07, 0.22, 0.4, 0.47, 0.4, 0.26, 0.17, 0.14, 0.13, 0.13, 0.12, 0.14, 0.22, 0.4, 0.65, 0.88, 1., 0.98, 0.88, 0.78, 0.78, 0.83, 0.84, 0.79, 0.74, 0.75, 0.78, 0.8, 0.81, 0.83, 0.85, 0.89, 0.94, 0.95, 0.87, 0.71, 0.59,
    0.58, 0.69, 0.81, 0.88, 0.91, 0.91, 0.91, 0.89, 0.85, 0.82, 0.81, 0.81,
];
const BANDS: [f32; 40] = [
    0.96, 1., 0.95, 0.96, 0.78, 0.55, 0.39, 0.37, 0.32, 0.42, 0.47, 0.66, 0.62, 0.32, 0.21, 0.41, 0.49, 0.24, 0.37, 0.31, 0.26, 0.25, 0.21, 0.13, 0.15, 0.39, 0.27, 0.21, 0.14, 0.08, 0.08, 0.08, 0.08, 0.08, 0.08, 0.08,
    0.08, 0.08, 0.08, 0.08,
];

/// Total length: `setTimeout(finish, 1100)`.
pub const DURATION: f64 = 1.1;

pub struct BootIntro {
    th: Theme,
    start: f64,
    backdrop: Option<Arc<gpui::RenderImage>>,
    done: bool,
    /// Skip pressed at this clock time: everything fades over 120 ms, finishing after 140 ms.
    skipped_at: Option<f64>,
}

impl BootIntro {
    pub fn new(th: Theme, backdrop: Option<Arc<gpui::RenderImage>>) -> Self {
        Self { th, start: crate::clock::now(), backdrop, done: false, skipped_at: None }
    }
    pub fn skip(&mut self, cx: &mut Context<Self>) {
        if self.skipped_at.is_none() {
            self.skipped_at = Some(crate::clock::now());
            cx.notify();
        }
    }
}

fn clamp01(x: f64) -> f32 {
    x.clamp(0.0, 1.0) as f32
}
fn ease_out(u: f32) -> f32 {
    cubic_bezier(0.0, 0.0, 0.58, 1.0, u)
}
fn ease_in(u: f32) -> f32 {
    cubic_bezier(0.42, 0.0, 1.0, 1.0, u)
}

/// A CSS animation of `dur` s starting at `delay` s, evaluated at `t`: progress 0..1 (before the delay = 0, `fill: forwards`).
fn prog(t: f64, delay: f64, dur: f64) -> f32 {
    clamp01((t - delay) / dur)
}

fn rgba(c: (u8, u8, u8), a: f32) -> Hsla {
    Rgba { r: c.0 as f32 / 255.0, g: c.1 as f32 / 255.0, b: c.2 as f32 / 255.0, a: a.clamp(0.0, 1.0) }.into()
}

/// One WAAPI frame of `playWave`: (opacity, scale) of a ring/bloom at its own time `el` seconds (40 linear keyframes).
fn wave_frame(el: f64, dur: f64, peak: f32, bloom: bool) -> (f32, f32) {
    let u = (el / dur).clamp(0.0, 1.0);
    let at = |t: f64| ENV[((t / 0.02).round().max(0.0) as usize).min(ENV.len() - 1)];
    let key = |i: usize| {
        let u = i as f64 / 40.0;
        let ease = 1.0 - (1.0 - u).powi(3);
        let fade = (u / 0.2).min(1.0) * (1.0 - u).powf(1.2);
        let loud = 0.45 + 0.55 * at(u * dur) as f64;
        (peak as f64 * fade * loud, 0.02 + (if bloom { 0.88 } else { 0.98 }) * ease)
    };
    let pos = u * 40.0;
    let i = (pos.floor() as usize).min(39);
    let f = pos - i as f64;
    let (a, b) = (key(i), key(i + 1));
    ((a.0 + (b.0 - a.0) * f) as f32, (a.1 + (b.1 - a.1) * f) as f32)
}

/// A circle outline of outer radius `r`, `thick` wide, centred at `c` (CSS px).
fn annulus(window: &mut Window, c: (f32, f32), r: f32, thick: f32, color: Hsla) {
    if color.a <= 0.002 || r <= 0.3 {
        return;
    }
    let t = thick.min(r);
    let b = Bounds::new(point(px(c.0 - r), px(c.1 - r)), size(px(2.0 * r), px(2.0 * r)));
    window.paint_quad(quad(b, px(r), gpui::transparent_black(), t, color, BorderStyle::Solid));
}

/// Gradient stops as `(position 0..1, alpha)`; returns the interpolated alpha.
fn stops_at(stops: &[(f32, f32)], p: f32) -> f32 {
    for w in stops.windows(2) {
        if p >= w[0].0 && p <= w[1].0 {
            let f = if w[1].0 > w[0].0 { (p - w[0].0) / (w[1].0 - w[0].0) } else { 0.0 };
            return w[0].1 + (w[1].1 - w[0].1) * f;
        }
    }
    0.0
}

impl Render for BootIntro {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = self.th;
        let now = crate::clock::now();
        let t = (now - self.start).max(0.0);
        let skip_t = self.skipped_at.map(|s| now - s);
        if (skip_t.map(|s| s >= 0.14).unwrap_or(false) || t >= DURATION) && !self.done {
            self.done = true;
            cx.emit(BootEvent::Done);
        }
        if !self.done && !crate::clock::frozen() {
            window.request_animation_frame();
        }
        let backdrop = self.backdrop.clone();
        let light = th.light;
        let (c1, c2, c3) = if light { ((255, 110, 180), (90, 155, 255), (50, 205, 170)) } else { ((255, 60, 150), (60, 130, 255), (20, 215, 185)) };
        let (meter, text, muted, accent_fg) = (th.meter, th.text, th.muted, th.accent_fg);
        // `.skip { opacity: 0; transition: opacity 120ms linear }`
        let master = skip_t.map(|s| 1.0 - clamp01(s / 0.12)).unwrap_or(1.0);
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| this.skip(cx)))
            .on_key_down(cx.listener(|this, _, _, cx| {
                this.skip(cx);
                cx.stop_propagation();
            }))
            .child(
                canvas(
                    |_, _, _| (),
                    move |b, _, window, cx| {
                        let (vw, vh) = (f32::from(b.size.width), f32::from(b.size.height));
                        let (ox, oy) = (f32::from(b.origin.x), f32::from(b.origin.y));
                        // ---- shutter halves: translateY(-100% / 100%) after 820 ms (cubic-bezier(.8,0,.2,1), 300 ms)
                        let open = cubic_bezier(0.8, 0.0, 0.2, 1.0, prog(t, 0.82, 0.3));
                        for (half, dir) in [(0usize, -1.0f32), (1, 1.0)] {
                            let hh = vh / 2.0;
                            let y0 = if half == 0 { 0.0 } else { hh };
                            let shift = dir * open * hh;
                            let area = Bounds::new(point(px(ox), px(oy + y0 + shift)), size(px(vw), px(hh)));
                            let mask = ContentMask { bounds: area };
                            window.with_content_mask(Some(mask), |window| {
                                let full = Bounds::new(point(px(ox), px(oy + shift)), size(px(vw), px(vh)));
                                match &backdrop {
                                    Some(img) => {
                                        let _ = window.paint_image(full, gpui::Corners::default(), img.clone(), 0, false);
                                    }
                                    None => window.paint_quad(gpui::fill(full, th.base)),
                                }
                            });
                        }
                        // ---- `.run .wave, .run .content { animation: vanish 200ms ease-in 800ms }`
                        let vanish = 1.0 - ease_in(prog(t, 0.8, 0.2));
                        let alpha_all = vanish * master;
                        // ---- rings + bloom (centre of the screen, 220vmax elements)
                        let c = (ox + vw / 2.0, oy + vh / 2.0);
                        let vmax = vw.max(vh);
                        let (bl_o, bl_s) = wave_frame(t, 0.85, 0.8, true);
                        let bloom_stops = [(0.0f32, 0.4f32), (0.28, 0.36), (0.55, 0.3), (1.0, 0.0)];
                        let bloom_col = |p: f32| -> Hsla {
                            // colours: w3 0 %, w1 28 %, w2 55 %, transparent (w2-ish) 100 %
                            let (a, bcol, f) = if p < 0.28 { (c3, c1, p / 0.28) } else if p < 0.55 { (c1, c2, (p - 0.28) / 0.27) } else { (c2, c2, 0.0) };
                            let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * f) as u8;
                            rgba((mix(a.0, bcol.0), mix(a.1, bcol.1), mix(a.2, bcol.2)), stops_at(&bloom_stops, p) * bl_o * alpha_all)
                        };
                        let r_bloom = 1.1 * vmax * bl_s;
                        let steps = 40;
                        for i in 0..steps {
                            // outermost first: the annulus from p_i to p_{i+1} of the radius
                            let p0 = 1.0 - (i as f32 + 1.0) / steps as f32;
                            let p1 = 1.0 - i as f32 / steps as f32;
                            let pm = (p0 + p1) / 2.0;
                            annulus(window, c, r_bloom * p1, r_bloom * (p1 - p0) + 0.15, bloom_col(pm));
                        }
                        let ring_stops = [(0.0f32, 0.0f32), (0.86, 0.0), (0.93, 0.55), (0.97, 0.2), (1.0, 0.0)];
                        for (k, col) in [c1, c2, c3].into_iter().enumerate() {
                            let delay = k as f64 * 0.14;
                            if t < delay {
                                continue;
                            }
                            let (o, s) = wave_frame(t - delay, 0.8, 0.9, false);
                            let r = 1.1 * vmax * s;
                            let n = 28;
                            for i in 0..n {
                                let p0 = 0.86 + 0.14 * i as f32 / n as f32;
                                let p1 = 0.86 + 0.14 * (i + 1) as f32 / n as f32;
                                let a = stops_at(&ring_stops, (p0 + p1) / 2.0) * o * alpha_all;
                                annulus(window, c, r * p1, r * (p1 - p0) + 0.15, rgba(col, a));
                            }
                        }
                        // ---- content: wordmark, bars, meta
                        let font_px = (vw * 0.11).clamp(64.0, 148.0);
                        let tracking = -0.04 * font_px;
                        let letters = ["S", "O", "N", "I", "X"];
                        let mut f = gpui::font(DISPLAY);
                        f.weight = gpui::FontWeight(700.0);
                        let shaped: Vec<_> = letters
                            .iter()
                            .map(|l| {
                                let run = TextRun { len: l.len(), font: f.clone(), color: text, background_color: None, underline: None, strikethrough: None };
                                window.text_system().shape_line(SharedString::from(*l), px(font_px), &[run], None)
                            })
                            .collect();
                        let widths: Vec<f32> = shaped.iter().map(|s| f32::from(s.width) + tracking).collect();
                        let total: f32 = widths.iter().sum();
                        let content_h = font_px + 22.0 + 34.0 + 22.0 + 14.0;
                        let top = oy + (vh - content_h) / 2.0;
                        let mut x = ox + (vw - total) / 2.0;
                        for (i, (l, w)) in shaped.iter().zip(&widths).enumerate() {
                            let d = (i as f64 - 2.0).abs();
                            let a = ease_out(prog(t, 0.15 + d * 0.06, 0.3));
                            // re-shape with the faded colour (the run colour is baked into the line)
                            let run = TextRun { len: letters[i].len(), font: f.clone(), color: text.opacity(a * alpha_all), background_color: None, underline: None, strikethrough: None };
                            let line = window.text_system().shape_line(SharedString::from(letters[i]), px(font_px), &[run], None);
                            let _ = l;
                            let _ = line.paint(point(px(x), px(top)), px(font_px), window, cx);
                            x += *w;
                        }
                        let bars_top = top + font_px + 22.0;
                        let total_w = 40.0 * 5.0 + 39.0 * 3.0;
                        let bx0 = ox + (vw - total_w) / 2.0;
                        for (j, h) in BANDS.iter().enumerate() {
                            let d = (j as f64 - 19.5).abs();
                            let u = prog(t, 0.22 + d * 0.008, 0.52);
                            // @keyframes bar: 0 % scaleY(0), 45 % scaleY(h), 100 % scaleY(.08); the easing applies per interval
                            let e = |x: f32| cubic_bezier(0.2, 0.8, 0.3, 1.0, x);
                            let s = if u < 0.45 { *h * e(u / 0.45) } else { *h + (0.08 - *h) * e((u - 0.45) / 0.55) };
                            let bh = 34.0 * s;
                            let r = Bounds::new(point(px(bx0 + j as f32 * 8.0), px(bars_top + 34.0 - bh)), size(px(5.0), px(bh)));
                            if bh > 0.0 && u > 0.0 {
                                window.paint_quad(gpui::fill(r, meter.opacity(alpha_all)));
                            }
                        }
                        // meta: `.meta { gap: 14px; letter-spacing: .18em }` mono-code 11 px
                        let meta_a = ease_out(prog(t, 0.45, 0.24)) * alpha_all;
                        let meta_top = bars_top + 34.0 + 22.0;
                        let parts = [("BIT-PERFECT AUDIO TERMINAL".to_string(), muted), (format!("v{}", env!("CARGO_PKG_VERSION")), accent_fg)];
                        let mut mf = gpui::font(MONO);
                        mf.weight = gpui::FontWeight(400.0);
                        let tr = 0.18 * 11.0;
                        let lines: Vec<_> = parts
                            .iter()
                            .map(|(s, col)| {
                                let run = TextRun { len: s.len(), font: mf.clone(), color: col.opacity(meta_a), background_color: None, underline: None, strikethrough: None };
                                let line = window.text_system().shape_line(SharedString::from(s.clone()), px(11.0), &[run], None);
                                let w = f32::from(line.width) + tr * s.chars().count() as f32;
                                (line, w, s.clone())
                            })
                            .collect();
                        let tw: f32 = lines.iter().map(|l| l.1).sum::<f32>() + 14.0;
                        let mut mx = ox + (vw - tw) / 2.0;
                        for (line, w, s) in &lines {
                            // paint glyph by glyph with the tracking step (the per-glyph path of `tracking.rs`)
                            let run_len = s.chars().count();
                            let _ = run_len;
                            let mut gx = mx;
                            for ch in s.chars() {
                                let g = ch.to_string();
                                let run = TextRun { len: g.len(), font: mf.clone(), color: if s.starts_with('v') { accent_fg } else { muted }.opacity(meta_a), background_color: None, underline: None, strikethrough: None };
                                let gl = window.text_system().shape_line(SharedString::from(g), px(11.0), &[run], None);
                                let _ = gl.paint(point(px(gx), px(meta_top)), px(14.0), window, cx);
                                gx += f32::from(gl.width) + tr;
                            }
                            let _ = line;
                            mx += *w + 14.0;
                        }
                    },
                )
                .size_full(),
            )
    }
}

#[allow(dead_code)]
fn _app(_: &App) {}
