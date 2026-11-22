//! Subsonic/OpenSubsonic API client.
//!
//! Talks to the Navidrome server over HTTP(S) using Subsonic API 1.16.1:
//! token auth (`salt` + `md5(password + salt)`), capability probe
//! (`ping`, `getOpenSubsonicExtensions`, `getScanStatus`), and every REST
//! call needed for library sync (`search3`, `getAlbumList2`, `getArtists`,
//! `getArtist`, `getAlbum`, `getPlaylists`, `getPlaylist`) and mutation
//! (star/unstar, playlist CRUD).
//!
//! No network calls happen on the app's startup critical path — the UI
//! renders from the local SQLite mirror first, this client is only used for
//! login, background sync, and on-demand calls not mirrored locally (the raw
//! audio stream and cover art).
//!
//! ## `TrackSource`
//!
//! [`TrackSource`] is the narrow, stable surface the `audio/` engine
//! (developed in parallel) is expected to consume: given a track id it
//! returns the fully-authenticated URL to fetch the original file bytes from
//! (`format=raw`, i.e. no server-side transcoding — required for bit-perfect
//! playback). [`Client`] implements it. The URL already carries `u`/`t`/`s`/
//! `v`/`c` query params, so the audio engine can `GET` it directly with a
//! plain HTTP client and does not need to know about Subsonic auth at all.

#![allow(dead_code)] // public client surface; not every method has a caller yet

mod models;
mod discovery_models;
pub use discovery_models::RawArtistInfo2;
mod play_queue;
pub mod lyrics;
pub use play_queue::RawPlayQueue;

use std::fmt;
use std::time::Duration;

use rand::Rng;
use reqwest::StatusCode;
use url::Url;

use crate::ipc::{IpcError, IpcErrorCode};

pub use models::{
    AlbumWithSongs, ArtistWithAlbums, PlaylistWithEntries, RawAlbum, RawArtist, RawPlaylist, RawReplayGain, RawSong, ScanStatus,
    Starred2,
};

const API_VERSION: &str = "1.16.1";
const CLIENT_NAME: &str = "sonix";
/// Subsonic/Navidrome caps page sizes for list endpoints at 500.
pub const MAX_PAGE_SIZE: u32 = 500;

/// Everything needed to open/re-open a connection: no secrets are logged or
/// persisted by this struct itself (that's `secrets::`'s job).
#[derive(Clone)]
pub struct Credentials {
    pub url: String,
    pub username: String,
    pub password: String,
}

