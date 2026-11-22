//! The small widget layer, built once and reused by every view. Each file is one widget
//! and stays small: icon, button variants, icon button, tooltip, slider/fader, styled
//! scrollbar, single-line text input, context menu.
pub mod button;
pub mod context_menu;
pub mod scrollbar;
pub mod skeleton;
pub mod slider;
pub mod smooth;
pub mod text_input;
pub mod tooltip;
pub mod ui;

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


/// `box-shadow: Npx Npx 0 <shadow>`: hard offset, no blur. Only for small static elements.
pub fn hard_shadow(th: &Theme, off: f32) -> Vec<BoxShadow> {
    vec![BoxShadow { color: th.shadow, offset: point(px(off), px(off)), blur_radius: px(0.), spread_radius: px(0.) }]
}

thread_local! {
    static KEY_NAV: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `:focus-visible` stand-in: true after a Tab / Shift+Tab until the next mouse press, so focus styles of buttons only
/// show for keyboard navigation (GPUI has no `:focus-visible`).
pub fn key_nav() -> bool {
    KEY_NAV.with(|k| k.get())
}

pub fn set_key_nav(on: bool) {
    KEY_NAV.with(|k| k.set(on));
}
