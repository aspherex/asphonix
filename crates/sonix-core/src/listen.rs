//! Local listening signal for a future on-device taste model: one
//! `listen_events` row per "track was audible" session (written when the
//! session ends, with *why* it ended) and one `feedback_events` row per
//! explicit like/unlike/rating. Data collection only: nothing here is sent to
//! the server, and there is no model or UI behind it yet.
//!
//! **Why the engine owns the tracker.** `PlayerState` alone cannot tell
//! "next" from "jump" from "finished", and the watch channel coalesces. So the
//! [`ListenTracker`] lives on the engine thread and is fed the very
//! `PlayerState` the engine is about to emit (every <= 250 ms while playing),
//! plus [`ListenTracker::note`]: the command handlers that change the
//! audible entry (Next/Previous/JumpTo/new queue/remove-current/error) set the
//! reason right before they mutate. A session whose track changes with no note
//! is, by elimination, a natural end: EOF or the gapless hand-off
//! (`finished`). Notes live for exactly one sample, so a command that turned
//! out to change nothing can never mislabel a later natural end. Nothing here
//! touches the render/decoder threads or gapless timing: the tracker is a few
//! comparisons per state sample, a closed session is one channel send (the
//! [`spawn_writer`] task does the SQLite insert on the blocking pool), and
//! only app exit writes synchronously (one tiny insert, after the session
//! file is saved). `Stop` goes through the writer too; logout / account
//! switch then wait for a [`WriterMsg::Flush`] before wiping the tables, so
//! no row of the old account lands after the wipe.
//!
//! Listening time reuses [`PlayThresholdTracker`] (pauses and seeks never add
//! time, seeking back never double counts) and `counted` is the same
//! threshold rule `PlayCounter` applies, so the two never disagree about what
//! a play is. Session boundaries use the same `is_new_play` repeat-one
//! detection as scrobbling: every loop is its own session.

use std::sync::Arc;

use rusqlite::params;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use crate::ipc::{IpcError, IpcResult, Paged, PlaybackStatus, QueueItemOrigin, SourceKind};
use crate::library::Db;
use crate::scrobble::is_new_play;
use crate::scrobble::threshold::PlayThresholdTracker;

/// Newest rows kept in `listen_events`: roughly 2-3 years of heavy listening
/// (~200 sessions/day). Each row is ~100 bytes, so the table stays ~20 MB.
pub const LISTEN_MAX_ROWS: i64 = 200_000;
/// Explicit feedback is far rarer; the cap only bounds a pathological loop.
pub const FEEDBACK_MAX_ROWS: i64 = 50_000;

/// A user-ended session that heard less than this share of the track is a skip.
const SKIP_UNDER_PERCENT: i64 = 80;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Why a listen session ended. Stored as TEXT (`as_str`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EndReason {
    /// Natural EOF, including the gapless hand-off and repeat-one loops.
    Finished,
    Next,
    Previous,
    /// JumpTo, a click on another track, a new queue/source started.
    Jump,
    /// The Stop command (logout, account switch).
    Stopped,
    /// App exit (or update install) while the session was open.
    Quit,
    /// Decoder/stream error ended the track.
    Error,
}

impl EndReason {
    pub fn as_str(self) -> &'static str {
        match self {
            EndReason::Finished => "finished",
            EndReason::Next => "next",
            EndReason::Previous => "previous",
            EndReason::Jump => "jump",
            EndReason::Stopped => "stopped",
            EndReason::Quit => "quit",
            EndReason::Error => "error",
        }
    }
}

/// How the track came to be playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenOrigin {
    /// Explicitly started/clicked, or user-queued.
    User,
    /// Drawn from a rolling source (or a static queue's later entries).
    Source,
    /// From the autoplay radio.
    Autoplay,
}

impl ListenOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            ListenOrigin::User => "user",
            ListenOrigin::Source => "source",
            ListenOrigin::Autoplay => "autoplay",
        }
    }
}

fn source_kind_str(k: SourceKind) -> &'static str {
    match k {
        SourceKind::Playlist => "playlist",
        SourceKind::AllTracks => "allTracks",
        SourceKind::Artist => "artist",
        SourceKind::Album => "album",
        SourceKind::Radio => "radio",
        SourceKind::Search => "search",
        SourceKind::Genre => "genre",
        SourceKind::Liked => "liked",
        SourceKind::Smart => "smart",
    }
}

