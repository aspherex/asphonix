//! Continuity features as plain functions: the radio pool, the production autoplay provider,
//! "Resume from server" check/apply.

use std::sync::{Arc, RwLock as StdRwLock};

use serde::Serialize;

use crate::audio::queue::{EntrySnapshot, QueueCore};
use crate::audio::session::{self, should_offer_server_queue, SessionState, SESSION_VERSION};
use crate::audio::{EngineCommand, EngineHandle, SessionStore};
use crate::ipc::{IpcError, IpcErrorCode, PlaybackStatus, QueueItemOrigin, RepeatMode, StarTargetType, Track};
use crate::library::{self, Db};
use crate::subsonic::Client;

/// Upper bound on server queue entries considered (Navidrome can store many).
const MAX_SERVER_QUEUE_ENTRIES: usize = 1000;
/// Server-side "count" requested from `getSimilarSongs`/`getSimilarSongs2`.
pub const RADIO_TRACK_COUNT: u32 = 50;
/// Below this many locally-known tracks, radio tops up from the local fallback.
pub const RADIO_MIN_TRACKS: usize = 10;

/// The radio pool for a seed (shared by `radio_start` and autoplay): server similarity first, topped up from the
/// local fallback, restricted to tracks the local mirror has, 1-star tracks removed. Parallel `(ids, artists)`.
pub async fn radio_pool(client: &Client, db: &Db, kind: StarTargetType, seed_id: &str) -> Result<(Vec<String>, Vec<Option<String>>), IpcError> {
    let local_kind = match kind {
        StarTargetType::Track => library::StarKindLocal::Track,
        StarTargetType::Album => library::StarKindLocal::Album,
        StarTargetType::Artist => library::StarKindLocal::Artist,
    };
    let from_server = match kind {
        StarTargetType::Artist => client.get_similar_songs2(seed_id, RADIO_TRACK_COUNT).await,
        StarTargetType::Track | StarTargetType::Album => client.get_similar_songs(seed_id, RADIO_TRACK_COUNT).await,
    };
    let mut track_ids: Vec<String> = match from_server {
        Ok(songs) if !songs.is_empty() => songs.into_iter().map(|s| s.id).collect(),
        _ => Vec::new(),
    };
    let known_count = if track_ids.is_empty() { 0 } else { library::get_tracks_by_ids(db, &track_ids)?.len() };
    if known_count < RADIO_MIN_TRACKS {
        let mut seen: std::collections::HashSet<String> = track_ids.iter().cloned().collect();
        for id in library::radio_fallback_tracks(db, local_kind, seed_id, RADIO_TRACK_COUNT as usize)? {
            if seen.insert(id.clone()) {
                track_ids.push(id);
            }
        }
    }
    let mut artist_by_id: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if !track_ids.is_empty() {
        let known = library::get_tracks_by_ids(db, &track_ids)?;
        let ok_ids: std::collections::HashSet<&str> = known.iter().filter(|t| t.rating != Some(1)).map(|t| t.id.as_str()).collect();
        track_ids.retain(|id| ok_ids.contains(id.as_str()));
        artist_by_id = known.into_iter().map(|t| (t.id, t.artist)).collect();
    }
    let artists: Vec<Option<String>> = track_ids.iter().map(|id| artist_by_id.get(id).cloned()).collect();
    Ok((track_ids, artists))
}

/// Feeds the engine's autoplay lookups through [`radio_pool`]. The engine only asks while `Prefs::autoplay()`.
pub struct ServerAutoplay {
    pub client: Arc<StdRwLock<Option<Client>>>,
    pub db: Arc<Db>,
}

impl crate::audio::autoplay::AutoplaySource for ServerAutoplay {
    fn similar(&self, seed_track_id: &str) -> Vec<(String, Option<String>)> {
        let client = self.client.read().unwrap_or_else(|p| p.into_inner()).clone();
        let Some(client) = client else { return Vec::new() };
        // Runs on the engine's dedicated lookup thread, never a tokio worker.
        match crate::rt::block_on(radio_pool(&client, &self.db, StarTargetType::Track, seed_track_id)) {
            Ok((ids, artists)) => ids.into_iter().zip(artists).collect(),
            Err(e) => {
                tracing::info!(error = %e.message, "autoplay lookup failed");
                Vec::new()
            }
        }
    }
}

