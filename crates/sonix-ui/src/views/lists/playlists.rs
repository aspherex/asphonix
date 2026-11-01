//! `Playlists.svelte`: smart (local) and server playlists as a card grid.
use super::menus;
use super::modal;
use super::smart_rules::describe_smart;
use super::ui::scroll_page;
use super::ListsEvent;
use crate::anim;
use crate::app;
use crate::art::{self, ArtHandle};
use crate::metrics::{lists as lm, space};
use crate::probe::pid;
use crate::router::Route;
use crate::text::txt;
use crate::theme::Theme;
use crate::tracking::{BODY_LG, BODY_SM, HEADLINE_MD, LABEL_CAPS, MONO_CODE, MONO_DATA};
use crate::widgets::context_menu::{ContextMenu, MenuItem};
use crate::widgets::ui::{Tone, badge};
use crate::widgets::{hard_shadow, icon, icon_g};
use gpui::{AnyElement, ClickEvent, Context, CursorStyle, Entity, EventEmitter, IntoElement, MouseButton, MouseDownEvent, Render, ScrollHandle, Window, div, prelude::*, px};
use sonix_api::format as fmt;
use sonix_api::*;
use std::sync::Arc;

impl EventEmitter<ListsEvent> for PlaylistsView {}

pub struct PlaylistsView {
    backend: Arc<dyn Backend>,
    art: ArtHandle,
    menu: Entity<ContextMenu>,
    lists: Vec<Playlist>,
    smart: Vec<SmartSummary>,
    loaded: bool,
    hover: Option<(bool, usize)>,
    scroll: ScrollHandle,
}

