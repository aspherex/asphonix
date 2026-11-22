//! Local library mirror: SQLite (via `rusqlite`, bundled) + FTS5 full-text
//! index of artists, albums, tracks, playlists and stars pulled from the
//! Subsonic server.
//!
//! The UI never holds the full library in memory: every
//! browse/search command pages results straight out of this store (see
//! `queries.rs`). A background full sync populates it on first login (see
//! `sync.rs`); incremental resyncs run afterwards. Every sync page is written
//! in its own transaction so the UI thread is never blocked mid-sync and a
//! crash mid-sync leaves whole pages committed, not partial rows.

mod convert;
pub mod discovery;
pub mod flow;
mod liked;
mod queries;
mod schema;
mod sync;
pub(crate) mod mcp_queries;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::Connection;

use crate::ipc::{Album, Artist, IpcError, IpcErrorCode, Playlist, Track};

pub use sync::{run_full_sync, run_incremental_sync, scan_fingerprint, SyncProgressSink};
pub(crate) use sync::META_LAST_SYNC_AT;
#[allow(unused_imports)]
pub(crate) use sync::META_SERVER_FINGERPRINT;

pub use convert::playlist_from_raw;
pub use liked::{
    get_liked, get_liked_play_source, get_liked_summary, playlist_access, playlist_access_flags, record_local_play,
};

pub use queries::{
    delete_playlist_local, get_album_detail, get_albums, get_all_track_ids, get_artist_detail, get_artist_play_source,
    get_artist_tracks, get_artists, get_home_sections, get_playlist_distinct_covers, get_playlist_play_source, get_tracks,
    get_tracks_by_ids, playlist_detail, playlists_list, radio_fallback_tracks, rename_playlist_local, search, track_artists_for_ids,
    set_playlist_tracks_local, set_rating_local, set_starred_local, upsert_playlist_meta, RatingKindLocal, StarKindLocal,
};

const DB_FILE_NAME: &str = "library.sqlite3";

/// Thread-safe handle to the local library DB.
///
/// Two connections, WAL mode: one **writer** (sync pages, star/playlist
/// write-through, meta) and one read-only **reader** that every browse/
/// search query uses. WAL lets the reader see the last committed snapshot
/// while a sync page transaction is open on the writer, so UI queries never
/// wait on sync writes. Each lock is a plain `std::sync::Mutex` held only
/// for the duration of one synchronous SQLite call/transaction — never
/// across an `.await`.
pub struct Db {
    writer: Mutex<Connection>,
    /// `None` for the in-memory test DB (a second `:memory:` connection
    /// would be a different database); reads then go to the writer.
    reader: Option<Mutex<Connection>>,
}

impl Db {
    /// Opens (creating if needed) the library DB in `data_dir`, enables WAL +
    /// sane pragmas, and ensures the schema exists. If the file can't be
    /// opened as a valid DB (corrupt, not a DB, truncated header), it is
    /// deleted and recreated empty — it's only a cache of the server, the
    /// next background sync refills it.
    pub fn open(data_dir: &Path) -> Result<Self, IpcError> {
        std::fs::create_dir_all(data_dir).map_err(io_err)?;
        let path = data_dir.join(DB_FILE_NAME);
        match Self::open_at(&path) {
            Ok(db) => Ok(db),
            Err(e) => {
                log::warn!("library DB at {} unusable ({}); recreating", path.display(), e.message);
                remove_db_files(&path);
                Self::open_at(&path)
            }
        }
    }

