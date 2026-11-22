//! Skeleton placeholders for pages whose data arrives after the first frame. Native-only (the web shows a one-line
//! `LOADING …` status): a page paints the geometry of the loaded page with these fills, so arrival only fills content
//! in and nothing moves (docs/PARITY.md, "Layout-stable skeletons"). Static like the web's `.skeleton-*` rules
//! (no animation), so a loading page never keeps the frame loop alive.
use crate::theme::Theme;
use gpui::{Div, ParentElement, Styled, div, px};

/// `.skeleton-line`: `surface-mid` at 60 % opacity, `h` px tall, full width unless narrowed by the caller.
pub fn line(th: &Theme, h: f32) -> Div {
    div().h(px(h)).bg(th.mid).opacity(0.6)
}

/// A filled `surface-mid` rectangle (60 %), `w` x `h` px, not shrinking.
pub fn block(th: &Theme, w: f32, h: f32) -> Div {
    div().w(px(w)).h(px(h)).flex_none().bg(th.mid).opacity(0.6)
}

/// `.skeleton-row`: a `h` px list row with a 1 px bottom rule and a bar over its first 40 %.
pub fn row(th: &Theme, h: f32) -> Div {
    div().relative().h(px(h)).border_b_1().border_color(th.mid).opacity(0.5).child(div().absolute().left_0().top_0().bottom_0().w(gpui::relative(0.4)).bg(th.mid))
}

/// A text-line slot: `slot` px tall (the line-height of the text that will fill it) holding a centred `bar` px bar of
/// `width` (a fraction of the slot's width), so a skeleton line takes the height the real text line will.
pub fn text_slot(th: &Theme, slot: f32, bar: f32, width: f32) -> Div {
    div().h(px(slot)).flex().flex_col().justify_center().child(line(th, bar).w(gpui::relative(width)))
}
