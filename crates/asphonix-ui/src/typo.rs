//! Plain-text styling from the type scale (font only: no tracking, no caps). Single-line text that needs
//! letter-spacing or ellipsis goes through `text::txt`.
use crate::theme::Theme;
use crate::tracking::TypeSpec;
use gpui::{Div, FontWeight, div, prelude::*, px};

/// A div carrying the font family/size/line-height/weight of `spec` (no tracking, no caps).
pub fn ts(th: &Theme, spec: TypeSpec) -> Div {
    div().font_family(spec.family.name(th.body_font)).text_size(px(spec.size)).line_height(px(spec.line)).font_weight(FontWeight(spec.weight))
}