/// An authenticated Subsonic/OpenSubsonic API client. Cheaply `Clone`able
/// (the inner `reqwest::Client` is reference-counted), so it can be shared
/// across async tasks (sync worker, commands, art fetcher) without an
/// `Arc<Mutex<_>>` wrapper.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: Url,
    username: String,
    password: String,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("subsonic::Client")
            .field("base_url", &self.base_url.as_str())
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Server capability probe result (`ping` + `getOpenSubsonicExtensions` +
/// `getScanStatus`), used by `auth_connect`/`auth_probe`.
#[derive(Debug, Clone)]
pub struct Capabilities {
    pub server_type: String,
    pub server_version: String,
    pub api_version: String,
    pub open_subsonic: bool,
    pub extensions: Vec<String>,
    pub ping_ms: u32,
    pub track_count: Option<u64>,
}

impl Client {
    /// Builds a client against `url` (scheme + host + optional port/path).
    /// Does not perform any network I/O.
    pub fn new(url: &str, username: &str, password: &str) -> Result<Self, IpcError> {
        let base_url = normalize_base_url(url)?;
        let http = reqwest::Client::builder()
            .gzip(true)
            .pool_max_idle_per_host(4)
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| IpcError {
                code: IpcErrorCode::Network,
                message: format!("failed to build HTTP client: {e}"),
            })?;
        Ok(Self {
            http,
            base_url,
            username: username.to_string(),
            password: password.to_string(),
        })
    }

    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    /// Builds a fully-authenticated URL for `rest/{endpoint}` with the given
    /// extra query params plus a fresh salt/token pair.
    fn build_url(&self, endpoint: &str, extra: &[(&str, String)]) -> Url {
        let mut url = self.base_url.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .expect("base_url is http(s), always has segments");
            segments.pop_if_empty();
            segments.push("rest");
            segments.push(endpoint);
        }
        let (salt, token) = auth_pair(&self.password);
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("u", &self.username);
            q.append_pair("t", &token);
            q.append_pair("s", &salt);
            q.append_pair("v", API_VERSION);
            q.append_pair("c", CLIENT_NAME);
            q.append_pair("f", "json");
            for (k, v) in extra {
                q.append_pair(k, v);
            }
        }
        url
    }

    /// Sends one GET. Logged at trace (failures at debug), with the auth params redacted: the connection monitor probes
    /// periodically, and an offline server must not turn that into periodic warnings. Callers log failures
    /// that matter.
    async fn send_logged(&self, endpoint: &str, url: Url) -> Result<reqwest::Response, IpcError> {
        tracing::trace!(endpoint, url = %crate::redact::redact_url(url.as_str()), "subsonic request");
        self.http.get(url).send().await.map_err(|e| {
            let e = map_reqwest_err(e); // `without_url`: no auth params in the message
            tracing::debug!(endpoint, error = %e.message, "subsonic request failed");
            e
        })
    }

    /// `ping` — cheapest possible liveness/latency check.
    pub async fn ping(&self) -> Result<u32, IpcError> {
        let started = std::time::Instant::now();
        let _body: models::Body = self.get_body("ping", &[]).await?;
        Ok(started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32)
    }

    /// `getScanStatus`, timed. Used by the background connection monitor as
    /// its liveness probe (the round-trip time is reported as the ping) and,
    /// via `lastScan`, as the cheap "has the library changed?" check that
    /// lets a resync be skipped entirely.
    pub async fn get_scan_status(&self) -> Result<(ScanStatus, u32), IpcError> {
        self.get_scan_status_with_versions().await.map(|(s, ms, _)| (s, ms))
    }

    /// [`Self::get_scan_status`] plus the `(serverVersion, apiVersion)` every
    /// response envelope carries, so the connection monitor can fill in the
    /// session's versions after a start from stored credentials (where
    /// `auth_connect`'s full probe never ran) without an extra request.
    pub async fn get_scan_status_with_versions(&self) -> Result<(ScanStatus, u32, (Option<String>, String)), IpcError> {
        let started = std::time::Instant::now();
        let body = self.get_body("getScanStatus", &[]).await?;
        let ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
        let status = body.scan_status.ok_or_else(|| IpcError {
            code: IpcErrorCode::ServerError,
            message: "getScanStatus: response has no scanStatus".into(),
        })?;
        Ok((status, ms, (body.server_version, body.version)))
    }

    /// `getStarred2`: everything the user has starred (small; used to
    /// refresh star state without a full resync).
    pub async fn get_starred2(&self) -> Result<Starred2, IpcError> {
        let body = self.get_body("getStarred2", &[]).await?;
        Ok(body.starred2.unwrap_or_default())
    }

    /// Fetches the raw envelope body (status/version/error + whichever
    /// optional field the endpoint fills in).
    async fn get_body(&self, endpoint: &str, extra: &[(&str, String)]) -> Result<models::Body, IpcError> {
        #[cfg(test)]
        tests::record_request(endpoint);
        let url = self.build_url(endpoint, extra);
        let started = std::time::Instant::now();
        let resp = self.send_logged(endpoint, url).await?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(map_reqwest_err)?;
        tracing::trace!(endpoint, status = status.as_u16(), bytes = bytes.len(), duration_ms = started.elapsed().as_millis() as u64, "subsonic response");
        if !status.is_success() {
            return Err(IpcError {
                code: if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                    IpcErrorCode::AuthFailed
                } else {
                    IpcErrorCode::ServerError
                },
                message: format!("{endpoint}: HTTP {status}"),
            });
        }
        let envelope: models::Envelope = serde_json::from_slice(&bytes).map_err(|e| IpcError {
            code: IpcErrorCode::ServerError,
            message: format!("{endpoint}: malformed response ({e})"),
        })?;
        let body = envelope.subsonic_response;
        if body.status != "ok" {
            return Err(map_subsonic_error(endpoint, body.error));
        }
        Ok(body)
    }

    /// Full capability probe: `ping` (for latency) + `getOpenSubsonicExtensions`
    /// + `getScanStatus` (for an immediate track-count reading).
    pub async fn probe_capabilities(&self) -> Result<Capabilities, IpcError> {
        let started = std::time::Instant::now();
        let body = self.get_body("ping", &[]).await?;
        let ping_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;

        let ext_body = self.get_body("getOpenSubsonicExtensions", &[]).await?;
        let extensions = ext_body
            .open_subsonic_extensions
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.name)
            .collect();

        let track_count = match self.get_body("getScanStatus", &[]).await {
            Ok(b) => b.scan_status.and_then(|s| s.count),
            Err(_) => None, // not fatal to the probe
        };

        Ok(Capabilities {
            server_type: body.server_type.unwrap_or_else(|| "unknown".into()),
            server_version: body.server_version.unwrap_or_else(|| "unknown".into()),
            api_version: body.version,
            open_subsonic: body.open_subsonic.unwrap_or(false),
            extensions,
            ping_ms,
            track_count,
        })
    }

    /// One page of the full-library track scan. `offset`/`count` map to
    /// `songOffset`/`songCount`; `albumCount`/`artistCount` are forced to 0
    /// so the server doesn't waste work building those result sets too.
    pub async fn search_songs_page(&self, offset: u32, count: u32) -> Result<Vec<RawSong>, IpcError> {
        let count = count.min(MAX_PAGE_SIZE);
        let body = self
            .get_body(
                "search3",
                &[
                    ("query", String::new()),
                    ("songCount", count.to_string()),
                    ("songOffset", offset.to_string()),
                    ("albumCount", "0".to_string()),
                    ("artistCount", "0".to_string()),
                ],
            )
            .await?;
        Ok(body.search_result3.map(|r| r.song).unwrap_or_default())
    }

    /// One page of `getAlbumList2` (any Subsonic list `type`: e.g.
    /// `alphabeticalByName`, `newest`, `recent`, `frequent`, `random`).
    pub async fn get_album_list2(&self, list_type: &str, offset: u32, size: u32) -> Result<Vec<RawAlbum>, IpcError> {
        let size = size.min(MAX_PAGE_SIZE);
        let body = self
            .get_body(
                "getAlbumList2",
                &[
                    ("type", list_type.to_string()),
                    ("offset", offset.to_string()),
                    ("size", size.to_string()),
                ],
            )
            .await?;
        Ok(body.album_list2.map(|r| r.album).unwrap_or_default())
    }

    /// `getArtists` flattened across its alphabetical index groups.
    pub async fn get_artists(&self) -> Result<Vec<RawArtist>, IpcError> {
        let body = self.get_body("getArtists", &[]).await?;
        let indexes = body.artists.map(|a| a.index).unwrap_or_default();
        Ok(indexes.into_iter().flat_map(|i| i.artist).collect())
    }

    pub async fn get_artist(&self, id: &str) -> Result<ArtistWithAlbums, IpcError> {
        let body = self.get_body("getArtist", &[("id", id.to_string())]).await?;
        body.artist.ok_or_else(|| IpcError {
            code: IpcErrorCode::NotFound,
            message: format!("artist {id} not found"),
        })
    }

    pub async fn get_album(&self, id: &str) -> Result<AlbumWithSongs, IpcError> {
        let body = self.get_body("getAlbum", &[("id", id.to_string())]).await?;
        body.album.ok_or_else(|| IpcError {
            code: IpcErrorCode::NotFound,
            message: format!("album {id} not found"),
        })
    }

    pub async fn get_playlists(&self) -> Result<Vec<RawPlaylist>, IpcError> {
        let body = self.get_body("getPlaylists", &[]).await?;
        Ok(body.playlists.map(|p| p.playlist).unwrap_or_default())
    }

    pub async fn get_playlist(&self, id: &str) -> Result<PlaylistWithEntries, IpcError> {
        let body = self.get_body("getPlaylist", &[("id", id.to_string())]).await?;
        body.playlist.ok_or_else(|| IpcError {
            code: IpcErrorCode::NotFound,
            message: format!("playlist {id} not found"),
        })
    }

    pub async fn create_playlist(&self, name: &str) -> Result<RawPlaylist, IpcError> {
        let body = self
            .get_body("createPlaylist", &[("name", name.to_string())])
            .await?;
        body.playlist
            .map(|p| p.playlist)
            .ok_or_else(|| IpcError {
                code: IpcErrorCode::ServerError,
                message: "createPlaylist: server did not return the new playlist".into(),
            })
    }

    pub async fn delete_playlist(&self, id: &str) -> Result<(), IpcError> {
        self.get_body("deletePlaylist", &[("id", id.to_string())]).await?;
        Ok(())
    }

    pub async fn update_playlist_rename(&self, id: &str, name: &str) -> Result<(), IpcError> {
        self.get_body(
            "updatePlaylist",
            &[("playlistId", id.to_string()), ("name", name.to_string())],
        )
        .await?;
        Ok(())
    }

    pub async fn update_playlist_add_songs(&self, id: &str, song_ids: &[String]) -> Result<(), IpcError> {
        if song_ids.is_empty() {
            return Ok(());
        }
        let mut extra: Vec<(&str, String)> = vec![("playlistId", id.to_string())];
        for sid in song_ids {
            extra.push(("songIdToAdd", sid.clone()));
        }
        self.get_body("updatePlaylist", &extra).await?;
        Ok(())
    }

    /// Removes `remove_indices` and appends `add_ids` in ONE `updatePlaylist`
    /// request (Navidrome applies both in a single transaction), so a
    /// reorder can never leave the playlist emptied if the add half fails.
    pub async fn update_playlist_replace(&self, id: &str, remove_indices: &[u32], add_ids: &[String]) -> Result<(), IpcError> {
        let mut extra: Vec<(&str, String)> = vec![("playlistId", id.to_string())];
        for idx in remove_indices {
            extra.push(("songIndexToRemove", idx.to_string()));
        }
        for sid in add_ids {
            extra.push(("songIdToAdd", sid.clone()));
        }
        self.get_body("updatePlaylist", &extra).await?;
        Ok(())
    }

    pub async fn update_playlist_remove_indices(&self, id: &str, indices: &[u32]) -> Result<(), IpcError> {
        if indices.is_empty() {
            return Ok(());
        }
        let mut extra: Vec<(&str, String)> = vec![("playlistId", id.to_string())];
        for idx in indices {
            extra.push(("songIndexToRemove", idx.to_string()));
        }
        self.get_body("updatePlaylist", &extra).await?;
        Ok(())
    }

    /// `setRating`: 1-5 user rating, or `0` to clear it. Valid for track,
    /// album or artist ids alike (Subsonic doesn't distinguish by endpoint).
    pub async fn set_rating(&self, id: &str, rating: u8) -> Result<(), IpcError> {
        self.get_body("setRating", &[("id", id.to_string()), ("rating", rating.to_string())])
            .await?;
        Ok(())
    }

    /// `getSimilarSongs2` (artist-seeded "radio"): server-side similarity,
    /// capped at `count` songs. Not every server/library supports this
    /// (needs Last.fm-style artist similarity data) — an empty result or
    /// error is a normal, expected outcome callers fall back from.
    pub async fn get_similar_songs2(&self, id: &str, count: u32) -> Result<Vec<RawSong>, IpcError> {
        let body = self
            .get_body("getSimilarSongs2", &[("id", id.to_string()), ("count", count.to_string())])
            .await?;
        Ok(body.similar_songs2.map(|w| w.song).unwrap_or_default())
    }

    /// `savePlayQueue`: stores the user's play queue on the server (ids in
    /// play order, the current song id, and the position in it in ms) so
    /// another client can pick it up. Overwrites whatever was saved before.
    pub async fn save_play_queue(&self, ids: &[String], current: Option<&str>, position_ms: u64) -> Result<(), IpcError> {
        let mut extra: Vec<(&str, String)> = ids.iter().map(|id| ("id", id.clone())).collect();
        if let Some(c) = current {
            extra.push(("current", c.to_string()));
        }
        extra.push(("position", position_ms.to_string()));
        self.get_body("savePlayQueue", &extra).await?;
        Ok(())
    }

    /// `getPlayQueue` (read-only): the queue last saved with `savePlayQueue`
    /// by any client, or `None` if the user never saved one.
    pub async fn get_play_queue(&self) -> Result<Option<RawPlayQueue>, IpcError> {
        let body = self.get_body("getPlayQueue", &[]).await?;
        Ok(body.play_queue.filter(|q| !q.entry.is_empty()))
    }

    /// `getSimilarSongs` (track/album-seeded "radio").
    pub async fn get_similar_songs(&self, id: &str, count: u32) -> Result<Vec<RawSong>, IpcError> {
        let body = self
            .get_body("getSimilarSongs", &[("id", id.to_string()), ("count", count.to_string())])
            .await?;
        Ok(body.similar_songs.map(|w| w.song).unwrap_or_default())
    }

    /// `getTopSongs` (Navidrome: Last.fm + local plays). Needs the artist's
    /// *name*; an empty list is a normal outcome (no agent configured).
    pub async fn get_top_songs(&self, artist_name: &str, count: u32) -> Result<Vec<RawSong>, IpcError> {
        let body = self
            .get_body("getTopSongs", &[("artist", artist_name.to_string()), ("count", count.to_string())])
            .await?;
        Ok(body.top_songs.map(|w| w.song).unwrap_or_default())
    }

    /// OpenSubsonic `getLyricsBySongId` (needs the `songLyrics` extension).
    pub async fn get_lyrics_by_song_id(&self, id: &str) -> Result<Option<lyrics::ParsedLyrics>, IpcError> {
        let body = self.get_body("getLyricsBySongId", &[("id", id.to_string())]).await?;
        Ok(body.lyrics_list.as_ref().and_then(lyrics::from_structured))
    }

    /// Classic `getLyrics` (plain text, looked up by artist + title).
    pub async fn get_lyrics(&self, artist: &str, title: &str) -> Result<Option<lyrics::ParsedLyrics>, IpcError> {
        let body = self
            .get_body("getLyrics", &[("artist", artist.to_string()), ("title", title.to_string())])
            .await?;
        Ok(body.lyrics.and_then(|l| l.value).as_deref().and_then(lyrics::from_plain))
    }

    /// `getArtistInfo2` (biography, similar artists). Fields may all be empty.
    pub async fn get_artist_info2(&self, id: &str) -> Result<RawArtistInfo2, IpcError> {
        let body = self.get_body("getArtistInfo2", &[("id", id.to_string())]).await?;
        Ok(body.artist_info2.unwrap_or_default())
    }

    pub async fn star(&self, id: &str, kind: StarKind) -> Result<(), IpcError> {
        let param = kind.query_param();
        self.get_body("star", &[(param, id.to_string())]).await?;
        Ok(())
    }

    pub async fn unstar(&self, id: &str, kind: StarKind) -> Result<(), IpcError> {
        let param = kind.query_param();
        self.get_body("unstar", &[(param, id.to_string())]).await?;
        Ok(())
    }

    /// Lighter than [`Self::probe_capabilities`] (skips `ping`/`getScanStatus`):
    /// just the OpenSubsonic extension names, used by `scrobble::` to decide
    /// once per session whether the `playbackReport` extension is available.
    pub async fn get_open_subsonic_extensions(&self) -> Result<Vec<String>, IpcError> {
        let body = self.get_body("getOpenSubsonicExtensions", &[]).await?;
        Ok(body.open_subsonic_extensions.unwrap_or_default().into_iter().map(|e| e.name).collect())
    }

    /// Classic Subsonic `scrobble`: a "now playing" ping when `submission` is
    /// `false`, a real scrobble (added to play history/count) when `true`.
    /// `time_ms` is the client timestamp Subsonic's `time` param expects
    /// (milliseconds since epoch) — pass the moment the track actually
    /// *started* playing, not the moment the threshold was crossed, so a
    /// submission retried later (see `scrobble::queue`) still lands with the
    /// original listening time.
    pub async fn scrobble(&self, id: &str, submission: bool, time_ms: Option<i64>) -> Result<(), IpcError> {
        let mut extra: Vec<(&str, String)> = vec![("id", id.to_string()), ("submission", submission.to_string())];
        if let Some(t) = time_ms {
            extra.push(("time", t.to_string()));
        }
        self.get_body("scrobble", &extra).await?;
        Ok(())
    }

    /// OpenSubsonic `playbackReport` extension (`reportPlayback`): reports a
    /// client-side playback timeline state (`starting|playing|paused|stopped`)
    /// with a millisecond position. Navidrome performs the 50%/4-minute
    /// scrobble decision itself from this stream when `state == "stopped"`;
    /// `ignore_scrobble = true` is used for connectivity checks/tests so a
    /// report never creates real play history.
    pub async fn report_playback(&self, id: &str, state: PlaybackReportState, position_ms: u64, ignore_scrobble: bool) -> Result<(), IpcError> {
        let extra: Vec<(&str, String)> = vec![
            ("mediaId", id.to_string()),
            ("mediaType", "song".to_string()),
            ("positionMs", position_ms.to_string()),
            ("state", state.as_str().to_string()),
            ("ignoreScrobble", ignore_scrobble.to_string()),
        ];
        self.get_body("reportPlayback", &extra).await?;
        Ok(())
    }

    /// Fetches cover art bytes at the given pixel size (server-side resized,
    /// so the client never downloads a full-res image). Returns the bytes
    /// plus the server's declared content-type, if any.
    pub async fn get_cover_art(&self, id: &str, size: Option<u32>) -> Result<(Vec<u8>, Option<String>), IpcError> {
        let mut extra = vec![("id", id.to_string())];
        if let Some(s) = size {
            extra.push(("size", s.to_string()));
        }
        let url = self.build_url("getCoverArt", &extra);
        let resp = self.send_logged("getCoverArt", url).await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(IpcError {
                code: if status == StatusCode::NOT_FOUND {
                    IpcErrorCode::NotFound
                } else {
                    IpcErrorCode::ServerError
                },
                message: format!("getCoverArt: HTTP {status}"),
            });
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        // Subsonic error responses for getCoverArt come back as JSON with a
        // 200 status in some server implementations; Navidrome returns a
        // proper image or a 404, so we don't special-case JSON here.
        let bytes = read_body_capped(resp, MAX_COVER_ART_BYTES, "getCoverArt").await?;
        Ok((bytes, content_type))
    }

    /// The authenticated URL to stream a track's **original** bytes
    /// (`format=raw`) — required for bit-perfect playback (no server-side
    /// transcoding). See [`TrackSource`].
    pub fn stream_url(&self, id: &str) -> Url {
        self.build_url("stream", &[("id", id.to_string()), ("format", "raw".to_string())])
    }

    /// The authenticated URL to fetch cover art at `size` px.
    pub fn cover_art_url(&self, id: &str, size: u32) -> Url {
        self.build_url("getCoverArt", &[("id", id.to_string()), ("size", size.to_string())])
    }
}