/// One finished session, ready to insert.
#[derive(Debug, Clone, PartialEq)]
pub struct ListenSession {
    pub track_id: String,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
    pub listened_ms: i64,
    pub duration_ms: i64,
    pub end_reason: EndReason,
    pub source_kind: Option<&'static str>,
    pub source_id: Option<String>,
    pub origin: ListenOrigin,
    pub counted: bool,
    pub max_position_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackKind {
    Like,
    Unlike,
    Rating,
}

impl FeedbackKind {
    fn as_str(self) -> &'static str {
        match self {
            FeedbackKind::Like => "like",
            FeedbackKind::Unlike => "unlike",
            FeedbackKind::Rating => "rating",
        }
    }
}

// ---------------------------------------------------------------------------
// Tracker (pure, engine thread)
// ---------------------------------------------------------------------------

/// Borrowed view of what the engine is doing right now (no allocation).
pub struct Sample<'a> {
    pub track_id: Option<&'a str>,
    pub status: PlaybackStatus,
    pub position_sec: f64,
    pub duration_sec: f64,
    pub entry_origin: Option<QueueItemOrigin>,
    /// `(kind, id, label)` of the active rolling source.
    pub source: Option<(SourceKind, Option<&'a str>, &'a str)>,
}

struct Open {
    track_id: String,
    started_at_ms: i64,
    tracker: PlayThresholdTracker,
    last_position_sec: f64,
    max_position_sec: f64,
    duration_sec: f64,
    source_kind: Option<&'static str>,
    source_id: Option<String>,
    origin: ListenOrigin,
}

#[derive(Default)]
pub struct ListenTracker {
    open: Option<Open>,
    /// Reason for the next sample's session end; consumed by that sample.
    note: Option<EndReason>,
    /// The next session to start was explicitly chosen (new queue / jump).
    explicit_start: bool,
    /// Track whose session `finish` just ended while it kept playing (exit
    /// flush): not reopened until the track changes or stops playing, so a
    /// `PersistNow` followed by `Stop` (update install) is one row, not two.
    held: Option<String>,
}

impl ListenTracker {
    /// A command is about to change (or end) the audible entry. Call *after*
    /// feeding a sample taken before the mutation.
    pub fn note(&mut self, reason: EndReason) {
        self.note = Some(reason);
        if reason == EndReason::Jump {
            self.explicit_start = true;
        }
    }

    /// Feeds one state sample; returns the session it ended, if any.
    pub fn observe(&mut self, s: &Sample, now_ms: i64, threshold_sec: u32) -> Option<ListenSession> {
        let note = self.note.take();
        let mut closed = None;
        if let Some(o) = &self.open {
            let track_gone = s.track_id != Some(o.track_id.as_str());
            let stopped = s.status == PlaybackStatus::Stopped;
            // Same track, explicit command, and the engine reopened it
            // (Loading): Next/JumpTo/new queue landed on the same id.
            let restarted_by_command = !track_gone && note.is_some() && s.status == PlaybackStatus::Loading;
            let loop_restart = !track_gone
                && s.status != PlaybackStatus::Loading
                && is_new_play(&o.track_id, o.last_position_sec, o.duration_sec, &o.track_id, s.position_sec);
            if track_gone || stopped || restarted_by_command || loop_restart {
                closed = self.close(note.unwrap_or(EndReason::Finished), now_ms, threshold_sec);
            }
        }
        if let Some(held) = &self.held {
            if s.track_id == Some(held.as_str()) && s.status == PlaybackStatus::Playing {
                return closed;
            }
            self.held = None;
        }
        if let (Some(o), Some(_)) = (self.open.as_mut(), s.track_id) {
            {
                o.tracker.observe(s.status == PlaybackStatus::Playing, s.position_sec);
                o.last_position_sec = s.position_sec;
                o.max_position_sec = o.max_position_sec.max(s.position_sec);
                if s.duration_sec > 0.0 {
                    o.duration_sec = s.duration_sec;
                }
            }
        } else if let (None, Some(id), PlaybackStatus::Playing) = (&self.open, s.track_id, s.status) {
            {
                let (source_kind, source_id, label) = match s.source {
                    Some((k, id, label)) => (Some(source_kind_str(k)), id.map(str::to_string), Some((k, label))),
                    None => (None, None, None),
                };
                let origin = if std::mem::take(&mut self.explicit_start) || s.entry_origin == Some(QueueItemOrigin::User) {
                    ListenOrigin::User
                } else if matches!(label, Some((SourceKind::Radio, l)) if l.starts_with(crate::audio::autoplay::LABEL_PREFIX)) {
                    ListenOrigin::Autoplay
                } else {
                    ListenOrigin::Source
                };
                let mut tracker = PlayThresholdTracker::new();
                tracker.observe(true, s.position_sec);
                self.open = Some(Open {
                    track_id: id.to_string(),
                    started_at_ms: now_ms,
                    tracker,
                    last_position_sec: s.position_sec,
                    max_position_sec: s.position_sec,
                    duration_sec: s.duration_sec.max(0.0),
                    source_kind,
                    source_id,
                    origin,
                });
            }
        }
        closed
    }

