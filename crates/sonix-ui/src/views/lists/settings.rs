//! `Settings.svelte`: Output, Scrobbling, Shuffle, Playback, Server, Updates, Claude (MCP), Log out.
//! Every control talks to the backend like the web page (optimistic local state, then the setter).
use super::ui::{scroll_page, wrap_text};
use super::ListsEvent;
use crate::app;
use crate::metrics::space;
use crate::probe::pid;
use crate::text::txt;
use crate::theme::{MONO, Theme};
use crate::tracking::{BODY_SM, HEADLINE_MD, LABEL_CAPS, MONO_DATA};
use crate::widgets::context_menu::{ContextMenu, MenuItem};
use crate::widgets::text_input::{InputEvent, TextInput};
use crate::widgets::ui::segmented;
use crate::widgets::{hard_shadow, icon};
use gpui::{AnyElement, ClickEvent, ClipboardItem, Context, CursorStyle, Entity, EventEmitter, IntoElement, Render, ScrollHandle, Subscription, Timer, WeakEntity, Window, div, prelude::*, px};
use sonix_api::format as fmt;
use sonix_api::*;
use std::sync::Arc;
use std::time::Duration;

impl EventEmitter<ListsEvent> for SettingsView {}

pub struct SettingsView {
    backend: Arc<dyn Backend>,
    menu: Entity<ContextMenu>,
    s: Settings,
    mcp: Option<McpStatus>,
    status: Option<SessionStatus>,
    devices: Vec<OutputDevice>,
    out: Option<OutputStatus>,
    art: Option<ArtCacheStatus>,
    update_msg: Option<String>,
    checking: bool,
    copied: bool,
    threshold: Entity<TextInput>,
    window_size: Entity<TextInput>,
    /// number inputs fire `change` on Enter or when they lose focus
    focused: [bool; 2],
    scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

/// `<input type=number>` accepts only digits (and a sign / dot, which these integer settings never use).
fn digits_only(i: &Entity<TextInput>, cx: &mut Context<SettingsView>) {
    let c = i.read(cx).content.to_string();
    let f: String = c.chars().filter(|ch| ch.is_ascii_digit()).collect();
    if f != c {
        i.update(cx, |i, cx| i.set_content(&f, cx));
    }
}

/// `Number(text)`: an empty box is 0 (then clamped), garbage is NaN (ignored).
fn js_number(t: &str) -> f64 {
    let t = t.trim();
    if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) }
}

fn onoff() -> [(&'static str, &'static str); 2] {
    [("on", "On"), ("off", "Off")]
}

