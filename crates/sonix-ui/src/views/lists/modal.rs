//! Stand-in for `Dialog.svelte` (`promptText` / `confirmAction`), used by the list routes' rename / delete /
//! new-playlist / clear-history flows. The dialogs agent owns the real shell-level modal; when that lands,
//! `prompt()` / `confirm()` below are the only two entry points to re-point.
use super::ui::{Callback, panel};
use crate::app;
use crate::metrics::controls as ctl;
use crate::text::txt;
use crate::theme::{MONO, Theme};
use crate::tracking::{BODY_MD, MONO_DATA};
use crate::widgets::button::{Variant, button};
use crate::widgets::hard_shadow;
use crate::widgets::text_input::{InputEvent, TextInput};
use gpui::{App, Context, Entity, FocusHandle, Global, IntoElement, MouseButton, Render, SharedString, Subscription, Window, div, prelude::*, px, rgba};

pub struct ModalHandle(pub Entity<Modal>);
impl Global for ModalHandle {}

struct Req {
    prompt: bool,
    title: SharedString,
    message: Option<SharedString>,
    confirm_label: SharedString,
    danger: bool,
    cb: Option<Callback>,
    prev_focus: Option<FocusHandle>,
}

pub struct Modal {
    req: Option<Req>,
    input: Entity<TextInput>,
    focus: FocusHandle,
    pending_submit: bool,
    _sub: Subscription,
}

/// `promptText(title, { value, placeholder, confirmLabel })`: `then(Some(trimmed))` or `then(None)` when cancelled / empty.
pub fn prompt(cx: &mut App, title: &str, value: &str, placeholder: &str, confirm_label: &str, then: impl FnOnce(Option<String>, &mut Window, &mut App) + 'static) {
    let h = cx.global::<ModalHandle>().0.clone();
    let (title, value, placeholder, confirm_label): (SharedString, String, String, SharedString) = (title.to_string().into(), value.to_string(), placeholder.to_string(), confirm_label.to_string().into());
    cx.defer(move |cx| {
        let Some(w) = cx.active_window() else { return };
        let _ = w.update(cx, |_, window, cx| {
            let _ = placeholder; // the web dialog passes it to the Input, which shows no placeholder in the native field
            let req = Req { prompt: true, title, message: None, confirm_label, danger: false, cb: Some(Box::new(then)), prev_focus: None };
            h.update(cx, |m, cx| m.open(req, Some(value), window, cx));
        });
    });
}

/// `confirmAction(title, message, { confirmLabel, danger })`: `then(Some(""))` = confirmed, `then(None)` = cancelled.
pub fn confirm(cx: &mut App, title: &str, message: &str, confirm_label: &str, danger: bool, then: impl FnOnce(Option<String>, &mut Window, &mut App) + 'static) {
    let h = cx.global::<ModalHandle>().0.clone();
    let (title, message, confirm_label): (SharedString, SharedString, SharedString) = (title.to_string().into(), message.to_string().into(), confirm_label.to_string().into());
    cx.defer(move |cx| {
        let Some(w) = cx.active_window() else { return };
        let _ = w.update(cx, |_, window, cx| {
            let req = Req { prompt: false, title, message: Some(message), confirm_label, danger, cb: Some(Box::new(then)), prev_focus: None };
            h.update(cx, |m, cx| m.open(req, None, window, cx));
        });
    });
}

impl Modal {
    pub fn new(th: &Theme, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new(cx, "", th, MONO, MONO_DATA.size, MONO_DATA.line, MONO_DATA.weight));
        let sub = cx.subscribe(&input, |this: &mut Self, _, ev: &InputEvent, cx| {
            if *ev == InputEvent::Submit {
                this.pending_submit = true;
                cx.notify();
            }
            if *ev == InputEvent::Changed {
                cx.notify();
            }
        });
        Self { req: None, input, focus: cx.focus_handle(), pending_submit: false, _sub: sub }
    }

    pub fn is_open(&self) -> bool {
        self.req.is_some()
    }

    fn open(&mut self, mut req: Req, init: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        // only one dialog at a time: a newer request cancels the older one
        self.close(None, window, cx);
        req.prev_focus = window.focused(cx);
        let prompt = req.prompt;
        if let Some(v) = init {
            self.input.update(cx, |i, cx| i.set_content(&v, cx));
        }
        self.req = Some(req);
        if prompt {
            let f = self.input.read(cx).focus_handle.clone();
            window.focus(&f);
        } else {
            window.focus(&self.focus);
        }
        cx.notify();
    }

    pub fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close(None, window, cx);
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = &self.req else { return };
        let v = if r.prompt {
            let v = self.input.read(cx).content.trim().to_string();
            if v.is_empty() {
                return;
            }
            Some(v)
        } else {
            Some(String::new())
        };
        self.close(v, window, cx);
    }

    fn close(&mut self, result: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut r) = self.req.take() else { return };
        if let Some(f) = r.prev_focus.take() {
            window.focus(&f);
        }
        if let Some(cb) = r.cb.take() {
            cb(result, window, cx);
        }
        cx.notify();
    }
}

impl Render for Modal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = app::theme(cx);
        let Some(r) = &self.req else { return div().into_any_element() };
        let (prompt, danger) = (r.prompt, r.danger);
        let disabled = prompt && self.input.read(cx).content.trim().is_empty();
        let cancel = button("dlg-cancel", th, Variant::Secondary, "Cancel", None, false, window).on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx)));
        let ok = button("dlg-ok", th, if danger { Variant::Danger } else { Variant::Primary }, &r.confirm_label, None, disabled, window).on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)));
        let mut body = div().flex().flex_col().gap(px(crate::metrics::space::MD)).p(px(crate::metrics::space::MD));
        if let Some(m) = &r.message {
            body = body.child(txt(&th, BODY_MD, m.clone()).color(th.text));
        }
        if prompt {
            body = body.child(super::ui::field(&th, &self.input, Some("//"), "", window, cx).h(px(ctl::FIELD_H)));
        }
        body = body.child(div().flex().flex_row().justify_end().gap(px(crate::metrics::space::SM)).child(cancel).child(ok));
        let title = r.title.to_string();
        if self.pending_submit {
            self.pending_submit = false;
            cx.defer_in(window, |this, window, cx| this.submit(window, cx));
        }
        div()
            .id("dialog-overlay")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x0a0a0cb3))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.cancel(window, cx)))
            .track_focus(&self.focus)
            .child(
                div()
                    .id("dialog-box")
                    .w(px(380.))
                    .shadow(hard_shadow(&th, 4.0))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(panel(&th, &title, body)),
            )
            .into_any_element()
    }
}
