//! Parity automation hook (cargo feature `parity`, never part of a release build).
//!
//! A test runner that launched this process (`SONIX_PARITY_PORT=<port>`) talks JSON lines over a
//! loopback TCP socket (127.0.0.1 only). Every request is `{"id":N,"cmd":"...",...}` and gets one
//! `{"id":N,"ok":true,...}` / `{"id":N,"ok":false,"error":"..."}` reply. Commands run on the UI
//! thread; synthetic input goes through the public `Window::dispatch_event`, so it never needs
//! OS focus and never moves the user's real cursor. See "Parity automation" in docs/PARITY.md.
//!
//! | cmd | args | effect |
//! |---|---|---|
//! | `hello` | | scale factor, viewport (logical px), route, theme |
//! | `route` | `route`, `fresh` | navigate (hash-router names); `fresh`: the history restarts there (a scenario start) |
//! | `size` | `w`,`h` | resize the window (logical px) |
//! | `theme` | `light` | set the theme |
//! | `mouse` | `type` move/down/up/click/dblclick/wheel, `x`,`y`, `button`, `mods`, `dx`,`dy` | input |
//! | `key` | `keys` ("ctrl-a", "enter", ...) or `text` | input |
//! | `clock` | `freeze` secs / `step` secs / `run` | test clock (also steps the mock backend) |
//! | `play` | `startTrackId`? | start playback of the Tracks order |
//! | `seek` | `sec` | seek |
//! | `settle` | `frames` | wait until N more frames were painted |
//! | `boxes` | | `{id: [x,y,w,h]}` of every probed element (logical px) |
//! | `state` | | route, selection, focus, menu, queue, scroll, player |
//! | `capture` | `path` | client-area PNG |
//! | `quit` | | exit |
use crate::router::Route;
use crate::views::shell::Shell;
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::{
    Context, Keystroke, Modifiers, Window, px, size,
};
use serde_json::{Value, json};
use sonix_mock::MockBackend;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

pub struct Req {
    cmd: Value,
    reply: Sender<Value>,
}

static RX: Mutex<Option<UnboundedReceiver<Req>>> = Mutex::new(None);
static MOCK: OnceLock<Arc<MockBackend>> = OnceLock::new();

/// Start the control server if `SONIX_PARITY_PORT` is set. Returns whether the hook is active.
pub fn start(mock: Arc<MockBackend>) -> bool {
    let Ok(port) = std::env::var("SONIX_PARITY_PORT") else { return false };
    let Ok(port) = port.parse::<u16>() else { return false };
    let _ = MOCK.set(mock);
    let (tx, rx) = unbounded::<Req>();
    *RX.lock().unwrap() = Some(rx);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).expect("parity port");
    std::thread::Builder::new()
        .name("parity-ctl".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let tx = tx.clone();
                std::thread::spawn(move || serve(stream, tx));
            }
        })
        .ok();
    true
}

fn serve(stream: std::net::TcpStream, tx: UnboundedSender<Req>) {
    let mut out = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    for line in BufReader::new(stream).lines().map_while(Result::ok) {
        let Ok(cmd) = serde_json::from_str::<Value>(&line) else { continue };
        let id = cmd.get("id").cloned().unwrap_or(Value::Null);
        let (rtx, rrx) = channel();
        if tx.unbounded_send(Req { cmd, reply: rtx }).is_err() {
            return;
        }
        let mut v = rrx.recv_timeout(Duration::from_secs(60)).unwrap_or_else(|_| json!({"ok": false, "error": "timeout"}));
        v["id"] = id;
        if writeln!(out, "{v}").is_err() {
            return;
        }
    }
}

fn mods(v: &Value) -> Modifiers {
    let mut m = Modifiers::default();
    if let Some(a) = v.get("mods").and_then(Value::as_array) {
        for s in a.iter().filter_map(Value::as_str) {
            match s {
                "ctrl" => m.control = true,
                "shift" => m.shift = true,
                "alt" => m.alt = true,
                _ => {}
            }
        }
    }
    m
}

fn err(e: impl std::fmt::Display) -> Value {
    json!({"ok": false, "error": e.to_string()})
}

