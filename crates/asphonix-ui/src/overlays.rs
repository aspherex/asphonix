//! Shell-level overlays: the confirm / prompt dialogs, the track info and update dialogs, toasts and the
//! add-to-playlist popover. One `Overlays` entity is mounted last in the shell; any view opens them through the free
//! functions below (callbacks instead of awaited results), so routes never own a dialog.
use crate::app;
use crate::theme::{MONO, Theme};
use crate::text::txt;
use crate::tracking::{BODY_MD, BODY_SM, LABEL_CAPS, MONO_CODE, MONO_DATA};
use crate::widgets::button::{Variant, button};
use crate::widgets::text_input::{InputEvent, TextInput};
use crate::widgets::{hard_shadow, icon};
use gpui::{
    App, BoxShadow, ClickEvent, Context, CursorStyle, Entity, FocusHandle, Focusable, Global, IntoElement, MouseButton, Pixels, Point, Render, SharedString, Subscription, Window, div, point,
    prelude::*, px, rgba,
};
use asphonix_api::{Playlist, Track, UpdateInfo, UpdateProgress, format as fmt};
use std::time::Duration;

/// Callback of a prompt dialog: receives the trimmed text.
pub type PromptDone = Box<dyn FnOnce(String, &mut App)>;

pub struct OverlayHandle(pub Entity<Overlays>);
impl Global for OverlayHandle {}

/// Hard offset shadow of the overlays: lime in dark, the panel ink in light.
fn hot_shadow(th: &Theme, off: f32) -> Vec<BoxShadow> {
    let c = if th.light { th.shadow } else { th.accent };
    vec![BoxShadow { color: c, offset: point(px(off), px(off)), blur_radius: px(0.), spread_radius: px(0.) }]
}

enum Modal {
    Confirm { title: SharedString, message: SharedString, confirm_label: SharedString, danger: bool, on_ok: Box<dyn FnOnce(&mut App)> },
    Prompt { title: SharedString, confirm_label: SharedString, input: Entity<TextInput>, on_ok: PromptDone, _sub: Subscription },
    TrackInfo(Box<Track>),
    Update,
}

/// Update state: the pending release and the install progress.
#[derive(Default)]
struct UpdState {
    info: Option<UpdateInfo>,
    installing: bool,
    downloaded: u64,
    total: Option<u64>,
    error: Option<String>,
    playing: bool,
}

struct AddMenu {
    ids: Vec<String>,
    /// bottom-right of the anchor; the popover opens 4 px below it, right-aligned (left edge at least 8 px)
    anchor: Point<Pixels>,
    list: Vec<Playlist>,
}

pub struct Overlays {
    modal: Option<Modal>,
    add_menu: Option<AddMenu>,
    /// the popover's list (280 px max, scrolls), wheel-animated by `widgets::smooth`
    add_scroll: gpui::ScrollHandle,
    toast: Option<SharedString>,
    toast_gen: u64,
    focus: FocusHandle,
    want_focus: bool,
    upd: UpdState,
}

