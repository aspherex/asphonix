//! Animation primitives driven by `crate::clock` (so the parity runner can freeze/step time):
//! CSS timing functions, the hover-transition tracker and the now-playing bars' keyframes.
use crate::metrics::anim as m;
use gpui::{Window, point, px};
use std::cell::RefCell;
use std::collections::HashMap;

/// CSS `cubic-bezier(x1, y1, x2, y2)` evaluated at time fraction `t` in [0, 1].
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let (cx, bx) = (3.0 * x1, 3.0 * (x2 - x1) - 3.0 * x1);
    let ax = 1.0 - cx - bx;
    let (cy, by) = (3.0 * y1, 3.0 * (y2 - y1) - 3.0 * y1);
    let ay = 1.0 - cy - by;
    let x_at = |s: f32| ((ax * s + bx) * s + cx) * s;
    let dx_at = |s: f32| (3.0 * ax * s + 2.0 * bx) * s + cx;
    // Newton, then bisection fallback
    let mut s = t;
    for _ in 0..8 {
        let err = x_at(s) - t;
        if err.abs() < 1e-6 {
            break;
        }
        let d = dx_at(s);
        if d.abs() < 1e-6 {
            break;
        }
        s -= err / d;
    }
    if !(0.0..=1.0).contains(&s) || (x_at(s) - t).abs() > 1e-4 {
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        s = t;
        for _ in 0..40 {
            if x_at(s) < t {
                lo = s;
            } else {
                hi = s;
            }
            s = (lo + hi) / 2.0;
        }
    }
    ((ay * s + by) * s + cy) * s
}

/// CSS `ease` (the default `transition-timing-function`).
pub fn ease(t: f32) -> f32 {
    cubic_bezier(0.25, 0.1, 0.25, 1.0, t)
}
/// CSS `ease-in-out`.
pub fn ease_in_out(t: f32) -> f32 {
    cubic_bezier(0.42, 0.0, 0.58, 1.0, t)
}

/// `scaleY` of bar `i` at clock time `t` seconds (`@keyframes bar`, 0.9 s ease-in-out infinite, negative delays).
pub fn bar_scale(i: usize, t: f32) -> f32 {
    let p = ((t - m::BARS_DELAY[i]) / m::BARS_PERIOD).rem_euclid(1.0);
    // animation-timing-function applies per keyframe interval: 0..50 % up, 50..100 % down
    let (a, b, u) = if p < 0.5 { (m::BARS_MIN, m::BARS_MAX, p / 0.5) } else { (m::BARS_MAX, m::BARS_MIN, (p - 0.5) / 0.5) };
    a + (b - a) * ease_in_out(u)
}

/// Heights (CSS px) of the three bars: animated while playing, static `scaleY(.33)` when paused.
pub fn bar_heights(playing: bool, t: f32) -> [f32; 3] {
    let h = crate::metrics::tracks::BARS_H;
    if playing { [bar_scale(0, t) * h, bar_scale(1, t) * h, bar_scale(2, t) * h] } else { [m::BARS_PAUSED * h; 3] }
}

#[derive(Clone, Copy)]
struct Tween {
    from: f32,
    to: f32,
    start: f64,
}

thread_local! {
    static TWEENS: RefCell<HashMap<(&'static str, usize), Tween>> = RefCell::new(HashMap::new());
}

/// A CSS-like `transition` of one value: call every frame with the *target* value; returns the
/// current eased value and asks for another frame while it is still moving (not while the test
/// clock is frozen: the runner steps the clock and repaints itself). `key` identifies the element.
pub fn transition(window: &mut Window, key: (&'static str, usize), target: f32, duration: f32) -> f32 {
    let now = crate::clock::now();
    TWEENS.with(|t| {
        let mut t = t.borrow_mut();
        let tw = t.entry(key).or_insert(Tween { from: target, to: target, start: now - duration as f64 });
        if (tw.to - target).abs() > f32::EPSILON {
            // retarget from where the old transition currently is
            let cur = value_at(tw, now, duration);
            *tw = Tween { from: cur, to: target, start: now };
        }
        let v = value_at(tw, now, duration);
        if (now - tw.start) < duration as f64 && !crate::clock::frozen() {
            window.request_animation_frame();
        }
        v
    })
}

fn value_at(tw: &Tween, now: f64, duration: f32) -> f32 {
    let u = (((now - tw.start) as f32) / duration).clamp(0.0, 1.0);
    tw.from + (tw.to - tw.from) * ease(u)
}

/// Drop all running transitions (minimize: nothing may keep animating).
pub fn clear() {
    TWEENS.with(|t| t.borrow_mut().clear());
}

/// Paint helper kept here so views stay free of quad maths.
pub fn paint_bars(window: &mut Window, origin: gpui::Point<gpui::Pixels>, color: gpui::Hsla, heights: [f32; 3]) {
    let (w, gap, full) = (crate::metrics::tracks::BAR_W, crate::metrics::tracks::BARS_GAP, crate::metrics::tracks::BARS_H);
    for (i, h) in heights.iter().enumerate() {
        let x = f32::from(origin.x) + i as f32 * (w + gap);
        let y = f32::from(origin.y) + full - h;
        window.paint_quad(gpui::fill(gpui::Bounds::new(point(px(x), px(y)), gpui::size(px(w), px(*h))), color));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bezier_endpoints_and_symmetry() {
        assert!(ease(0.0).abs() < 1e-5 && (ease(1.0) - 1.0).abs() < 1e-5);
        assert!((ease_in_out(0.5) - 0.5).abs() < 1e-3);
        // CSS `ease` is ahead of linear early on
        assert!(ease(0.25) > 0.35 && ease(0.25) < 0.50);
    }

    #[test]
    fn bars_follow_the_keyframes() {
        // bar 3 (no delay): starts at scaleY(.25), peaks at 0.45 s, back at 0.9 s
        assert!((bar_scale(2, 0.0) - 0.25).abs() < 1e-4);
        assert!((bar_scale(2, 0.45) - 1.0).abs() < 1e-3);
        assert!((bar_scale(2, 0.9) - 0.25).abs() < 1e-3);
        // negative delay -0.6 s: bar 1 is 0.6 s into the cycle at t = 0 (past its peak, heading down)
        let s = bar_scale(0, 0.0);
        assert!(s > 0.25 && s < 1.0);
        assert_eq!(bar_heights(false, 3.3), [0.33 * 12.0; 3]);
    }
}