    /// Ends the open session (app exit / Stop) and returns it.
    /// The same continuous playback is not reopened afterwards (`held`).
    pub fn finish(&mut self, reason: EndReason, now_ms: i64, threshold_sec: u32) -> Option<ListenSession> {
        self.note = None;
        self.explicit_start = false;
        let closed = self.close(reason, now_ms, threshold_sec);
        self.held = closed.as_ref().map(|s| s.track_id.clone());
        closed
    }

    fn close(&mut self, reason: EndReason, now_ms: i64, threshold_sec: u32) -> Option<ListenSession> {
        let o = self.open.take()?;
        let listened_sec = o.tracker.listened_sec();
        Some(ListenSession {
            counted: PlayThresholdTracker::counts(listened_sec, o.duration_sec, threshold_sec),
            track_id: o.track_id,
            started_at_ms: o.started_at_ms,
            ended_at_ms: now_ms.max(o.started_at_ms),
            listened_ms: (listened_sec * 1000.0).round() as i64,
            duration_ms: (o.duration_sec * 1000.0).round() as i64,
            end_reason: reason,
            source_kind: o.source_kind,
            source_id: o.source_id,
            origin: o.origin,
            max_position_ms: (o.max_position_sec.max(0.0) * 1000.0).round() as i64,
        })
    }
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

pub fn insert_session(db: &Db, s: &ListenSession) -> Result<(), IpcError> {
    insert_session_capped(db, s, LISTEN_MAX_ROWS)
}

fn insert_session_capped(db: &Db, s: &ListenSession, cap: i64) -> Result<(), IpcError> {
    db.with_tx(|tx| {
        tx.execute(
            "INSERT INTO listen_events (track_id, started_at, ended_at, listened_ms, duration_ms, end_reason,
                                        source_kind, source_id, origin, counted, max_position_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                s.track_id,
                s.started_at_ms,
                s.ended_at_ms,
                s.listened_ms,
                s.duration_ms,
                s.end_reason.as_str(),
                s.source_kind,
                s.source_id,
                s.origin.as_str(),
                s.counted as i64,
                s.max_position_ms
            ],
        )?;
        // Ids only grow, so "newest `cap` rows" is a PK range delete (an
        // OFFSET scan over 200k rows on every insert would not be).
        tx.execute("DELETE FROM listen_events WHERE id <= (SELECT MAX(id) FROM listen_events) - ?1", [cap])?;
        Ok(())
    })
}

pub fn record_feedback(db: &Db, track_id: &str, at_ms: i64, kind: FeedbackKind, value: Option<i64>) -> Result<(), IpcError> {
    record_feedback_capped(db, track_id, at_ms, kind, value, FEEDBACK_MAX_ROWS)
}

fn record_feedback_capped(db: &Db, track_id: &str, at_ms: i64, kind: FeedbackKind, value: Option<i64>, cap: i64) -> Result<(), IpcError> {
    db.with_tx(|tx| {
        tx.execute(
            "INSERT INTO feedback_events (track_id, at, kind, value) VALUES (?1, ?2, ?3, ?4)",
            params![track_id, at_ms, kind.as_str(), value],
        )?;
        tx.execute("DELETE FROM feedback_events WHERE id <= (SELECT MAX(id) FROM feedback_events) - ?1", [cap])?;
        Ok(())
    })
}