impl Overlays {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self { modal: None, add_menu: None, add_scroll: gpui::ScrollHandle::new(), toast: None, toast_gen: 0, focus: cx.focus_handle(), want_focus: false, upd: UpdState::default() }
    }

    /// A modal or the Add-to-playlist popover is open.
    pub fn is_open(&self) -> bool {
        self.modal.is_some() || self.add_menu.is_some()
    }

    /// Escape: closes the topmost overlay; true when one was open.
    pub fn escape(&mut self, cx: &mut Context<Self>) -> bool {
        if self.modal.is_some() {
            if matches!(self.modal, Some(Modal::Update)) && self.upd.installing {
                return true;
            }
            self.modal = None;
            cx.notify();
            true
        } else if self.add_menu.is_some() {
            self.add_menu = None;
            cx.notify();
            true
        } else {
            false
        }
    }

    fn show_toast(&mut self, msg: SharedString, ms: u64, cx: &mut Context<Self>) {
        self.toast = Some(msg);
        self.toast_gen += 1;
        let gen_ = self.toast_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(ms)).await;
            let _ = this.update(cx, |t, cx| {
                if t.toast_gen == gen_ {
                    t.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn open_add_menu(&mut self, ids: Vec<String>, anchor: Point<Pixels>, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        self.add_menu = Some(AddMenu { ids, anchor, list: Vec::new() });
        self.add_scroll.set_offset(gpui::point(px(0.), px(0.)));
        let b = app::backend(cx);
        cx.spawn(async move |this, cx| {
            let l = cx.background_executor().spawn(async move { b.playlists_list() }).await;
            let _ = this.update(cx, |t, cx| {
                if let (Some(m), Ok(l)) = (t.add_menu.as_mut(), l) {
                    m.list = l.into_iter().filter(|p| !p.read_only).collect();
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn add_to(&mut self, playlist: String, cx: &mut Context<Self>) {
        let Some(m) = self.add_menu.take() else { return };
        app::call(cx, move |b| {
            let _ = b.playlists_add_tracks(asphonix_api::AddToPlaylistRequest { id: playlist, track_ids: m.ids });
        });
        cx.notify();
    }

    fn create_and_add(&mut self, cx: &mut Context<Self>) {
        let Some(m) = self.add_menu.take() else { return };
        let ids = m.ids;
        self.open_prompt(
            "New playlist".into(),
            "Playlist name".into(),
            "Create".into(),
            String::new(),
            Box::new(move |name, cx| {
                let ids = ids.clone();
                app::call(cx, move |b| {
                    if let Ok(pl) = b.playlists_create(asphonix_api::CreatePlaylistRequest { name, track_ids: Vec::new() }) {
                        let _ = b.playlists_add_tracks(asphonix_api::AddToPlaylistRequest { id: pl.id, track_ids: ids });
                    }
                });
            }),
            cx,
        );
    }

    fn open_prompt(&mut self, title: SharedString, placeholder: SharedString, confirm_label: SharedString, value: String, on_ok: PromptDone, cx: &mut Context<Self>) {
        let th = app::theme(cx);
        let input = cx.new(|cx| TextInput::new(cx, &placeholder, &th, MONO, 12.0, 16.0, 500.0));
        input.update(cx, |i, _| {
            i.content = value.into();
            i.context_menu = false; // the shell menu paints below the dialog
        });
        let sub = cx.subscribe(&input, |this: &mut Self, input, ev: &InputEvent, cx| {
            if *ev == InputEvent::Submit {
                let v = input.read(cx).content.to_string();
                this.submit_prompt(v, cx);
            }
        });
        self.modal = Some(Modal::Prompt { title, confirm_label, input, on_ok, _sub: sub });
        self.want_focus = true;
        cx.notify();
    }

    fn submit_prompt(&mut self, value: String, cx: &mut Context<Self>) {
        let v = value.trim().to_string();
        if v.is_empty() {
            return;
        }
        if let Some(Modal::Prompt { on_ok, .. }) = self.modal.take() {
            cx.notify();
            on_ok(v, cx);
        }
    }

    fn submit_confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(Modal::Confirm { on_ok, .. }) = self.modal.take() {
            cx.notify();
            on_ok(cx);
        }
    }
}

// ---- the free-function API

fn with<R>(cx: &mut App, f: impl FnOnce(&mut Overlays, &mut Context<Overlays>) -> R) -> R {
    let e = cx.global::<OverlayHandle>().0.clone();
    e.update(cx, f)
}

/// A confirm dialog; `on_ok` runs when the user confirms.
pub fn confirm(cx: &mut App, title: &str, message: &str, confirm_label: &str, danger: bool, on_ok: impl FnOnce(&mut App) + 'static) {
    let (title, message, confirm_label): (SharedString, SharedString, SharedString) = (title.to_string().into(), message.to_string().into(), confirm_label.to_string().into());
    with(cx, move |o, cx| {
        o.modal = Some(Modal::Confirm { title, message, confirm_label, danger, on_ok: Box::new(on_ok) });
        o.want_focus = true;
        cx.notify();
    });
}
/// A text prompt (playlist create/rename, the add-to-playlist popover's "New playlist…"): `on_ok` gets the trimmed, non-empty value.
pub fn prompt(cx: &mut App, title: &str, placeholder: &str, confirm_label: &str, value: &str, on_ok: impl FnOnce(String, &mut App) + 'static) {
    let (t, p, c, v) = (title.to_string(), placeholder.to_string(), confirm_label.to_string(), value.to_string());
    with(cx, move |o, cx| o.open_prompt(t.into(), p.into(), c.into(), v, Box::new(on_ok), cx));
}
/// A toast that stays 2.6 s.
pub fn show_toast(cx: &mut App, msg: &str) {
    let m: SharedString = msg.to_string().into();
    with(cx, move |o, cx| o.show_toast(m, 2600, cx));
}
/// The track info dialog.
pub fn track_info(cx: &mut App, t: Track) {
    with(cx, move |o, cx| {
        o.modal = Some(Modal::TrackInfo(Box::new(t)));
        o.want_focus = true;
        cx.notify();
    });
}
/// The add-to-playlist popover: `anchor_br` is the bottom-right corner of the anchor element.
pub fn add_to_playlist(cx: &mut App, ids: Vec<String>, anchor_br: Point<Pixels>) {
    with(cx, move |o, cx| o.open_add_menu(ids, anchor_br, cx));
}

impl Focusable for Overlays {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// A panel: low surface, 1 px rule, mid header strip with title (mono-data 700 caps) and meta.
fn panel(th: &Theme, title: &str, meta: Option<&str>, body: impl IntoElement) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .bg(th.low)
        .border_1()
        .border_color(th.border)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .px(px(12.))
                .py(px(8.))
                .bg(th.mid)
                .border_b_1()
                .border_color(th.border)
                .child(txt(th, MONO_DATA, title.to_uppercase()).weight(700.0).color(th.text))
                .children(meta.map(|m| div().flex().min_w_0().child(txt(th, MONO_CODE, m.to_string()).color(th.muted).ellipsis()))),
        )
        .child(body)
}

impl Render for Overlays {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::rcount::hit("overlays");
        let th = app::theme(cx);
        let vw = f32::from(window.viewport_size().width);
        let vh = f32::from(window.viewport_size().height);
        if self.want_focus {
            self.want_focus = false;
            match &self.modal {
                Some(Modal::Prompt { input, .. }) => {
                    let f = input.read(cx).focus_handle.clone();
                    window.focus(&f);
                }
                Some(_) => window.focus(&self.focus),
                None => {}
            }
        }
        let mut root = div().absolute().top_0().left_0().size_full().flex().flex_col().items_center().justify_center();

        // ---- modal
        let modal = self.modal.as_ref().map(|m| {
            let (width, inner): (f32, gpui::AnyElement) = match m {
                Modal::Confirm { title, message, confirm_label, danger, .. } => {
                    let cancel = button("dlg-cancel", th, Variant::Secondary, "Cancel", None, false, window).on_click(cx.listener(|this, _, _, cx| {
                        this.modal = None;
                        cx.notify();
                    }));
                    let ok = button("dlg-ok", th, if *danger { Variant::Danger } else { Variant::Primary }, confirm_label, None, false, window).on_click(cx.listener(|this, _, _, cx| this.submit_confirm(cx)));
                    (
                        380.0,
                        panel(
                            &th,
                            title,
                            None,
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(12.))
                                .p(px(12.))
                                .child(crate::typo::ts(&th, BODY_MD).text_color(th.text).child(message.clone()))
                                .child(div().flex().flex_row().justify_end().gap(px(8.)).child(cancel).child(ok)),
                        )
                        .into_any_element(),
                    )
                }
                Modal::Prompt { title, confirm_label, input, .. } => {
                    let focused = input.read(cx).focus_handle.is_focused(window);
                    let empty = input.read(cx).content.trim().is_empty();
                    let cancel = button("dlg-cancel", th, Variant::Secondary, "Cancel", None, false, window).on_click(cx.listener(|this, _, _, cx| {
                        this.modal = None;
                        cx.notify();
                    }));
                    let inp = input.clone();
                    let ok = button("dlg-ok", th, Variant::Primary, confirm_label, None, empty, window).on_click(cx.listener(move |this, _, _, cx| {
                        let v = inp.read(cx).content.to_string();
                        this.submit_prompt(v, cx);
                    }));
                    let field = div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(4.))
                        .h(px(36.))
                        .px(px(12.))
                        .bg(th.low)
                        .border_1()
                        .border_color(if focused { th.focus } else { th.border })
                        .when(focused, |d| d.shadow(hot_shadow(&th, 2.)))
                        .child(txt(&th, MONO_DATA, "//").color(th.muted))
                        .child(div().flex_1().min_w_0().child(input.clone()));
                    (
                        380.0,
                        panel(&th, title, None, div().flex().flex_col().gap(px(12.)).p(px(12.)).child(field).child(div().flex().flex_row().justify_end().gap(px(8.)).child(cancel).child(ok))).into_any_element(),
                    )
                }
                Modal::Update => {
                    let u = &self.upd;
                    let info = u.info.clone().unwrap_or(UpdateInfo { available: false, current_version: String::new(), version: None, notes: None, date: None });
                    let pct = u.total.filter(|t| *t > 0).map(|t| ((u.downloaded as f64 / t as f64) * 100.0).round().min(100.0) as u32);
                    let later = button("upd-later", th, Variant::Secondary, "Later", None, u.installing, window).on_click(cx.listener(|this, _, _, cx| {
                        if !this.upd.installing {
                            this.modal = None;
                            cx.notify();
                        }
                    }));
                    let install = button("upd-install", th, Variant::Primary, "Install & restart", None, u.installing, window).on_click(cx.listener(|this, _, _, cx| this.install_update(cx)));
                    let versions = div()
                        .flex()
                        .flex_row()
                        .child(txt(&th, MONO_DATA, format!("v{} → ", info.current_version)).color(th.muted))
                        .child(txt(&th, MONO_DATA, format!("v{}", info.version.clone().unwrap_or_default())).color(th.accent_fg));
                    let mut body = div().flex().flex_col().gap(px(12.)).p(px(12.)).child(versions);
                    if let Some(n) = &info.notes {
                        // plain text only (never markup), line breaks kept
                        body = body.child(
                            div()
                                .max_h(px(220.))
                                .p(px(8.))
                                .bg(th.base)
                                .border_1()
                                .border_color(th.border)
                                .overflow_hidden()
                                .child(crate::typo::ts(&th, BODY_SM).text_color(th.text).child(n.clone())),
                        );
                    }
                    if u.playing {
                        body = body.child(crate::typo::ts(&th, BODY_SM).text_color(th.clip).child("Playback will stop while ASPHONIX updates and restarts."));
                    }
                    if u.installing {
                        let frac = pct.map(|p| p as f32 / 100.0).unwrap_or(0.0);
                        body = body.child(div().h(px(6.)).w_full().bg(th.high).overflow_hidden().child(div().h_full().w(gpui::relative(frac)).bg(th.accent)));
                        let text = match pct {
                            None => "Downloading…".to_string(),
                            Some(p) if p >= 100 => "Verifying & installing…".to_string(),
                            Some(p) => format!("Downloading {p}% · {}", format_bytes(u.downloaded as f64)),
                        };
                        body = body.child(txt(&th, MONO_CODE, text).color(th.muted));
                    }
                    if let Some(e) = &u.error {
                        body = body.child(crate::typo::ts(&th, BODY_SM).text_color(th.clip).child(e.clone()));
                    }
                    body = body.child(div().flex().flex_row().justify_end().gap(px(8.)).child(later).child(install));
                    (460.0f32.min(vw - 32.0), panel(&th, "Update available", info.date.as_deref(), body).into_any_element())
                }
                Modal::TrackInfo(t) => {
                    let f = &t.format;
                    let mut rows: Vec<(&str, String, bool)> = vec![("Format", f.suffix.to_uppercase(), false)];
                    if let Some(d) = f.bit_depth.filter(|d| *d > 0) {
                        rows.push(("Bit depth", format!("{d}-bit"), false));
                    }
                    rows.push(("Sample rate", format!("{} kHz", fmt_khz(f.sampling_rate)), false));
                    if let Some(b) = f.bit_rate.filter(|b| *b > 0) {
                        rows.push(("Bitrate", format!("{b} kbps"), false));
                    }
                    rows.push(("Channels", match f.channel_count {
                        1 => "Mono".into(),
                        2 => "Stereo".into(),
                        n => n.to_string(),
                    }, false));
                    rows.push(("Duration", fmt::format_duration(Some(t.duration)), false));
                    if let Some(s) = t.size {
                        rows.push(("File size", format_bytes(s as f64), false));
                    }
                    if let Some(rg) = &t.replay_gain {
                        if let Some(g) = rg.track_gain_db {
                            rows.push(("Track gain", format!("{g:.2} dB"), false));
                        }
                        if let Some(g) = rg.album_gain_db {
                            rows.push(("Album gain", format!("{g:.2} dB"), false));
                        }
                    }
                    if let Some(a) = &t.album {
                        rows.push(("Album", a.clone(), false));
                    }
                    if let Some(y) = t.year {
                        rows.push(("Year", y.to_string(), false));
                    }
                    if let Some(g) = &t.genre {
                        rows.push(("Genre", g.clone(), true));
                    }
                    let list = div().flex().flex_col().gap(px(4.)).children(rows.into_iter().map(|(label, value, genre)| {
                        div()
                            .flex()
                            .flex_row()
                            .justify_between()
                            .gap(px(12.))
                            .child(txt(&th, LABEL_CAPS, label).color(th.muted))
                            .child(txt(&th, MONO_DATA, value).color(if genre { th.accent_fg } else { th.text }))
                    }));
                    let close = button("dlg-close", th, Variant::Primary, "Close", None, false, window).on_click(cx.listener(|this, _, _, cx| {
                        this.modal = None;
                        cx.notify();
                    }));
                    (360.0, panel(&th, "Track info", Some(&t.title), div().flex().flex_col().gap(px(12.)).p(px(12.)).child(list).child(div().flex().flex_row().justify_end().child(close))).into_any_element())
                }
            };
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x0a0a0cb2))
                .occlude()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.modal = None;
                    cx.notify();
                }))
                .child(div()
                        .id("dialog-box")
                        .track_focus(&self.focus)
                        .key_context("Dialog")
                        .w(px(width))
                        .shadow(hard_shadow(&th, 4.))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                            cx.stop_propagation();
                            match ev.keystroke.key.as_str() {
                                "escape" => {
                                    if !(matches!(this.modal, Some(Modal::Update)) && this.upd.installing) {
                                        this.modal = None;
                                        cx.notify();
                                    }
                                }
                                "enter" if matches!(this.modal, Some(Modal::Confirm { .. })) => this.submit_confirm(cx),
                                "enter" if matches!(this.modal, Some(Modal::TrackInfo(_))) => {
                                    this.modal = None;
                                    cx.notify();
                                }
                                _ => {}
                            }
                        }))
                        .child(inner))
        });
        root = root.children(modal);

        // ---- add-to-playlist popover
        if let Some(m) = &self.add_menu {
            let top = f32::from(m.anchor.y) + 4.0;
            let left = (f32::from(m.anchor.x) - 220.0).max(8.0);
            let n = m.ids.len();
            let header = format!("Add to playlist{}", if n > 1 { format!(" ({n} tracks)") } else { String::new() });
            let item = |id: gpui::ElementId| {
                div().id(id).group("hv").flex().flex_row().items_center().gap(px(8.)).w_full().px(px(12.)).py(px(4.)).text_color(th.text).cursor(CursorStyle::PointingHand).hover(move |s| s.bg(th.high).text_color(th.accent_fg))
            };
            let list = div()
                .id("add-menu")
                .track_scroll(&self.add_scroll)
                .max_h(px(278.)) // 280 - the 1 px border on both sides
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .py(px(4.))
                .child(div().px(px(12.)).py(px(4.)).child(txt(&th, LABEL_CAPS, header).color(th.muted)))
                .child(item("add-new".into()).child(icon("add", 13., th.text)).child(txt(&th, BODY_SM, "New playlist…")).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.create_and_add(cx))))
                .children((!m.list.is_empty()).then(|| div().h(px(1.)).bg(th.border).my(px(4.))))
                .children(m.list.iter().enumerate().map(|(i, p)| {
                    let id = p.id.clone();
                    item(("add-pl", i).into()).child(txt(&th, BODY_SM, p.name.clone())).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.add_to(id.clone(), cx)))
                }));
            let menu = div()
                    .absolute()
                    .top(px(top.min(vh - 40.)))
                    .left(px(left.min(vw - 228.)))
                    .w(px(220.))
                    .bg(th.mid)
                    .border_1()
                    .border_color(th.border)
                    .shadow(hard_shadow(&th, 2.))
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(list)
                    .child(crate::widgets::smooth::drive(window, crate::widgets::smooth::Scrollable::div(std::ptr::addr_of!(self.add_scroll) as usize, &self.add_scroll)));
            root = root.child(
                div().absolute().top_0().left_0().size_full().on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.add_menu = None;
                    cx.notify();
                })).child(menu),
            );
        }

        // ---- toast
        if let Some(msg) = &self.toast {
            root = root.child(
                div().absolute().left_0().bottom(px(crate::metrics::layout::DOCK_H + 16.)).w_full().flex().justify_center().child(
                    div()
                        .max_w(px(560.0f32.min(vw * 0.9)))
                        .px(px(12.))
                        .py(px(8.))
                        .bg(th.mid)
                        .border_1()
                        .border_color(th.accent_fg)
                        .shadow(hot_shadow(&th, 2.))
                        .overflow_hidden()
                        .child(txt(&th, MONO_DATA, msg.clone()).color(th.text).ellipsis()),
                ),
            );
        }
        root
    }
}