impl PlaylistsView {
    pub fn new(backend: Arc<dyn Backend>, art: ArtHandle, menu: &Entity<ContextMenu>, cx: &mut Context<Self>) -> Self {
        let mut this = Self { backend, art, menu: menu.clone(), lists: Vec::new(), smart: Vec::new(), loaded: false, hover: None, scroll: ScrollHandle::new() };
        this.reload(cx);
        this
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let b = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let (pl, sm) = cx.background_executor().spawn(async move { (b.playlists_list(), b.smart_list()) }).await;
            let _ = this.update(cx, |t, cx| {
                t.lists = pl.unwrap_or_default();
                t.smart = sm.unwrap_or_default();
                t.loaded = true;
                cx.notify();
            });
        })
        .detach();
    }

    pub fn release(&mut self) {}

    #[cfg(feature = "parity")]
    pub(crate) fn parity_state(&self) -> serde_json::Value {
        serde_json::json!({ "playlists": self.lists.len(), "smart": self.smart.len(), "hover": self.hover.map(|h| format!("{}{}", if h.0 { "s" } else { "p" }, h.1)) })
    }

    fn show_menu(&self, items: Vec<MenuItem>, pos: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let menu = self.menu.clone();
        cx.defer(move |cx| {
            let Some(w) = cx.active_window() else { return };
            let _ = w.update(cx, |_, window, cx| menu.update(cx, |m, cx| m.show(items, pos, window, cx)));
        });
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        modal::prompt(cx, "New playlist", "", "Playlist name", "Create", move |name, _, cx| {
            let Some(name) = name else { return };
            let b = app::backend(cx);
            cx.spawn(async move |cx| {
                let r = cx.background_executor().spawn(async move { b.playlists_create(CreatePlaylistRequest { name, track_ids: vec![] }) }).await;
                if let (Some(t), Ok(p)) = (this.upgrade(), r) {
                    let _ = t.update(cx, |_, cx| {
                        cx.emit(ListsEvent::PlaylistsChanged);
                        cx.emit(ListsEvent::Navigate(Route::Playlist(p.id)));
                    });
                }
            })
            .detach();
        });
    }

    fn card(&mut self, th: Theme, smart: bool, ix: usize, cw: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let hovered = self.hover == Some((smart, ix));
        let pfx = if smart { format!("pl.smart.{ix}") } else { format!("pl.card.{ix}") };
        let actions_w = if smart { 0.0 } else if self.lists[ix].read_only { lm::CARD_ICON_BTN } else { 3.0 * lm::CARD_ICON_BTN + 2.0 * lm::CARD_ACTIONS_GAP };
        let meta_w = cw - 2.0 - 2.0 * lm::CARD_PAD - lm::CARD_ART - lm::CARD_GAP - if smart { 0.0 } else { actions_w + lm::CARD_GAP };
        let key = ix + if smart { 10_000 } else { 0 };
        let t = anim::transition(window, ("pl-overlay", key), if hovered { 1.0 } else { 0.0 }, 0.1);
        let (title, sub, id, art_el, rules): (String, String, String, AnyElement, Option<String>);
        let mut badges: Option<gpui::Div> = None;
        let mut pl_for_menu: Option<Playlist> = None;
        let mut sm_for_menu: Option<SmartSummary> = None;
        if smart {
            let sp = self.smart[ix].clone();
            title = sp.input.name.clone();
            sub = format!("{} TRACKS · {}", fmt::fmt_int(sp.track_count as u64), fmt::format_total_duration(sp.duration));
            rules = Some(describe_smart(&sp.input));
            id = sp.id.clone();
            art_el = icon("bolt", 20., th.accent_fg).into_any_element();
            sm_for_menu = Some(sp);
        } else {
            let pl = self.lists[ix].clone();
            title = pl.name.clone();
            sub = format!("{} TRACKS · {}", pl.song_count, fmt::format_total_duration(pl.duration));
            rules = None;
            id = pl.id.clone();
            art_el = if pl.song_count > 0 { art::cover(&self.art, &self.backend, pl.cover_art.as_deref(), lm::CARD_ART, window, cx) } else { icon("queue", 20., th.muted).into_any_element() };
            if pl.read_only {
                badges = Some(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(space::XS))
                        .mt(px(2.))
                        .children(pl.smart.then(|| badge(&th, Tone::Accent, "Smart")))
                        .child(badge(&th, Tone::Default, "Read-only")),
                );
            }
            pl_for_menu = Some(pl);
        }
        let overlay = {
            let (id2, sm) = (id.clone(), smart);
            div()
                .id(("pl-play", key))
                .absolute()
                .right(px(lm::OVERLAY_INSET))
                .bottom(px(lm::OVERLAY_INSET - (1.0 - t) * 3.0))
                .size(px(lm::OVERLAY))
                .flex()
                .items_center()
                .justify_center()
                .bg(th.accent)
                .shadow(hard_shadow(&th, 2.0))
                .opacity(t)
                .child(icon("play", lm::OVERLAY_ICON, th.accent_text))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    let id = id2.clone();
                    app::call(cx, move |b| {
                        let req = PlaySourceRequest { id, ..Default::default() };
                        let _ = if sm { b.player_play_smart(req) } else { b.player_play_playlist(req) };
                    });
                })
        };
        let art_box = div()
            .relative()
            .size(px(lm::CARD_ART))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .bg(th.high)
            .text_color(th.muted)
            .when(smart, |d| d.border_1().border_color(th.border))
            .child(art_el)
            .child(overlay);
        let mut meta = div()
            .flex()
            .flex_col()
            .min_w_0()
            .flex_1()
            .child(div().flex().child(txt(&th, BODY_LG, title).color(th.text).ellipsis()))
            .child(div().flex().flex_col().children(wrap_lines(window, &sub, MONO_DATA, meta_w).into_iter().map(|l| txt(&th, MONO_DATA, l).color(th.muted))));
        if let Some(r) = rules {
            meta = meta.child(div().flex().min_w_0().child(txt(&th, MONO_CODE, r).color(th.muted).ellipsis()));
        }
        if let Some(b) = badges {
            meta = meta.child(b);
        }
        let mut card = div()
            .id(("card", key))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(lm::CARD_GAP))
            .w(px(cw))
            .flex_none()
            .p(px(lm::CARD_PAD))
            .bg(th.low)
            .border_1()
            .border_color(if hovered { th.focus } else { th.border })
            .cursor(CursorStyle::PointingHand)
            .child(pid(format!("{pfx}.art"), art_box))
            .child(pid(format!("{pfx}.meta"), meta));
        if !smart {
            let pl = pl_for_menu.clone().unwrap();
            let btn = |name: &'static str, bid: &'static str| {
                div()
                    .id((bid, key))
                    .group("hv")
                    .size(px(lm::CARD_ICON_BTN))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(th.muted)
                    .hover(move |s| s.text_color(th.accent_fg))
                    .child(icon_g(name, lm::CARD_ICON, th.muted, th.accent_fg))
            };
            let mut actions = div().flex().flex_row().gap(px(lm::CARD_ACTIONS_GAP)).flex_none();
            if !pl.read_only {
                let p2 = pl.clone();
                actions = actions.child(btn("edit", "pl-rename").on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    let (id, name, me) = (p2.id.clone(), p2.name.clone(), cx.entity().downgrade());
                    let _ = this;
                    let name0 = name.clone();
                    modal::prompt(cx, "Rename playlist", &name0, "", "Rename", move |n, _, cx| {
                        let Some(n) = n.filter(|n| *n != name) else { return };
                        let id2 = id.clone();
                        app::call(cx, move |b| {
                            let _ = b.playlists_rename(RenamePlaylistRequest { id: id2, name: n });
                        });
                        if let Some(t) = me.upgrade() {
                            t.update(cx, |_, cx| cx.emit(ListsEvent::PlaylistsChanged));
                        }
                    });
                })));
                let p3 = pl.clone();
                actions = actions.child(btn("trash", "pl-delete").on_click(cx.listener(move |_, _, _, cx| {
                    cx.stop_propagation();
                    let (id, name, me) = (p3.id.clone(), p3.name.clone(), cx.entity().downgrade());
                    modal::confirm(cx, "Delete playlist", &format!("Delete \"{name}\"? This cannot be undone."), "Delete", true, move |ok, _, cx| {
                        if ok.is_none() {
                            return;
                        }
                        let id2 = id.clone();
                        app::call(cx, move |b| {
                            let _ = b.playlists_delete(DeletePlaylistRequest { id: id2 });
                        });
                        if let Some(t) = me.upgrade() {
                            t.update(cx, |_, cx| cx.emit(ListsEvent::PlaylistsChanged));
                        }
                    });
                })));
            }
            let p4 = pl.clone();
            actions = actions.child(btn("more", "pl-more").on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                cx.stop_propagation();
                let r = ev.position();
                let items = menus::playlist_menu_items(cx.entity().downgrade(), &p4, false);
                this.show_menu(items, gpui::point(r.x, r.y + px(8.)), cx);
            })));
            card = card.child(pid(format!("{pfx}.actions"), actions));
            let p5 = pl.clone();
            card = card.on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    let items = menus::playlist_menu_items(cx.entity().downgrade(), &p5, false);
                    this.show_menu(items, ev.position, cx);
                    cx.stop_propagation();
                }),
            );
            let target = Route::Playlist(id);
            card = card.on_click(cx.listener(move |_, _, _, cx| cx.emit(ListsEvent::Navigate(target.clone()))));
        } else {
            let sp = sm_for_menu.clone().unwrap();
            card = card.on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    let items = menus::smart_menu_items(cx.entity().downgrade(), &sp, 0, false);
                    this.show_menu(items, ev.position, cx);
                    cx.stop_propagation();
                }),
            );
            let target = Route::Smart(id);
            card = card.on_click(cx.listener(move |_, _, _, cx| cx.emit(ListsEvent::Navigate(target.clone()))));
        }
        pid(pfx.clone(), card.on_hover(cx.listener(move |this, h: &bool, _, cx| {
            if *h {
                this.hover = Some((smart, ix));
            } else if this.hover == Some((smart, ix)) {
                this.hover = None;
            }
            cx.notify();
        })))
        .into_any_element()
    }
}