impl Shell {
    /// Drains control requests for the lifetime of the window.
    pub(crate) fn parity_attach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut rx) = RX.lock().unwrap().take() else { return };
        cx.spawn_in(window, async move |this, cx| {
            while let Some(req) = rx.next().await {
                let cmd = req.cmd.clone();
                let name = cmd.get("cmd").and_then(Value::as_str).unwrap_or("").to_string();
                let out = if name == "mouse" {
                    let info = this.update_in(cx, |_, w, _| (crate::winffi::hwnd_of(w), w.scale_factor())).ok();
                    match info {
                        Some((Some(hwnd), scale)) => {
                            let g = |k: &str| cmd.get(k).and_then(Value::as_f64).unwrap_or(0.0) as f32;
                            let m = mods(&cmd);
                            let button = cmd.get("button").and_then(Value::as_str).unwrap_or("left").to_string();
                            let (x, y) = ((g("x") * scale).round() as i32, (g("y") * scale).round() as i32);
                            let seq: &[&str] = match cmd.get("type").and_then(Value::as_str).unwrap_or("") {
                                "move" => &["move"],
                                "down" => &["move", "down"],
                                "dragmove" => &["dragmove"],
                                "up" => &["up"],
                                "click" => &["move", "down", "up"],
                                "dblclick" => &["move", "down", "up", "down", "up"],
                                "wheel" => &["move", "wheel"],
                                _ => &[],
                            };
                            for k in seq {
                                // Not inside an app update: SendMessage re-enters GPUI's window procedure.
                                crate::winffi::send_mouse(hwnd, k, x, y, &button, (m.control, m.shift, m.alt), g("dy") / 100.0); // one notch (120 units) per 100 px; `widgets::smooth` scrolls 100 px per notch like Chromium
                            }
                            let _ = this.update_in(cx, |_, w, _| w.refresh());
                            json!({"ok": true})
                        }
                        _ => err("no hwnd"),
                    }
                } else if name == "key" {
                    // Dispatched with the window + app only, NOT inside an update of the Shell entity: actions bound on the
                    // root (Escape, Space, N, ...) update the Shell and would re-enter its lease (abort).
                    let keys: Vec<Keystroke> = match cmd.get("text").and_then(Value::as_str) {
                        Some(t) => t
                            .chars()
                            .map(|ch| {
                                let mut ks = Keystroke { modifiers: Modifiers::default(), key: ch.to_string(), key_char: Some(ch.to_string()) };
                                if ch.is_uppercase() {
                                    ks.key = ch.to_lowercase().to_string();
                                    ks.modifiers.shift = true;
                                }
                                ks
                            })
                            .collect(),
                        None => match Keystroke::parse(cmd.get("keys").and_then(Value::as_str).unwrap_or("")) {
                            Ok(mut ks) => {
                                let m = ks.modifiers;
                                if ks.key_char.is_none() && ks.key.chars().count() == 1 && !m.control && !m.alt && !m.platform {
                                    ks.key_char = Some(ks.key.clone());
                                }
                                vec![ks]
                            }
                            Err(e) => {
                                let _ = req.reply.send(err(e));
                                continue;
                            }
                        },
                    };
                    let _ = cx.update(|window, cx| {
                        for ks in keys {
                            window.dispatch_keystroke(ks, cx);
                        }
                        window.refresh();
                    });
                    json!({"ok": true})
                } else if name == "settle" {
                    let n = cmd.get("frames").and_then(Value::as_u64).unwrap_or(2);
                    let target = crate::probe::frames() + n;
                    let mut waited = 0;
                    while crate::probe::frames() < target && waited < 600 {
                        let _ = this.update_in(cx, |_, w, _| w.refresh());
                        gpui::Timer::after(Duration::from_millis(5)).await;
                        waited += 1;
                    }
                    if crate::probe::frames() < target { err("no frames were painted (window hidden/minimized?)") } else { json!({"ok": true}) }
                } else {
                    this.update_in(cx, |sh, window, cx| sh.parity_exec(&name, &cmd, window, cx)).unwrap_or_else(err)
                };
                let quit = name == "quit";
                let _ = req.reply.send(out);
                if quit {
                    let _ = cx.update(|_, cx| cx.quit());
                }
            }
        })
        .detach();
    }

    fn parity_exec(&mut self, name: &str, c: &Value, window: &mut Window, cx: &mut Context<Self>) -> Value {
        let f = |k: &str| c.get(k).and_then(Value::as_f64).unwrap_or(0.0) as f32;
        let s = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        match name {
            "hello" => json!({
                "ok": true,
                "scale": window.scale_factor(),
                "viewport": [f32::from(window.viewport_size().width), f32::from(window.viewport_size().height)],
                "route": self.router.current().path(),
                "light": self.th.light
            }),
            "card" => {
                let id = s("trackId");
                let b = self.backend.clone();
                cx.spawn(async move |this, cx| {
                    let t = cx
                        .background_executor()
                        .spawn(async move {
                            let _ = b.player_play_all(sonix_api::PlayAllRequest {
                                order: sonix_api::TrackOrder::Sort { field: sonix_api::TrackSortField::Title, dir: sonix_api::SortDir::Asc },
                                filter: None,
                                start_track_id: Some(id),
                                flow: None,
                            });
                            b.player_get_state().ok().and_then(|s| s.track)
                        })
                        .await;
                    let _ = this.update(cx, |sh, cx| {
                        if let Some(t) = t {
                            sh.show_card(t, cx);
                        }
                    });
                })
                .detach();
                json!({"ok": true})
            }
            "ocapture" => {
                let Some(c) = &self.card else { return err("no card") };
                let path = s("path");
                let r = c.win.update(cx, |_, w, _| {
                    crate::winffi::hwnd_of(w).and_then(crate::winffi::capture_client)
                });
                match r {
                    Ok(Some((w, h, mut bgra))) => {
                        for p in bgra.chunks_mut(4) {
                            p.swap(0, 2);
                            p[3] = 255;
                        }
                        match image::save_buffer(path, &bgra, w, h, image::ColorType::Rgba8) {
                            Ok(()) => json!({"ok": true, "w": w, "h": h}),
                            Err(e) => err(e),
                        }
                    }
                    _ => err("card capture failed"),
                }
            }
            "mockupdate" => {
                if let Some(m) = MOCK.get() {
                    m.set_mock_update(true);
                }
                json!({"ok": true})
            }
            "updcheck" => {
                crate::app::call(cx, |b| {
                    let _ = b.updates_check();
                });
                json!({"ok": true})
            }
            "boot" => {
                self.start_boot(window, cx);
                window.refresh();
                json!({"ok": true})
            }
            "route" => match Route::parse(&s("route")) {
                Some(r) => {
                    self.navigate(r, cx);
                    // `fresh`: the scenario's start route, like the web's page load at `#/<route>` (history index 0:
                    // Back disabled), not a navigation from the native cold-start route (Home).
                    if c.get("fresh").and_then(Value::as_bool).unwrap_or(false) {
                        self.router = crate::router::Router::new(self.router.current().clone());
                        cx.notify();
                    }
                    window.refresh();
                    json!({"ok": true})
                }
                None => err("unknown route"),
            },
            "size" => {
                window.resize(size(px(f("w")), px(f("h"))));
                window.refresh();
                json!({"ok": true})
            }
            "theme" => {
                let want = c.get("light").and_then(Value::as_bool).unwrap_or(false);
                if want != self.th.light {
                    self.toggle_theme(cx);
                }
                window.refresh();
                json!({"ok": true})
            }
            "mouse" => err("handled by the async loop"),
            "key" => err("handled by the async loop"),
            "clock" => {
                if let Some(t) = c.get("freeze").and_then(Value::as_f64) {
                    crate::clock::manual::freeze(t);
                } else if let Some(dt) = c.get("step").and_then(Value::as_f64) {
                    crate::clock::manual::step(dt);
                    if c.get("tick").and_then(Value::as_bool).unwrap_or(true)
                        && let Some(m) = MOCK.get()
                    {
                        let t = c.get("spectrumT").and_then(Value::as_f64).unwrap_or_else(crate::clock::now);
                        m.advance(dt, t);
                    }
                } else {
                    crate::clock::manual::unfreeze();
                }
                if let Some(c) = &self.card {
                    let _ = c.win.update(cx, |_, w, cx| {
                        cx.notify();
                        w.refresh();
                    });
                }
                window.refresh();
                json!({"ok": true, "t": crate::clock::now()})
            }
            "play" => {
                let start = c.get("startTrackId").and_then(Value::as_str).map(str::to_string);
                crate::app::call(cx, move |b| {
                    let _ = b.player_play_all(sonix_api::PlayAllRequest {
                        order: sonix_api::TrackOrder::Sort { field: sonix_api::TrackSortField::Title, dir: sonix_api::SortDir::Asc },
                        filter: None,
                        start_track_id: start,
                        flow: None,
                    });
                });
                json!({"ok": true})
            }
            "seek" => {
                let sec = f("sec") as f64;
                crate::app::call(cx, move |b| {
                    let _ = b.player_seek(sonix_api::SeekRequest { position_sec: sec });
                });
                json!({"ok": true})
            }
            "measure" => {
                // shaped width of a string with a bundled family/weight (font matching check)
                let fam: &'static str = match s("family").as_str() {
                    "Space Grotesk" => "Space Grotesk",
                    "Inter" => "Inter",
                    _ => "JetBrains Mono",
                };
                let fnt = crate::text::css_font(fam, f("weight"));
                let text = s("text");
                let run = gpui::TextRun { len: text.len(), font: fnt, color: gpui::black(), background_color: None, underline: None, strikethrough: None };
                let l = window.text_system().layout_line(&text, px(f("size")), &[run], None);
                json!({"ok": true, "width": f32::from(l.width), "ascent": f32::from(l.ascent), "descent": f32::from(l.descent)})
            }
            "boxes" => json!({"ok": true, "boxes": crate::probe::boxes()}),
            "state" => self.parity_state(window, cx),
            "capture" => {
                let Some(hwnd) = crate::winffi::hwnd_of(window) else { return err("no hwnd") };
                match crate::winffi::capture_client(hwnd) {
                    Some((w, h, mut bgra)) => {
                        for p in bgra.chunks_mut(4) {
                            p.swap(0, 2);
                            p[3] = 255;
                        }
                        match image::save_buffer(s("path"), &bgra, w, h, image::ColorType::Rgba8) {
                            Ok(()) => json!({"ok": true, "w": w, "h": h}),
                            Err(e) => err(e),
                        }
                    }
                    None => err("capture failed"),
                }
            }
            "quit" => json!({"ok": true}),
            other => err(format!("unknown cmd {other}")),
        }
    }

    fn parity_state(&self, window: &mut Window, cx: &mut Context<Self>) -> Value {
        let mut tracks = self.tracks.read(cx).parity_state(window, cx);
        // list routes (port-lists): the visible table reports through the same fields as the Tracks route
        let lp = self.lists_parity_state(window, cx);
        if lp.get("selection").is_some() {
            for k in ["selection", "selectionCount", "scrollTop"] {
                tracks[k] = lp[k].clone();
            }
            tracks["sort"] = serde_json::Value::Null;
            tracks["trackCount"] = serde_json::Value::Null;
            tracks["titles"] = lp["titles"].clone();
            tracks["focus"] = if lp["tableFocus"] == true { "tracklist".into() } else { "body".into() };
        }
        let menu_open = self.menu.read(cx).is_open();
        let _ = menu_open;
        let focus = if menu_open && self.menu.read(cx).has_focus(window) {
            "menu"
        } else if gpui::Focusable::focus_handle(self.queue.read(cx), cx).is_focused(window) {
            "tracklist"
        } else if self.search.read(cx).focus_handle.is_focused(window) {
            "search"
        } else {
            // any focused element inside a `[data-tracklist]` list (Tracks, album tracks, popular tracks)
            let in_list = window.context_stack().iter().any(|c| c.contains("TrackList"));
            let filter = tracks["focus"].as_str() == Some("filter") && *self.router.current() == Route::Tracks;
            if filter { "filter" } else if in_list { "tracklist" } else { "body" }
        };
        let ps = self.player.read(cx).state.clone();
        json!({
            "ok": true,
            "route": self.router.current().path(),
            "theme": if self.th.light { "light" } else { "dark" },
            "viewport": [f32::from(window.viewport_size().width), f32::from(window.viewport_size().height)],
            "focus": focus,
            "selection": tracks["selection"],
            "selectionCount": tracks["selectionCount"],
            "sort": tracks["sort"],
            "trackCount": tracks["trackCount"],
            "titles": tracks["titles"],
            "scrollTop": tracks["scrollTop"],
            "lists": lp,
            "menu": self.menu.read(cx).parity_state(),
            "queueDrawer": self.queue_drawer,
            "player": {
                "status": format!("{:?}", ps.status).to_lowercase(),
                "trackId": ps.track.as_ref().map(|t| t.id.clone()),
                "positionSec": ps.position_sec,
                "queueLength": ps.queue_length,
                "shuffle": ps.shuffle,
            },
        })
    }
}
