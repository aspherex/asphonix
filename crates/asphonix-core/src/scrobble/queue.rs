//! Durable retry queue for scrobbles that failed to submit (server
//! unreachable, request error). Deliberately its *own* tiny SQLite file
//! (`scrobble_queue.sqlite3`), separate from `library::Db`: the library DB is
//! an explicitly disposable cache of the server that gets dropped and
//! recreated on a schema bump (see `library/schema.rs`), which would be the
//! wrong behavior for unsent scrobbles — those are durable local user data
//! and must survive a library schema change untouched. This module therefore
//! only ever does additive `CREATE TABLE IF NOT EXISTS`, never a destructive
//! drop/recreate.
//!
//! Retried by `scrobble::spawn`'s background loop (plain fixed interval; a
//! scrobble isn't time-critical) until it succeeds, carrying the *original*
//! listening timestamp (`time_ms`) the whole time so a submission delayed by
//! an outage still lands in the server's history at the right moment.

// `len`/`is_empty`/`get` are a small, deliberately complete query surface for
// this queue (used by tests now; `len`/`is_empty` are the natural "anything
// to do?" check a future retry-backoff policy would want) — not every method
// has a non-test caller yet.
#![allow(dead_code)]

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};

use crate::ipc::{IpcError, IpcErrorCode};

const QUEUE_FILE_NAME: &str = "scrobble_queue.sqlite3";

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS pending_scrobbles (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    track_id   TEXT NOT NULL,
    time_ms    INTEGER NOT NULL,
    queued_at  TEXT NOT NULL,
    attempts   INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    account    TEXT NOT NULL DEFAULT ''
);
"#;

#[derive(Debug, Clone, PartialEq)]
pub struct PendingScrobble {
    pub id: i64,
    pub track_id: String,
    pub time_ms: i64,
    pub attempts: i64,
}

pub struct ScrobbleQueue {
    conn: Mutex<Connection>,
}

impl ScrobbleQueue {
    pub fn open(data_dir: &Path) -> Result<Self, IpcError> {
        std::fs::create_dir_all(data_dir).map_err(io_err)?;
        let conn = Connection::open(data_dir.join(QUEUE_FILE_NAME)).map_err(sqlite_err)?;
        // A handful of tiny rows: 128 KiB of page cache is plenty (default 2 MiB).
        conn.pragma_update(None, "cache_size", -128).map_err(sqlite_err)?;
        conn.execute_batch(SCHEMA_SQL).map_err(sqlite_err)?;
        // Queues created before accounts were tracked: add the column (error = already there).
        let _ = conn.execute("ALTER TABLE pending_scrobbles ADD COLUMN account TEXT NOT NULL DEFAULT ''", []);
        Ok(Self { conn: Mutex::new(conn) })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Self {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_SQL).unwrap();
        Self { conn: Mutex::new(conn) }
    }

    /// Queues a scrobble submission of `account` (`"{username}@{url}"`) for later retry, keeping its original
    /// listening timestamp. A queued scrobble only ever replays to the account it was queued for.
    pub fn enqueue_for(&self, account: &str, track_id: &str, time_ms: i64) -> Result<(), IpcError> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO pending_scrobbles (track_id, time_ms, queued_at, account) VALUES (?1, ?2, ?3, ?4)",
            params![track_id, time_ms, now, account],
        )
        .map_err(sqlite_err)?;
        Ok(())
    }

    /// Rows queued for `account`, oldest first (retry order = scrobble order).
    pub fn pending_for(&self, account: &str) -> Result<Vec<PendingScrobble>, IpcError> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = conn
            .prepare("SELECT id, track_id, time_ms, attempts FROM pending_scrobbles WHERE account = ?1 ORDER BY id ASC")
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map(params![account], |row| {
                Ok(PendingScrobble { id: row.get(0)?, track_id: row.get(1)?, time_ms: row.get(2)?, attempts: row.get(3)? })
            })
            .map_err(sqlite_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_err)
    }

    /// Drops every row that belongs to another account (switch / logout / pre-account rows): they must never
    /// be submitted to a different server or user. Returns how many were dropped.
    pub fn purge_other_accounts(&self, account: &str) -> Result<usize, IpcError> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute("DELETE FROM pending_scrobbles WHERE account <> ?1", params![account]).map_err(sqlite_err)
    }

    #[cfg(test)]
    pub fn enqueue(&self, track_id: &str, time_ms: i64) -> Result<(), IpcError> {
        self.enqueue_for("", track_id, time_ms)
    }

    #[cfg(test)]
    pub fn pending(&self) -> Result<Vec<PendingScrobble>, IpcError> {
        self.pending_for("")
    }

    pub fn len(&self) -> Result<u64, IpcError> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row("SELECT COUNT(*) FROM pending_scrobbles", [], |r| r.get::<_, i64>(0))
            .map(|n| n as u64)
            .map_err(sqlite_err)
    }

    pub fn is_empty(&self) -> Result<bool, IpcError> {
        Ok(self.len()? == 0)
    }

    /// Removes a row once its retry succeeded.
    pub fn remove(&self, id: i64) -> Result<(), IpcError> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute("DELETE FROM pending_scrobbles WHERE id = ?1", params![id]).map_err(sqlite_err)?;
        Ok(())
    }

    /// Records a failed retry attempt (kept in the queue; the caller decides
    /// whether/when to try again).
    pub fn record_failed_attempt(&self, id: i64, error: &str) -> Result<(), IpcError> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "UPDATE pending_scrobbles SET attempts = attempts + 1, last_error = ?2 WHERE id = ?1",
            params![id, error],
        )
        .map_err(sqlite_err)?;
        Ok(())
    }

    #[cfg(test)]
    pub fn get(&self, id: i64) -> Result<Option<PendingScrobble>, IpcError> {
        use rusqlite::OptionalExtension;
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row("SELECT id, track_id, time_ms, attempts FROM pending_scrobbles WHERE id = ?1", params![id], |row| {
            Ok(PendingScrobble {
                id: row.get(0)?,
                track_id: row.get(1)?,
                time_ms: row.get(2)?,
                attempts: row.get(3)?,
            })
        })
        .optional()
        .map_err(sqlite_err)
    }
}

