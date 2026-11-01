//! Letter-spacing (tracking) for GPUI 0.2.x, which has no `letter-spacing` in
//! `TextStyle`. Done in our own code, no GPUI patch: shape the whole line once (so
//! kerning/shaping stay intact), then paint each glyph ourselves through the public
//! `Window::paint_glyph` at `x + i * tracking`. Like CSS, the spacing is added after
//! *every* glyph, including the last, so a tracked line is `n * tracking` wider.
//!
//! Every `.type-label-caps`-style label (and every display headline with negative
//! tracking) goes through [`tracked`]; plain body/data text keeps GPUI's native text
//! path (the 0.01-0.02em tracking of those styles is below a pixel) so ellipsis
//! truncation still works.
use crate::theme::{DISPLAY, MONO};
use gpui::{Hsla, IntoElement, Pixels, SharedString, TextRun, Window, canvas, div, point, prelude::*, px};

/// One entry of the DESIGN.md type scale (tokens.css `--type-*`).
#[derive(Clone, Copy, Debug)]
pub struct TypeSpec {
    pub family: Family,
    pub size: f32,
    pub line: f32,
    /// `letter-spacing` in em.
    pub tracking_em: f32,
    pub weight: f32,
    /// `text-transform: uppercase`
    pub caps: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Display,
    Body,
    Mono,
}

impl Family {
    pub fn name(self, body: &'static str) -> &'static str {
        match self {
            Family::Display => DISPLAY,
            Family::Body => body,
            Family::Mono => MONO,
        }
    }
}

macro_rules! ty {
    ($n:ident, $fam:ident, $size:expr, $line:expr, $tr:expr, $w:expr, $caps:expr) => {
        // the whole DESIGN.md scale is kept, including steps no ported screen uses yet (MONO_METRIC)
        #[allow(dead_code)]
        pub const $n: TypeSpec = TypeSpec { family: Family::$fam, size: $size, line: $line, tracking_em: $tr, weight: $w, caps: $caps };
    };
}
// name, family, size, line, tracking (em), weight, caps  (tokens.css + base.css `.type-*`)
ty!(DISPLAY_XL, Display, 48.0, 52.0, -0.03, 700.0, true);
ty!(HEADLINE_LG, Display, 32.0, 38.0, -0.02, 700.0, true);
ty!(HEADLINE_MD, Display, 24.0, 30.0, -0.01, 600.0, true);
ty!(HEADLINE_SM, Display, 18.0, 24.0, 0.0, 600.0, true);
ty!(BODY_LG, Body, 15.0, 22.0, -0.01, 500.0, false);
ty!(BODY_MD, Body, 13.0, 18.0, 0.0, 400.0, false);
ty!(BODY_SM, Body, 12.0, 16.0, 0.01, 400.0, false);
ty!(MONO_METRIC, Mono, 20.0, 24.0, -0.02, 700.0, false);
ty!(MONO_DATA, Mono, 12.0, 16.0, 0.02, 500.0, false);
ty!(MONO_CODE, Mono, 11.0, 14.0, 0.04, 400.0, false);
ty!(LABEL_CAPS, Mono, 10.0, 12.0, 0.08, 700.0, true);

impl TypeSpec {
    pub fn tracking_px(&self) -> f32 {
        self.tracking_em * self.size
    }
}

/// Width of a tracked line: shaped width plus one tracking step per glyph.
pub fn tracked_width(base_width: f32, glyphs: usize, tracking: f32) -> f32 {
    base_width + glyphs as f32 * tracking
}

/// x of the `i`-th glyph (0-based) of a tracked line, given its untracked x.
pub fn tracked_glyph_x(base_x: f32, i: usize, tracking: f32) -> f32 {
    base_x + i as f32 * tracking
}

/// A tracked (and, per the spec, optionally uppercased) single-line label.
/// Returns a fixed-size element; wrap it in the caller's own flex container.
pub fn tracked(text: &str, spec: TypeSpec, body_font: &'static str, color: Hsla, window: &mut Window) -> impl IntoElement + use<> {
    let text: SharedString = if spec.caps { text.to_uppercase().into() } else { text.to_string().into() };
    let f = crate::text::css_font(spec.family.name(body_font), spec.weight);
    let run = TextRun { len: text.len(), font: f, color, background_color: None, underline: None, strikethrough: None };
    let layout = window.text_system().layout_line(&text, px(spec.size), &[run], None);
    let tracking = spec.tracking_px();
    let glyphs: usize = layout.runs.iter().map(|r| r.glyphs.len()).sum();
    let width = tracked_width(f32::from(layout.width), glyphs, tracking);
    let line = spec.line;
    let size = spec.size;
    div().w(px(width.max(0.0))).h(px(line)).flex_none().child(
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let pad_top = (px(line) - layout.ascent - layout.descent) / 2.0;
                let baseline: Pixels = bounds.origin.y + pad_top + layout.ascent;
                let mut i = 0usize;
                for run in &layout.runs {
                    for g in &run.glyphs {
                        if g.is_emoji {
                            continue;
                        }
                        let x = px(tracked_glyph_x(f32::from(bounds.origin.x + g.position.x), i, tracking));
                        let _ = window.paint_glyph(point(x, baseline), run.font_id, g.id, px(size), color);
                        i += 1;
                    }
                }
            },
        )
        .size_full(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_adds_spacing_after_every_glyph_including_the_last() {
        // "TRACKS" at 10 px / 0.08em = 0.8 px per glyph, 6 glyphs
        let t = LABEL_CAPS.tracking_px();
        assert!((t - 0.8).abs() < 1e-6);
        assert!((tracked_width(36.0, 6, t) - 40.8).abs() < 1e-5);
        assert_eq!(tracked_width(36.0, 0, t), 36.0);
        assert_eq!(tracked_width(0.0, 6, t), 6.0 * t);
    }

    #[test]
    fn glyph_positions_shift_by_index_times_tracking() {
        let t = 0.8;
        assert_eq!(tracked_glyph_x(10.0, 0, t), 10.0);
        assert!((tracked_glyph_x(10.0, 5, t) - 14.0).abs() < 1e-6);
        // the last glyph's trailing space makes the box wider than its right edge
        let advance = 6.0;
        let last_right = tracked_glyph_x(0.0, 2, t) + advance;
        assert!(tracked_width(3.0 * advance, 3, t) > last_right);
    }

    #[test]
    fn negative_tracking_tightens_display_type() {
        let t = DISPLAY_XL.tracking_px();
        assert!((t + 1.44).abs() < 1e-5, "-0.03em of 48px");
        assert!(tracked_width(300.0, 10, t) < 300.0);
    }

    #[test]
    fn type_scale_matches_tokens() {
        assert_eq!((LABEL_CAPS.size, LABEL_CAPS.line, LABEL_CAPS.weight), (10.0, 12.0, 700.0));
        assert_eq!((LABEL_CAPS.caps, HEADLINE_MD.caps, MONO_DATA.caps), (true, true, false));
        assert_eq!((BODY_MD.size, BODY_MD.line), (13.0, 18.0));
        assert_eq!(MONO_DATA.family, Family::Mono);
    }
}
