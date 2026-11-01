//! The one single-line text primitive of the app: CSS-faithful `letter-spacing`, `text-transform:
//! uppercase`, `text-overflow: ellipsis`, fixed `line-height` boxes and a CSS-like baseline.
//!
//! GPUI 0.2 has no letter-spacing and its `text_ellipsis` cannot be combined with per-glyph
//! painting, so [`Txt`] is a small custom element: it asks the layout engine for a *measured* node
//! whose width is `min(natural width, available width)` (like an inline-block with
//! `overflow: hidden; text-overflow: ellipsis` inside a flex row), shapes the line once through
//! the public text system and paints every glyph with `Window::paint_glyph` at
//! `x + i * tracking`. When the text does not fit it keeps the longest glyph prefix that leaves
//! room for "…" (the same rule Chromium applies).
use crate::theme::Theme;
use crate::tracking::TypeSpec;
use gpui::{
    AbsoluteLength, App, AvailableSpace, Bounds, DefiniteLength, Element, ElementId, FontWeight, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, Length, Pixels,
    SharedString, Size, Style, TextRun, Window, font, point, px, size,
};
use std::sync::Arc;

pub struct Txt {
    text: SharedString,
    family: &'static str,
    size: f32,
    line: f32,
    tracking: f32,
    weight: f32,
    /// `None` = inherit the parent's `text_color` (so hover styles on the parent recolour the text).
    color: Option<Hsla>,
    ellipsis: bool,
}

/// A text box in the style of `spec` (family from the theme's body font where the spec says body).
pub fn txt(th: &Theme, spec: TypeSpec, text: impl Into<SharedString>) -> Txt {
    let text: SharedString = text.into();
    let text = if spec.caps { SharedString::from(text.to_uppercase()) } else { text };
    Txt { text, family: spec.family.name(th.body_font), size: spec.size, line: spec.line, tracking: spec.tracking_px(), weight: spec.weight, color: None, ellipsis: false }
}

impl Txt {
    pub fn color(mut self, c: Hsla) -> Self {
        self.color = Some(c);
        self
    }
    /// `letter-spacing: normal`: UA buttons (e.g. the `EntityLink` / `sort-btn` buttons inside a styled span) reset the
    /// inherited tracking, so their text is not letter-spaced even when the surrounding class says so.
    pub fn no_tracking(mut self) -> Self {
        self.tracking = 0.0;
        self
    }
    pub fn weight(mut self, w: f32) -> Self {
        self.weight = w;
        self
    }
    /// `overflow: hidden; text-overflow: ellipsis; min-width: 0` (the box shrinks, the text is cut with "…").
    /// Without it the box is always the natural width and never shrinks (taffy's intrinsic-size pass
    /// over-counts shrinkable measured leaves in `flex-shrink: 0` containers, e.g. the top bar pill).
    pub fn ellipsis(mut self) -> Self {
        self.ellipsis = true;
        self
    }
}

/// GPUI snaps flex allocations to device pixels (1.5 per CSS px here), Chromium rounds intrinsic widths up:
/// an allocation up to one CSS px short of the natural width is not an overflow.
const SNAP: f32 = 1.0;

fn run_for(text: &str, family: &'static str, weight: f32, color: Hsla) -> TextRun {
    let f = css_font(family, weight);
    TextRun { len: text.len(), font: f, color, background_color: None, underline: None, strikethrough: None }
}

/// Natural width in px of `text` (shaped width + one tracking step per glyph).
pub fn natural_width(window: &Window, text: &str, family: &'static str, size: f32, weight: f32, tracking: f32) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    let l = window.text_system().layout_line(text, px(size), &[run_for(text, family, weight, gpui::black())], None);
    let glyphs: usize = l.runs.iter().map(|r| r.glyphs.len()).sum();
    f32::from(l.width) + glyphs as f32 * tracking
}

