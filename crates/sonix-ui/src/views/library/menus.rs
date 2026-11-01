//! Context-menu contents of the library routes (`menus.ts`: `trackMenuItems`, `albumMenuItems`, `artistMenuItems`,
//! `playlistMenuItems` (play part), `flowMenuEntry`) wired to the `Backend`, plus the playback helpers of `playback.ts`.
use super::navigate;
use crate::app;
use crate::router::Route;
use crate::widgets::context_menu::MenuItem;
use gpui::{App, ClipboardItem};
use sonix_api::*;

// ---------------------------------------------------------------- playback.ts
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

/// `playAlbum`: the album's tracks as a plain static queue.
pub fn play_album(cx: &App, id: String, shuffle: bool) {
    app::call(cx, move |b| {
        if let Ok(d) = b.browse_get_album(&id) {
            let ids: Vec<String> = d.tracks.into_iter().map(|t| t.id).collect();
            let ids = if shuffle { shuffled(ids) } else { ids };
            let _ = b.player_play_queue(PlayQueueRequest { track_ids: ids, start_index: 0 });
        }
    });
}

/// `playTracks`: an explicit selection as a static queue starting at `start`.
pub fn play_tracks(cx: &App, ids: Vec<String>, start: usize) {
    app::call(cx, move |b| {
        let _ = b.player_play_queue(PlayQueueRequest { track_ids: ids, start_index: start });
    });
}

pub fn play_artist(cx: &App, id: String, shuffle: bool) {
    app::call(cx, move |b| {
        let _ = b.player_play_artist(PlayArtistRequest { id, shuffle, start_track_id: None });
    });
}

pub fn play_genre(cx: &App, name: String, shuffle: bool) {
    app::call(cx, move |b| {
        let _ = b.player_play_genre(PlayGenreRequest { name, shuffle, start_track_id: None });
    });
}

/// `playPlaylist` through the existing `playlists_get` + `player_play_queue` until the playlists port wires the
/// rolling-window source command. TODO(playlists-port): switch to `player_play_playlist`.
pub fn play_playlist(cx: &App, id: String, shuffle: bool) {
    app::call(cx, move |b| {
        if let Ok(d) = b.playlists_get(&id) {
            let ids: Vec<String> = d.tracks.into_iter().map(|t| t.id).collect();
            let ids = if shuffle { shuffled(ids) } else { ids };
            let _ = b.player_play_queue(PlayQueueRequest { track_ids: ids, start_index: 0 });
        }
    });
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

// ---------------------------------------------------------------- shared entries
/// `flowMenuEntry`: "Flow >" with Steady / Build / Peak. Flow sequencing is not ported (docs/PORTING.md), the entries are inert.
pub fn flow_entry(disabled: bool) -> MenuItem {
    let arcs = [("Steady", "Smooth transitions, even energy"), ("Build", "Energy rises over each ~40 tracks"), ("Peak", "Rise, then cool down")];
    MenuItem::submenu("Flow", arcs.iter().map(|(l, h)| MenuItem::action(*l, |_, _| {}).hint(*h)).collect()).icon("sliders").disabled(disabled)
}

/// `ratingSubmenu`: ★ .. ★★★★★ (1★ is the danger entry), separator, "Clear rating".
fn rating_submenu(ids: Vec<String>, kind: StarTargetType) -> Vec<MenuItem> {
    let apply = |ids: Vec<String>, value: u8| {
        move |_: &mut gpui::Window, cx: &mut App| {
            let ids = ids.clone();
            app::call(cx, move |b| {
                for id in ids {
                    let _ = b.rating_set(SetRatingRequest { id, kind, rating: value });
                }
            });
        }
    };
    let mut v: Vec<MenuItem> = (1..=5u8)
        .map(|n| {
            let it = MenuItem::action("★".repeat(n as usize), apply(ids.clone(), n));
            if n == 1 { it.danger().hint("Purges from library on NAS nightly.") } else { it }
        })
        .collect();
    v.push(MenuItem::separator());
    v.push(MenuItem::action("Clear rating", apply(ids, 0)));
    v
}

fn add_to_playlist(label: String, playlists: &[Playlist], ids: impl Fn(&dyn Backend) -> Vec<String> + Clone + Send + 'static) -> MenuItem {
    let entries: Vec<MenuItem> = playlists
        .iter()
        .filter(|p| !p.read_only)
        .map(|p| {
            let (pid, ids) = (p.id.clone(), ids.clone());
            MenuItem::action(p.name.clone(), move |_, cx| {
                let (pid, ids) = (pid.clone(), ids.clone());
                app::call(cx, move |b| {
                    let track_ids = ids(b);
                    let _ = b.playlists_add_tracks(AddToPlaylistRequest { id: pid, track_ids });
                });
            })
        })
        .collect();
    MenuItem::submenu(label, entries).icon("add")
}

// ---------------------------------------------------------------- entities
/// `albumMenuItems`.
pub fn album_menu(album: &Album, playlists: &[Playlist]) -> Vec<MenuItem> {
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
    // The web opens the AddToPlaylist popover here; the native menu lists the writable playlists as a submenu.
    {
        let id = id.clone();
        items.push(add_to_playlist("Add to playlist".into(), playlists, move |b| album_ids(b, &id)));
    }
    items.push(
        MenuItem::action(if starred { "Unstar" } else { "Star" }, {
            let id = id.clone();
            move |_, cx| toggle_star(cx, id.clone(), StarTargetType::Album, starred)
        })
        .icon(if starred { "star-filled" } else { "star" }),
    );
    items.push(MenuItem::submenu("Rate", rating_submenu(vec![id.clone()], StarTargetType::Album)).icon("star"));
    // `radio_start` is not part of the Backend contract yet: present for structural parity, inert.
    items.push(MenuItem::action("Album radio", |_, _| {}).icon("equalizer"));
    if let Some(ar) = album.artist_id.clone() {
        items.push(MenuItem::action("Go to artist", move |_, cx| navigate(cx, Route::Artist(ar.clone()))).icon("artist"));
    }
    items
}

/// `artistMenuItems`.
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
        flow_entry(false),
        MenuItem::action("Artist radio", |_, _| {}).icon("equalizer"),
        MenuItem::action(if starred { "Unstar" } else { "Star" }, {
            let id = id.clone();
            move |_, cx| toggle_star(cx, id.clone(), StarTargetType::Artist, starred)
        })
        .icon(if starred { "star-filled" } else { "star" }),
    ]
}

