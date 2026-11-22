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

use crate::ipc::{IpcError, Paged, PlaybackStatus, QueueItemOrigin, SourceKind};
#[allow(unused_imports)]
use crate::ipc::IpcResult;
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
                } else if matches!(label, Some((SourceKind::Radio, l)) if l.starts_with(crate::config::AUTOPLAY_LABEL_PREFIX)) {
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
            tracing::warn!(error = %e.message, "failed to record feedback event");
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
                                tracing::warn!(error = %e.message, "failed to record listen event");
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


#[cfg(test)]
mod tests {
    use super::*;

    const THR: u32 = 30;

    fn sample<'a>(track: Option<&'a str>, status: PlaybackStatus, pos: f64, dur: f64) -> Sample<'a> {
        Sample { track_id: track, status, position_sec: pos, duration_sec: dur, entry_origin: Some(QueueItemOrigin::Source), source: None }
    }

    /// Plays `track` in 250 ms ticks from `from` to `to`, returning any
    /// sessions closed on the way (none expected for one track).
    fn play(t: &mut ListenTracker, track: &str, from: f64, to: f64, dur: f64, now: &mut i64) -> Vec<ListenSession> {
        let mut out = vec![];
        let mut p = from;
        while p <= to + 1e-9 {
            *now += 250;
            out.extend(t.observe(&sample(Some(track), PlaybackStatus::Playing, p, dur), *now, THR));
            p += 0.25;
        }
        out
    }

    #[test]
    fn natural_transition_is_finished_and_counted() {
        let mut t = ListenTracker::default();
        let mut now = 1_000_000;
        assert!(play(&mut t, "a", 0.0, 100.0, 100.0, &mut now).is_empty());
        // Gapless hand-off: the next sample already shows track b, no note.
        let s = t.observe(&sample(Some("b"), PlaybackStatus::Playing, 0.0, 200.0), now + 250, THR).unwrap();
        assert_eq!(s.track_id, "a");
        assert_eq!(s.end_reason, EndReason::Finished);
        assert!(s.counted);
        assert_eq!(s.listened_ms, 100_000);
        assert_eq!(s.duration_ms, 100_000);
        assert_eq!(s.max_position_ms, 100_000);
        assert_eq!(s.origin, ListenOrigin::Source);
        // b's session is open and starts now.
        let fin = t.finish(EndReason::Quit, now + 5_000, THR).unwrap();
        assert_eq!((fin.track_id.as_str(), fin.end_reason, fin.started_at_ms), ("b", EndReason::Quit, now + 250));
    }

    #[test]
    fn each_noted_reason_labels_the_session_it_ends() {
        for reason in [EndReason::Next, EndReason::Previous, EndReason::Jump, EndReason::Error] {
            let mut t = ListenTracker::default();
            let mut now = 0;
            play(&mut t, "a", 0.0, 12.0, 200.0, &mut now);
            t.note(reason);
            // The command lands: new track is Loading.
            let s = t.observe(&sample(Some("b"), PlaybackStatus::Loading, 0.0, 0.0), now + 100, THR).unwrap();
            assert_eq!(s.end_reason, reason);
            assert!(!s.counted, "12 s of 200 s is under the threshold");
            assert_eq!(s.listened_ms, 12_000);
        }
    }

    #[test]
    fn stop_and_end_of_queue() {
        let mut t = ListenTracker::default();
        let mut now = 0;
        play(&mut t, "a", 0.0, 50.0, 60.0, &mut now);
        t.note(EndReason::Next); // Next at the end of the queue: engine just stops
        let s = t.observe(&sample(Some("a"), PlaybackStatus::Stopped, 50.0, 60.0), now + 10, THR).unwrap();
        assert_eq!(s.end_reason, EndReason::Next);
        // Queue ran out with no command: finished.
        play(&mut t, "b", 0.0, 10.0, 10.0, &mut now);
        let s = t.observe(&sample(None, PlaybackStatus::Stopped, 0.0, 0.0), now + 10, THR).unwrap();
        assert_eq!(s.end_reason, EndReason::Finished);
        // Stop command.
        play(&mut t, "c", 0.0, 3.0, 100.0, &mut now);
        assert_eq!(t.finish(EndReason::Stopped, now, THR).unwrap().end_reason, EndReason::Stopped);
    }

    #[test]
    fn pause_and_seek_never_inflate_listened_time() {
        let mut t = ListenTracker::default();
        let mut now = 0;
        play(&mut t, "a", 0.0, 10.0, 300.0, &mut now);
        // Paused for a long while.
        for _ in 0..100 {
            now += 250;
            t.observe(&sample(Some("a"), PlaybackStatus::Paused, 10.0, 300.0), now, THR);
        }
        // Seek forward to 200 s, play 5 s there.
        play(&mut t, "a", 200.0, 205.0, 300.0, &mut now);
        // Seek back to 5 s (heard before) and replay 3 s.
        play(&mut t, "a", 5.0, 8.0, 300.0, &mut now);
        let s = t.finish(EndReason::Quit, now, THR).unwrap();
        // 10 s + 5 s + 3 s of real, contiguous playback; the seeks add nothing.
        assert_eq!(s.listened_ms, 18_000);
        assert_eq!(s.max_position_ms, 205_000);
    }

    #[test]
    fn repeat_one_loops_are_separate_sessions() {
        let mut t = ListenTracker::default();
        let mut now = 0;
        play(&mut t, "a", 0.0, 40.0, 40.0, &mut now);
        // Same id restarts from zero right after its end.
        let s = t.observe(&sample(Some("a"), PlaybackStatus::Playing, 0.0, 40.0), now + 250, THR).unwrap();
        assert_eq!(s.end_reason, EndReason::Finished);
        assert!(s.counted);
        play(&mut t, "a", 0.25, 5.0, 40.0, &mut now);
        let s2 = t.finish(EndReason::Next, now, THR).unwrap();
        assert!(!s2.counted);
        assert_eq!(s2.listened_ms, 5_000);
    }

    #[test]
    fn jump_to_the_same_track_restarts_the_session() {
        let mut t = ListenTracker::default();
        let mut now = 0;
        play(&mut t, "a", 0.0, 20.0, 200.0, &mut now);
        t.note(EndReason::Jump);
        let s = t.observe(&sample(Some("a"), PlaybackStatus::Loading, 0.0, 0.0), now + 50, THR).unwrap();
        assert_eq!(s.end_reason, EndReason::Jump);
    }

    #[test]
    fn a_stale_note_never_labels_a_later_natural_end() {
        let mut t = ListenTracker::default();
        let mut now = 0;
        play(&mut t, "a", 0.0, 5.0, 100.0, &mut now);
        t.note(EndReason::Next); // command that changed nothing
        assert!(t.observe(&sample(Some("a"), PlaybackStatus::Playing, 5.25, 100.0), now + 250, THR).is_none());
        let s = t.observe(&sample(Some("b"), PlaybackStatus::Playing, 0.0, 100.0), now + 500, THR).unwrap();
        assert_eq!(s.end_reason, EndReason::Finished);
    }

    #[test]
    fn origin_user_source_autoplay_and_source_identity() {
        let mut t = ListenTracker::default();
        let src = |label| Some((SourceKind::Playlist, Some("pl1"), label));
        // Explicit start (new queue) is user, even though the entry is "source".
        t.note(EndReason::Jump);
        let mut s = sample(Some("a"), PlaybackStatus::Playing, 0.0, 100.0);
        s.source = src("Mix");
        t.observe(&s, 1, THR);
        let a = t.finish(EndReason::Next, 2, THR).unwrap();
        assert_eq!((a.origin, a.source_kind, a.source_id.as_deref()), (ListenOrigin::User, Some("playlist"), Some("pl1")));
        // User-queued entry.
        let mut s = sample(Some("b"), PlaybackStatus::Playing, 0.0, 100.0);
        s.entry_origin = Some(QueueItemOrigin::User);
        t.observe(&s, 3, THR);
        assert_eq!(t.finish(EndReason::Next, 4, THR).unwrap().origin, ListenOrigin::User);
        // Drawn from the autoplay radio.
        let mut s = sample(Some("c"), PlaybackStatus::Playing, 0.0, 100.0);
        s.source = Some((SourceKind::Radio, None, "Autoplay · Some Song"));
        t.observe(&s, 5, THR);
        assert_eq!(t.finish(EndReason::Quit, 6, THR).unwrap().origin, ListenOrigin::Autoplay);
        // Static ad-hoc queue: no source.
        t.observe(&sample(Some("d"), PlaybackStatus::Playing, 0.0, 100.0), 7, THR);
        let d = t.finish(EndReason::Quit, 8, THR).unwrap();
        assert_eq!((d.origin, d.source_kind), (ListenOrigin::Source, None));
    }

    #[test]
    fn never_audible_tracks_are_not_recorded() {
        let mut t = ListenTracker::default();
        assert!(t.observe(&sample(Some("a"), PlaybackStatus::Loading, 0.0, 0.0), 1, THR).is_none());
        assert!(t.observe(&sample(Some("a"), PlaybackStatus::Paused, 0.0, 100.0), 2, THR).is_none());
        assert!(t.finish(EndReason::Quit, 3, THR).is_none());
    }

    fn session(track: &str, started: i64, reason: EndReason, listened: i64, duration: i64) -> ListenSession {
        ListenSession {
            track_id: track.into(),
            started_at_ms: started,
            ended_at_ms: started + listened,
            listened_ms: listened,
            duration_ms: duration,
            end_reason: reason,
            source_kind: Some("playlist"),
            source_id: Some("p".into()),
            origin: ListenOrigin::Source,
            counted: listened >= 30_000,
            max_position_ms: listened,
        }
    }

    #[test]
    fn insert_prunes_to_the_newest_cap_rows() {
        let db = Db::open_in_memory().unwrap();
        for i in 0..10 {
            insert_session_capped(&db, &session("t", i, EndReason::Finished, 1000, 2000), 4).unwrap();
        }
        let page = listen_events_page(&db, &ListenRangeRequest { offset: 0, limit: 100, since: None, until: None }).unwrap();
        assert_eq!(page.total, 4);
        assert_eq!(page.items.iter().map(|e| e.started_at).collect::<Vec<_>>(), vec![9, 8, 7, 6]);
        record_feedback_capped(&db, "t", 1, FeedbackKind::Like, None, 2).unwrap();
        record_feedback_capped(&db, "t", 2, FeedbackKind::Rating, Some(5), 2).unwrap();
        record_feedback_capped(&db, "t", 3, FeedbackKind::Unlike, None, 2).unwrap();
        let fb = feedback_events_page(&db, &ListenRangeRequest { offset: 0, limit: 10, since: None, until: None }).unwrap();
        assert_eq!(fb.total, 2);
    }

    #[test]
    fn paging_window_and_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        insert_session(&db, &session("a", 100, EndReason::Next, 5_000, 200_000)).unwrap();
        insert_session(&db, &session("b", 200, EndReason::Finished, 200_000, 200_000)).unwrap();
        insert_session(&db, &session("c", 300, EndReason::Quit, 1_000, 0)).unwrap();
        let all = listen_events_page(&db, &ListenRangeRequest { offset: 0, limit: 2, since: None, until: None }).unwrap();
        assert_eq!((all.total, all.items.len(), all.items[0].track_id.as_str()), (3, 2, "c"));
        let win = listen_events_page(&db, &ListenRangeRequest { offset: 0, limit: 10, since: Some(100), until: Some(300) }).unwrap();
        assert_eq!(win.items.iter().map(|e| e.track_id.as_str()).collect::<Vec<_>>(), vec!["b", "a"]);
        let a = &win.items[1];
        assert_eq!((a.end_reason.as_str(), a.origin.as_str(), a.source_kind.as_deref(), a.counted), ("next", "source", Some("playlist"), false));
    }

    #[test]
    fn skip_stats_aggregate_per_track() {
        let db = Db::open_in_memory().unwrap();
        // Track a: skipped at 10%, skipped at 90% (not a skip), finished.
        insert_session(&db, &session("a", 1, EndReason::Next, 20_000, 200_000)).unwrap();
        insert_session(&db, &session("a", 2, EndReason::Next, 180_000, 200_000)).unwrap();
        insert_session(&db, &session("a", 3, EndReason::Finished, 200_000, 200_000)).unwrap();
        insert_session(&db, &session("b", 4, EndReason::Quit, 1_000, 0)).unwrap();
        let p = skip_stats_page(&db, &SkipStatsRequest { offset: 0, limit: 10, since: None, until: None, min_sessions: None }).unwrap();
        assert_eq!(p.total, 2);
        let a = &p.items[0];
        assert_eq!((a.track_id.as_str(), a.sessions, a.skips, a.counted, a.last_listened_at), ("a", 3, 1, 2, 3));
        assert!((a.avg_listened_fraction.unwrap() - (0.1 + 0.9 + 1.0) / 3.0).abs() < 1e-9);
        assert_eq!(p.items[1].avg_listened_fraction, None);
        let p = skip_stats_page(&db, &SkipStatsRequest { offset: 0, limit: 10, since: None, until: None, min_sessions: Some(2) }).unwrap();
        assert_eq!((p.total, p.items.len()), (1, 1));
    }

    #[test]
    fn finish_does_not_reopen_the_same_playback_and_clears_the_explicit_start() {
        let mut t = ListenTracker::default();
        let mut now = 0;
        play(&mut t, "a", 0.0, 10.0, 100.0, &mut now);
        assert_eq!(t.finish(EndReason::Quit, now, THR).unwrap().end_reason, EndReason::Quit);
        // Still playing after the exit flush (update install): no new session,
        // so the Stop that follows has nothing to close.
        assert!(play(&mut t, "a", 10.25, 11.0, 100.0, &mut now).is_empty());
        assert!(t.finish(EndReason::Stopped, now, THR).is_none());
        // A Jump note that never led to a session (the new queue never
        // played), then Stop: the next naturally drawn session is not "user".
        t.note(EndReason::Jump);
        assert!(t.observe(&sample(Some("x"), PlaybackStatus::Loading, 0.0, 0.0), now, THR).is_none());
        t.finish(EndReason::Stopped, now, THR);
        play(&mut t, "b", 0.0, 1.0, 100.0, &mut now);
        assert_eq!(t.finish(EndReason::Quit, now, THR).unwrap().origin, ListenOrigin::Source);
        // Once the held track stops playing, sessions open normally again.
        t.observe(&sample(Some("b"), PlaybackStatus::Paused, 1.0, 100.0), now, THR);
        play(&mut t, "b", 1.0, 2.0, 100.0, &mut now);
        assert!(t.finish(EndReason::Quit, now, THR).is_some());
    }

    #[test]
    fn writer_flush_answers_after_earlier_sessions_are_stored() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let tx = spawn_writer(db.clone());
        for i in 0..5 {
            tx.send(WriterMsg::Session(session("t", i, EndReason::Next, 1000, 2000))).unwrap();
        }
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        tx.send(WriterMsg::Flush(done_tx)).unwrap();
        crate::rt::block_on(done_rx).unwrap();
        let n: i64 = db.read(|c| c.query_row("SELECT COUNT(*) FROM listen_events", [], |r| r.get(0))).unwrap();
        assert_eq!(n, 5);
    }

    #[test]
    fn wire_shape_is_camel_case() {
        let e = ListenEvent {
            id: 1,
            track_id: "t".into(),
            started_at: 2,
            ended_at: 3,
            listened_ms: 4,
            duration_ms: 5,
            end_reason: "next".into(),
            source_kind: None,
            source_id: None,
            origin: "user".into(),
            counted: true,
            max_position_ms: 6,
        };
        let v = serde_json::to_value(&e).unwrap();
        for k in ["trackId", "startedAt", "endedAt", "listenedMs", "durationMs", "endReason", "sourceKind", "maxPositionMs"] {
            assert!(v.get(k).is_some(), "missing {k}");
        }
    }
}
