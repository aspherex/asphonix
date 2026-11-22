//! The player dock: cover + title, transport, seek, dB volume fader,
//! output readout, queue toggle. Its own entity: the 4 Hz position tick re-renders only
//! the dock (and Now Playing), never the Tracks list. Sizes: `crate::metrics::dock`.
use super::library::entity_link::{Kind as LinkKind, entity_link};
use crate::app::{self, PlayerModel};
use crate::art::{self, ArtHandle};
use crate::metrics::{bp, dock as dk, layout};
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{BODY_LG, MONO_CODE, MONO_DATA};
use crate::widgets::button::{ghost_icon, ghost_icon_with};
use crate::widgets::slider::{self, SliderHandle, slider};
use crate::widgets::tooltip::{Tip, tip};
use crate::widgets::{hard_shadow, icon, icon_g};
use gpui::{Context, CursorStyle, Entity, EventEmitter, IntoElement, MouseButton, Render, ScrollWheelEvent, SharedString, Subscription, Window, div, prelude::*, px};
use asphonix_api::format as fmt;
use asphonix_api::*;
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
    /// Hovering the cover or the title turns the title accent-fg.
    art_hover: bool,
    title_hover: bool,
    _sub: Subscription,
    pos_gate: crate::app::PosGate,
    scale: f32,
}

impl DockView {
    pub fn new(backend: Arc<dyn Backend>, art: ArtHandle, player: &Entity<PlayerModel>, cx: &mut Context<Self>) -> Self {
        // cached view (`crate::part`): repaint when the shown second or the seek bar's pixel changes, not on every tick
        let sub = cx.observe(player, |this: &mut Self, p, cx| {
            // the seek bar width in device px (`scale`: the window scale factor at the last render)
            let steps = f32::from(this.seek.bounds.get().size.width) * this.scale;
            if this.pos_gate.changed(&p.read(cx).state, steps) {
                cx.notify();
            }
        });
        Self { backend, art, player: player.clone(), seek: slider::new_handle(), seek_preview: None, vol: slider::new_handle(), last_audible: 0.5, art_hover: false, title_hover: false, _sub: sub, pos_gate: Default::default(), scale: 1.0 }
    }
}

