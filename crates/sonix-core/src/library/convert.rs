//! Conversions from Subsonic wire types (`subsonic::Raw*`) to the IPC types
//! returned to the frontend (`ipc::Track`/`Album`/`Artist`/`Playlist`).
//! Centralized here so every query path (browse, search, album/artist
//! detail, playlist detail) produces identically-shaped objects.
//!
//! Only `playlist_from_raw` is wired into a live call path today (playlist
//! write-through needs to turn the server's fresh response straight into an
//! `ipc::Playlist` without a round trip through SQLite); the track/album/
//! artist converters exist for completeness and any future on-demand path
//! that needs an `ipc::*` type straight from a live Subsonic response
//! instead of the local mirror.
#![allow(dead_code)]

use crate::ipc::{Album, Artist, Playlist, ReplayGain, Track, TrackFormat};
use crate::subsonic::{RawAlbum, RawArtist, RawPlaylist, RawReplayGain, RawSong};

/// `0` is Subsonic's way of saying "not applicable" for `bitDepth` on lossy
/// formats — treat it as absent rather than a literal 0-bit sample.
fn non_zero_u8(v: Option<u32>) -> Option<u8> {
    v.filter(|&v| v > 0).map(|v| v.min(u32::from(u8::MAX)) as u8)
}

/// `0`/absent `userRating` both mean "unrated" — never render a 0-star row.
fn rating_from_raw(v: Option<u32>) -> Option<u8> {
    v.filter(|&v| v > 0).map(|v| v.min(5) as u8)
}

pub fn track_format_from_raw(song: &RawSong) -> TrackFormat {
    TrackFormat {
        suffix: song.suffix.clone(),
        bit_rate: song.bit_rate.filter(|&v| v > 0),
        bit_depth: non_zero_u8(song.bit_depth),
        sampling_rate: song.sampling_rate.unwrap_or(0), // 0 = unknown, never a guessed rate
        channel_count: song.channel_count.unwrap_or(0).min(255) as u8,
    }
}

pub fn replay_gain_from_raw(rg: &RawReplayGain) -> Option<ReplayGain> {
    if rg.track_gain.is_none() && rg.album_gain.is_none() && rg.track_peak.is_none() && rg.album_peak.is_none() {
        return None;
    }
    Some(ReplayGain {
        track_gain_db: rg.track_gain,
        album_gain_db: rg.album_gain,
        track_peak: rg.track_peak,
        album_peak: rg.album_peak,
    })
}

pub fn track_from_raw(song: &RawSong) -> Track {
    Track {
        id: song.id.clone(),
        title: song.title.clone(),
        artist: song.artist.clone().unwrap_or_default(),
        artist_id: song.artist_id.clone(),
        album: song.album.clone(),
        album_id: song.album_id.clone(),
        album_artist: song.display_album_artist.clone(),
        track_number: song.track,
        disc_number: song.disc_number,
        year: song.year,
        genre: song.genre.clone(),
        duration: song.duration.unwrap_or(0.0),
        format: track_format_from_raw(song),
        cover_art: song.cover_art.clone(),
        starred: song.starred.clone(),
        rating: rating_from_raw(song.user_rating),
        size: song.size,
        replay_gain: song.replay_gain.as_ref().and_then(replay_gain_from_raw),
        path: song.path.clone(),
        date_added: song.created.clone(),
    }
}

pub fn album_from_raw(album: &RawAlbum) -> Album {
    Album {
        id: album.id.clone(),
        name: album.name.clone(),
        artist: album.artist.clone().unwrap_or_default(),
        artist_id: album.artist_id.clone(),
        year: album.year,
        genre: album.genre.clone(),
        cover_art: album.cover_art.clone(),
        song_count: album.song_count.unwrap_or(0),
        duration: album.duration.unwrap_or(0.0),
        starred: album.starred.clone(),
        rating: rating_from_raw(album.user_rating),
        play_count: album.play_count,
        created: album.created.clone(),
    }
}

pub fn artist_from_raw(artist: &RawArtist) -> Artist {
    Artist {
        id: artist.id.clone(),
        name: artist.name.clone(),
        album_count: artist.album_count.unwrap_or(0),
        cover_art: artist.cover_art.clone(),
        starred: artist.starred.clone(),
        rating: rating_from_raw(artist.user_rating),
    }
}

pub fn playlist_from_raw(pl: &RawPlaylist) -> Playlist {
    Playlist {
        id: pl.id.clone(),
        name: pl.name.clone(),
        comment: pl.comment.clone(),
        owner: pl.owner.clone(),
        public: pl.public.unwrap_or(false),
        song_count: pl.song_count.unwrap_or(0),
        duration: pl.duration.unwrap_or(0.0),
        cover_art: pl.cover_art.clone(),
        created_at: pl.created.clone(),
        updated_at: pl.changed.clone(),
        smart: false,
        read_only: pl.readonly.unwrap_or(false),
        server_read_only: pl.readonly,
    }
}
