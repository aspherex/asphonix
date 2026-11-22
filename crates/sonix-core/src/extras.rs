//! Read-only discovery and player extras: lyrics, artist top tracks / info (the radio pool is `continuity::radio_pool`).
//! Plain async functions over `(&Db, Option<Client>)`.
//! Everything here only reads from the server (`getLyrics*`, `getTopSongs`, `getArtistInfo2`, `getSimilarSongs*`).

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::discovery::{
    cache_is_fresh, now_ms, resolve_top, sanitize_bio, ArtistInfo, ArtistTopTracks, CachedInfo, CachedTop, MAX_SIMILAR, TOP_SONGS_COUNT,
};
use crate::ipc::{Artist, IpcError, IpcErrorCode, IpcResult, Track};
use crate::library::{self, Db};
use crate::subsonic::lyrics::ParsedLyrics;
use crate::subsonic::Client;


// ---------------------------------------------------------------------------
// Lyrics
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LyricLine {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_ms: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Lyrics {
    /// Every line carries `startMs`.
    pub synced: bool,
    pub lines: Vec<LyricLine>,
    /// `opensubsonic` (getLyricsBySongId) or `classic` (getLyrics).
    pub source: String,
}

pub fn to_lyrics(p: ParsedLyrics, source: &str) -> Lyrics {
    Lyrics {
        synced: p.synced,
        lines: p.lines.into_iter().map(|l| LyricLine { start_ms: l.start_ms, text: l.text }).collect(),
        source: source.to_string(),
    }
}

/// Whether the server advertises `songLyrics`, remembered per server URL so
/// an account switch to another server probes again (one cheap call).
static SONG_LYRICS: Mutex<Option<(String, bool)>> = Mutex::new(None);

const LYRICS_TTL_FOUND_MS: i64 = 7 * 24 * 3600 * 1000;
const LYRICS_TTL_NONE_MS: i64 = 3600 * 1000;

async fn song_lyrics_supported(client: &Client) -> Option<bool> {
    let server = client.base_url().as_str().to_string();
    if let Some((s, yes)) = SONG_LYRICS.lock().unwrap_or_else(|p| p.into_inner()).as_ref()
        && *s == server {
            return Some(*yes);
        }
    let exts = client.get_open_subsonic_extensions().await.ok()?;
    let yes = exts.iter().any(|e| e.eq_ignore_ascii_case("songLyrics"));
    *SONG_LYRICS.lock().unwrap_or_else(|p| p.into_inner()) = Some((server, yes));
    Some(yes)
}

/// `Err(())` = could not ask (network): not cached. `Ok(None)` = the server has none.
/// Order: OpenSubsonic `getLyricsBySongId` (when `songLyrics` is advertised or unknown), then classic `getLyrics`.
async fn fetch_lyrics(client: &Client, track: &Track) -> Result<Option<Lyrics>, ()> {
    let ext = song_lyrics_supported(client).await;
    let mut failed = ext.is_none();
    if ext != Some(false) {
        match client.get_lyrics_by_song_id(&track.id).await {
            Ok(Some(l)) => return Ok(Some(to_lyrics(l, "opensubsonic"))),
            Ok(None) if ext == Some(true) => return Ok(None),
            Ok(None) => failed = false,
            Err(e) => {
                log::debug!("getLyricsBySongId failed: {}", e.message);
                failed = true;
            }
        }
    }
    if track.artist.is_empty() || track.title.is_empty() {
        return if failed { Err(()) } else { Ok(None) };
    }
    match client.get_lyrics(&track.artist, &track.title).await {
        Ok(l) => Ok(l.map(|l| to_lyrics(l, "classic"))),
        Err(e) => {
            log::debug!("getLyrics failed: {}", e.message);
            Err(())
        }
    }
}

/// Lyrics for a track, or `None` when there are none (or the server cannot be asked).
/// Cached in the local DB (7 days, 1 hour for "none").
pub async fn lyrics_get(db: &Db, client: Option<Client>, track_id: &str) -> IpcResult<Option<Lyrics>> {
    let key = format!("lyrics:{track_id}");
    let now = now_ms();
    if let Some((json, at)) = library::discovery::cache_get(db, &key)?
        && let Ok(cached) = serde_json::from_str::<Option<Lyrics>>(&json) {
            let ttl = if cached.is_some() { LYRICS_TTL_FOUND_MS } else { LYRICS_TTL_NONE_MS };
            if now >= at && now - at < ttl {
                return Ok(cached);
            }
        }
    let Some(track) = library::get_tracks_by_ids(db, std::slice::from_ref(&track_id.to_string()))?.into_iter().next() else {
        return Ok(None);
    };
    let Some(client) = client else { return Ok(None) };
    match fetch_lyrics(&client, &track).await {
        Ok(found) => {
            if let Ok(json) = serde_json::to_string(&found) {
                let _ = library::discovery::cache_put(db, &key, &json, now);
            }
            Ok(found)
        }
        Err(()) => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Artist page (discovery.rs)
// ---------------------------------------------------------------------------

/// "Popular" (server `getTopSongs`) or, when the server has nothing the local mirror knows, "Most played" from
/// local history. Served from the 7-day cache when fresh; a network failure falls back to a stale cache, then to
/// local data: never an error, never fabricated.
pub async fn artist_top_tracks(db: &Db, client: Option<Client>, id: &str) -> IpcResult<ArtistTopTracks> {
    let key = format!("top:{id}");
    let now = now_ms();
    let entry = library::discovery::cache_get(db, &key)?;
    let parsed = entry.and_then(|(json, at)| serde_json::from_str::<CachedTop>(&json).ok().map(|c| (c, at)));
    let fresh = parsed.as_ref().map(|(c, at)| cache_is_fresh(*at, now, c.ids.is_empty())).unwrap_or(false);
    let mut ids = parsed.map(|(c, _)| c.ids).unwrap_or_default();
    if !fresh
        && let Some(client) = client {
            let name = library::get_artist_detail(db, id)?.map(|d| d.artist.name);
            if let Some(name) = name {
                match client.get_top_songs(&name, TOP_SONGS_COUNT).await {
                    Ok(songs) => {
                        ids = songs.into_iter().map(|s| s.id).collect();
                        if let Ok(json) = serde_json::to_string(&CachedTop { ids: ids.clone() }) {
                            let _ = library::discovery::cache_put(db, &key, &json, now);
                        }
                    }
                    Err(e) => log::debug!("getTopSongs failed: {}", e.message),
                }
            }
        }
    let local = library::get_tracks_by_ids(db, &ids)?;
    let fallback = if local.is_empty() { library::discovery::artist_most_played(db, id, TOP_SONGS_COUNT)? } else { Vec::new() };
    let (source, items) = resolve_top(&ids, local, fallback);
    Ok(ArtistTopTracks { source, total: items.len() as u64, offset: 0, items })
}

/// Biography (sanitized plain text) and similar artists present in the local library. Cached 7 days;
/// offline/no-agent yields an empty result.
pub async fn artist_info(db: &Db, client: Option<Client>, id: &str) -> IpcResult<ArtistInfo> {
    let key = format!("info:{id}");
    let now = now_ms();
    let entry = library::discovery::cache_get(db, &key)?;
    let parsed = entry.as_ref().and_then(|(json, at)| serde_json::from_str::<CachedInfo>(json).ok().map(|c| (c, *at)));
    let fresh = parsed.as_ref().map(|(c, at)| cache_is_fresh(*at, now, c.biography.is_none() && c.similar.is_empty())).unwrap_or(false);
    let mut info = parsed.map(|(c, _)| c).unwrap_or_default();
    if !fresh
        && let Some(client) = client {
            match client.get_artist_info2(id).await {
                Ok(raw) => {
                    info = CachedInfo {
                        biography: raw.biography.as_deref().and_then(sanitize_bio),
                        similar: raw.similar_artist.into_iter().map(|s| (s.id, s.name)).collect(),
                    };
                    if let Ok(json) = serde_json::to_string(&info) {
                        let _ = library::discovery::cache_put(db, &key, &json, now);
                    }
                }
                Err(e) => log::debug!("getArtistInfo2 failed: {}", e.message),
            }
        }
    let mut similar: Vec<Artist> = Vec::new();
    for (sid, name) in &info.similar {
        if let Some(a) = library::discovery::find_artist_by_id_or_name(db, sid.as_deref(), name)?
            && a.id != id && !similar.iter().any(|s| s.id == a.id) {
                similar.push(a);
            }
        if similar.len() >= MAX_SIMILAR {
            break;
        }
    }
    Ok(ArtistInfo { biography: info.biography, similar })
}

// ---------------------------------------------------------------------------
// Radio: the pool is `crate::continuity::radio_pool`
// ---------------------------------------------------------------------------

/// `radio_start`'s error when the pool is empty.
pub fn no_similar() -> IpcError {
    IpcError { code: IpcErrorCode::NotFound, message: "no similar tracks found for this radio seed".into() }
}