impl Render for DockView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::rcount::hit("dock");
        self.scale = window.scale_factor();
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
        let cover = art::cover(&self.art, &backend_for_art, track.as_ref().and_then(|t| t.cover_art.as_deref()), dk::ART, window, cx);

        let pos_label = fmt::format_duration(Some(ps.position_sec)); // the label never follows the drag preview
        let dur_label = fmt::format_duration(Some(ps.duration_sec));
        let t_btn = |id: &'static str, name: &str, size: f32, on: bool| ghost_icon_with(id, th, name, size, dk::T_BTN, on, !has, th.muted);

        // --- now playing zone: cover + meta (content-sized, shrinks) then the like button
        let title = track.as_ref().map(|t| t.title.clone()).unwrap_or_else(|| "No track loaded".into());
        let subtitle = match &track {
            Some(t) => match &t.album {
                Some(a) => format!("{} · {}", t.artist, a),
                None => t.artist.clone(),
            },
            None => "Pick an album or track to play".into(),
        };
        let np = div()
            .flex_1()
            .min_w(px(dk::NP_MIN_W))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(dk::NP_GAP))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(dk::NP_GAP))
                    .min_w_0()
                    .flex_shrink()
                    .child(tip(div()
                            .id("dock-art")
                            .size(px(dk::ART))
                            .flex_none()
                            .border_1()
                            .border_color(th.border)
                            .bg(th.high)
                            .overflow_hidden()
                            .cursor(CursorStyle::PointingHand)
                            .when(!has, |d| d.flex().items_center().justify_center().text_color(th.muted).child(icon("equalizer", 18., th.muted)))
                            .on_hover(cx.listener(|this, h: &bool, _, cx| {
                                this.art_hover = *h;
                                cx.notify();
                            }))
                            .when(has, |d| d.child(cover)), "Now Playing (N)", th)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DockEvent::OpenNowPlaying))))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(// the title opens Now Playing; stretched across the meta column
                                tip(
                                    div()
                                        .id("dock-title")
                                        .flex()
                                        .min_w_0()
                                        .cursor(CursorStyle::PointingHand)
                                        .on_hover(cx.listener(|this, h: &bool, _, cx| {
                                            this.title_hover = *h;
                                            cx.notify();
                                        }))
                                        .child(txt(&th, BODY_LG, title).color(if self.title_hover || self.art_hover { th.accent_fg } else { th.text }).ellipsis()),
                                    "Now Playing (N)",
                                    th,
                                )
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(DockEvent::OpenNowPlaying))))
                            .child(div().flex().flex_row().min_w_0().child(match &track {
                                // artist link · album link
                                Some(t) => {
                                    let mut row = div().flex().flex_row().min_w_0().child(entity_link(&th, (LinkKind::Artist, t.artist_id.as_deref()), &t.artist, MONO_DATA, th.muted, "dock", window));
                                    if let Some(a) = &t.album {
                                        row = row.child(txt(&th, MONO_DATA, " · ").color(th.muted)).child(entity_link(&th, (LinkKind::Album, t.album_id.as_deref()), a, MONO_DATA, th.muted, "dock", window));
                                    }
                                    row.into_any_element()
                                }
                                None => txt(&th, MONO_DATA, subtitle).color(th.muted).ellipsis().into_any_element(),
                            })),
                    ),
            )
            .children(track.as_ref().map(|t| {
                let starred = t.starred.is_some();
                let (id, on) = (t.id.clone(), starred);
                ghost_icon("dock-like", th, if starred { "heart-filled" } else { "heart" }, dk::ICON_LIKE, dk::T_BTN, starred, false).tip(if starred { "Liked - click or press L to unlike" } else { "Like (L)" }, th).on_click(cx.listener(move |_, _, _, cx| {
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
                .size(px(dk::PLAY_BTN))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .bg(th.accent)
                .text_color(th.accent_text)
                .shadow(hard_shadow(&th, dk::PLAY_SHADOW))
                .when(has, |d| d.cursor(CursorStyle::PointingHand).hover(move |s| s.bg(th.accent_hover)))
                .when(!has, |d| d.opacity(0.5))
                .child(icon(if playing { "pause" } else { "play" }, dk::ICON_PLAY, th.accent_text))
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
        let basis = if vw < bp::COMPACT_BELOW { dk::TRANSPORT_BASIS_COMPACT } else { dk::TRANSPORT_BASIS };
        let time_cell = |s: String| div().w(px(dk::TIME_W)).flex_none().flex().justify_center().child(txt(&th, MONO_CODE, s).color(th.muted));
        let transport = div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(dk::TRANSPORT_GAP))
                .flex_shrink()
                .flex_basis(px(basis))
                .min_w(px(dk::TRANSPORT_MIN_W))
                .max_w(px(dk::TRANSPORT_MAX_W))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(dk::CONTROLS_GAP))
                        .child(t_btn("dock-shuffle", "shuffle", dk::ICON_SMALL, ps.shuffle).on_click(cx.listener(|this, _, _, cx| {
                            let on = !this.player.read(cx).state.shuffle;
                            app::call(cx, move |b| {
                                let _ = b.player_set_shuffle(on);
                            });
                        })))
                        .child(t_btn("dock-prev", "prev", dk::ICON_SKIP, false).on_click(cx.listener(|_, _, _, cx| {
                            app::call(cx, |b| {
                                let _ = b.player_previous();
                            })
                        })))
                        .child(play_btn)
                        .child(t_btn("dock-next", "next", dk::ICON_SKIP, false).on_click(cx.listener(|_, _, _, cx| {
                            app::call(cx, |b| {
                                let _ = b.player_next();
                            })
                        })))
                        .child(t_btn("dock-repeat", if ps.repeat == RepeatMode::One { "repeat-one" } else { "repeat" }, dk::ICON_SMALL, ps.repeat != RepeatMode::Off).on_click(cx.listener(
                            |this, _, _, cx| {
                                let next = match this.player.read(cx).state.repeat {
                                    RepeatMode::Off => RepeatMode::All,
                                    RepeatMode::All => RepeatMode::One,
                                    RepeatMode::One => RepeatMode::Off,
                                };
                                app::call(cx, move |b| {
                                    let _ = b.player_set_repeat(next);
                                });
                            },
                        ))),
                )
                .child(div()
                        .w_full()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(dk::SCRUB_GAP))
                        .child(time_cell(pos_label))
                        .child(div().flex_1().child(slider("dock-seek", th, &self.seek, seek_ratio, None, seek_cb)))
                        .child(time_cell(dur_label)));

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
            div().id("dock-out-err").tip(e.clone(), th).max_w(px(dk::WARN_MAX_W)).flex().min_w_0().child(txt(&th, MONO_CODE, e.clone()).color(th.clip).ellipsis()).into_any_element()
        } else if loading {
            txt(&th, MONO_CODE, "LOADING…").color(th.muted).into_any_element()
        } else {
            let full = vw >= bp::FULL_READOUT_FROM;
            let title = format!("{readout}{}", if bit_perfect { " · BIT-PERFECT" } else { "" });
            let text = if full { title.clone() } else { short };
            div().id("dock-out").tip(title, th).flex().min_w_0().child(txt(&th, MONO_CODE, text).color(if bit_perfect { th.accent_fg } else { th.muted }).ellipsis()).into_any_element()
        };
        let aux = div()
                .flex_1()
                .min_w(px(dk::AUX_MIN_W))
                .flex()
                .flex_row()
                .items_center()
                .justify_end()
                .gap(px(dk::AUX_GAP))
                .child(output)
                .child(div()
                        .id("dock-volume")
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(dk::VOLUME_GAP))
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
                                // double-click on the fader = 0 dB
                                if ev.click_count == 2 {
                                    set_volume(1.0, cx);
                                } else if vol_id > 0.0 {
                                    this.last_audible = vol_id;
                                }
                            }),
                        )
                        .child(icon("volume", dk::ICON_SMALL, th.muted))
                        .child(div().w(px(dk::VOLUME_FADER_W)).child(slider("dock-vol", th, &self.vol, ps.volume as f32, None, vol_cb)))
                        .child(div().min_w(px(dk::DB_MIN_W)).flex_none().flex().justify_end().child(txt(&th, MONO_CODE, vol_label).color(th.muted)))
                        .children(rg.map(|r| div().id("dock-rg").tip("ReplayGain applied to this track (dB)", th).flex_none().child(txt(&th, MONO_CODE, r).color(th.muted)))))
                .child({
                    let q = ps.queue_length;
                    tip(
                            div()
                                .id("dock-queue")
                                .relative()
                                .size(px(dk::T_BTN))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .group("hv")
                                .cursor(CursorStyle::PointingHand)
                                .child(icon_g("queue", dk::ICON_QUEUE, th.muted, th.accent_fg))
                                .children((q > 0).then(|| {
                                    div()
                                        .absolute()
                                        .top(px(dk::QCOUNT_TOP))
                                        .right(px(dk::QCOUNT_RIGHT))
                                        .px(px(dk::QCOUNT_PAD_X))
                                        .bg(th.base)
                                        .child(txt(&th, MONO_CODE, q.to_string()).color(th.accent_fg))
                                })),
                            "Toggle queue rail",
                            th,
                        )
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(DockEvent::ToggleQueue)))
                });

        div()
                .h(px(layout::DOCK_H))
                .w_full()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(dk::GAP))
                .px(px(dk::PAD_X))
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

#[allow(dead_code)]
fn _unused(_: SharedString) {}