/// A newer queue saved on the server by another client, offered (never applied automatically).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerQueueOffer {
    pub track: Track,
    pub position_sec: f64,
    pub track_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_by: Option<String>,
}

fn server_queue_ids(pq: &crate::subsonic::RawPlayQueue) -> Vec<String> {
    pq.entry.iter().take(MAX_SERVER_QUEUE_ENTRIES).map(|s| s.id.clone()).collect()
}

fn server_current(pq: &crate::subsonic::RawPlayQueue) -> Option<String> {
    pq.current.clone().or_else(|| pq.entry.first().map(|s| s.id.clone()))
}

/// `session_check_server_queue`: read-only (`getPlayQueue`). `None` when sync is off, nothing is newer, something is
/// playing right now, the server has no queue, or it is unreachable. Gated by `sync_play_queue` BEFORE any request.
pub async fn check_server_queue(
    sync_play_queue: bool,
    client: Option<Client>,
    db: &Db,
    engine: &EngineHandle,
    session: Option<&SessionStore>,
) -> Result<Option<ServerQueueOffer>, IpcError> {
    if !sync_play_queue {
        return Ok(None);
    }
    let Some(client) = client else { return Ok(None) };
    if engine.is_started()
        && let Some(ps) = engine.request(EngineCommand::GetPlayerState).await
            && ps.status == PlaybackStatus::Playing {
                return Ok(None);
            }
    let pq = match client.get_play_queue().await {
        Ok(Some(pq)) => pq,
        Ok(None) => return Ok(None),
        Err(e) => {
            tracing::debug!(error = %e.message, "getPlayQueue failed");
            return Ok(None);
        }
    };
    let Some(current) = server_current(&pq) else { return Ok(None) };
    let changed_ms = pq.changed.as_deref().and_then(session::parse_server_time_ms);
    let mark = session.map(|s| s.local_mark()).unwrap_or_default();
    if !should_offer_server_queue(&mark, Some(&current), changed_ms) {
        return Ok(None);
    }
    let Some(track) = library::get_tracks_by_ids(db, std::slice::from_ref(&current))?.into_iter().next() else {
        return Ok(None);
    };
    Ok(Some(ServerQueueOffer {
        track,
        position_sec: pq.position.unwrap_or(0) as f64 / 1000.0,
        track_count: pq.entry.len().min(MAX_SERVER_QUEUE_ENTRIES) as u32,
        changed_at: pq.changed.clone(),
        changed_by: pq.changed_by.clone(),
    }))
}

/// `session_resume_from_server`: replaces the local queue with the server's saved one (static order, shuffle off)
/// at its saved position, and plays. Read-only against the server (`getPlayQueue`).
pub async fn resume_from_server(client: Option<Client>, engine: &EngineHandle) -> Result<(), IpcError> {
    let client = client.ok_or_else(|| IpcError { code: IpcErrorCode::NotConnected, message: "not connected to a server".into() })?;
    let pq = client
        .get_play_queue()
        .await?
        .ok_or_else(|| IpcError { code: IpcErrorCode::NotFound, message: "the server has no saved play queue".into() })?;
    let ids = server_queue_ids(&pq);
    let current = server_current(&pq);
    let current_idx = current.as_ref().and_then(|c| ids.iter().position(|id| id == c)).or(Some(0)).filter(|_| !ids.is_empty());
    let queue = QueueCore {
        entries: ids.iter().map(|id| EntrySnapshot { id: id.clone(), origin: QueueItemOrigin::Source }).collect(),
        current: current_idx,
        shuffle: false,
        repeat: RepeatMode::Off, // replaced by the engine's own repeat mode
        static_natural_order: ids,
        source: None,
    };
    let state = SessionState {
        version: SESSION_VERSION,
        saved_at_ms: chrono::Utc::now().timestamp_millis(),
        position_ms: pq.position.unwrap_or(0),
        volume: None, // marks "from server": volume is left alone
        queue,
    };
    engine.send(EngineCommand::RestoreSession { state: Box::new(state), pools: None, force: true });
    Ok(())
}