/// Greedy word wrap of a single  line into  px (what a block box does to its text).
fn wrap_lines(window: &Window, text: &str, spec: crate::tracking::TypeSpec, width: f32) -> Vec<String> {
    let fam = crate::theme::MONO;
    let w = |t: &str| crate::text::natural_width(window, t, fam, spec.size, spec.weight, spec.tracking_px());
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split(' ') {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if !cur.is_empty() && w(&cand) > width {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
        } else {
            cur = cand;
        }
    }
    lines.push(cur);
    lines
}

impl Render for PlaylistsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = app::theme(cx);
        let handle = self.scroll.clone();
        scroll_page(th, "playlists-scroll", &handle, |overflow| {
            let w = super::ui::page_content_width(window, overflow);
            let n = (((w + lm::GRID_GAP) / (lm::GRID_MIN + lm::GRID_GAP)).floor() as usize).max(1);
            let cw = (w - lm::GRID_GAP * (n as f32 - 1.0)) / n as f32;
            let new_btn = |id: &'static str, label: &'static str, icon_name: &'static str, alt: bool| {
                let (bg, fg) = if alt { (th.mid, th.accent_fg) } else { (th.accent, th.accent_text) };
                div()
                    .id(id)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(lm::NEW_GAP))
                    .px(px(lm::NEW_PAD_X))
                    .py(px(lm::NEW_PAD_Y))
                    .bg(bg)
                    .text_color(fg)
                    .cursor(CursorStyle::PointingHand)
                    .when(!alt, |d| d.shadow(hard_shadow(&th, 2.0)).hover(move |s| s.bg(th.accent_hover)))
                    .when(alt, |d| d.border_1().border_color(th.border).hover(move |s| s.border_color(th.focus).bg(th.high)))
                    .child(icon(icon_name, lm::NEW_ICON, fg))
                    .child(txt(&th, LABEL_CAPS, label).color(fg))
            };
            let header = div().flex().flex_row().items_center().justify_between().child(pid("page.title", txt(&th, HEADLINE_MD, "Playlists // Vectors").color(th.text))).child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(space::SM))
                    .child(pid("pls.btn.smart", new_btn("new-smart", "New smart playlist", "bolt", true).on_click(cx.listener(|_, _, _, cx| cx.emit(ListsEvent::EditSmart(None))))))
                    .child(pid("pls.btn.new", new_btn("new-pl", "New playlist", "add", false).on_click(cx.listener(|this, _, _, cx| this.create(cx))))),
            );
            let mut page = div().flex().flex_col().gap(px(lm::PAGE_GAP)).p(px(lm::PAGE_PAD)).w_full().child(header);
            let grid = |cards: Vec<AnyElement>| div().flex().flex_row().flex_wrap().gap(px(lm::GRID_GAP)).children(cards);
            if !self.smart.is_empty() {
                page = page.child(txt(&th, LABEL_CAPS, "Smart playlists · local, evaluated on this machine").color(th.muted));
                let cards = (0..self.smart.len()).map(|i| self.card(th, true, i, cw, window, cx)).collect();
                page = page.child(grid(cards));
                page = page.child(txt(&th, LABEL_CAPS, "Server playlists").color(th.muted));
            }
            if self.loaded && self.lists.is_empty() {
                page = page.child(txt(&th, BODY_SM, "No playlists yet.").color(th.muted));
            } else {
                let cards = (0..self.lists.len()).map(|i| self.card(th, false, i, cw, window, cx)).collect();
                page = page.child(grid(cards));
            }
            pid("page", page).into_any_element()
        })
    }
}
