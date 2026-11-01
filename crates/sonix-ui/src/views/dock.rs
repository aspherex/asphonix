//! The player dock (`PlayerDock.svelte`): cover + title, transport, seek, dB volume fader,
//! output readout, queue toggle. Its own entity: the 4 Hz position tick re-renders only
//! the dock (and Now Playing), never the Tracks list.
use crate::app::{self, PlayerModel};
use crate::art::{self, ArtHandle};
use crate::theme::Theme;
use crate::tracking::{BODY_LG, MONO_CODE, MONO_DATA};
use crate::typo::{ellipsis, ts};
use crate::widgets::button::ghost_icon;
use crate::widgets::slider::{self, SliderHandle, slider};
use crate::widgets::tooltip::tip;
use crate::widgets::{hard_shadow, icon, icon_g};
use gpui::{Context, CursorStyle, Entity, EventEmitter, IntoElement, MouseButton, Render, ScrollWheelEvent, SharedString, Subscription, Window, div, prelude::*, px};
use sonix_api::format as fmt;
use sonix_api::*;
use std::rc::Rc;
use std::sync::Arc;

pub enum DockEvent {
    OpenNowPlaying,
    ToggleQueue,
}
impl EventEmitter<DockEvent> for DockView {}

pub struct DockView {
    backend: Arc<dyn Backend>,
    art: ArtHandle,
    player: Entity<PlayerModel>,
    seek: SliderHandle,
    seek_preview: Option<f32>,
    vol: SliderHandle,
    last_audible: f64,
    _sub: Subscription,
}

impl DockView {
    pub fn new(backend: Arc<dyn Backend>, art: ArtHandle, player: &Entity<PlayerModel>, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(player, |_, _, cx| cx.notify());
        Self { backend, art, player: player.clone(), seek: slider::new_handle(), seek_preview: None, vol: slider::new_handle(), last_audible: 0.5, _sub: sub }
    }
}

