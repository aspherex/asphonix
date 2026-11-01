//! `Login.svelte`: the server connect screen (shown without top bar, sidebar and dock).
use super::ui::{field, scroll_page, wrap_text};
use super::ListsEvent;
use crate::app;
use crate::metrics::space;
use crate::probe::pid;
use crate::router::Route;
use crate::text::txt;
use crate::theme::{MONO, Theme};
use crate::tracking::{BODY_SM, HEADLINE_SM, LABEL_CAPS, MONO_CODE, MONO_DATA, TypeSpec};
use crate::widgets::text_input::{InputEvent, TextInput};
use crate::widgets::{hard_shadow, icon, icon_g};
use gpui::{AnyElement, Context, CursorStyle, Entity, EventEmitter, IntoElement, Render, ScrollHandle, Subscription, Timer, Window, div, prelude::*, px};
use sonix_api::*;
use std::sync::Arc;
use std::time::Duration;

impl EventEmitter<ListsEvent> for LoginView {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Connecting,
    Error,
    Connected,
}

pub struct LoginView {
    backend: Arc<dyn Backend>,
    url: Entity<TextInput>,
    user: Entity<TextInput>,
    pass: Entity<TextInput>,
    show_pw: bool,
    phase: Phase,
    caps: Option<ServerCapabilities>,
    error: Option<String>,
    signed_in: bool,
    scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

impl LoginView {
    pub fn new(backend: Arc<dyn Backend>, cx: &mut Context<Self>) -> Self {
        let th = app::theme(cx);
        let mk = |cx: &mut Context<Self>| cx.new(|cx| TextInput::new(cx, "", &th, MONO, MONO_DATA.size, MONO_DATA.line, MONO_DATA.weight));
        let (url, user, pass) = (mk(cx), mk(cx), mk(cx));
        let mut subs = Vec::new();
        for i in [&url, &user, &pass] {
            subs.push(cx.subscribe(i, |this: &mut Self, _, ev: &InputEvent, cx| match ev {
                InputEvent::Submit => this.connect(cx),
                InputEvent::Changed => cx.notify(),
            }));
        }
        let mut this = Self { backend: backend.clone(), url, user, pass, show_pw: false, phase: Phase::Idle, caps: None, error: None, signed_in: false, scroll: ScrollHandle::new(), _subs: subs };
        cx.spawn(async move |t, cx| {
            let s = cx.background_executor().spawn(async move { backend.auth_session_status() }).await;
            let _ = t.update(cx, |t, cx| {
                t.signed_in = s.map(|s| s.state != ConnectionState::Disconnected).unwrap_or(false);
                cx.notify();
            });
        })
        .detach();
        this.phase = Phase::Idle;
        this
    }

    pub fn release(&mut self) {}

    #[cfg(feature = "parity")]
    pub(crate) fn parity_state(&self) -> serde_json::Value {
        serde_json::json!({ "loginPhase": match self.phase { Phase::Idle => "idle", Phase::Connecting => "connecting", Phase::Error => "error", Phase::Connected => "connected" } })
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        if self.phase == Phase::Connecting || self.url.read(cx).content.is_empty() {
            return;
        }
        self.phase = Phase::Connecting;
        self.error = None;
        self.caps = None;
        let req = ConnectRequest { url: self.url.read(cx).content.to_string(), username: self.user.read(cx).content.to_string(), password: self.pass.read(cx).content.to_string() };
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move {
                let c = b.auth_connect(req);
                if c.is_ok() {
                    let _ = b.library_sync_start(SyncStartRequest { full: true });
                }
                c
            })
            .await;
            let ok = this
                .update(cx, |t, cx| {
                    match r {
                        Ok(c) => {
                            t.caps = Some(c);
                            t.phase = Phase::Connected;
                        }
                        Err(e) => {
                            t.phase = Phase::Error;
                            t.error = Some(if e.message.is_empty() { "Connection failed.".into() } else { e.message });
                        }
                    }
                    cx.notify();
                    t.phase == Phase::Connected
                })
                .unwrap_or(false);
            if ok {
                Timer::after(Duration::from_millis(500)).await;
                let _ = this.update(cx, |_, cx| cx.emit(ListsEvent::Navigate(Route::Home)));
            }
        })
        .detach();
        cx.notify();
    }

    fn group(th: &Theme, label: &str, input: AnyElement) -> AnyElement {
        div().flex().flex_col().gap(px(space::XS)).child(div().flex().child(txt(th, LABEL_CAPS, label.to_string()).color(th.muted))).child(input).into_any_element()
    }
}