/// Upper bound for one cover art response. A server-side resized cover is
/// tens of KB; anything past this is a broken or hostile server and is
/// refused instead of being buffered (and later decoded) in full.
pub const MAX_COVER_ART_BYTES: usize = 20 * 1024 * 1024;

/// Reads a response body, failing as soon as it exceeds `cap` bytes: an
/// over-cap `Content-Length` is refused before any byte is read, and a body
/// without one (chunked) is read chunk by chunk and dropped once it passes
/// the cap, so at most `cap` + one chunk is ever held.
async fn read_body_capped(mut resp: reqwest::Response, cap: usize, endpoint: &str) -> Result<Vec<u8>, IpcError> {
    let too_large = || IpcError {
        code: IpcErrorCode::ServerError,
        message: format!("{endpoint}: response larger than {cap} bytes"),
    };
    if resp.content_length().is_some_and(|n| n > cap as u64) {
        return Err(too_large());
    }
    let mut out = Vec::with_capacity(resp.content_length().map_or(0, |n| n as usize));
    while let Some(chunk) = resp.chunk().await.map_err(map_reqwest_err)? {
        if out.len() + chunk.len() > cap {
            return Err(too_large());
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StarKind {
    Track,
    Album,
    Artist,
}

impl StarKind {
    fn query_param(self) -> &'static str {
        match self {
            StarKind::Track => "id",
            StarKind::Album => "albumId",
            StarKind::Artist => "artistId",
        }
    }
}

/// `state` values for `Client::report_playback` (OpenSubsonic `playbackReport`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackReportState {
    Starting,
    Playing,
    Paused,
    Stopped,
}

impl PlaybackReportState {
    fn as_str(self) -> &'static str {
        match self {
            PlaybackReportState::Starting => "starting",
            PlaybackReportState::Playing => "playing",
            PlaybackReportState::Paused => "paused",
            PlaybackReportState::Stopped => "stopped",
        }
    }
}