fn fmt_khz(hz: u32) -> String {
    // kHz with up to 3 fraction digits, trailing zeros trimmed
    let k = hz as f64 / 1000.0;
    let s = format!("{:.3}", k);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Byte count as B / KB / MB / GB (2 decimals below 10, else 1).
fn format_bytes(b: f64) -> String {
    if b < 1024.0 {
        return format!("{} B", b as u64);
    }
    let units = ["KB", "MB", "GB"];
    let (mut v, mut i) = (b / 1024.0, 0);
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{:.*} {}", if v < 10.0 { 2 } else { 1 }, v, units[i])
}

impl Overlays {
    /// Explicit user action only; on success the app restarts.
    fn install_update(&mut self, cx: &mut Context<Self>) {
        if self.upd.installing {
            return;
        }
        self.upd.installing = true;
        self.upd.downloaded = 0;
        self.upd.total = None;
        self.upd.error = None;
        cx.notify();
        let b = app::backend(cx);
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { b.updates_install() }).await;
            let _ = this.update(cx, |t, cx| {
                if let Err(e) = r {
                    t.upd.error = Some(e.to_string());
                    t.upd.installing = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

/// The update dialog (title-bar badge click): `playing` adds the "Playback will stop" warning.
pub fn open_update(cx: &mut App, info: UpdateInfo, playing: bool) {
    with(cx, move |o, cx| {
        o.upd.info = Some(info);
        o.upd.error = None;
        o.upd.playing = playing;
        o.modal = Some(Modal::Update);
        o.want_focus = true;
        cx.notify();
    });
}

/// Download progress of an update install.
pub fn update_progress(cx: &mut App, p: UpdateProgress) {
    with(cx, move |o, cx| {
        o.upd.downloaded = p.downloaded;
        o.upd.total = p.total;
        cx.notify();
    });
}
