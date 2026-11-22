//! Context-menu contents of the library routes (album and artist menus; tracks use the shared `crate::menus` builder,
//! playlists the list routes' playlist menu), plus the album/selection playback helpers (rolling sources are
//! `crate::playback`).
use super::navigate;
use crate::app;
use crate::router::Route;
use crate::widgets::context_menu::MenuItem;
use gpui::{App, Pixels, Point, px};
use std::rc::Rc;
use asphonix_api::*;

// ---------------------------------------------------------------- playback
fn shuffled<T>(mut v: Vec<T>) -> Vec<T> {
    let mut s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1;
    for i in (1..v.len()).rev() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        v.swap(i, (s % (i as u64 + 1)) as usize);
    }
    v
}

/// Plays the album's tracks as a plain static queue.
pub fn play_album(cx: &App, id: String, shuffle: bool) {
    app::call(cx, move |b| {
        if let Ok(d) = b.browse_get_album(&id) {
            let ids: Vec<String> = d.tracks.into_iter().map(|t| t.id).collect();
            let ids = if shuffle { shuffled(ids) } else { ids };
            let _ = b.player_play_queue(PlayQueueRequest { track_ids: ids, start_index: 0 });
        }
    });
}

/// Plays an explicit selection as a static queue starting at `start`.
pub fn play_tracks(cx: &App, ids: Vec<String>, start: usize) {
    app::call(cx, move |b| {
        let _ = b.player_play_queue(PlayQueueRequest { track_ids: ids, start_index: start });
    });
}

pub fn play_artist(cx: &App, id: String, shuffle: bool) {
    crate::playback::play_artist(cx, id, shuffle, None);
}

pub fn play_genre(cx: &App, name: String, shuffle: bool) {
    crate::playback::play_genre(cx, name, shuffle, None);
}

/// Plays a playlist as the rolling `player_play_playlist` source.
pub fn play_playlist(cx: &App, id: String, shuffle: bool) {
    crate::playback::play_playlist(cx, id, shuffle, None);
}

pub fn enqueue(cx: &App, ids: Vec<String>, position: EnqueuePosition) {
    if ids.is_empty() {
        return;
    }
    app::call(cx, move |b| {
        let _ = b.player_enqueue(EnqueueRequest { track_ids: ids, position });
    });
}

pub fn toggle_star(cx: &App, id: String, kind: StarTargetType, starred: bool) {
    app::call(cx, move |b| {
        let req = StarRequest { id, kind };
        let _ = if starred { b.star_remove(req) } else { b.star_add(req) };
    });
}

fn album_ids(b: &dyn Backend, album: &str) -> Vec<String> {
    b.browse_get_album(album).map(|d| d.tracks.into_iter().map(|t| t.id).collect()).unwrap_or_default()
}

// ---------------------------------------------------------------- entities
/// The album context menu.
/// `at` = where the menu opened (the add-to-playlist popover anchors there).
pub fn album_menu(album: &Album, at: Point<Pixels>) -> Vec<MenuItem> {
    let id = album.id.clone();
    let starred = album.starred.is_some();
    let mut items = vec![
        MenuItem::action("Play", {
            let id = id.clone();
            move |_, cx| play_album(cx, id.clone(), false)
        })
        .icon("play"),
        MenuItem::action("Shuffle", {
            let id = id.clone();
            move |_, cx| play_album(cx, id.clone(), true)
        })
        .icon("shuffle"),
        MenuItem::action("Play next", {
            let id = id.clone();
            move |_, cx| {
                let id = id.clone();
                app::call(cx, move |b| {
                    let ids = album_ids(b, &id);
                    if !ids.is_empty() {
                        let _ = b.player_enqueue(EnqueueRequest { track_ids: ids, position: EnqueuePosition::Next });
                    }
                })
            }
        })
        .icon("next"),
        MenuItem::action("Add to queue", {
            let id = id.clone();
            move |_, cx| {
                let id = id.clone();
                app::call(cx, move |b| {
                    let ids = album_ids(b, &id);
                    if !ids.is_empty() {
                        let _ = b.player_enqueue(EnqueueRequest { track_ids: ids, position: EnqueuePosition::End });
                    }
                })
            }
        })
        .icon("queue"),
    ];
    {
        let id = id.clone();
        items.push(
            MenuItem::action("Add to playlist", move |_, cx| {
                let id = id.clone();
                let b = app::backend(cx);
                cx.spawn(async move |cx| {
                    let ids = cx.background_executor().spawn(async move { album_ids(&*b, &id) }).await;
                    let _ = cx.update(|cx| crate::overlays::add_to_playlist(cx, ids, gpui::point(at.x + px(220.), at.y)));
                })
                .detach();
            })
            .icon("add"),
        );
    }
    items.push(
        MenuItem::action(if starred { "Unstar" } else { "Star" }, {
            let id = id.clone();
            move |_, cx| toggle_star(cx, id.clone(), StarTargetType::Album, starred)
        })
        .icon(if starred { "star-filled" } else { "star" }),
    );
    items.push(MenuItem::submenu("Rate", crate::menus::rating_submenu(vec![id.clone()], StarTargetType::Album)).icon("star"));
    {
        let id = id.clone();
        items.push(MenuItem::action("Album radio", move |_, cx| crate::playback::radio(cx, RadioSeedKind::Album, id.clone())).icon("equalizer"));
    }
    if let Some(ar) = album.artist_id.clone() {
        items.push(MenuItem::action("Go to artist", move |_, cx| navigate(cx, Route::Artist(ar.clone()))).icon("artist"));
    }
    items
}

/// The artist context menu.
pub fn artist_menu(artist: &Artist) -> Vec<MenuItem> {
    let id = artist.id.clone();
    let starred = artist.starred.is_some();
    vec![
        MenuItem::action("Play all", {
            let id = id.clone();
            move |_, cx| play_artist(cx, id.clone(), false)
        })
        .icon("play"),
        MenuItem::action("Shuffle", {
            let id = id.clone();
            move |_, cx| play_artist(cx, id.clone(), true)
        })
        .icon("shuffle"),
        crate::flow::menu_entry(
            {
                let id = id.clone();
                Rc::new(move |arc, cx: &mut App| crate::playback::play_artist(cx, id.clone(), false, Some(arc)))
            },
            false,
        ),
        MenuItem::action("Artist radio", {
            let id = id.clone();
            move |_, cx| crate::playback::radio(cx, RadioSeedKind::Artist, id.clone())
        })
        .icon("equalizer"),
        MenuItem::action(if starred { "Unstar" } else { "Star" }, {
            let id = id.clone();
            move |_, cx| toggle_star(cx, id.clone(), StarTargetType::Artist, starred)
        })
        .icon(if starred { "star-filled" } else { "star" }),
    ]
}

/// The track context menu for a selection (the shared builder; "Play now" is the caller's context).
pub fn track_menu(tracks: &[Track], at: Point<Pixels>, on_play_now: impl Fn(&mut App) + 'static) -> Vec<MenuItem> {
    crate::menus::track_menu_items(crate::menus::TrackMenu::new(tracks.to_vec(), at, on_play_now))
}
