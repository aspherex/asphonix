//! Plain-text styling from the type scale. Text that needs tracking goes through
//! `tracking::tracked`; everything else (row cells, ellipsised titles) uses these.
use crate::theme::Theme;
use crate::tracking::TypeSpec;
use gpui::{Div, FontWeight, IntoElement, div, prelude::*, px};

/// A div carrying the font family/size/line-height/weight of `spec` (no tracking, no caps).
pub fn ts(th: &Theme, spec: TypeSpec) -> Div {
    div().font_family(spec.family.name(th.body_font)).text_size(px(spec.size)).line_height(px(spec.line)).font_weight(FontWeight(spec.weight))
}

/// Single-line text that truncates with an ellipsis inside a flex cell.
pub fn ellipsis(d: Div) -> Div {
    d.whitespace_nowrap().overflow_hidden().text_ellipsis().min_w_0().w_full()
}

/// Ellipsised line inside a *column* container. Wrapped in a one-item row so the text node
/// gets its width from flex growth (percentage widths resolved to 0 in GPUI's text measure).
/// Use like a div: `ellipsis_line(ts(..).text_color(..)).child("text")`.
pub struct EllipsisLine(Div);

pub fn ellipsis_line(d: Div) -> EllipsisLine {
    EllipsisLine(d)
}

impl EllipsisLine {
    pub fn text_color(self, c: gpui::Hsla) -> Self {
        Self(self.0.text_color(c))
    }
    pub fn child(self, c: impl IntoElement) -> Div {
        div().flex().flex_row().w_full().min_w_0().child(self.0.flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_left().child(c))
    }
}