/// The narrow surface the `audio/` engine depends on: turning a track id
/// into the URL to fetch its original (untranscoded) bytes from. Kept as a
/// trait (rather than exposing `subsonic::Client` directly) so the audio
/// engine can be tested against a fake implementation and so this module can
/// change its internals without breaking that boundary.
pub trait TrackSource: Send + Sync {
    /// Returns the fully-authenticated URL to `GET` for the original audio
    /// bytes of track `id` (`format=raw`, no transcoding).
    fn stream_url(&self, id: &str) -> Url;
}

impl TrackSource for Client {
    fn stream_url(&self, id: &str) -> Url {
        Client::stream_url(self, id)
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn normalize_base_url(raw: &str) -> Result<Url, IpcError> {
    let trimmed = raw.trim();
    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    Url::parse(&with_scheme).map_err(|e| IpcError {
        code: IpcErrorCode::Unknown,
        message: format!("invalid server URL: {e}"),
    })
}

/// Generates a fresh random salt and the matching Subsonic token
/// (`md5(password + salt)`, lowercase hex) for one request. A new salt is
/// used per-request (never reused) since it's cheap and avoids any replay
/// concerns.
fn auth_pair(password: &str) -> (String, String) {
    let salt = random_salt(12);
    let mut hasher = <md5::Md5 as md5::Digest>::new();
    md5::Digest::update(&mut hasher, password.as_bytes());
    md5::Digest::update(&mut hasher, salt.as_bytes());
    let digest = md5::Digest::finalize(hasher);
    let token = digest.iter().map(|b| format!("{b:02x}")).collect::<String>();
    (salt, token)
}

fn random_salt(len: usize) -> String {
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

fn map_reqwest_err(e: reqwest::Error) -> IpcError {
    // Every `reqwest::Error` we see here (connect/timeout/body-read) is a
    // transport-level failure, so it's always `Network` — kept as its own
    // function (rather than inlined `.map_err`) as the one place to extend
    // if a future case needs finer-grained mapping.
    IpcError {
        code: IpcErrorCode::Network,
        // `without_url`: reqwest's Display embeds the full request URL, which for
        // Subsonic carries the per-request auth params (u/t/s). That message
        // reaches the log file and the UI, so never include it.
        message: format!("request failed: {}", e.without_url()),
    }
}

fn map_subsonic_error(endpoint: &str, error: Option<models::SubsonicError>) -> IpcError {
    let Some(err) = error else {
        return IpcError {
            code: IpcErrorCode::ServerError,
            message: format!("{endpoint}: server returned a failure status with no error detail"),
        };
    };
    let code = match err.code {
        40 | 41 | 44 => IpcErrorCode::AuthFailed,
        50 => IpcErrorCode::AuthFailed,
        70 => IpcErrorCode::NotFound,
        10 | 20 | 30 => IpcErrorCode::ServerError,
        _ => IpcErrorCode::ServerError,
    };
    IpcError {
        code,
        message: err
            .message
            .unwrap_or_else(|| format!("{endpoint}: subsonic error {}", err.code)),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn token_auth_matches_known_vector() {
        // Spec example from subsonic docs: password "sesame", salt "c19b2d",
        // expected token "26719a1196d2a940705a59634eb18eab".
        let mut hasher = <md5::Md5 as md5::Digest>::new();
        md5::Digest::update(&mut hasher, b"sesame");
        md5::Digest::update(&mut hasher, b"c19b2d");
        let digest = md5::Digest::finalize(hasher);
        let token = digest.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(token, "26719a1196d2a940705a59634eb18eab");
    }

    /// `Client::new` runs on the startup critical path (from cached config):
    /// it must be local-only and cheap.
    #[test]
    fn client_new_is_cheap_and_offline() {
        let started = std::time::Instant::now();
        let _c = Client::new("http://192.0.2.1:4533", "u", "p").unwrap();
        let first = started.elapsed();
        println!("Client::new: {first:?}");
        assert!(first < std::time::Duration::from_millis(500));
    }

    #[test]
    fn random_salt_is_alphanumeric_and_requested_length() {
        let salt = random_salt(12);
        assert_eq!(salt.len(), 12);
        assert!(salt.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn build_url_includes_auth_and_extra_params() {
        let client = Client::new("navidrome.example.com:4533", "alice", "hunter2").unwrap();
        let url = client.build_url("ping", &[]);
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.path(), "/rest/ping");
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs.get("u").unwrap(), "alice");
        assert_eq!(pairs.get("v").unwrap(), API_VERSION);
        assert_eq!(pairs.get("c").unwrap(), CLIENT_NAME);
        assert_eq!(pairs.get("f").unwrap(), "json");
        assert!(pairs.contains_key("t"));
        assert!(pairs.contains_key("s"));
    }

    #[test]
    fn normalize_base_url_adds_scheme_when_missing() {
        let url = normalize_base_url("192.168.1.10:4533").unwrap();
        assert_eq!(url.scheme(), "http");
        let url2 = normalize_base_url("https://navidrome.example.com").unwrap();
        assert_eq!(url2.scheme(), "https");
    }

    #[test]
    fn stream_url_uses_raw_format() {
        let client = Client::new("http://example.com", "u", "p").unwrap();
        let url = client.stream_url("track123");
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs.get("format").unwrap(), "raw");
        assert_eq!(pairs.get("id").unwrap(), "track123");
        assert_eq!(url.path(), "/rest/stream");
    }

    /// One-shot mock HTTP server: answers the first request with `body`
    /// (JSON) and returns the request line it saw. Never touches a real
    /// server, so `savePlayQueue` is tested without mutating anything.
    fn mock_server_once(body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let n = sock.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            sock.write_all(resp.as_bytes()).unwrap();
            req.lines().next().unwrap_or("").to_string()
        });
        (url, handle)
    }

    /// One-shot mock HTTP server answering with a raw, caller-built response.
    fn mock_server_raw(resp: Vec<u8>) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf);
            let _ = sock.write_all(&resp);
        });
        url
    }

    async fn capped_get(url: &str, cap: usize) -> Result<Vec<u8>, IpcError> {
        let resp = reqwest::Client::new().get(url).send().await.unwrap();
        read_body_capped(resp, cap, "getCoverArt").await
    }

    #[tokio::test]
    async fn capped_body_accepts_up_to_the_cap() {
        let url = mock_server_raw(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n0123456789".to_vec());
        assert_eq!(capped_get(&url, 10).await.unwrap(), b"0123456789");
    }

    #[tokio::test]
    async fn capped_body_rejects_an_over_cap_content_length() {
        let url = mock_server_raw(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\n0123456789A".to_vec());
        let err = capped_get(&url, 10).await.unwrap_err();
        assert!(err.message.contains("larger than 10 bytes"), "{}", err.message);
    }

    #[tokio::test]
    async fn capped_body_rejects_an_over_cap_chunked_body() {
        // No Content-Length: the cap must trip while streaming.
        let mut resp = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
        for _ in 0..4 {
            resp.extend_from_slice(b"8\r\nABCDEFGH\r\n");
        }
        resp.extend_from_slice(b"0\r\n\r\n");
        let url = mock_server_raw(resp);
        let err = capped_get(&url, 20).await.unwrap_err();
        assert!(err.message.contains("larger than 20 bytes"), "{}", err.message);
    }

    #[tokio::test]
    async fn cover_art_over_the_cap_is_refused() {
        // Declares 20 MB + 1 and sends nothing: refused from the header alone.
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            MAX_COVER_ART_BYTES + 1
        );
        let url = mock_server_raw(header.into_bytes());
        let client = Client::new(&url, "u", "p").unwrap();
        let err = client.get_cover_art("al-1", Some(320)).await.unwrap_err();
        assert!(err.message.contains("larger than"), "{}", err.message);
        assert_eq!(MAX_COVER_ART_BYTES, 20 * 1024 * 1024);
    }

    /// The request log sites (`send_logged`, `get_body`, `get_cover_art`) at trace: the events are there, with
    /// the endpoint, and no credential (user, password, token, salt) reaches the output.
    #[tokio::test]
    async fn request_logs_carry_no_credentials() {
        let logs = sonix_logging::test_support::capture();
        let (url, handle) = mock_server_once(r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#);
        let client = Client::new(&url, "alice-user", "pw-secret-123").unwrap();
        client.ping().await.unwrap();
        let seen = handle.join().unwrap();
        assert!(seen.contains("u=alice-user") && seen.contains("&t="), "the server did get the credentials: {seen}");

        let art = mock_server_raw(b"HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc".to_vec());
        let client2 = Client::new(&art, "alice-user", "pw-secret-123").unwrap();
        client2.get_cover_art("al-1", Some(320)).await.unwrap();

        // connection refused: the failure site
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead = format!("http://alice-user:pw-secret-123@{}", closed.local_addr().unwrap());
        drop(closed);
        let client3 = Client::new(&dead, "alice-user", "pw-secret-123").unwrap();
        assert!(client3.ping().await.is_err());

        let req: Vec<_> = logs.events().into_iter().filter(|e| e["message"] == "subsonic request").collect();
        assert_eq!(req.len(), 3, "{}", logs.text());
        assert_eq!(req[0]["fields"]["endpoint"], "ping");
        assert!(req[0]["fields"]["url"].as_str().unwrap().contains("/rest/ping?u=REDACTED&t=REDACTED&s=REDACTED"), "{}", req[0]);
        assert_eq!(req[1]["fields"]["endpoint"], "getCoverArt");
        assert!(req[1]["fields"]["url"].as_str().unwrap().contains("id=al-1"), "harmless params stay: {}", req[1]);
        assert!(logs.find("subsonic response").is_some());
        assert!(logs.find("subsonic request failed").is_some(), "{}", logs.text());
        crate::redact::assert_redacted(&logs.text(), &["alice-user", "pw-secret-123"]);
    }

    #[tokio::test]
    async fn save_play_queue_sends_ids_current_and_position() {
        let (url, handle) = mock_server_once(r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#);
        let client = Client::new(&url, "u", "p").unwrap();
        client
            .save_play_queue(&["t1".to_string(), "t2".to_string()], Some("t2"), 12_345)
            .await
            .unwrap();
        let line = handle.join().unwrap();
        assert!(line.starts_with("GET /rest/savePlayQueue?"), "{line}");
        assert!(line.contains("id=t1&id=t2"), "{line}");
        assert!(line.contains("current=t2"), "{line}");
        assert!(line.contains("position=12345"), "{line}");
    }

    #[tokio::test]
    async fn get_play_queue_parses_and_treats_missing_as_none() {
        let (url, handle) = mock_server_once(
            r#"{"subsonic-response":{"status":"ok","version":"1.16.1","playQueue":{"entry":[{"id":"t9","title":"Nine","suffix":"flac"}],"current":"t9","position":5000,"changed":"2026-10-11T09:30:00Z","changedBy":"phone"}}}"#,
        );
        let client = Client::new(&url, "u", "p").unwrap();
        let pq = client.get_play_queue().await.unwrap().expect("queue present");
        assert_eq!(pq.current.as_deref(), Some("t9"));
        assert_eq!(pq.position, Some(5000));
        assert!(handle.join().unwrap().starts_with("GET /rest/getPlayQueue?"));

        let (url, handle) = mock_server_once(r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#);
        let client = Client::new(&url, "u", "p").unwrap();
        assert!(client.get_play_queue().await.unwrap().is_none());
        handle.join().unwrap();
    }

    #[test]
    fn map_subsonic_error_codes() {
        let e = map_subsonic_error(
            "ping",
            Some(models::SubsonicError {
                code: 40,
                message: Some("Wrong username or password".into()),
            }),
        );
        assert_eq!(e.code, IpcErrorCode::AuthFailed);

        let e = map_subsonic_error(
            "getAlbum",
            Some(models::SubsonicError {
                code: 70,
                message: None,
            }),
        );
        assert_eq!(e.code, IpcErrorCode::NotFound);
    }

    // -----------------------------------------------------------------
    // Integration tests against a real Navidrome/Subsonic server. Run with:
    //   cargo test -- --ignored --nocapture
    // Credentials come from the `SONIX_DEV_SERVER` / `SONIX_DEV_USER` /
    // `SONIX_DEV_PASS` env vars. These tests never call startScan or
    // user/settings management, and mutate nothing beyond `SONIX_TEST_*`-prefixed playlists they create
    // and delete themselves.
    // -----------------------------------------------------------------

    thread_local! {
        static REQUESTS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    /// Test-only request log (per test thread) so integration tests can
    /// assert how many round-trips a code path makes.
    pub(crate) fn record_request(endpoint: &str) {
        REQUESTS.with(|r| r.borrow_mut().push(endpoint.to_string()));
    }

    pub(crate) fn take_requests() -> Vec<String> {
        REQUESTS.with(|r| std::mem::take(&mut *r.borrow_mut()))
    }

    pub(crate) fn dev_server_env() -> (String, String, String) {
        (
            std::env::var("SONIX_DEV_SERVER").expect("SONIX_DEV_SERVER not set"),
            std::env::var("SONIX_DEV_USER").expect("SONIX_DEV_USER not set"),
            std::env::var("SONIX_DEV_PASS").expect("SONIX_DEV_PASS not set"),
        )
    }

    #[tokio::test]
    #[ignore]
    async fn integration_ping_and_probe_capabilities() {
        let (url, user, pass) = dev_server_env();
        let client = Client::new(&url, &user, &pass).unwrap();

        let ping_ms = client.ping().await.unwrap();
        println!("ping: {ping_ms}ms");

        let caps = client.probe_capabilities().await.unwrap();
        println!(
            "server={} version={} api={} openSubsonic={} extensions={:?} trackCount={:?}",
            caps.server_type, caps.server_version, caps.api_version, caps.open_subsonic, caps.extensions, caps.track_count
        );
        assert_eq!(caps.server_type, "navidrome");
        assert!(caps.open_subsonic);
        assert!(caps.track_count.unwrap_or(0) > 0);
    }

    #[tokio::test]
    #[ignore]
    async fn integration_wrong_password_maps_to_auth_failed() {
        let (url, user, _) = dev_server_env();
        let client = Client::new(&url, &user, "definitely-not-the-password").unwrap();
        let err = client.ping().await.unwrap_err();
        assert_eq!(err.code, IpcErrorCode::AuthFailed);
    }

    #[tokio::test]
    #[ignore]
    async fn integration_search_songs_page_has_complete_format_data() {
        let (url, user, pass) = dev_server_env();
        let client = Client::new(&url, &user, &pass).unwrap();
        let page = client.search_songs_page(0, 25).await.unwrap();
        assert!(!page.is_empty());
        for song in &page {
            assert!(!song.suffix.is_empty());
            assert!(song.sampling_rate.unwrap_or(0) > 0);
        }
    }

    #[tokio::test]
    #[ignore]
    async fn integration_get_artists_and_album_list2() {
        let (url, user, pass) = dev_server_env();
        let client = Client::new(&url, &user, &pass).unwrap();
        let artists = client.get_artists().await.unwrap();
        assert!(!artists.is_empty());
        let albums = client.get_album_list2("alphabeticalByName", 0, 10).await.unwrap();
        assert!(!albums.is_empty());
    }

    /// Read-only: `getTopSongs` + `getArtistInfo2` must parse against the real
    /// server whatever its agent configuration (empty answers are fine).
    #[tokio::test]
    #[ignore]
    async fn integration_top_songs_and_artist_info2_parse() {
        let (url, user, pass) = dev_server_env();
        let client = Client::new(&url, &user, &pass).unwrap();
        let artists = client.get_artists().await.unwrap();
        let a = artists.first().expect("library has artists");
        let top = client.get_top_songs(&a.name, 10).await.unwrap();
        let info = client.get_artist_info2(&a.id).await.unwrap();
        println!(
            "top songs for {:?}: {}; bio chars: {}; similar: {}",
            a.name,
            top.len(),
            info.biography.as_deref().map(str::len).unwrap_or(0),
            info.similar_artist.len()
        );
    }

    #[tokio::test]
    #[ignore]
    async fn integration_get_cover_art_bytes() {
        let (url, user, pass) = dev_server_env();
        let client = Client::new(&url, &user, &pass).unwrap();
        let page = client.search_songs_page(0, 1).await.unwrap();
        let song = page.first().expect("dev server has at least one track");
        let cover_id = song.cover_art.clone().expect("track has cover art");
        let (bytes, content_type) = client.get_cover_art(&cover_id, Some(160)).await.unwrap();
        println!("cover art: {} bytes, content-type={:?}", bytes.len(), content_type);
        assert!(!bytes.is_empty());
    }

    /// Read-only safety check: playlist CRUD against this admin account must
    /// only ever touch playlists this test itself creates (prefixed
    /// `SONIX_TEST_`), and must clean them up.
    #[tokio::test]
    #[ignore]
    async fn integration_playlist_crud_on_sonix_test_playlist_only() {
        let (url, user, pass) = dev_server_env();
        let client = Client::new(&url, &user, &pass).unwrap();

        let name = format!("SONIX_TEST_{}", std::process::id());
        let created = client.create_playlist(&name).await.unwrap();
        assert_eq!(created.name, name);

        // Add a couple of real tracks to it, then remove one, then rename,
        // then delete — never touching any other playlist.
        let page = client.search_songs_page(0, 2).await.unwrap();
        let song_ids: Vec<String> = page.iter().map(|s| s.id.clone()).collect();
        client.update_playlist_add_songs(&created.id, &song_ids).await.unwrap();

        let detail = client.get_playlist(&created.id).await.unwrap();
        assert_eq!(detail.entry.len(), song_ids.len());

        client.update_playlist_remove_indices(&created.id, &[0]).await.unwrap();
        let detail = client.get_playlist(&created.id).await.unwrap();
        assert_eq!(detail.entry.len(), song_ids.len() - 1);

        // Reorder primitive: replace the whole list in one request.
        let reversed: Vec<String> = song_ids.iter().rev().cloned().collect();
        client.update_playlist_replace(&created.id, &[0], &reversed).await.unwrap();
        let detail = client.get_playlist(&created.id).await.unwrap();
        let got: Vec<String> = detail.entry.iter().map(|s| s.id.clone()).collect();
        assert_eq!(got, reversed);

        let renamed = format!("{name}_renamed");
        client.update_playlist_rename(&created.id, &renamed).await.unwrap();

        client.delete_playlist(&created.id).await.unwrap();

        // Confirm deletion.
        let remaining = client.get_playlists().await.unwrap();
        assert!(!remaining.iter().any(|p| p.id == created.id));
    }

    /// Safe by construction: `submission=false` is Subsonic's "now playing"
    /// ping, which never writes play history/count — this is the one
    /// `scrobble` call the task's safety rule allows running for real.
    #[tokio::test]
    #[ignore]
    async fn integration_scrobble_now_playing_does_not_error() {
        let (url, user, pass) = dev_server_env();
        let client = Client::new(&url, &user, &pass).unwrap();
        let page = client.search_songs_page(0, 1).await.unwrap();
        let song = page.first().expect("dev server has at least one track");
        client.scrobble(&song.id, false, None).await.unwrap();
    }

    /// Confirms the real dev server's OpenSubsonic extension list (used by
    /// `scrobble::` to pick playbackReport vs. the classic fallback), and
    /// exercises `reportPlayback` with `ignoreScrobble: true` so it never
    /// creates a real history entry even if the extension is present.
    #[tokio::test]
    #[ignore]
    async fn integration_open_subsonic_extensions_and_playback_report_are_safe() {
        let (url, user, pass) = dev_server_env();
        let client = Client::new(&url, &user, &pass).unwrap();
        let extensions = client.get_open_subsonic_extensions().await.unwrap();
        println!("extensions: {extensions:?}");

        let page = client.search_songs_page(0, 1).await.unwrap();
        let song = page.first().expect("dev server has at least one track");
        client
            .report_playback(&song.id, PlaybackReportState::Starting, 0, true)
            .await
            .unwrap();
    }
}