impl SettingsView {
    pub fn new(backend: Arc<dyn Backend>, menu: &Entity<ContextMenu>, cx: &mut Context<Self>) -> Self {
        let th = app::theme(cx);
        let mk = |cx: &mut Context<Self>| cx.new(|cx| TextInput::new(cx, "", &th, MONO, MONO_DATA.size, MONO_DATA.line, MONO_DATA.weight));
        let (threshold, window_size) = (mk(cx), mk(cx));
        let subs = vec![
            cx.subscribe(&threshold, |this: &mut Self, i, ev: &InputEvent, cx| {
                if *ev == InputEvent::Changed {
                    digits_only(&i, cx);
                }
                if *ev == InputEvent::Submit {
                    let v: f64 = js_number(&i.read(cx).content);
                    this.set_threshold(v, cx);
                }
            }),
            cx.subscribe(&window_size, |this: &mut Self, i, ev: &InputEvent, cx| {
                if *ev == InputEvent::Changed {
                    digits_only(&i, cx);
                }
                if *ev == InputEvent::Submit {
                    let v: f64 = js_number(&i.read(cx).content);
                    this.set_window(v, cx);
                }
            }),
        ];
        let mut this = Self {
            backend,
            menu: menu.clone(),
            s: Settings {
                scrobbling_enabled: true,
                auto_check_updates: true,
                spread_artists_in_shuffle: false,
                queue_window_size: 20,
                autoplay_enabled: true,
                sync_play_queue: true,
                play_threshold_sec: 30,
                free_memory_when_minimized: true,
                replay_gain_mode: ReplayGainMode::Off,
                track_overlay: true,
                track_overlay_corner: OverlayCorner::TopLeft,
                mcp_enabled: false,
                mcp_allow_control: false,
            },
            mcp: None,
            status: None,
            devices: Vec::new(),
            out: None,
            art: None,
            update_msg: None,
            checking: false,
            copied: false,
            threshold,
            window_size,
            focused: [false; 2],
            scroll: ScrollHandle::new(),
            _subs: subs,
        };
        this.load(cx);
        this
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move { (b.auth_session_status(), b.output_list_devices(), b.output_get_status(), b.settings_get(), b.mcp_status(), b.art_cache_status()) })
                .await;
            let _ = this.update(cx, |t, cx| {
                t.status = r.0.ok();
                t.devices = r.1.unwrap_or_default();
                t.out = r.2.ok();
                if let Ok(s) = r.3 {
                    t.s = s;
                }
                t.mcp = r.4.ok();
                t.art = r.5.ok();
                let (th, ws) = (t.s.play_threshold_sec.to_string(), t.s.queue_window_size.to_string());
                t.threshold.update(cx, |i, cx| i.set_content(&th, cx));
                t.window_size.update(cx, |i, cx| i.set_content(&ws, cx));
                cx.notify();
            });
        })
        .detach();
    }

    #[cfg(feature = "parity")]
    pub(crate) fn parity_state(&self) -> serde_json::Value {
        serde_json::json!({ "settings": serde_json::to_value(&self.s).unwrap_or_default() })
    }

    fn set(&mut self, ch: SettingChange, cx: &mut Context<Self>) {
        match &ch {
            SettingChange::Scrobbling(v) => self.s.scrobbling_enabled = *v,
            SettingChange::AutoCheckUpdates(v) => self.s.auto_check_updates = *v,
            SettingChange::SpreadArtists(v) => self.s.spread_artists_in_shuffle = *v,
            SettingChange::QueueWindowSize(v) => self.s.queue_window_size = *v,
            SettingChange::Autoplay(v) => self.s.autoplay_enabled = *v,
            SettingChange::SyncPlayQueue(v) => self.s.sync_play_queue = *v,
            SettingChange::ReplayGain(v) => self.s.replay_gain_mode = *v,
            SettingChange::PlayThreshold(v) => self.s.play_threshold_sec = *v,
            SettingChange::TrackOverlay(v) => self.s.track_overlay = *v,
            SettingChange::TrackOverlayCorner(v) => self.s.track_overlay_corner = *v,
            SettingChange::FreeMemoryWhenMinimized(v) => self.s.free_memory_when_minimized = *v,
            SettingChange::McpEnabled(v) => self.s.mcp_enabled = *v,
            SettingChange::McpAllowControl(v) => self.s.mcp_allow_control = *v,
        }
        if matches!(ch, SettingChange::TrackOverlay(_) | SettingChange::TrackOverlayCorner(_)) {
            cx.emit(ListsEvent::TrackOverlay(self.s.track_overlay, self.s.track_overlay_corner));
        }
        let mcp = matches!(ch, SettingChange::McpEnabled(_) | SettingChange::McpAllowControl(_));
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let m = cx
                .background_executor()
                .spawn(async move {
                    let _ = b.settings_set(ch);
                    if mcp { b.mcp_status().ok() } else { None }
                })
                .await;
            let _ = this.update(cx, |t, cx| {
                if m.is_some() {
                    t.mcp = m;
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn set_threshold(&mut self, v: f64, cx: &mut Context<Self>) {
        if !v.is_finite() {
            return;
        }
        let c = v.round().clamp(10.0, 300.0) as u32;
        let s = c.to_string();
        self.threshold.update(cx, |i, cx| i.set_content(&s, cx));
        self.set(SettingChange::PlayThreshold(c), cx);
    }
    fn set_window(&mut self, v: f64, cx: &mut Context<Self>) {
        if !v.is_finite() {
            return;
        }
        let c = v.round().clamp(10.0, 100.0) as u32;
        let s = c.to_string();
        self.window_size.update(cx, |i, cx| i.set_content(&s, cx));
        self.set(SettingChange::QueueWindowSize(c), cx);
    }

    fn refresh_output(&mut self, cx: &mut Context<Self>) {
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { b.output_get_status() }).await;
            let _ = this.update(cx, |t, cx| {
                t.out = r.ok();
                cx.notify();
            });
        })
        .detach();
    }

    fn check_now(&mut self, cx: &mut Context<Self>) {
        self.checking = true;
        self.update_msg = None;
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { b.updates_check() }).await;
            let _ = this.update(cx, |t, cx| {
                t.checking = false;
                t.update_msg = Some(match r {
                    Ok(i) if i.available => format!("Update {} available", i.version.unwrap_or_default()),
                    Ok(_) => "You are up to date".into(),
                    Err(_) => "Could not check for updates".into(),
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    // ------------------------------------------------------------ render helpers
    fn row(th: &Theme, label: &str, right: impl IntoElement) -> gpui::Div {
        div().flex().flex_row().items_center().justify_between().gap(px(space::MD)).child(div().flex_none().child(txt(th, BODY_SM, label.to_string()).color(th.muted))).child(right)
    }
    fn value(th: &Theme, v: impl Into<String>) -> impl IntoElement {
        div().flex().child(txt(th, MONO_DATA, v.into()).color(th.text))
    }
    fn toggle(&self, th: &Theme, id: &'static str, on: bool, f: fn(bool) -> SettingChange, cx: &mut Context<Self>) -> impl IntoElement {
        let this: WeakEntity<Self> = cx.entity().downgrade();
        segmented(id, th, &onoff(), if on { "on" } else { "off" }, move |v, _, cx| {
            let on = v == "on";
            if let Some(t) = this.upgrade() {
                t.update(cx, |t, cx| t.set(f(on), cx));
            }
        })
    }
    fn panel(th: &Theme, id: &'static str, title: &str, rows: Vec<AnyElement>) -> AnyElement {
        let th = *th;
        pid(
            id,
            div()
                .flex()
                .flex_col()
                .bg(th.low)
                .border_1()
                .border_color(th.border)
                .child(div().px(px(space::MD)).py(px(space::SM)).bg(th.mid).border_b_1().border_color(th.border).child(txt(&th, LABEL_CAPS, title.to_string()).color(th.muted)))
                .child(div().flex().flex_col().gap(px(space::MD)).p(px(space::MD)).children(rows)),
        )
        .into_any_element()
    }
    /// `.select` button / `<select>` box: 220 px min, bg base, 1 px frame.
    fn select_box(th: &Theme, id: &'static str, content: AnyElement) -> gpui::Stateful<gpui::Div> {
        let th = *th;
        div().id(id).flex().flex_row().items_center().justify_center().min_w(px(220.)).px(px(space::SM)).py(px(space::XS)).bg(th.base).border_1().border_color(th.border).text_color(th.text).cursor(CursorStyle::PointingHand).child(content)
    }
    fn number_box(th: &Theme, input: &Entity<TextInput>) -> gpui::Div {
        div().w(px(72.)).px(px(space::SM)).py(px(space::XS)).bg(th.base).border_1().border_color(th.border).flex().items_center().child(div().flex_1().min_w_0().child(input.clone()))
    }
    fn hint(th: &Theme, window: &Window, text: &str, danger: bool) -> AnyElement {
        let w = 584.0 - if danger { 10.0 } else { 0.0 };
        let lines = wrap_text(th, window, text, BODY_SM, w);
        let col = div().flex().flex_col().children(lines.into_iter().map(|l| txt(th, BODY_SM, l).color(th.muted)));
        if danger { col.border_l_2().border_color(th.clip).pl(px(space::SM)).into_any_element() } else { col.into_any_element() }
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = app::theme(cx);
        // blur commits (`onchange`)
        for (k, ent) in [self.threshold.clone(), self.window_size.clone()].into_iter().enumerate() {
            let f = ent.read(cx).focus_handle.is_focused(window);
            if self.focused[k] && !f {
                let v: f64 = js_number(&ent.read(cx).content);
                let this = cx.entity().downgrade();
                cx.defer(move |cx| {
                    if let Some(t) = this.upgrade() {
                        t.update(cx, |t, cx| if k == 0 { t.set_threshold(v, cx) } else { t.set_window(v, cx) });
                    }
                });
            }
            self.focused[k] = f;
        }
        let handle = self.scroll.clone();
        scroll_page(th, "settings-scroll", &handle, std::ptr::addr_of!(self.scroll) as usize, window, |_, window| {
            let this: WeakEntity<Self> = cx.entity().downgrade();
            // ---- Output
            let dev_label = {
                let id = self.out.as_ref().and_then(|o| o.device_id.clone()).or_else(|| self.devices.iter().find(|d| d.is_default).map(|d| d.id.clone())).unwrap_or_default();
                self.devices.iter().find(|d| d.id == id).map(|d| format!("{}{}", d.name, if d.is_default { " (default)" } else { "" })).unwrap_or_default()
            };
            let devs = self.devices.clone();
            let this_d = this.clone();
            // `<select>`: intrinsic width = text + padding/border + the 20 px arrow, 28 px high
            let dev_w = crate::text::natural_width(window, &dev_label, MONO, MONO_DATA.size, MONO_DATA.weight, MONO_DATA.tracking_px()) + 2.0 * space::SM + 2.0 + 20.0;
            let device_sel = pid(
                "set.device",
                Self::select_box(&th, "set-device", div().flex().flex_1().justify_start().child(txt(&th, MONO_DATA, dev_label).color(th.text)).child(super::ui::select_arrow(th.text)).into_any_element()).h(px(28.)).w(px(dev_w.max(220.))).on_click(cx.listener(move |t, ev: &ClickEvent, _, cx| {
                    let items: Vec<MenuItem> = devs
                        .iter()
                        .map(|d| {
                            let (id, this) = (d.id.clone(), this_d.clone());
                            MenuItem::action(format!("{}{}", d.name, if d.is_default { " (default)" } else { "" }), move |_, cx| {
                                let id2 = id.clone();
                                app::call(cx, move |b| {
                                    let _ = b.output_set_device(&id2);
                                });
                                if let Some(t) = this.upgrade() {
                                    t.update(cx, |t, cx| t.refresh_output(cx));
                                }
                            })
                        })
                        .collect();
                    let (menu, p) = (t.menu.clone(), ev.position());
                    cx.defer(move |cx| {
                        let Some(w) = cx.active_window() else { return };
                        let _ = w.update(cx, |_, window, cx| menu.update(cx, |m, cx| m.show(items, gpui::point(p.x, p.y + px(8.)), window, cx)));
                    });
                })),
            );
            let mode = self.out.as_ref().map(|o| if o.exclusive_error.is_some() { OutputMode::Exclusive } else { o.mode }).unwrap_or(OutputMode::Shared);
            let this_m = this.clone();
            let mode_tog = segmented("set-mode", &th, &[("exclusive", "Exclusive"), ("shared", "Shared")], if mode == OutputMode::Exclusive { "exclusive" } else { "shared" }, move |v, _, cx| {
                let m = if v == "exclusive" { OutputMode::Exclusive } else { OutputMode::Shared };
                let b = app::backend(cx);
                if let Some(t) = this_m.upgrade() {
                    cx.spawn(async move |cx| {
                        let r = cx
                            .background_executor()
                            .spawn(async move {
                                let _ = b.output_set_mode(m);
                                b.output_get_status()
                            })
                            .await;
                        let _ = t.update(cx, |t, cx| {
                            t.out = r.ok();
                            cx.notify();
                        });
                    })
                    .detach();
                }
            });
            let mut out_rows: Vec<AnyElement> = vec![Self::row(&th, "Device", device_sel).into_any_element(), Self::row(&th, "Mode", pid("set.mode", mode_tog)).into_any_element()];
            if let Some(o) = &self.out {
                if let Some(e) = &o.exclusive_error {
                    out_rows.push(Self::row(&th, "Exclusive unavailable", div().flex().child(txt(&th, MONO_DATA, e.clone()).color(th.clip))).into_any_element());
                }
                if let Some(sr) = o.sample_rate {
                    let khz = (sr as f64 / 1000.0 * 10.0).round() / 10.0;
                    let label = o.format_label.clone().unwrap_or_else(|| "—".into());
                    let text = if o.mode == OutputMode::Shared {
                        format!("SHARED · {}F MIX · {} KHZ · {label}", o.bit_depth.unwrap_or(32), khz)
                    } else {
                        format!("EXCLUSIVE · {}/{} · {label}", o.bit_depth.map(|b| b.to_string()).unwrap_or_else(|| "—".into()), (sr as f64 / 1000.0).round())
                    };
                    out_rows.push(Self::row(&th, "Current stream", Self::value(&th, text)).into_any_element());
                }
            }
            // ---- Scrobbling
            let scrobble = vec![
                Self::row(&th, "Submit plays to Navidrome", self.toggle(&th, "set-scrobble", self.s.scrobbling_enabled, SettingChange::Scrobbling, cx)).into_any_element(),
                Self::row(&th, "Count a play after (seconds, history and scrobbles)", Self::number_box(&th, &self.threshold)).into_any_element(),
            ];
            let shuffle = vec![
                Self::row(&th, "Spread artists in shuffle", self.toggle(&th, "set-spread", self.s.spread_artists_in_shuffle, SettingChange::SpreadArtists, cx)).into_any_element(),
                Self::row(&th, "Queue window", Self::number_box(&th, &self.window_size)).into_any_element(),
            ];
            // ---- Playback
            let mut playback = vec![
                Self::row(&th, "Autoplay similar music when the queue ends", self.toggle(&th, "set-autoplay", self.s.autoplay_enabled, SettingChange::Autoplay, cx)).into_any_element(),
                Self::row(&th, "Sync play queue with server", self.toggle(&th, "set-syncq", self.s.sync_play_queue, SettingChange::SyncPlayQueue, cx)).into_any_element(),
                Self::row(&th, "Show track preview when SONIX is in the background", self.toggle(&th, "set-overlay", self.s.track_overlay, SettingChange::TrackOverlay, cx)).into_any_element(),
            ];
            if self.s.track_overlay {
                let this_c = this.clone();
                let c = match self.s.track_overlay_corner {
                    OverlayCorner::TopLeft => "topLeft",
                    OverlayCorner::TopRight => "topRight",
                    OverlayCorner::BottomLeft => "bottomLeft",
                    OverlayCorner::BottomRight => "bottomRight",
                };
                playback.push(
                    Self::row(
                        &th,
                        "Track preview position",
                        segmented("set-corner", &th, &[("topLeft", "Top left"), ("topRight", "Top right"), ("bottomLeft", "Bottom left"), ("bottomRight", "Bottom right")], c, move |v, _, cx| {
                            let c = match v {
                                "topRight" => OverlayCorner::TopRight,
                                "bottomLeft" => OverlayCorner::BottomLeft,
                                "bottomRight" => OverlayCorner::BottomRight,
                                _ => OverlayCorner::TopLeft,
                            };
                            if let Some(t) = this_c.upgrade() {
                                t.update(cx, |t, cx| t.set(SettingChange::TrackOverlayCorner(c), cx));
                            }
                        }),
                    )
                    .into_any_element(),
                );
            }
            playback.push(Self::row(&th, "Free memory when minimized (UI reloads on restore)", self.toggle(&th, "set-freemem", self.s.free_memory_when_minimized, SettingChange::FreeMemoryWhenMinimized, cx)).into_any_element());
            let rg = match self.s.replay_gain_mode {
                ReplayGainMode::Off => "off",
                ReplayGainMode::Track => "track",
                ReplayGainMode::Album => "album",
            };
            let this_r = this.clone();
            playback.push(
                Self::row(
                    &th,
                    "Volume normalization (ReplayGain)",
                    segmented("set-rg", &th, &[("off", "Off"), ("track", "Track"), ("album", "Album")], rg, move |v, _, cx| {
                        let m = match v {
                            "track" => ReplayGainMode::Track,
                            "album" => ReplayGainMode::Album,
                            _ => ReplayGainMode::Off,
                        };
                        if let Some(t) = this_r.upgrade() {
                            t.update(cx, |t, cx| t.set(SettingChange::ReplayGain(m), cx));
                        }
                    }),
                )
                .into_any_element(),
            );
            if self.s.replay_gain_mode != ReplayGainMode::Off {
                playback.push(Self::hint(&th, window, "Applies the file's ReplayGain tag as digital gain, so output is no longer bit-perfect while a gain is applied. Tracks without a tag play unchanged.", false));
            }
            // ---- Server
            let st = self.status.clone();
            let dash = || "—".to_string();
            let mut server = vec![
                Self::row(&th, "URL", Self::value(&th, st.as_ref().and_then(|s| s.server_url.clone()).unwrap_or_else(dash))).into_any_element(),
                Self::row(&th, "User", Self::value(&th, st.as_ref().and_then(|s| s.username.clone()).unwrap_or_else(dash))).into_any_element(),
                Self::row(&th, "Server version", Self::value(&th, st.as_ref().and_then(|s| s.server_version.clone()).unwrap_or_else(dash))).into_any_element(),
                Self::row(&th, "API version", Self::value(&th, st.as_ref().and_then(|s| s.api_version.clone()).unwrap_or_else(dash))).into_any_element(),
                Self::row(&th, "Ping", Self::value(&th, st.as_ref().and_then(|s| s.ping_ms).map(|p| format!("{p}MS")).unwrap_or_else(dash))).into_any_element(),
                Self::row(&th, "Tracks indexed", Self::value(&th, st.as_ref().and_then(|s| s.track_count).map(fmt::fmt_int).unwrap_or_else(dash))).into_any_element(),
            ];
            if let Some(a) = self.art.as_ref().filter(|a| a.total > 0) {
                server.push(Self::row(&th, "Artwork cache", Self::value(&th, format!("{} / {} · {} MB", fmt::fmt_int(a.cached), fmt::fmt_int(a.total), (a.bytes as f64 / 1048576.0).round()))).into_any_element());
            }
            // ---- Updates
            let this_u = this.clone();
            let check = Self::select_box(&th, "set-check", txt(&th, LABEL_CAPS, if self.checking { "Checking…" } else { "Check now" }).color(th.text).into_any_element())
                .when(self.checking, |d| d.opacity(0.5).cursor(CursorStyle::OperationNotAllowed))
                .when(!self.checking, |d| d.on_click(cx.listener(|t, _, _, cx| t.check_now(cx))));
            let _ = this_u;
            let mut upd_row = div().flex().flex_row().items_center().justify_between().gap(px(space::MD)).child(pid("set.check", check));
            if let Some(m) = &self.update_msg {
                upd_row = upd_row.child(txt(&th, BODY_SM, m.clone()).color(th.muted));
            }
            let updates = vec![
                Self::row(&th, "Current version", Self::value(&th, "v0.6.1")).into_any_element(),
                Self::row(&th, "Check for updates automatically", self.toggle(&th, "set-autoupd", self.s.auto_check_updates, SettingChange::AutoCheckUpdates, cx)).into_any_element(),
                upd_row.into_any_element(),
            ];
            // ---- MCP
            let mut mcp_rows: Vec<AnyElement> = vec![
                Self::hint(
                    &th,
                    window,
                    "Lets Claude Code or Claude Desktop read your library, listening history and play queue through a local-only connection (no network port). Everything Claude reads is sent to Claude (Anthropic) as part of your conversation: track, album and artist names, playlists, play history, what is playing. It never sees your server address, password or file paths, and this version cannot change anything on the server (no playlist edits, likes or ratings).",
                    false,
                ),
                Self::row(&th, "Let Claude read my library", pid("set.mcp", self.toggle(&th, "set-mcp", self.mcp.as_ref().map(|m| m.enabled).unwrap_or(false), SettingChange::McpEnabled, cx))).into_any_element(),
            ];
            if let Some(m) = self.mcp.clone().filter(|m| m.enabled) {
                mcp_rows.push(Self::row(&th, "Allow playback control (play, skip, queue, volume)", self.toggle(&th, "set-mcp-ctl", m.allow_control, SettingChange::McpAllowControl, cx)).into_any_element());
                mcp_rows.push(Self::row(&th, "Connection", Self::value(&th, if m.listening { "Listening" } else { "Not listening" })).into_any_element());
                if let Some(cmd) = m.register_command.clone() {
                    mcp_rows.push(Self::hint(&th, window, "Register it once in Claude Code:", false));
                    let lines = wrap_text(&th, window, &cmd, MONO_DATA, 584.0 - 2.0 - 2.0 * space::SM);
                    mcp_rows.push(
                        div()
                            .flex()
                            .flex_col()
                            .px(px(space::SM))
                            .py(px(space::XS))
                            .bg(th.base)
                            .border_1()
                            .border_color(th.border)
                            .children(lines.into_iter().map(|l| txt(&th, MONO_DATA, l).color(th.text)))
                            .into_any_element(),
                    );
                    let cmd2 = cmd.clone();
                    let copy = Self::select_box(&th, "set-copy", txt(&th, LABEL_CAPS, if self.copied { "Copied" } else { "Copy command" }).color(th.text).into_any_element()).on_click(cx.listener(move |t, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(cmd2.clone()));
                        t.copied = true;
                        cx.notify();
                        cx.spawn(async move |this, cx| {
                            Timer::after(Duration::from_millis(1500)).await;
                            let _ = this.update(cx, |t, cx| {
                                t.copied = false;
                                cx.notify();
                            });
                        })
                        .detach();
                    }));
                    mcp_rows.push(div().flex().flex_row().child(copy).into_any_element());
                }
                if !m.bridge_exists {
                    mcp_rows.push(Self::hint(&th, window, "sonix-mcp was not found next to SONIX. The Windows installer puts it there, so reinstalling or updating SONIX fixes this. Development builds and macOS: build it with npm run build:mcp (or cargo build --release in tools/sonix-mcp) and put it at that path.", true));
                }
            }
            // ---- logout
            let logout = div()
                .id("set-logout")
                .flex()
                .flex_row()
                .items_center()
                .gap(px(space::SM))
                .px(px(space::LG))
                .py(px(space::SM))
                .bg(th.mid)
                .border_1()
                .border_color(th.clip)
                .text_color(th.text)
                .cursor(CursorStyle::PointingHand)
                .hover(move |s| s.bg(th.clip))
                .child(icon("logout", 14., th.text))
                .child(txt(&th, LABEL_CAPS, "Log out").color(th.text))
                .on_click(cx.listener(|t, _, _, cx| {
                    let b = t.backend.clone();
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().spawn(async move { let _ = b.auth_logout(); }).await;
                        let _ = this.update(cx, |_, cx| cx.emit(ListsEvent::Navigate(crate::router::Route::Login)));
                    })
                    .detach();
                }));
            let _ = hard_shadow;
            pid("page", div()
                .flex()
                .flex_col()
                .gap(px(space::LG))
                .p(px(space::LG))
                .max_w(px(640.))
                .child(pid("page.title", div().flex().child(txt(&th, HEADLINE_MD, "Settings").color(th.text))))
                .child(Self::panel(&th, "set.panel.0", "Output", out_rows))
                .child(Self::panel(&th, "set.panel.1", "Scrobbling", scrobble))
                .child(Self::panel(&th, "set.panel.2", "Shuffle", shuffle))
                .child(Self::panel(&th, "set.panel.3", "Playback", playback))
                .child(Self::panel(&th, "set.panel.4", "Server", server))
                .child(Self::panel(&th, "set.panel.5", "Updates", updates))
                .child(Self::panel(&th, "set.panel.6", "Claude (MCP)", mcp_rows))
                .child(div().flex().flex_row().child(pid("set.logout", logout))))
                .into_any_element()
        })
    }
}