    fn open_at(path: &Path) -> Result<Self, IpcError> {
        let mut writer = Connection::open(path).map_err(sqlite_err)?;
        configure_connection(&writer)?;
        schema::ensure(&mut writer).map_err(sqlite_err)?;
        let reader = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_URI,
        )
        .map_err(sqlite_err)?;
        reader.pragma_update(None, "temp_store", "MEMORY").map_err(sqlite_err)?;
        reader.pragma_update(None, "mmap_size", MMAP_SIZE).map_err(sqlite_err)?;
        Ok(Self {
            writer: Mutex::new(writer),
            reader: Some(Mutex::new(reader)),
        })
    }

    /// In-memory DB for unit tests and examples (no disk I/O) — not gated
    /// behind `#[cfg(test)]` so `examples/engine_test.rs` (an external crate
    /// against `asphonix_lib`) can also use it to exercise the audio engine
    /// without a real app data directory.
    pub fn open_in_memory() -> Result<Self, IpcError> {
        let mut conn = Connection::open_in_memory().map_err(sqlite_err)?;
        schema::ensure(&mut conn).map_err(sqlite_err)?;
        Ok(Self {
            writer: Mutex::new(conn),
            reader: None,
        })
    }

    /// Where `open()` puts the DB file under `data_dir` — exposed so
    /// tooling (perf/size measurements, integration tests) can find it
    /// without duplicating the filename constant.
    #[allow(dead_code)]
    pub fn db_file_path(data_dir: &Path) -> PathBuf {
        data_dir.join(DB_FILE_NAME)
    }

    fn lock_writer(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.writer.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Runs `f` on the read-only connection (falls back to the writer for
    /// the in-memory test DB).
    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, IpcError> {
        match &self.reader {
            Some(reader) => {
                let conn = reader.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                f(&conn).map_err(sqlite_err)
            }
            None => self.with_conn(f),
        }
    }

    /// Runs `f` on the writer connection (single writer).
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, IpcError> {
        let conn = self.lock_writer();
        f(&conn).map_err(sqlite_err)
    }

    /// Runs `f` in one write transaction on the writer connection.
    pub fn with_tx<T>(&self, f: impl FnOnce(&rusqlite::Transaction) -> rusqlite::Result<T>) -> Result<T, IpcError> {
        let mut conn = self.lock_writer();
        let tx = conn.transaction().map_err(sqlite_err)?;
        let result = f(&tx).map_err(sqlite_err)?;
        tx.commit().map_err(sqlite_err)?;
        Ok(result)
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>, IpcError> {
        self.read(|conn| {
            conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| row.get(0))
                .optional()
        })
    }

    #[allow(dead_code)] // used by tests; sync writes meta inside its own transactions
    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), IpcError> {
        self.with_conn(|conn| set_meta_tx(conn, key, value))
    }

    /// Hands SQLite's cached pages back to the allocator on both connections
    /// (`PRAGMA shrink_memory`) — called after a sync, whose bulk writes and
    /// the reads that follow it are the peak of the page caches. Best effort.
    pub fn shrink_memory(&self) {
        let _ = self.with_conn(|conn| conn.execute_batch("PRAGMA shrink_memory"));
        if let Some(reader) = &self.reader {
            let conn = reader.lock().unwrap_or_else(|p| p.into_inner());
            let _ = conn.execute_batch("PRAGMA shrink_memory");
        }
    }

    pub fn track_count(&self) -> Result<u64, IpcError> {
        self.read(|conn| conn.query_row("SELECT COUNT(*) FROM tracks", [], |row| row.get::<_, i64>(0)))
            .map(|v| v.max(0) as u64)
    }

    /// Every distinct cover-art id in the local mirror: albums first, then
    /// artists (artists may not carry art at all — rows with a `NULL`
    /// `cover_art` are simply absent from either list). Used only by
    /// `art::prefetch::Prefetcher` to walk the catalog in the background;
    /// never on any UI-facing path.
    pub fn list_art_ids(&self) -> Result<Vec<String>, IpcError> {
        self.read(|conn| {
            let mut ids = Vec::new();
            let mut stmt = conn.prepare("SELECT cover_art FROM albums WHERE cover_art IS NOT NULL ORDER BY id")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                ids.push(row.get::<_, String>(0)?);
            }
            let mut stmt = conn.prepare("SELECT cover_art FROM artists WHERE cover_art IS NOT NULL ORDER BY id")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                ids.push(row.get::<_, String>(0)?);
            }
            Ok(ids)
        })
    }

    /// Wipes every mirrored table (and FTS indexes) and the sync metadata by
    /// dropping and recreating the schema (much faster than row-by-row
    /// deletes through the FTS triggers). Used by `auth_logout` —
    /// credentials are cleared separately via `secrets::`.
    pub fn clear_all(&self) -> Result<(), IpcError> {
        let mut conn = self.lock_writer();
        schema::reset(&mut conn).map_err(sqlite_err)?;
        // Discovery data (history, artist cache) belongs to the old account.
        discovery::clear_user_data(&conn).map_err(sqlite_err)
    }
}

/// `INSERT OR REPLACE` into `meta` on any connection/transaction.
pub(crate) fn set_meta_tx(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )
    .map(|_| ())
}

use rusqlite::{OpenFlags, OptionalExtension};

fn remove_db_files(path: &Path) {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut p = path.as_os_str().to_owned();
        p.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(p));
    }
}