/// Fire-and-forget feedback row for the star/rating commands (blocking pool,
/// failures only logged: the server write already succeeded).
pub fn spawn_feedback(db: &Arc<Db>, track_id: &str, kind: FeedbackKind, value: Option<i64>) {
    let (db, id) = (db.clone(), track_id.to_string());
    let at = chrono::Utc::now().timestamp_millis();
    crate::rt::spawn_blocking(move || {
        if let Err(e) = record_feedback(&db, &id, at, kind, value) {
            log::warn!("failed to record feedback event: {}", e.message);
        }
    });
}

/// What the engine sends the writer task.
#[derive(Debug)]
pub enum WriterMsg {
    Session(ListenSession),
    /// Answered once every session sent before it is in the DB (logout /
    /// account switch wait for it before `Db::clear_all`).
    Flush(tokio::sync::oneshot::Sender<()>),
}

/// The writer task the engine's closed sessions go to (one channel send on
/// the engine thread, the insert on the blocking pool, batched when several
/// arrive together; messages are handled in order).
pub fn spawn_writer(db: Arc<Db>) -> UnboundedSender<WriterMsg> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<WriterMsg>();
    crate::rt::spawn(async move {
        while let Some(first) = rx.recv().await {
            let mut batch = vec![first];
            while let Ok(next) = rx.try_recv() {
                batch.push(next);
            }
            let db = db.clone();
            let _ = crate::rt::spawn_blocking(move || {
                for msg in batch {
                    match msg {
                        WriterMsg::Session(s) => {
                            if let Err(e) = insert_session(&db, &s) {
                                log::warn!("failed to record listen event: {}", e.message);
                            }
                        }
                        WriterMsg::Flush(reply) => {
                            let _ = reply.send(());
                        }
                    }
                }
            })
            .await;
        }
    });
    tx
}