impl Render for LoginView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = app::theme(cx);
        let handle = self.scroll.clone();
        scroll_page(th, "login-scroll", &handle, |overflow| {
            let avail = f32::from(window.viewport_size().width) - 2.0 * space::LG - if overflow { 10.0 } else { 0.0 };
            let card_w = avail.min(700.0);
            let url_f = field(&th, &self.url, None, "https://music.example.com:4533", window, cx).into_any_element();
            let user_f = field(&th, &self.user, None, "username", window, cx).into_any_element();
            // password: the real input sits under an opaque bullet overlay while "hidden"
            let masked = !self.show_pw && !self.pass.read(cx).content.is_empty();
            let n = self.pass.read(cx).content.chars().count();
            let pw_input = field(&th, &self.pass, None, "••••••••••", window, cx);
            let pw_field = div().relative().flex_1().min_w_0().flex().flex_col().child(pw_input.h(px(crate::metrics::controls::FIELD_H))).when(masked, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(px(1.))
                        .bottom(px(1.))
                        .left(px(1.))
                        .right(px(1.))
                        .bg(th.low)
                        .px(px(crate::metrics::controls::FIELD_PAD_X - 1.0))
                        .flex()
                        .items_center()
                        .child(txt(&th, MONO_DATA, "•".repeat(n)).color(th.text)),
                )
            });
            let eye = div()
                .id("login-eye")
                .group("hv")
                .w(px(36.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .bg(th.low)
                .border_1()
                .border_color(th.border)
                .text_color(th.muted)
                .cursor(CursorStyle::PointingHand)
                .hover(move |s| s.text_color(th.accent_fg))
                .child(icon_g("eye", 14., th.muted, th.accent_fg))
                .on_click(cx.listener(|t, _, _, cx| {
                    t.show_pw = !t.show_pw;
                    cx.notify();
                }));
            let pw_row = div().flex().flex_row().gap(px(space::XS)).child(pw_field).child(eye).into_any_element();

            let mut form = div()
                .flex()
                .flex_col()
                .gap(px(space::LG))
                .p(px(space::LG))
                .child(pid("login.group.0", Self::group(&th, "// Server URL", url_f)))
                .child(pid("login.group.1", Self::group(&th, "// Username", user_f)))
                .child(pid("login.group.2", Self::group(&th, "// Password", pw_row)));
            if self.phase == Phase::Error {
                let msg = self.error.clone().unwrap_or_default();
                let lines = wrap_text(&th, window, &msg, BODY_SM, card_w - 2.0 * space::LG - 2.0 - 2.0 * space::MD);
                form = form.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(space::XS))
                        .p(px(space::MD))
                        .bg(th.base)
                        .border_1()
                        .border_color(th.clip)
                        .child(div().flex().child(txt(&th, LABEL_CAPS, "Status: error").color(th.clip)))
                        .child(div().flex().flex_col().children(lines.into_iter().map(|l| txt(&th, BODY_SM, l).color(th.muted)))),
                );
            }
            if let Some(c) = &self.caps {
                let mut probe = div().flex().flex_col().gap(px(space::XS)).p(px(space::MD)).bg(th.base).border_1().border_color(th.border).child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(space::XS))
                        .child(div().size(px(6.)).flex_none().bg(th.accent))
                        .child(txt(&th, LABEL_CAPS, "Status: OK").color(th.text))
                        .child(div().flex_1())
                        .child(txt(&th, MONO_CODE, format!("LATENCY: {}MS", c.ping_ms)).color(th.muted)),
                );
                probe = probe.child(div().flex().child(txt(&th, MONO_DATA, format!("HOST: {} {} (Subsonic API {})", c.server_name, c.server_version, c.api_version)).color(th.muted)));
                if !c.open_subsonic_extensions.is_empty() {
                    probe = probe.child(div().flex().child(txt(&th, MONO_DATA, format!("CAPS: {}", c.open_subsonic_extensions.join(", "))).color(th.muted)));
                }
                form = form.child(probe);
            }
            let disabled = self.phase == Phase::Connecting || self.url.read(cx).content.is_empty();
            let label = match self.phase {
                Phase::Connecting => "CONNECTING…",
                Phase::Connected => "CONNECTED",
                _ => "INITIALIZE & ENTER TERMINAL",
            };
            let btn = div()
                .id("login-connect")
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .gap(px(space::SM))
                .h(px(48.))
                .bg(th.accent)
                .text_color(th.accent_text)
                .shadow(hard_shadow(&th, 2.0))
                .child(icon("sync", 16., th.accent_text))
                .child(txt(&th, HEADLINE_SM, label).color(th.accent_text))
                .map(|d| if disabled { d.opacity(0.5).cursor(CursorStyle::OperationNotAllowed) } else { d.cursor(CursorStyle::PointingHand).hover(move |s| s.bg(th.accent_hover)).on_click(cx.listener(|t, _, _, cx| t.connect(cx))) });
            form = form.child(pid("login.btn", btn));
            if self.signed_in && self.phase != Phase::Connected {
                form = form.child(
                    div().flex().flex_row().justify_center().child(
                        div()
                            .id("login-cancel")
                            .px(px(space::SM))
                            .py(px(space::XS))
                            .text_color(th.muted)
                            .cursor(CursorStyle::PointingHand)
                            .hover(move |s| s.text_color(th.accent_fg))
                            .child(txt(&th, LABEL_CAPS, "Cancel · back to current profile"))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ListsEvent::Back))),
                    ),
                );
            }
            let header_bar = div()
                .w(px(card_w))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(space::XS))
                .mb(px(space::SM))
                .child(div().size(px(6.)).flex_none().bg(th.accent))
                .child(txt(&th, MONO_CODE, "SYS_PROTOCOL // V1.16.1 SPEC").color(th.muted));
            let card_header = pid("login.card.head", div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(space::MD))
                .p(px(space::LG))
                .bg(th.mid)
                .border_b_1()
                .border_color(th.border)
                .child(pid("login.brand.mark", div().size(px(40.)).flex_none().flex().items_center().justify_center().bg(th.accent).child(icon("equalizer", 20., th.accent_text))))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            pid("login.brand.row", div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(space::SM))
                                .child(pid("login.brand.name", div().flex().child(txt(&th, HEADLINE_SM, "SONIX").color(th.text))))
                                .child(pid("login.brand.tag", div().px(px(space::XS)).py(px(1.)).bg(th.high).child(txt(&th, LABEL_CAPS, "Engine").color(th.muted))))),
                        )
                        .child(div().h(px(18.)).child(pid("login.brand.sub", div().mt(px(2.)).h(px(15.)).flex().items_center().child(txt(&th, BODY_SM, "High-performance audio workstation").color(th.muted))))),
                ));
            let card = div().w(px(card_w)).flex().flex_col().bg(th.low).border_1().border_color(th.border).shadow(hard_shadow(&th, 4.0)).child(card_header).child(form);
            let status = match self.phase {
                Phase::Idle => "IDLE",
                Phase::Connecting => "CONNECTING",
                Phase::Error => "ERROR",
                Phase::Connected => "CONNECTED",
            };
            let host = self.caps.as_ref().map(|c| format!("{} {}", c.server_name.to_uppercase(), c.server_version)).unwrap_or_else(|| "—".into());
            let footer = div()
                .w(px(card_w))
                .flex()
                .flex_row()
                .justify_between()
                .mt(px(space::SM))
                .child(txt(&th, MONO_CODE, format!("STATUS: {status}")).color(th.muted))
                .child(txt(&th, MONO_CODE, host).color(th.muted));
            let _ = TypeSpec { ..MONO_CODE };
            div()
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .px(px(space::LG))
                .py(px(space::XL))
                .child(pid("login.header", header_bar))
                .child(pid("login.card", card))
                .child(pid("login.footer", footer))
                .into_any_element()
        })
    }
}