/// Memory-mapped I/O window per library connection (clamped to the file size by
/// SQLite): reads come straight from the OS file cache, which is shared
/// file-backed memory rather than private working set.
const MMAP_SIZE: i64 = 64 * 1024 * 1024;

fn configure_connection(conn: &Connection) -> Result<(), IpcError> {
    // `journal_mode` is the first statement that actually reads the file
    // header, so a non-DB / corrupt file fails here (-> recreate in `open`).
    conn.pragma_update(None, "journal_mode", "WAL").map_err(sqlite_err)?;
    conn.pragma_update(None, "synchronous", "NORMAL").map_err(sqlite_err)?;
    conn.pragma_update(None, "foreign_keys", "OFF").map_err(sqlite_err)?;
    conn.pragma_update(None, "temp_store", "MEMORY").map_err(sqlite_err)?;
    // Writer page cache 1 MiB (default 2 MiB): the writer only runs syncs and
    // small edits, reads go through the mmap'd reader.
    conn.pragma_update(None, "cache_size", -1024).map_err(sqlite_err)?;
    conn.pragma_update(None, "mmap_size", MMAP_SIZE).map_err(sqlite_err)?;
    // A reader holding a snapshot can make the writer's commit wait briefly;
    // never fail with SQLITE_BUSY for that.
    conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(sqlite_err)?;
    Ok(())
}

pub(crate) fn sqlite_err(e: rusqlite::Error) -> IpcError {
    IpcError {
        code: IpcErrorCode::Io,
        message: format!("database error: {e}"),
    }
}

