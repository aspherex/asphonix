//! `lib/menus.ts trackMenuItems`: the one place that assembles a track context menu (same order, labels, icons and
//! conditions as the web). Used by the queue rail; the Tracks route builds the same list in `views/tracks.rs`.
//!
use crate::app;
use crate::router::Route;
use crate::widgets::context_menu::MenuItem;
use gpui::{App, ClipboardItem, Pixels, Point, px};
use sonix_api::*;
use std::rc::Rc;

/// A menu action callback.
pub type MenuAction = Rc<dyn Fn(&mut App)>;

/// Navigation callback of a menu.
pub type NavFn = dyn Fn(Route, &mut App);

pub struct TrackMenu {
    pub tracks: Vec<Track>,
    /// Where the menu opened (the add-to-playlist popover anchors here, `anchorPoint()`).
    pub at: Point<Pixels>,
    pub on_play_now: Rc<dyn Fn(&mut App)>,
    pub on_play_next: Option<MenuAction>,
    pub on_remove_from_queue: Option<MenuAction>,
    pub on_navigate: Rc<NavFn>,
}

pub fn track_menu_items(m: TrackMenu) -> Vec<MenuItem> {
    let ids: Vec<String> = m.tracks.iter().map(|t| t.id.clone()).collect();
    let n = m.tracks.len();
    let suffix = if n > 1 { format!(" ({n} tracks)") } else { String::new() };
    let single = if n == 1 { m.tracks.first().cloned() } else { None };
    let all_starred = m.tracks.iter().all(|t| t.starred.is_some());

    let play_now = m.on_play_now.clone();
    let mut items = vec![MenuItem::action("Play now", move |_, cx| play_now(cx)).icon("play")];
    let enqueue = |label: String, pos: EnqueuePosition, ids: Vec<String>, icon: &'static str| {
        MenuItem::action(label, move |_, cx| {
            let ids = ids.clone();
            app::call(cx, move |b| {
                let _ = b.player_enqueue(EnqueueRequest { track_ids: ids, position: pos });
            });
        })
        .icon(icon)
    };
    items.push(match &m.on_play_next {
        Some(f) => {
            let f = f.clone();
            MenuItem::action(format!("Play next{suffix}"), move |_, cx| f(cx)).icon("next")
        }
        None => enqueue(format!("Play next{suffix}"), EnqueuePosition::Next, ids.clone(), "next"),
    });
    items.push(enqueue(format!("Add to queue{suffix}"), EnqueuePosition::End, ids.clone(), "queue"));
    if let Some(t) = &single {
        let id = t.id.clone();
        items.push(
            MenuItem::action("Start radio", move |_, cx| {
                let id = id.clone();
                app::call(cx, move |b| {
                    let _ = b.radio_start(RadioStartRequest { seed: RadioSeed { kind: RadioSeedKind::Track, id } });
                });
            })
            .icon("equalizer"),
        );
    }
    let (ids_p, at) = (ids.clone(), m.at);
    items.push(
        MenuItem::action(format!("Add to playlist{suffix}"), move |_, cx| {
            // the popover anchors at the menu's point: `top = y + 4`, `left = x` (right edge = x + 220)
            crate::overlays::add_to_playlist(cx, ids_p.clone(), gpui::point(at.x + px(220.), at.y));
        })
        .icon("add"),
    );
    let ids_l = ids.clone();
    items.push(
        MenuItem::action(format!("{}{suffix}", if all_starred { "Unlike" } else { "Like" }), move |_, cx| {
            let ids = ids_l.clone();
            app::call(cx, move |b| {
                for id in ids {
                    let req = StarRequest { id, kind: StarTargetType::Track };
                    let _ = if all_starred { b.star_remove(req) } else { b.star_add(req) };
                }
            });
        })
        .icon(if all_starred { "heart-filled" } else { "heart" }),
    );
    let rate = |stars: u8, ids: Vec<String>| {
        let label = if stars == 0 { "Clear rating".to_string() } else { format!("{stars}★") };
        let mut it = MenuItem::action(label, move |_, cx| {
            let ids = ids.clone();
            app::call(cx, move |b| {
                for id in ids {
                    let _ = b.rating_set(SetRatingRequest { id, kind: StarTargetType::Track, rating: stars });
                }
            });
        });
        if stars == 1 {
            it = it.danger().hint("1★ tracks are purged by the NAS nightly.");
        }
        it
    };
    items.push(MenuItem::submenu(format!("Rate{suffix}"), vec![rate(0, ids.clone()), rate(5, ids.clone()), rate(4, ids.clone()), rate(3, ids.clone()), rate(2, ids.clone()), rate(1, ids.clone())]).icon("star"));
    if let Some(t) = &single {
        if let Some(al) = t.album_id.clone() {
            let nav = m.on_navigate.clone();
            items.push(MenuItem::action("Go to album", move |_, cx| nav(Route::Album(al.clone()), cx)).icon("album"));
        }
        if let Some(ar) = t.artist_id.clone() {
            let nav = m.on_navigate.clone();
            items.push(MenuItem::action("Go to artist", move |_, cx| nav(Route::Artist(ar.clone()), cx)).icon("artist"));
        }
    }
    if let Some(f) = &m.on_remove_from_queue {
        let f = f.clone();
        items.push(MenuItem::separator());
        items.push(MenuItem::action(format!("Remove from queue{suffix}"), move |_, cx| f(cx)).icon("close").danger());
    }
    if let Some(t) = single {
        items.push(MenuItem::separator());
        let t0 = t.clone();
        items.push(MenuItem::action("Track info", move |_, cx| crate::overlays::track_info(cx, t0.clone())).icon("eye"));
        let text = format!("{} – {}", t.artist, t.title);
        items.push(MenuItem::action("Copy \"Artist – Title\"", move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))));
    }
    items
}