impl IntoElement for Txt {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Txt {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, _cx: &mut App) -> (LayoutId, ()) {
        let (text, family, sz, weight, tracking, ellipsis, line) = (self.text.clone(), self.family, self.size, self.weight, self.tracking, self.ellipsis, self.line);
        let mut style = Style { flex_shrink: if ellipsis { 1.0 } else { 0.0 }, ..Style::default() };
        if ellipsis {
            style.min_size.width = Length::Definite(DefiniteLength::Absolute(AbsoluteLength::Pixels(px(0.))));
        }
        let id = window.request_measured_layout(style, move |known, avail: Size<AvailableSpace>, window, _| {
            let nat = natural_width(window, &text, family, sz, weight, tracking);
            let w = match known.width {
                Some(w) => f32::from(w),
                None => match avail.width {
                    AvailableSpace::Definite(a) if ellipsis => if nat <= f32::from(a) + SNAP { nat } else { f32::from(a) },
                    AvailableSpace::MinContent if ellipsis => 0.0,
                    _ => nat,
                },
            };
            size(px(w), px(known.height.map(f32::from).unwrap_or(line)))
        });
        (id, ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut Window, _: &mut App) {}

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, _cx: &mut App) {
        if self.text.is_empty() {
            return;
        }
        let ts = window.text_system().clone();
        let color = self.color.unwrap_or_else(|| window.text_style().color);
        let run = run_for(&self.text, self.family, self.weight, color);
        let layout = ts.layout_line(&self.text, px(self.size), std::slice::from_ref(&run), None);
        let tr = self.tracking;
        // glyphs in order: (byte index, x, run index, glyph)
        let mut glyphs: Vec<(usize, f32, usize, usize)> = Vec::new();
        for (ri, r) in layout.runs.iter().enumerate() {
            for (gi, g) in r.glyphs.iter().enumerate() {
                glyphs.push((g.index, f32::from(g.position.x), ri, gi));
            }
        }
        let n = glyphs.len();
        let natural = f32::from(layout.width) + n as f32 * tr;
        let avail = f32::from(bounds.size.width);
        let mut keep = n;
        let mut ell: Option<Arc<gpui::LineLayout>> = None;
        if self.ellipsis && natural > avail + SNAP {
            let el = ts.layout_line("…", px(self.size), &[run_for("…", self.family, self.weight, color)], None);
            let ew = f32::from(el.width) + tr;
            keep = 0;
            for k in (0..=n).rev() {
                let prefix = if k == n { f32::from(layout.width) } else { glyphs[k].1 };
                if prefix + k as f32 * tr + ew <= avail + 0.01 {
                    keep = k;
                    break;
                }
            }
            ell = Some(el);
        }
        // CSS baseline: font metrics are rounded to whole device pixels, half-leading splits the rest.
        let s = window.scale_factor();
        let asc = (f32::from(layout.ascent) * s).round();
        let desc = (f32::from(layout.descent) * s).round();
        let half = (self.line * s - (asc + desc)) / 2.0;
        let baseline_dev = (f32::from(bounds.origin.y) * s + half + asc).round();
        let baseline = px(baseline_dev / s);
        let x0 = f32::from(bounds.origin.x);
        for (i, (_, gx, ri, gi)) in glyphs.iter().take(keep).enumerate() {
            let r = &layout.runs[*ri];
            let g = &r.glyphs[*gi];
            if !g.is_emoji {
                let _ = window.paint_glyph(point(px(x0 + gx + i as f32 * tr), baseline), r.font_id, g.id, px(self.size), color);
            }
        }
        if let Some(el) = ell {
            let base = if keep == n { f32::from(layout.width) } else { glyphs[keep].1 };
            let ex = x0 + base + keep as f32 * tr;
            let mut j = 0usize;
            for r in &el.runs {
                for g in &r.glyphs {
                    if !g.is_emoji {
                        let _ = window.paint_glyph(point(px(ex + f32::from(g.position.x) + j as f32 * tr), baseline), r.font_id, g.id, px(self.size), color);
                    }
                    j += 1;
                }
            }
        }
    }
}

/// Chromium shapes with the font's `kern` (GPOS pair kerning) on; DirectWrite via GPUI leaves it off unless the
/// feature is requested explicitly ("AV", "AY", "PLAY" were 2-6 % wider natively). Every face we create goes through this.
pub fn css_font(family: &'static str, weight: f32) -> gpui::Font {
    let mut f = font(family);
    f.weight = FontWeight(weight);
    f.features = gpui::FontFeatures(Arc::new(vec![("kern".into(), 1)]));
    f
}
