//! The small widget layer, built once and reused by every view. Each file is one widget
//! and stays small: icon, button variants, icon button, tooltip, slider/fader, styled
//! scrollbar, single-line text input, context menu.
pub mod button;
pub mod context_menu;
pub mod scrollbar;
pub mod slider;
pub mod text_input;
pub mod tooltip;

use crate::theme::Theme;
use gpui::{BoxShadow, Hsla, Svg, point, prelude::*, px, svg};

/// Inline-sprite icon (one SVG per icon, white mask recoloured by `color`).
pub fn icon(name: &str, size: f32, color: Hsla) -> Svg {
    svg().path(format!("icons/{name}.svg")).size(px(size)).flex_none().text_color(color)
}

/// Icon with a hover colour: the parent must carry `.group("hv")`. (GPUI svgs only read
/// their own `text_color`, never the parent's, so hover tints go through group styles.)
pub fn icon_g(name: &str, size: f32, color: Hsla, hover: Hsla) -> Svg {
    icon(name, size, color).group_hover("hv", move |s| s.text_color(hover))
}


/// Distance one mouse-wheel notch scrolls: Chromium moves 100 CSS px per notch. GPUI turns a notch into
/// `lines x line height of the scroll element`, so every scroll container (`overflow_y_scroll`, `uniform_list`) calls
/// [`WheelScroll::wheel_scroll`], which sets its line height to a third of that (3 lines per notch). GPUI rounds the
/// line height to whole px, so the effective distance is 3 x 33 = 99 px (1 % under Chromium; 100 / 3 and 33.5 both land
/// on a whole number too: 33 and 34).
pub const WHEEL_PX_PER_NOTCH: f32 = 100.0;

pub trait WheelScroll: Styled + Sized {
    fn wheel_scroll(self) -> Self {
        self.line_height(px((WHEEL_PX_PER_NOTCH / 3.0_f32).floor()))
    }
}
impl<T: Styled> WheelScroll for T {}

/// `box-shadow: Npx Npx 0 <shadow>`: hard offset, no blur. Only for small static elements.
pub fn hard_shadow(th: &Theme, off: f32) -> Vec<BoxShadow> {
    vec![BoxShadow { color: th.shadow, offset: point(px(off), px(off)), blur_radius: px(0.), spread_radius: px(0.) }]
}