/// `playlistMenuItems` (the entries that exist without the dialogs: play / shuffle / flow / queue; rename, duplicate and
/// delete need the Dialog component of the dialogs port and are listed inert). TODO(dialogs-port).
pub fn playlist_menu(p: &Playlist) -> Vec<MenuItem> {
    let empty = p.song_count == 0;
    let id = p.id.clone();
    let ids_of = |id: String| move |b: &dyn Backend| b.playlists_get(&id).map(|d| d.tracks.into_iter().map(|t| t.id).collect::<Vec<_>>()).unwrap_or_default();
    let enq = |label: &'static str, icon: &'static str, pos: EnqueuePosition, id: String| {
        MenuItem::action(label, move |_, cx| {
            let f = ids_of(id.clone());
            app::call(cx, move |b| {
                let ids = f(b);
                if !ids.is_empty() {
                    let _ = b.player_enqueue(EnqueueRequest { track_ids: ids, position: pos });
                }
            })
        })
        .icon(icon)
        .disabled(empty)
    };
    vec![
        MenuItem::action("Play", {
            let id = id.clone();
            move |_, cx| play_playlist(cx, id.clone(), false)
        })
        .icon("play")
        .disabled(empty),
        MenuItem::action("Shuffle", {
            let id = id.clone();
            move |_, cx| play_playlist(cx, id.clone(), true)
        })
        .icon("shuffle")
        .disabled(empty),
        flow_entry(empty),
        enq("Play next", "next", EnqueuePosition::Next, id.clone()),
        enq("Add to queue", "queue", EnqueuePosition::End, id.clone()),
        MenuItem::separator(),
        read_only_hint(MenuItem::action("Rename", |_, _| {}).icon("edit").disabled(p.read_only), p.read_only),
        MenuItem::action("Duplicate", |_, _| {}).icon("add"),
        read_only_hint(MenuItem::action("Delete", |_, _| {}).icon("trash").danger().disabled(p.read_only), p.read_only),
    ]
}

fn read_only_hint(it: MenuItem, read_only: bool) -> MenuItem {
    if read_only { it.hint("Read-only playlist.") } else { it }
}

/// `trackMenuItems` for a selection (same order/labels as the Tracks route's menu; "Play now" is the caller's context).
pub fn track_menu(tracks: &[Track], playlists: &[Playlist], on_play_now: impl Fn(&mut App) + 'static) -> Vec<MenuItem> {
    let n = tracks.len();
    let suffix = if n > 1 { format!(" ({n} tracks)") } else { String::new() };
    let ids: Vec<String> = tracks.iter().map(|t| t.id.clone()).collect();
    let all_starred = tracks.iter().all(|t| t.starred.is_some());
    let enq = |label: String, pos: EnqueuePosition, icon: &'static str| {
        let ids = ids.clone();
        MenuItem::action(label, move |_, cx| enqueue(cx, ids.clone(), pos)).icon(icon)
    };
    let mut items = vec![MenuItem::action("Play now", move |_, cx| on_play_now(cx)).icon("play"), enq(format!("Play next{suffix}"), EnqueuePosition::Next, "next"), enq(format!("Add to queue{suffix}"), EnqueuePosition::End, "queue")];
    if n == 1 {
        items.push(MenuItem::action("Start radio", |_, _| {}).icon("equalizer"));
    }
    {
        let ids = ids.clone();
        items.push(add_to_playlist(format!("Add to playlist{suffix}"), playlists, move |_| ids.clone()));
    }
    items.push(
        MenuItem::action(format!("{}{suffix}", if all_starred { "Unlike" } else { "Like" }), {
            let ids = ids.clone();
            move |_, cx| {
                for id in ids.clone() {
                    toggle_star(cx, id, StarTargetType::Track, all_starred);
                }
            }
        })
        .icon(if all_starred { "heart-filled" } else { "heart" }),
    );
    items.push(MenuItem::submenu(format!("Rate{suffix}"), rating_submenu(ids.clone(), StarTargetType::Track)).icon("star"));
    if n == 1 {
        let t = &tracks[0];
        if let Some(al) = t.album_id.clone() {
            items.push(MenuItem::action("Go to album", move |_, cx| navigate(cx, Route::Album(al.clone()))).icon("album"));
        }
        if let Some(ar) = t.artist_id.clone() {
            items.push(MenuItem::action("Go to artist", move |_, cx| navigate(cx, Route::Artist(ar.clone()))).icon("artist"));
        }
        items.push(MenuItem::separator());
        items.push(MenuItem::action("Track info", |_, _| {}).icon("eye")); // TODO(dialogs-port): TrackInfoDialog
        let text = format!("{} – {}", t.artist, t.title);
        items.push(MenuItem::action("Copy \"Artist – Title\"", move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))));
    }
    items
}