// ---------------------------------------------------------------------------
// Read commands
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ListenEvent {
    pub id: i64,
    pub track_id: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub listened_ms: i64,
    pub duration_ms: i64,
    pub end_reason: String,
    pub source_kind: Option<String>,
    pub source_id: Option<String>,
    pub origin: String,
    pub counted: bool,
    pub max_position_ms: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackEvent {
    pub id: i64,
    pub track_id: String,
    pub at: i64,
    pub kind: String,
    pub value: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TrackSkipStats {
    pub track_id: String,
    pub sessions: i64,
    pub skips: i64,
    pub counted: i64,
    /// Mean of `min(1, listened / duration)` over sessions with a known
    /// duration; null when none had one.
    pub avg_listened_fraction: Option<f64>,
    pub last_listened_at: i64,
}

/// Paging + optional unix-ms window (`since` inclusive, `until` exclusive,
/// both on `startedAt`/`at`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListenRangeRequest {
    #[serde(default)]
    pub offset: u32,
    pub limit: u32,
    pub since: Option<i64>,
    pub until: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkipStatsRequest {
    #[serde(default)]
    pub offset: u32,
    pub limit: u32,
    pub since: Option<i64>,
    pub until: Option<i64>,
    /// Only tracks with at least this many sessions (default 1).
    pub min_sessions: Option<u32>,
}

const MAX_PAGE: u32 = 1000;

pub fn listen_events_page(db: &Db, req: &ListenRangeRequest) -> Result<Paged<ListenEvent>, IpcError> {
    let limit = req.limit.clamp(1, MAX_PAGE);
    let (since, until) = (req.since.unwrap_or(i64::MIN), req.until.unwrap_or(i64::MAX));
    db.read(|c| {
        let total: i64 =
            c.query_row("SELECT COUNT(*) FROM listen_events WHERE started_at >= ?1 AND started_at < ?2", [since, until], |r| r.get(0))?;
        let mut stmt = c.prepare_cached(
            "SELECT id, track_id, started_at, ended_at, listened_ms, duration_ms, end_reason, source_kind, source_id,
                    origin, counted, max_position_ms
             FROM listen_events WHERE started_at >= ?1 AND started_at < ?2
             ORDER BY started_at DESC, id DESC LIMIT ?3 OFFSET ?4",
        )?;
        let items = stmt
            .query_map(params![since, until, limit, req.offset], |r| {
                Ok(ListenEvent {
                    id: r.get(0)?,
                    track_id: r.get(1)?,
                    started_at: r.get(2)?,
                    ended_at: r.get(3)?,
                    listened_ms: r.get(4)?,
                    duration_ms: r.get(5)?,
                    end_reason: r.get(6)?,
                    source_kind: r.get(7)?,
                    source_id: r.get(8)?,
                    origin: r.get(9)?,
                    counted: r.get::<_, i64>(10)? != 0,
                    max_position_ms: r.get(11)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Paged { items, total: total as u64, offset: u64::from(req.offset) })
    })
}

pub fn feedback_events_page(db: &Db, req: &ListenRangeRequest) -> Result<Paged<FeedbackEvent>, IpcError> {
    let limit = req.limit.clamp(1, MAX_PAGE);
    let (since, until) = (req.since.unwrap_or(i64::MIN), req.until.unwrap_or(i64::MAX));
    db.read(|c| {
        let total: i64 = c.query_row("SELECT COUNT(*) FROM feedback_events WHERE at >= ?1 AND at < ?2", [since, until], |r| r.get(0))?;
        let mut stmt = c.prepare_cached(
            "SELECT id, track_id, at, kind, value FROM feedback_events WHERE at >= ?1 AND at < ?2
             ORDER BY at DESC, id DESC LIMIT ?3 OFFSET ?4",
        )?;
        let items = stmt
            .query_map(params![since, until, limit, req.offset], |r| {
                Ok(FeedbackEvent { id: r.get(0)?, track_id: r.get(1)?, at: r.get(2)?, kind: r.get(3)?, value: r.get(4)? })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Paged { items, total: total as u64, offset: u64::from(req.offset) })
    })
}

/// Per-track aggregate over the window, most-listened first. A *skip* is a
/// user-ended session (`next`/`previous`/`jump`) that heard under 80% of the
/// track (or of an unknown duration).
pub fn skip_stats_page(db: &Db, req: &SkipStatsRequest) -> Result<Paged<TrackSkipStats>, IpcError> {
    let limit = req.limit.clamp(1, MAX_PAGE);
    let (since, until) = (req.since.unwrap_or(i64::MIN), req.until.unwrap_or(i64::MAX));
    let min_sessions = i64::from(req.min_sessions.unwrap_or(1).max(1));
    db.read(|c| {
        let total: i64 = c.query_row(
            "SELECT COUNT(*) FROM (SELECT 1 FROM listen_events WHERE started_at >= ?1 AND started_at < ?2
                                   GROUP BY track_id HAVING COUNT(*) >= ?3)",
            params![since, until, min_sessions],
            |r| r.get(0),
        )?;
        let mut stmt = c.prepare_cached(&format!(
            "SELECT track_id, COUNT(*) AS sessions,
                    SUM(CASE WHEN end_reason IN ('next','previous','jump')
                              AND (duration_ms <= 0 OR listened_ms * 100 < duration_ms * {SKIP_UNDER_PERCENT}) THEN 1 ELSE 0 END),
                    SUM(counted),
                    AVG(CASE WHEN duration_ms > 0 THEN MIN(1.0, listened_ms * 1.0 / duration_ms) END),
                    MAX(started_at)
             FROM listen_events WHERE started_at >= ?1 AND started_at < ?2
             GROUP BY track_id HAVING COUNT(*) >= ?3
             ORDER BY sessions DESC, track_id LIMIT ?4 OFFSET ?5"
        ))?;
        let items = stmt
            .query_map(params![since, until, min_sessions, limit, req.offset], |r| {
                Ok(TrackSkipStats {
                    track_id: r.get(0)?,
                    sessions: r.get(1)?,
                    skips: r.get(2)?,
                    counted: r.get(3)?,
                    avg_listened_fraction: r.get(4)?,
                    last_listened_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Paged { items, total: total as u64, offset: u64::from(req.offset) })
    })
}