impl Render for DockView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th: Theme = app::theme(cx);
        let ps = self.player.read(cx).state.clone();
        let vw = f32::from(window.viewport_size().width);
        let track = ps.track.clone();
        let has = track.is_some();
        let playing = ps.status == PlaybackStatus::Playing;
        let loading = ps.status == PlaybackStatus::Loading;
        let bit_perfect = has && ps.bit_perfect;
        let seek_ratio = self.seek_preview.unwrap_or(if ps.duration_sec > 0.0 { (ps.position_sec / ps.duration_sec) as f32 } else { 0.0 });
        let readout = fmt::output_readout(&ps, "OUTPUT: —");
        let short = fmt::output_short(&ps, "—");
        let vol_label = fmt::volume_label(&ps);
        let rg = ps.replay_gain_db.map(|g| format!("RG {}{g:.1}", if g > 0.0 { "+" } else { "" }));
        let backend_for_art = self.backend.clone();
        let cover = art::cover(&self.art, &backend_for_art, track.as_ref().and_then(|t| t.cover_art.as_deref()), 44.0, window, cx);

        let pos_label = fmt::format_duration(Some(if self.seek_preview.is_some() { seek_ratio as f64 * ps.duration_sec } else { ps.position_sec }));
        let dur_label = fmt::format_duration(Some(ps.duration_sec));

        // --- now playing zone
        let np = div()
            .flex_1()
            .min_w(px(180.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .child(
                div()
                    .id("dock-art")
                    .size(px(44.))
                    .flex_none()
                    .border_1()
                    .border_color(th.border)
                    .bg(th.high)
                    .overflow_hidden()
                    .cursor(CursorStyle::PointingHand)
                    .when(!has, |d| d.flex().items_center().justify_center().text_color(th.muted).child(icon("equalizer", 18., th.muted)))
                    .when(has, |d| d.child(cover))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DockEvent::OpenNowPlaying))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(ellipsis(ts(&th, BODY_LG).text_color(th.text)).child(SharedString::from(track.as_ref().map(|t| t.title.clone()).unwrap_or_else(|| "No track loaded".into()))))
                    .child(ellipsis(ts(&th, MONO_DATA).text_color(th.muted)).child(SharedString::from(match &track {
                        Some(t) => match &t.album {
                            Some(a) => format!("{} · {}", t.artist, a),
                            None => t.artist.clone(),
                        },
                        None => "Pick an album or track to play".into(),
                    }))),
            )
            .children(track.as_ref().map(|t| {
                let starred = t.starred.is_some();
                let (id, on) = (t.id.clone(), starred);
                ghost_icon("dock-like", th, if starred { "heart-filled" } else { "heart" }, 18., 28., starred, false).on_click(cx.listener(move |_, _, _, cx| {
                    let id = id.clone();
                    app::call(cx, move |b| {
                        let req = StarRequest { id, kind: StarTargetType::Track };
                        let _ = if on { b.star_remove(req) } else { b.star_add(req) };
                    });
                }))
            }));

        // --- transport
        let play_btn = div()
            .id("dock-play")
            .size(px(36.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .bg(th.accent)
            .text_color(th.accent_text)
            .shadow(hard_shadow(&th, 2.))
            .when(has, |d| d.cursor(CursorStyle::PointingHand).hover(move |s| s.bg(th.accent_hover)))
            .when(!has, |d| d.opacity(0.5))
            .child(icon(if playing { "pause" } else { "play" }, 18., th.accent_text))
            .on_click(cx.listener(|this, _, _, cx| {
                if this.player.read(cx).state.track.is_some() {
                    app::call(cx, |b| {
                        let _ = b.player_play_pause();
                    });
                }
            }));
        let dur = ps.duration_sec;
        let seek_cb: slider::OnChange = {
            let this = cx.entity().downgrade();
            Rc::new(move |f, committed, _w, cx| {
                if let Some(t) = this.upgrade() {
                    t.update(cx, |d, cx| {
                        if committed {
                            d.seek_preview = None;
                            let pos = f as f64 * dur;
                            app::call(cx, move |b| {
                                let _ = b.player_seek(SeekRequest { position_sec: pos });
                            });
                        } else {
                            d.seek_preview = Some(f);
                        }
                        cx.notify();
                    });
                }
            })
        };
        let transport = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(6.))
            .flex_shrink()
            .flex_basis(px(if vw < 1280.0 { 420. } else { 520. }))
            .min_w(px(300.))
            .max_w(px(620.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.))
                    .child(ghost_icon("dock-shuffle", th, "shuffle", 15., 28., ps.shuffle, !has).on_click(cx.listener(|this, _, _, cx| {
                        let on = !this.player.read(cx).state.shuffle;
                        app::call(cx, move |b| {
                            let _ = b.player_set_shuffle(on);
                        });
                    })))
                    .child(ghost_icon("dock-prev", th, "prev", 16., 28., false, !has).on_click(cx.listener(|_, _, _, cx| {
                        app::call(cx, |b| {
                            let _ = b.player_previous();
                        })
                    })))
                    .child(play_btn)
                    .child(ghost_icon("dock-next", th, "next", 16., 28., false, !has).on_click(cx.listener(|_, _, _, cx| {
                        app::call(cx, |b| {
                            let _ = b.player_next();
                        })
                    })))
                    .child(ghost_icon("dock-repeat", th, if ps.repeat == RepeatMode::One { "repeat-one" } else { "repeat" }, 15., 28., ps.repeat != RepeatMode::Off, !has).on_click(
                        cx.listener(|this, _, _, cx| {
                            let next = match this.player.read(cx).state.repeat {
                                RepeatMode::Off => RepeatMode::All,
                                RepeatMode::All => RepeatMode::One,
                                RepeatMode::One => RepeatMode::Off,
                            };
                            app::call(cx, move |b| {
                                let _ = b.player_set_repeat(next);
                            });
                        }),
                    )),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.))
                    .child(ts(&th, MONO_CODE).w(px(40.)).flex_none().text_center().text_color(th.muted).child(SharedString::from(pos_label)))
                    .child(div().flex_1().child(slider("dock-seek", th, &self.seek, seek_ratio, None, seek_cb)))
                    .child(ts(&th, MONO_CODE).w(px(40.)).flex_none().text_center().text_color(th.muted).child(SharedString::from(dur_label))),
            );

        // --- aux zone
        let vol_cb: slider::OnChange = {
            let this = cx.entity().downgrade();
            Rc::new(move |f, _committed, _w, cx| {
                if let Some(t) = this.upgrade() {
                    t.update(cx, |_, cx| set_volume(f as f64, cx));
                }
            })
        };
        let vol_id = ps.volume;
        let output: gpui::AnyElement = if let Some(e) = &ps.error {
            ts(&th, MONO_CODE).max_w(px(220.)).text_color(th.clip).whitespace_nowrap().overflow_hidden().child(SharedString::from(e.clone())).into_any_element()
        } else if loading {
            ts(&th, MONO_CODE).text_color(th.muted).child("LOADING…").into_any_element()
        } else {
            let full = vw >= 1680.0;
            let text = if full { format!("{readout}{}", if bit_perfect { " · BIT-PERFECT" } else { "" }) } else { short };
            ellipsis(ts(&th, MONO_CODE)).text_color(if bit_perfect { th.accent_fg } else { th.muted }).when(!full, |d| d.flex_none()).child(SharedString::from(text)).into_any_element()
        };
        let aux = div()
            .flex_1()
            .min_w(px(260.))
            .flex()
            .flex_row()
            .items_center()
            .justify_end()
            .gap(px(12.))
            .child(output)
            .child(
                div()
                    .id("dock-volume")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .flex_none()
                    .text_color(th.muted)
                    .on_scroll_wheel(cx.listener(move |_, ev: &ScrollWheelEvent, _, cx| {
                        let dy = f32::from(ev.delta.pixel_delta(px(16.)).y);
                        if dy != 0.0 {
                            set_volume((vol_id + if dy > 0.0 { 1.0 } else { -1.0 } / 60.0).clamp(0.0, 1.0), cx);
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                            // double-click on the fader = 0 dB (Fader onReset)
                            if ev.click_count == 2 {
                                set_volume(1.0, cx);
                            } else if vol_id > 0.0 {
                                this.last_audible = vol_id;
                            }
                        }),
                    )
                    .child(icon("volume", 15., th.muted))
                    .child(div().w(px(112.)).child(slider("dock-vol", th, &self.vol, ps.volume as f32, None, vol_cb)))
                    .child(ts(&th, MONO_CODE).flex_none().min_w(px(52.)).text_right().whitespace_nowrap().text_color(th.muted).child(SharedString::from(vol_label)))
                    .children(rg.map(|r| ts(&th, MONO_CODE).flex_none().whitespace_nowrap().text_color(th.muted).child(SharedString::from(r)))),
            )
            .child({
                let q = ps.queue_length;
                tip(div().id("dock-queue").relative().size(px(28.)).flex_none().flex().items_center().justify_center().group("hv").cursor(CursorStyle::PointingHand).child(icon_g("queue", 16., th.muted, th.accent_fg)).children((q > 0).then(|| {
                    ts(&th, MONO_CODE).absolute().top(px(-2.)).right(px(-6.)).px(px(2.)).bg(th.base).text_color(th.accent_fg).child(SharedString::from(q.to_string()))
                })), "Toggle queue rail", th)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(DockEvent::ToggleQueue)))
            });

        div()
            .h(px(72.))
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(16.))
            .px(px(16.))
            .bg(th.chrome)
            .border_t_1()
            .border_color(th.frame)
            .child(np)
            .child(transport)
            .child(aux)
    }
}

fn set_volume(v: f64, cx: &gpui::App) {
    app::call(cx, move |b| {
        let _ = b.player_set_volume(SetVolumeRequest { volume: v.clamp(0.0, 1.0) });
    });
}
