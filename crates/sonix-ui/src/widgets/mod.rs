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


/// Hard offset shadow, no blur. Only for small static elements.
pub fn hard_shadow(th: &Theme, off: f32) -> Vec<BoxShadow> {
    vec![BoxShadow { color: th.shadow, offset: point(px(off), px(off)), blur_radius: px(0.), spread_radius: px(0.) }]
}

thread_local! {
    static KEY_NAV: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Keyboard-navigation flag: true after a Tab / Shift+Tab until the next mouse press, so focus styles of buttons only
/// show for keyboard navigation (GPUI has no focus-visible state).
pub fn key_nav() -> bool {
    KEY_NAV.with(|k| k.get())
}

pub fn set_key_nav(on: bool) {
    KEY_NAV.with(|k| k.set(on));
}

/// Keyboard focus indicator for controls without a border of their own: a ring drawn as a spread shadow outside the
/// element (no layout shift), in `th.focus` (>= 17:1 against the page in both themes, WCAG 2.4.7 / 1.4.11).
pub fn focus_ring(th: Theme) -> impl Fn(gpui::StyleRefinement) -> gpui::StyleRefinement + 'static {
    move |s| if key_nav() { ring(s, th) } else { s }
}

/// Adds the focus ring to a focus style that also changes other properties.
pub fn ring(s: gpui::StyleRefinement, th: Theme) -> gpui::StyleRefinement {
    s.shadow(vec![BoxShadow { color: th.focus, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(crate::metrics::FOCUS_RING) }])
}
