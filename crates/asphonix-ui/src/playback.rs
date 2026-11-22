//! What every Play / Shuffle / Flow / radio button and menu entry calls. Rolling sources
//! (playlist, artist, genre, Liked, smart) go through their `player_play_*` command with `shuffle` and `flow`.
use crate::app;
use asphonix_api::*;

pub fn play_playlist(cx: &gpui::App, id: String, shuffle: bool, flow: Option<FlowArc>) {
    app::call(cx, move |b| {
        let _ = b.player_play_playlist(PlaySourceRequest { id, shuffle, flow, ..Default::default() });
    });
}

pub fn play_artist(cx: &gpui::App, id: String, shuffle: bool, flow: Option<FlowArc>) {
    app::call(cx, move |b| {
        let _ = b.player_play_artist(PlayArtistRequest { id, shuffle, start_track_id: None, flow });
    });
}

pub fn play_genre(cx: &gpui::App, name: String, shuffle: bool, flow: Option<FlowArc>) {
    app::call(cx, move |b| {
        let _ = b.player_play_genre(PlayGenreRequest { name, shuffle, start_track_id: None, flow });
    });
}

pub fn play_liked(cx: &gpui::App, shuffle: bool, flow: Option<FlowArc>) {
    app::call(cx, move |b| {
        let _ = b.player_play_liked(PlaySourceRequest { shuffle, flow, ..Default::default() });
    });
}

pub fn play_smart(cx: &gpui::App, id: String, shuffle: bool, seed: u32, flow: Option<FlowArc>) {
    app::call(cx, move |b| {
        let _ = b.player_play_smart(PlaySourceRequest { id, shuffle, seed, flow, ..Default::default() });
    });
}

/// Starts a radio seeded from a track, album or artist.
pub fn radio(cx: &gpui::App, kind: RadioSeedKind, id: String) {
    app::call(cx, move |b| {
        let _ = b.radio_start(RadioStartRequest { seed: RadioSeed { kind, id } });
    });
}

/// Click on the play button of the source that is already loaded: pause / resume it (`player_play_pause`, the dock's toggle).
pub fn toggle(cx: &gpui::App) {
    app::call(cx, |b| {
        let _ = b.player_play_pause();
    });
}