fn io_err(e: std::io::Error) -> IpcError {
    IpcError { code: IpcErrorCode::Io, message: e.to_string() }
}

fn sqlite_err(e: rusqlite::Error) -> IpcError {
    IpcError { code: IpcErrorCode::Io, message: format!("scrobble queue: {e}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enqueue_list_and_remove_roundtrip() {
        let q = ScrobbleQueue::open_in_memory();
        assert!(q.is_empty().unwrap());

        q.enqueue("track-1", 1_700_000_000_000).unwrap();
        q.enqueue("track-2", 1_700_000_001_000).unwrap();
        assert_eq!(q.len().unwrap(), 2);

        let pending = q.pending().unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].track_id, "track-1");
        assert_eq!(pending[0].time_ms, 1_700_000_000_000);
        assert_eq!(pending[1].track_id, "track-2");
        assert_eq!(pending[0].attempts, 0);

        q.remove(pending[0].id).unwrap();
        let remaining = q.pending().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].track_id, "track-2");
    }

    #[test]
    fn failed_attempts_increment_and_row_stays_queued() {
        let q = ScrobbleQueue::open_in_memory();
        q.enqueue("track-1", 1).unwrap();
        let id = q.pending().unwrap()[0].id;

        q.record_failed_attempt(id, "network unreachable").unwrap();
        q.record_failed_attempt(id, "network unreachable").unwrap();

        let row = q.get(id).unwrap().unwrap();
        assert_eq!(row.attempts, 2);
        // Still queued — a failed retry never drops the row.
        assert_eq!(q.len().unwrap(), 1);
    }

    #[test]
    fn retries_preserve_the_original_listening_timestamp() {
        let q = ScrobbleQueue::open_in_memory();
        let original_time_ms = 1_650_000_000_000_i64;
        q.enqueue("track-1", original_time_ms).unwrap();
        let id = q.pending().unwrap()[0].id;
        q.record_failed_attempt(id, "timeout").unwrap();
        // A retry re-reads the row rather than re-stamping "now" — confirm
        // the stored timestamp never drifts from the original.
        let row = q.get(id).unwrap().unwrap();
        assert_eq!(row.time_ms, original_time_ms);
    }


    #[test]
    fn rows_are_scoped_to_their_account_and_other_accounts_are_purged() {
        let q = ScrobbleQueue::open_in_memory();
        q.enqueue_for("a@http://one", "t1", 1).unwrap();
        q.enqueue_for("b@http://two", "t2", 2).unwrap();
        q.enqueue("legacy", 3).unwrap();
        assert_eq!(q.pending_for("a@http://one").unwrap().len(), 1);
        assert_eq!(q.pending_for("b@http://two").unwrap()[0].track_id, "t2");
        assert_eq!(q.purge_other_accounts("b@http://two").unwrap(), 2);
        assert_eq!(q.len().unwrap(), 1);
        assert!(q.pending_for("a@http://one").unwrap().is_empty());
    }
    #[test]
    fn persists_across_reopen_on_disk() {
        let dir = std::env::temp_dir().join(format!("asphonix-scrobble-queue-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        {
            let q = ScrobbleQueue::open(&dir).unwrap();
            q.enqueue("track-1", 42).unwrap();
        }
        {
            let q = ScrobbleQueue::open(&dir).unwrap();
            let pending = q.pending().unwrap();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].track_id, "track-1");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
