//! Wire types for the Subsonic/OpenSubsonic REST API (JSON responses).
//!
//! These mirror the subset of the Subsonic 1.16.1 + OpenSubsonic response
//! shapes ASPHONIX actually consumes. Fields we don't use are omitted rather
//! than modeled — Navidrome's JSON has many more fields than this (bpm,
//! moods, musicBrainzId, etc.) which are simply ignored by serde.
#![allow(dead_code)] // some fields are parsed for completeness/debugging but not read yet

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Envelope {
    #[serde(rename = "subsonic-response")]
    pub subsonic_response: Body,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Body {
    pub status: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub server_version: Option<String>,
    #[serde(rename = "type")]
    #[serde(default)]
    pub server_type: Option<String>,
    #[serde(default)]
    pub open_subsonic: Option<bool>,
    #[serde(default)]
    pub error: Option<SubsonicError>,

    #[serde(default)]
    pub open_subsonic_extensions: Option<Vec<OpenSubsonicExtension>>,
    #[serde(default)]
    pub scan_status: Option<ScanStatus>,
    #[serde(default)]
    pub search_result3: Option<SearchResult3>,
    #[serde(default)]
    pub album_list2: Option<AlbumList2>,
    #[serde(default)]
    pub album: Option<AlbumWithSongs>,
    #[serde(default)]
    pub artists: Option<ArtistsIndexes>,
    #[serde(default)]
    pub artist: Option<ArtistWithAlbums>,
    #[serde(default)]
    pub playlists: Option<PlaylistsWrapper>,
    #[serde(default)]
    pub playlist: Option<PlaylistWithEntries>,
    #[serde(default)]
    pub starred2: Option<Starred2>,
    #[serde(default)]
    pub similar_songs: Option<SimilarSongsWrapper>,
    #[serde(default)]
    pub similar_songs2: Option<SimilarSongsWrapper>,
    // Discovery (artist page): getTopSongs / getArtistInfo2.
    #[serde(default)]
    pub top_songs: Option<SimilarSongsWrapper>,
    #[serde(default)]
    pub artist_info2: Option<super::discovery_models::RawArtistInfo2>,
    // --- continuity (play queue sync) ---
    #[serde(default)]
    pub play_queue: Option<super::play_queue::RawPlayQueue>,
    // Lyrics: OpenSubsonic getLyricsBySongId / classic getLyrics.
    #[serde(default)]
    pub lyrics_list: Option<super::lyrics::RawLyricsList>,
    #[serde(default)]
    pub lyrics: Option<super::lyrics::RawClassicLyrics>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SubsonicError {
    pub code: u32,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OpenSubsonicExtension {
    pub name: String,
    #[serde(default)]
    pub versions: Vec<u32>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ScanStatus {
    pub scanning: bool,
    #[serde(default)]
    pub count: Option<u64>,
    /// Timestamp of the last completed scan (Navidrome); changes whenever the
    /// library content may have changed. Absent on servers that don't report it.
    #[serde(default)]
    pub last_scan: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult3 {
    #[serde(default)]
    pub artist: Vec<RawArtist>,
    #[serde(default)]
    pub album: Vec<RawAlbum>,
    #[serde(default)]
    pub song: Vec<RawSong>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AlbumList2 {
    #[serde(default)]
    pub album: Vec<RawAlbum>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumWithSongs {
    #[serde(flatten)]
    pub album: RawAlbum,
    #[serde(default)]
    pub song: Vec<RawSong>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistsIndexes {
    #[serde(default)]
    pub index: Vec<ArtistIndex>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistIndex {
    #[serde(default)]
    pub artist: Vec<RawArtist>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistWithAlbums {
    #[serde(flatten)]
    pub artist: RawArtist,
    #[serde(default)]
    pub album: Vec<RawAlbum>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistsWrapper {
    #[serde(default)]
    pub playlist: Vec<RawPlaylist>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistWithEntries {
    #[serde(flatten)]
    pub playlist: RawPlaylist,
    #[serde(default)]
    pub entry: Vec<RawSong>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RawArtist {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub album_count: Option<u32>,
    #[serde(default)]
    pub cover_art: Option<String>,
    #[serde(default)]
    pub starred: Option<String>,
    /// 1-5 user rating (`setRating`), 0/absent = unrated. Not every server
    /// reports this on `getArtist`/index listings.
    #[serde(default)]
    pub user_rating: Option<u32>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RawAlbum {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub artist_id: Option<String>,
    #[serde(default)]
    pub year: Option<i32>,
    #[serde(default)]
    pub genre: Option<String>,
    #[serde(default)]
    pub cover_art: Option<String>,
    #[serde(default)]
    pub song_count: Option<u32>,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub starred: Option<String>,
    #[serde(default)]
    pub play_count: Option<u64>,
    #[serde(default)]
    pub created: Option<String>,
    /// 1-5 user rating (`setRating`), 0/absent = unrated.
    #[serde(default)]
    pub user_rating: Option<u32>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RawReplayGain {
    #[serde(default)]
    pub track_gain: Option<f32>,
    #[serde(default)]
    pub album_gain: Option<f32>,
    #[serde(default)]
    pub track_peak: Option<f32>,
    #[serde(default)]
    pub album_peak: Option<f32>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RawSong {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub artist_id: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub album_id: Option<String>,
    #[serde(default)]
    pub display_album_artist: Option<String>,
    #[serde(default)]
    pub track: Option<u32>,
    #[serde(default)]
    pub disc_number: Option<u32>,
    #[serde(default)]
    pub year: Option<i32>,
    #[serde(default)]
    pub genre: Option<String>,
    #[serde(default)]
    pub duration: Option<f64>,
    pub suffix: String,
    #[serde(default)]
    pub bit_rate: Option<u32>,
    #[serde(default)]
    pub bit_depth: Option<u32>,
    #[serde(default)]
    pub sampling_rate: Option<u32>,
    #[serde(default)]
    pub channel_count: Option<u32>,
    #[serde(default)]
    pub cover_art: Option<String>,
    #[serde(default)]
    pub starred: Option<String>,
    #[serde(default)]
    pub replay_gain: Option<RawReplayGain>,
    #[serde(default)]
    pub path: Option<String>,
    /// 1-5 user rating (`setRating`), 0/absent = unrated.
    #[serde(default)]
    pub user_rating: Option<u32>,
    /// File size in bytes, when the server reports it.
    #[serde(default)]
    pub size: Option<u64>,
    /// ISO timestamp the file was added to the server library ("date added"
    /// sort on the all-tracks list). Absent on servers that don't expose it.
    #[serde(default)]
    pub created: Option<String>,
    /// Server-side play count / last-played timestamp (Navidrome reports both
    /// per user); feed the local smart-playlist rules.
    #[serde(default)]
    pub play_count: Option<u32>,
    #[serde(default)]
    pub played: Option<String>,
    // flow block: tag-derived sequencing features. All parsed leniently (a
    // surprising type in one of these must never fail a whole sync page).
    /// Tagged tempo (Navidrome sends `bpm`, 0 = untagged).
    #[serde(default, deserialize_with = "lenient_f64")]
    pub bpm: Option<f64>,
    /// `moods` tag values. Parsed, not used for ordering (no honest numeric).
    #[serde(default, deserialize_with = "lenient_strings")]
    pub moods: Vec<String>,
    /// Musical key. Navidrome does not send one today; accepted if a server
    /// or proxy ever does. Only Camelot ("8A") and standard notation ("Am",
    /// "F# minor") are understood (`Camelot::parse`); anything else, Open Key
    /// ("1m") included, is stored as no key.
    #[serde(default, deserialize_with = "lenient_string")]
    pub initial_key: Option<String>,
}

fn lenient_f64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    let v = <Option<serde_json::Value>>::deserialize(d)?;
    Ok(match v {
        Some(serde_json::Value::Number(n)) => n.as_f64(),
        Some(serde_json::Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    })
}

fn lenient_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match <Option<serde_json::Value>>::deserialize(d)? {
        Some(serde_json::Value::String(s)) if !s.trim().is_empty() => Some(s),
        _ => None,
    })
}

fn lenient_strings<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    Ok(match <Option<serde_json::Value>>::deserialize(d)? {
        Some(serde_json::Value::Array(a)) => a.into_iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        Some(serde_json::Value::String(s)) if !s.is_empty() => vec![s],
        _ => Vec::new(),
    })
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RawPlaylist {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub public: Option<bool>,
    #[serde(default)]
    pub song_count: Option<u32>,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub cover_art: Option<String>,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    pub changed: Option<String>,
    /// OpenSubsonic `readonly`: the current user can't edit this playlist
    /// (Navidrome sets it for smart/.nsp playlists and other users' lists).
    #[serde(default)]
    pub readonly: Option<bool>,
}

/// `getSimilarSongs`/`getSimilarSongs2` response shape (both wrap a flat
/// `song` list the same way).
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SimilarSongsWrapper {
    #[serde(default)]
    pub song: Vec<RawSong>,
}

/// `getStarred2` — every starred artist/album/song for the current user.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Starred2 {
    #[serde(default)]
    pub artist: Vec<RawArtist>,
    #[serde(default)]
    pub album: Vec<RawAlbum>,
    #[serde(default)]
    pub song: Vec<RawSong>,
}
