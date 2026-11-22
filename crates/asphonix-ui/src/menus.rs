//! The one place that assembles a track context menu (order, labels, icons and conditions). Every track list uses
//! it: Tracks, the queue rail, album/artist pages, playlists, Liked, smart playlists, History and Search.
use crate::app;
use crate::router::Route;
use crate::widgets::context_menu::MenuItem;
use gpui::{App, ClipboardItem, Pixels, Point, px};
use asphonix_api::*;
use asphonix_i18n::tr;
use std::rc::Rc;

/// A menu action callback.
pub type MenuAction = Rc<dyn Fn(&mut App)>;

/// Navigation callback of a menu.
pub type NavFn = dyn Fn(Route, &mut App);

pub struct TrackMenu {
    pub tracks: Vec<Track>,
    /// Where the menu opened (the add-to-playlist popover anchors here).
    pub at: Point<Pixels>,
    pub on_play_now: Rc<dyn Fn(&mut App)>,
    pub on_play_next: Option<MenuAction>,
    pub on_remove_from_queue: Option<MenuAction>,
    /// present only inside a playlist's own track list
    pub on_remove_from_playlist: Option<MenuAction>,
    pub on_navigate: Rc<NavFn>,
}

impl TrackMenu {
    /// The plain context (no queue / playlist entries), navigating through the shell.
    pub fn new(tracks: Vec<Track>, at: Point<Pixels>, on_play_now: impl Fn(&mut App) + 'static) -> Self {
        Self { tracks, at, on_play_now: Rc::new(on_play_now), on_play_next: None, on_remove_from_queue: None, on_remove_from_playlist: None, on_navigate: Rc::new(|r, cx| crate::views::library::navigate(cx, r)) }
    }
}

/// The rating submenu: ★ .. ★★★★★ (one entry per star count), rule, "Clear rating".
pub fn rating_submenu(ids: Vec<String>, kind: StarTargetType) -> Vec<MenuItem> {
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
        .map(|n| MenuItem::action("★".repeat(n as usize), apply(ids.clone(), n)))
        .collect();
    v.push(MenuItem::separator());
    v.push(MenuItem::action(tr!(MenuClearRating), apply(ids, 0)));
    v
}

pub fn track_menu_items(m: TrackMenu) -> Vec<MenuItem> {
    let ids: Vec<String> = m.tracks.iter().map(|t| t.id.clone()).collect();
    let n = m.tracks.len();
    let suffix = if n > 1 { format!(" ({})", tr!(CountTracks, count = n)) } else { String::new() };
    let single = if n == 1 { m.tracks.first().cloned() } else { None };
    let all_starred = m.tracks.iter().all(|t| t.starred.is_some());

    let play_now = m.on_play_now.clone();
    let mut items = vec![MenuItem::action(tr!(MenuPlayNow), move |_, cx| play_now(cx)).icon("play")];
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
            MenuItem::action(format!("{}{suffix}", tr!(MenuPlayNext)), move |_, cx| f(cx)).icon("next")
        }
        None => enqueue(format!("{}{suffix}", tr!(MenuPlayNext)), EnqueuePosition::Next, ids.clone(), "next"),
    });
    items.push(enqueue(format!("{}{suffix}", tr!(MenuAddToQueue)), EnqueuePosition::End, ids.clone(), "queue"));
    if let Some(t) = &single {
        let id = t.id.clone();
        items.push(
            MenuItem::action(tr!(MenuStartRadio), move |_, cx| {
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
        MenuItem::action(format!("{}{suffix}", tr!(MenuAddToPlaylist)), move |_, cx| {
            // the popover anchors at the menu's point: `top = y + 4`, `left = x` (right edge = x + 220)
            crate::overlays::add_to_playlist(cx, ids_p.clone(), gpui::point(at.x + px(220.), at.y));
        })
        .icon("add"),
    );
    let ids_l = ids.clone();
    items.push(
        MenuItem::action(format!("{}{suffix}", if all_starred { tr!(MenuUnlike) } else { tr!(MenuLike) }), move |_, cx| {
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
    items.push(MenuItem::submenu(format!("{}{suffix}", tr!(MenuRate)), rating_submenu(ids.clone(), StarTargetType::Track)).icon("star"));
    if let Some(t) = &single {
        if let Some(al) = t.album_id.clone() {
            let nav = m.on_navigate.clone();
            items.push(MenuItem::action(tr!(MenuGoToAlbum), move |_, cx| nav(Route::Album(al.clone()), cx)).icon("album"));
        }
        if let Some(ar) = t.artist_id.clone() {
            let nav = m.on_navigate.clone();
            items.push(MenuItem::action(tr!(MenuGoToArtist), move |_, cx| nav(Route::Artist(ar.clone()), cx)).icon("artist"));
        }
    }
    if let Some(f) = &m.on_remove_from_playlist {
        let f = f.clone();
        items.push(MenuItem::separator());
        items.push(MenuItem::action(format!("{}{suffix}", tr!(MenuRemoveFromPlaylist)), move |_, cx| f(cx)).icon("trash").danger());
    }
    if let Some(f) = &m.on_remove_from_queue {
        let f = f.clone();
        items.push(MenuItem::separator());
        items.push(MenuItem::action(format!("{}{suffix}", tr!(MenuRemoveFromQueue)), move |_, cx| f(cx)).icon("close").danger());
    }
    if let Some(t) = single {
        items.push(MenuItem::separator());
        let t0 = t.clone();
        items.push(MenuItem::action(tr!(MenuTrackInfo), move |_, cx| crate::overlays::track_info(cx, t0.clone())).icon("eye"));
        let text = format!("{} – {}", t.artist, t.title);
        items.push(MenuItem::action(tr!(MenuCopyArtistTitle), move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))));
    }
    items
}