fn io_err(e: std::io::Error) -> IpcError {
    IpcError {
        code: IpcErrorCode::Io,
        message: e.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Row <-> IPC type mapping shared by queries.rs and sync.rs (read-back after
// write, e.g. `playlists_create`).
// ---------------------------------------------------------------------------

pub(crate) fn row_to_track(row: &rusqlite::Row) -> rusqlite::Result<Track> {
    use crate::ipc::{ReplayGain, TrackFormat};
    let track_gain_db: Option<f32> = row.get("track_gain_db")?;
    let album_gain_db: Option<f32> = row.get("album_gain_db")?;
    let track_peak: Option<f32> = row.get("track_peak")?;
    let album_peak: Option<f32> = row.get("album_peak")?;
    let replay_gain = if track_gain_db.is_none() && album_gain_db.is_none() && track_peak.is_none() && album_peak.is_none()
    {
        None
    } else {
        Some(ReplayGain {
            track_gain_db,
            album_gain_db,
            track_peak,
            album_peak,
        })
    };
    Ok(Track {
        id: row.get("id")?,
        title: row.get("title")?,
        artist: row.get("artist")?,
        artist_id: row.get("artist_id")?,
        album: row.get("album")?,
        album_id: row.get("album_id")?,
        album_artist: row.get("album_artist")?,
        track_number: row.get::<_, Option<i64>>("track_number")?.map(|v| v as u32),
        disc_number: row.get::<_, Option<i64>>("disc_number")?.map(|v| v as u32),
        year: row.get("year")?,
        genre: row.get("genre")?,
        duration: row.get("duration")?,
        format: TrackFormat {
            suffix: row.get("suffix")?,
            bit_rate: row.get::<_, Option<i64>>("bit_rate")?.map(|v| v as u32),
            bit_depth: row.get::<_, Option<i64>>("bit_depth")?.map(|v| v as u8),
            sampling_rate: row.get::<_, i64>("sampling_rate")? as u32,
            channel_count: row.get::<_, i64>("channel_count")? as u8,
        },
        cover_art: row.get("cover_art")?,
        starred: row.get("starred")?,
        rating: row.get::<_, Option<i64>>("rating")?.map(|v| v as u8),
        size: row.get::<_, Option<i64>>("size")?.map(|v| v as u64),
        replay_gain,
        path: row.get("path")?,
        date_added: row.get("created")?,
    })
}

pub(crate) fn row_to_album(row: &rusqlite::Row) -> rusqlite::Result<Album> {
    Ok(Album {
        id: row.get("id")?,
        name: row.get("name")?,
        artist: row.get("artist")?,
        artist_id: row.get("artist_id")?,
        year: row.get("year")?,
        genre: row.get("genre")?,
        cover_art: row.get("cover_art")?,
        song_count: row.get::<_, i64>("song_count")? as u32,
        duration: row.get("duration")?,
        starred: row.get("starred")?,
        rating: row.get::<_, Option<i64>>("rating")?.map(|v| v as u8),
        play_count: row.get::<_, Option<i64>>("play_count")?.map(|v| v as u64),
        created: row.get("created")?,
    })
}

pub(crate) fn row_to_artist(row: &rusqlite::Row) -> rusqlite::Result<Artist> {
    Ok(Artist {
        id: row.get("id")?,
        name: row.get("name")?,
        album_count: row.get::<_, i64>("album_count")? as u32,
        cover_art: row.get("cover_art")?,
        starred: row.get("starred")?,
        rating: row.get::<_, Option<i64>>("rating")?.map(|v| v as u8),
    })
}

pub(crate) fn row_to_playlist(row: &rusqlite::Row) -> rusqlite::Result<Playlist> {
    Ok(Playlist {
        id: row.get("id")?,
        name: row.get("name")?,
        comment: row.get("comment")?,
        owner: row.get("owner")?,
        public: row.get::<_, i64>("public")? != 0,
        song_count: row.get::<_, i64>("song_count")? as u32,
        duration: row.get("duration")?,
        cover_art: row.get("cover_art")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        smart: false,
        read_only: row.get::<_, Option<i64>>("readonly")?.is_some_and(|v| v != 0),
        server_read_only: row.get::<_, Option<i64>>("readonly")?.map(|v| v != 0),
    })
}

/// Turns free-text user input into an FTS5 MATCH query that does
/// prefix-matching per word (diacritics already folded by the `unicode61
/// remove_diacritics 2` tokenizer at index time): `"foo bar"` -> `"foo"* "bar"*`.
/// Every token is double-quoted so punctuation in the input can't be
/// mis-parsed as FTS5 query syntax.
pub(crate) fn fts_prefix_query(text: &str) -> Option<String> {
    let tokens: Vec<String> = text
        .split_whitespace()
        // A token with no letter/digit (e.g. "-", "*", "'") tokenizes to
        // nothing; as an empty phrase it would only add noise, so drop it.
        .filter(|t| t.chars().any(char::is_alphanumeric))
        // Bound the work a pasted wall of text can cause.
        .take(16)
        .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_db_applies_memory_pragmas_and_shrink_memory_is_harmless() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        let writer_mmap = db.with_conn(|c| c.query_row("PRAGMA mmap_size", [], |r| r.get::<_, i64>(0))).unwrap();
        let reader_mmap = db.read(|c| c.query_row("PRAGMA mmap_size", [], |r| r.get::<_, i64>(0))).unwrap();
        let cache = db.with_conn(|c| c.query_row("PRAGMA cache_size", [], |r| r.get::<_, i64>(0))).unwrap();
        // SQLite may clamp mmap_size to its compile-time maximum, never to 0.
        assert!(writer_mmap > 0 && reader_mmap > 0, "mmap on: {writer_mmap} / {reader_mmap}");
        assert_eq!(cache, -1024);
        db.set_meta("k", "v").unwrap();
        db.shrink_memory();
        assert_eq!(db.get_meta("k").unwrap().as_deref(), Some("v"));
    }

    #[test]
    fn meta_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.get_meta("lastSyncAt").unwrap(), None);
        db.set_meta("lastSyncAt", "2026-01-01T00:00:00Z").unwrap();
        assert_eq!(db.get_meta("lastSyncAt").unwrap().as_deref(), Some("2026-01-01T00:00:00Z"));
        db.set_meta("lastSyncAt", "2026-02-01T00:00:00Z").unwrap();
        assert_eq!(db.get_meta("lastSyncAt").unwrap().as_deref(), Some("2026-02-01T00:00:00Z"));
    }

    #[test]
    fn fts_prefix_query_quotes_and_stars_each_token() {
        assert_eq!(fts_prefix_query("foo bar"), Some("\"foo\"* \"bar\"*".to_string()));
        assert_eq!(fts_prefix_query(""), None);
        assert_eq!(fts_prefix_query("a\"b"), Some("\"a\"\"b\"*".to_string()));
        assert_eq!(fts_prefix_query("- * ' \""), None);
        assert_eq!(fts_prefix_query("AC/DC -live"), Some("\"AC/DC\"* \"-live\"*".to_string()));
    }

    #[test]
    fn empty_db_has_zero_tracks() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.track_count().unwrap(), 0);
    }
}
