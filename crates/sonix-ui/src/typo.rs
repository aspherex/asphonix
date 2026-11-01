//! Plain-text styling from the type scale. Text that needs tracking goes through
//! `tracking::tracked`; everything else (row cells, ellipsised titles) uses these.
use crate::theme::Theme;
use crate::tracking::TypeSpec;
use gpui::{Div, FontWeight, div, prelude::*, px};

/// A div carrying the font family/size/line-height/weight of `spec` (no tracking, no caps).
pub fn ts(th: &Theme, spec: TypeSpec) -> Div {
    div().font_family(spec.family.name(th.body_font)).text_size(px(spec.size)).line_height(px(spec.line)).font_weight(FontWeight(spec.weight))
}

/// Single-line text that truncates with an ellipsis inside a flex cell.
pub fn ellipsis(d: Div) -> Div {
    d.whitespace_nowrap().overflow_hidden().text_ellipsis().min_w_0().w_full()
}
