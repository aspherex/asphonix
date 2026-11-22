//! Tooltip: a small mono-code box on `surface-high`, shown by GPUI's own hover-delay
//! tooltip machinery. Use [`tip`] on any `Stateful<Div>`.
use crate::theme::{MONO, Theme};
use gpui::{AnyView, Context, Div, IntoElement, Render, SharedString, Stateful, Window, div, prelude::*, px};

struct TooltipView {
    text: SharedString,
    th: Theme,
}

impl Render for TooltipView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let th = self.th;
        div()
            .px(px(8.))
            .py(px(4.))
            .bg(th.high)
            .border_1()
            .border_color(th.border)
            .text_color(th.text)
            .font_family(MONO)
            .text_size(px(11.))
            .line_height(px(14.))
            .child(self.text.clone())
    }
}

pub fn tip(el: Stateful<Div>, text: impl Into<SharedString>, th: Theme) -> Stateful<Div> {
    let text = text.into();
    el.tooltip(move |_, cx| -> AnyView { cx.new(|_| TooltipView { text: text.clone(), th }).into() })
}

/// Chainable form of [`tip`] (the web's `title=` attribute): `div().id(..).tip("Lyrics (T)", th)`.
/// Appending keeps call sites one line, so the tooltip sweep stays additive.
pub trait Tip: Sized {
    fn tip(self, text: impl Into<SharedString>, th: Theme) -> Self;
    /// `title={cond ? text : undefined}`: no tooltip when `None`.
    fn tip_opt(self, text: Option<impl Into<SharedString>>, th: Theme) -> Self {
        match text {
            Some(t) => self.tip(t, th),
            None => self,
        }
    }
}

impl Tip for Stateful<Div> {
    fn tip(self, text: impl Into<SharedString>, th: Theme) -> Self {
        tip(self, text, th)
    }
}
