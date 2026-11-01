//! Local-DB side of the discovery features: genre browsing, listening
//! history and the artist-page network cache.
//!
//! The library DB is a disposable server mirror that `schema::ensure` drops
//! and recreates on a version bump, but **listening history is user data
//! that exists nowhere else**. So the tables here live outside the versioned
//! schema: `CREATE ... IF NOT EXISTS` run by [`ensure_extras`] on every open
//! (a handful of no-op statements), never dropped by `schema::DROP_SQL`, and
//! only wiped explicitly by logout/account switch ([`clear_user_data`]).
//! History rows reference `tracks.id` without a foreign key; every read
//! joins `tracks`, so a track that vanished from the library simply doesn't
//! show.

use rusqlite::{params, Connection, OptionalExtension};

use crate::discovery::{Genre, GenreSort, HistoryEntry, HistoryItem};
use crate::ipc::{IpcError, Paged, Track};

use super::{row_to_track, Db};

/// Rows of listening history kept; older ones are pruned on insert.
pub const HISTORY_MAX_ROWS: i64 = 10_000;

/// `(id, artist)` rows for a rolling-window play source.
pub type PlaySourceIds = Vec<(String, String)>;

const EXTRAS_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS idx_tracks_genre ON tracks(genre);
CREATE TABLE IF NOT EXISTS play_history (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    track_id  TEXT NOT NULL,
    played_at INTEGER NOT NULL, -- unix ms
    scrobbled INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_play_history_played_at ON play_history(played_at);
CREATE INDEX IF NOT EXISTS idx_play_history_track ON play_history(track_id);
-- listen-events block: richer local signal for a future on-device taste model.
-- Same rules as play_history: user data, never in DROP_SQL, wiped by logout.
CREATE TABLE IF NOT EXISTS listen_events (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    track_id        TEXT NOT NULL,
    started_at      INTEGER NOT NULL, -- unix ms, wall clock
    ended_at        INTEGER NOT NULL,
    listened_ms     INTEGER NOT NULL, -- real listening (no pauses, no seeks)
    duration_ms     INTEGER NOT NULL,
    end_reason      TEXT NOT NULL,    -- finished|next|previous|jump|stopped|quit|error
    source_kind     TEXT,             -- queue Source kind, NULL = static ad-hoc queue
    source_id       TEXT,
    origin          TEXT NOT NULL,    -- user|source|autoplay
    counted         INTEGER NOT NULL DEFAULT 0,
    max_position_ms INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_listen_events_started ON listen_events(started_at);
CREATE INDEX IF NOT EXISTS idx_listen_events_track ON listen_events(track_id);
CREATE TABLE IF NOT EXISTS feedback_events (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    track_id TEXT NOT NULL,
    at       INTEGER NOT NULL, -- unix ms
    kind     TEXT NOT NULL,    -- like|unlike|rating
    value    INTEGER           -- rating 0-5 (0 = cleared), NULL for like/unlike
);
CREATE INDEX IF NOT EXISTS idx_feedback_events_at ON feedback_events(at);
-- flow block: per-track sequencing features (library/flow.rs). Survives
-- schema bumps and resyncs so future analysis results are never thrown away;
-- wiped by logout.
CREATE TABLE IF NOT EXISTS track_features (
    track_id    TEXT PRIMARY KEY,
    bpm         REAL,
    key         TEXT,             -- Camelot ("8A"), NULL when unknown
    energy      REAL,             -- 0..1, NULL when unknown
    loudness_db REAL,             -- estimated from ReplayGain (reference -18), NULL when unknown
    source      TEXT NOT NULL,    -- 'tag' | 'analysis' | ...
    updated_at  INTEGER NOT NULL  -- unix ms
);
CREATE TABLE IF NOT EXISTS artist_cache (
    key        TEXT PRIMARY KEY, -- "top:<artistId>" / "info:<artistId>"
    json       TEXT NOT NULL,
    fetched_at INTEGER NOT NULL  -- unix ms
);
"#;

/// Creates the discovery tables/indexes if missing. Called from
/// `schema::ensure`/`schema::reset` (the `tracks` table must exist first).
pub fn ensure_extras(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(EXTRAS_SQL)
}

/// Logout / account switch: history and cached artist info belong to the
/// previous account's library.
pub fn clear_user_data(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("DELETE FROM play_history; DELETE FROM artist_cache; DELETE FROM listen_events; DELETE FROM feedback_events; DELETE FROM track_features;")
}

// ---------------------------------------------------------------------------
// Genres
// ---------------------------------------------------------------------------

/// Every genre with real album/track counts, from the local mirror. Genres
/// are the single `genre` string the server reports per song/album (see
/// `CLAUDE.md`, "Discovery" for why that is not normalised further). The
/// list is small (tens to a few hundred), so it is aggregated in one
/// grouped query (`idx_tracks_genre`/`idx_albums_genre_name` make both
/// scans index-only) and sliced for paging.
pub fn get_genres(db: &Db, offset: u32, limit: u32, sort: GenreSort) -> Result<Paged<Genre>, IpcError> {
    let all = db.read(|conn| {
        let mut stmt = conn.prepare_cached(
            "SELECT g.genre, COALESCE(a.n, 0), g.n
             FROM (SELECT genre, COUNT(*) AS n FROM tracks WHERE genre IS NOT NULL AND genre <> '' GROUP BY genre) g
             LEFT JOIN (SELECT genre, COUNT(*) AS n FROM albums WHERE genre IS NOT NULL AND genre <> '' GROUP BY genre) a
               ON a.genre = g.genre",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Genre { name: r.get(0)?, album_count: r.get::<_, i64>(1)? as u32, track_count: r.get::<_, i64>(2)? as u32 })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })?;
    Ok(page_genres(all, offset, limit, sort))
}

/// Sorting + slicing split out so it is unit-testable without a DB.
pub fn page_genres(mut all: Vec<Genre>, offset: u32, limit: u32, sort: GenreSort) -> Paged<Genre> {
    match sort {
        GenreSort::Tracks => all.sort_by(|a, b| {
            b.track_count.cmp(&a.track_count).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        }),
        GenreSort::Alphabetical => all.sort_by_key(|g| g.name.to_lowercase()),
    }
    let total = all.len() as u64;
    let items = all.into_iter().skip(offset as usize).take(limit as usize).collect();
    Paged { items, total, offset: offset as u64 }
}

pub fn get_genre(db: &Db, name: &str) -> Result<Option<Genre>, IpcError> {
    db.read(|conn| {
        let tracks: i64 = conn.query_row("SELECT COUNT(*) FROM tracks WHERE genre = ?1", [name], |r| r.get(0))?;
        if tracks == 0 {
            return Ok(None);
        }
        let albums: i64 = conn.query_row("SELECT COUNT(*) FROM albums WHERE genre = ?1", [name], |r| r.get(0))?;
        Ok(Some(Genre { name: name.to_string(), album_count: albums as u32, track_count: tracks as u32 }))
    })
}

/// `(label, [(id, artist)])` for "Play/Shuffle genre" (`player_play_genre`),
/// same narrow shape and 1★ filtering (explicit start kept) as
/// `get_artist_play_source`. Stored order is artist/album/disc/track.
pub fn get_genre_play_source(
    db: &Db,
    name: &str,
    start_track_id: Option<&str>,
) -> Result<Option<(String, PlaySourceIds)>, IpcError> {
    db.read(|conn| {
        let mut stmt = conn.prepare_cached(
            "SELECT id, artist FROM tracks WHERE genre = ?1 AND (rating IS NOT 1 OR id = ?2)
             ORDER BY artist COLLATE NOCASE, album COLLATE NOCASE, COALESCE(disc_number, 1), COALESCE(track_number, 0), title COLLATE NOCASE",
        )?;
        let items = stmt
            .query_map(params![name, start_track_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if items.is_empty() {
            return Ok(None);
        }
        Ok(Some((name.to_string(), items)))
    })
}

// ---------------------------------------------------------------------------
// Listening history
// ---------------------------------------------------------------------------

/// Records one play (the track crossed the scrobble threshold) and prunes to
/// the newest [`HISTORY_MAX_ROWS`] rows. `scrobbled` = scrobbling to the
/// server was enabled for this play (the row is recorded either way).
pub fn history_record(db: &Db, track_id: &str, played_at_ms: i64, scrobbled: bool) -> Result<(), IpcError> {
    history_record_capped(db, track_id, played_at_ms, scrobbled, HISTORY_MAX_ROWS)
}

fn history_record_capped(db: &Db, track_id: &str, played_at_ms: i64, scrobbled: bool, cap: i64) -> Result<(), IpcError> {
    db.with_tx(|tx| {
        tx.execute(
            "INSERT INTO play_history (track_id, played_at, scrobbled) VALUES (?1, ?2, ?3)",
            params![track_id, played_at_ms, scrobbled as i64],
        )?;
        // Delete everything older than the cap-th newest row (a PK-index seek).
        tx.execute(
            "DELETE FROM play_history WHERE id <= (SELECT id FROM play_history ORDER BY id DESC LIMIT 1 OFFSET ?1)",
            [cap],
        )?;
        Ok(())
    })
}

pub fn history_clear(db: &Db) -> Result<(), IpcError> {
    db.with_conn(|conn| conn.execute("DELETE FROM play_history", []).map(|_| ()))
}

/// The most recently played distinct tracks (Home's "Jump back in"): one
/// indexed query over the newest 200 history rows, joined to `tracks`.
pub fn history_recent_tracks(db: &Db, limit: u32) -> Result<Paged<HistoryEntry>, IpcError> {
    let items = db.read(|conn| {
        let mut stmt = conn.prepare_cached(
            "SELECT t.*, MAX(h.played_at) AS last_played
             FROM (SELECT track_id, played_at FROM play_history ORDER BY id DESC LIMIT 200) h
             JOIN tracks t ON t.id = h.track_id
             GROUP BY h.track_id ORDER BY last_played DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map([limit as i64], |r| Ok(HistoryEntry { played_at: r.get("last_played")?, track: row_to_track(r)? }))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })?;
    let total = items.len() as u64;
    Ok(Paged { items, total, offset: 0 })
}

/// `(local day "YYYY-MM-DD", plays)` newest day first, for tracks that still
/// exist. Days are computed by SQLite's `'localtime'` modifier, i.e. the OS
/// time zone *with its DST rules applied per timestamp* (a play from last
/// winter lands on its own local day, not shifted by today's offset).
fn history_days(conn: &Connection) -> rusqlite::Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT date(h.played_at / 1000, 'unixepoch', 'localtime') AS d, COUNT(*)
         FROM play_history h JOIN tracks t ON t.id = h.track_id
         GROUP BY d ORDER BY d DESC",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// One slice of the flattened day-header + track list.
#[derive(Debug, PartialEq, Eq)]
pub enum Slot<D> {
    Header { day: D, count: i64 },
    /// `count` track rows starting at track index `skip` of the whole
    /// newest-first history (local days are monotonic in time, so a day's
    /// rows are one contiguous run of that ordering).
    Tracks { skip: i64, count: i64 },
}

/// Pure layout: the history is shown as `[header, track, track, header, ...]`
/// (one header per day, newest first). Returns the slots covering the flat
/// window `[offset, offset + limit)` plus the flat total.
pub fn layout_window<D: Clone>(days: &[(D, i64)], offset: i64, limit: i64) -> (Vec<Slot<D>>, i64) {
    let end = offset + limit;
    let mut slots = Vec::new();
    let mut start = 0i64; // flat index of this day's header
    let mut tracks_before = 0i64; // track rows in all newer days
    for (day, count) in days {
        let count = *count;
        let tracks_start = start + 1;
        let day_end = tracks_start + count;
        if offset <= start && start < end {
            slots.push(Slot::Header { day: day.clone(), count });
        }
        let lo = offset.max(tracks_start);
        let hi = end.min(day_end);
        if lo < hi {
            slots.push(Slot::Tracks { skip: tracks_before + (lo - tracks_start), count: hi - lo });
        }
        start = day_end;
        tracks_before += count;
    }
    (slots, start)
}

/// A page of the day-grouped history (headers interleaved with track rows),
/// newest first, grouped by the user's local day. `Paged.total` counts
/// headers and tracks.
pub fn history_page(db: &Db, offset: u32, limit: u32) -> Result<Paged<HistoryItem>, IpcError> {
    db.read(|conn| {
        let days = history_days(conn)?;
        let (slots, total) = layout_window(&days, offset as i64, limit as i64);
        let mut items = Vec::new();
        let mut stmt = conn.prepare_cached(
            "SELECT t.*, h.played_at AS played_at FROM play_history h JOIN tracks t ON t.id = h.track_id
             ORDER BY h.played_at DESC, h.id DESC LIMIT ?1 OFFSET ?2",
        )?;
        for slot in slots {
            match slot {
                Slot::Header { day, count } => items.push(HistoryItem::Day { day, count: count as u32 }),
                Slot::Tracks { skip, count } => {
                    let rows = stmt
                        .query_map(params![count, skip], |r| {
                            Ok(HistoryItem::Track { played_at: r.get("played_at")?, track: row_to_track(r)? })
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    items.extend(rows);
                }
            }
        }
        Ok(Paged { items, total: total.max(0) as u64, offset: offset as u64 })
    })
}

/// The artist's most-played tracks by local play history — the honest
/// fallback when the server has no "popular" list. Empty if never played.
pub fn artist_most_played(db: &Db, artist_id: &str, limit: u32) -> Result<Vec<Track>, IpcError> {
    db.read(|conn| {
        let mut stmt = conn.prepare_cached(
            "SELECT t.*, COUNT(*) AS plays, MAX(h.played_at) AS last_played
             FROM play_history h JOIN tracks t ON t.id = h.track_id
             WHERE t.artist_id = ?1 GROUP BY t.id ORDER BY plays DESC, last_played DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![artist_id, limit as i64], row_to_track)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

// ---------------------------------------------------------------------------
// Artist-page cache
// ---------------------------------------------------------------------------

pub fn cache_get(db: &Db, key: &str) -> Result<Option<(String, i64)>, IpcError> {
    db.read(|conn| {
        conn.query_row("SELECT json, fetched_at FROM artist_cache WHERE key = ?1", [key], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
    })
}

pub fn cache_put(db: &Db, key: &str, json: &str, now_ms: i64) -> Result<(), IpcError> {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO artist_cache (key, json, fetched_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET json = excluded.json, fetched_at = excluded.fetched_at",
            params![key, json, now_ms],
        )
        .map(|_| ())
    })
}

/// Artist-info lookups by name fallback for similar artists: exact id first,
/// else case-insensitive name. `None` when the artist is not in the library.
pub fn find_artist_by_id_or_name(db: &Db, id: Option<&str>, name: &str) -> Result<Option<crate::ipc::Artist>, IpcError> {
    db.read(|conn| {
        if let Some(id) = id {
            let hit = conn.query_row("SELECT * FROM artists WHERE id = ?1", [id], super::row_to_artist).optional()?;
            if hit.is_some() {
                return Ok(hit);
            }
        }
        conn.query_row("SELECT * FROM artists WHERE name = ?1 COLLATE NOCASE LIMIT 1", [name], super::row_to_artist).optional()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(db: &Db) {
        db.with_tx(|tx| {
            tx.execute("INSERT INTO artists (id, name, album_count) VALUES ('ar1','Alpha',1),('ar2','Beta',1)", [])?;
            tx.execute(
                "INSERT INTO albums (id, name, artist, artist_id, genre, song_count, duration) VALUES
                 ('al1','A1','Alpha','ar1','Rock',2,300),('al2','B1','Beta','ar2','Rock',1,100),('al3','B2','Beta','ar2','Jazz',1,100)",
                [],
            )?;
            tx.execute(
                "INSERT INTO tracks (id, title, artist, artist_id, album, album_id, genre, rating, duration, suffix, sampling_rate, channel_count)
                 VALUES
                 ('t1','One','Alpha','ar1','A1','al1','Rock',NULL,100,'flac',44100,2),
                 ('t2','Two','Alpha','ar1','A1','al1','Rock',1,100,'flac',44100,2),
                 ('t3','Three','Beta','ar2','B1','al2','Rock',NULL,100,'flac',44100,2),
                 ('t4','Four','Beta','ar2','B2','al3','Jazz',NULL,100,'flac',44100,2),
                 ('t5','Five','Beta','ar2','B2','al3',NULL,NULL,100,'flac',44100,2)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn genres_have_real_counts_and_sort_both_ways() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        let by_tracks = get_genres(&db, 0, 10, GenreSort::Tracks).unwrap();
        assert_eq!(by_tracks.total, 2, "tracks without a genre are not a genre");
        assert_eq!(by_tracks.items[0], Genre { name: "Rock".into(), album_count: 2, track_count: 3 });
        assert_eq!(by_tracks.items[1], Genre { name: "Jazz".into(), album_count: 1, track_count: 1 });
        let alpha = get_genres(&db, 0, 10, GenreSort::Alphabetical).unwrap();
        assert_eq!(alpha.items[0].name, "Jazz");
        let page2 = get_genres(&db, 1, 1, GenreSort::Tracks).unwrap();
        assert_eq!((page2.items.len(), page2.total, page2.offset), (1, 2, 1));
        assert!(get_genre(&db, "Nope").unwrap().is_none());
        assert_eq!(get_genre(&db, "Rock").unwrap().unwrap().track_count, 3);
    }

    #[test]
    fn genre_play_source_drops_one_star_unless_explicit_start() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        let (label, rows) = get_genre_play_source(&db, "Rock", None).unwrap().unwrap();
        assert_eq!(label, "Rock");
        assert_eq!(rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), vec!["t1", "t3"]);
        let (_, rows) = get_genre_play_source(&db, "Rock", Some("t2")).unwrap().unwrap();
        assert_eq!(rows.len(), 3);
        assert!(get_genre_play_source(&db, "Nope", None).unwrap().is_none());
    }

    #[test]
    fn genre_albums_filter_uses_the_existing_album_filter() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        let page = crate::library::get_albums(
            &db,
            0,
            10,
            crate::ipc::AlbumSort::Alphabetical,
            Some(crate::ipc::AlbumFilter { genre: Some("Rock".into()), year: None, starred_only: None }),
        )
        .unwrap();
        assert_eq!(page.total, 2);
    }

    #[test]
    fn history_insert_prunes_to_cap_and_clear_empties() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        for i in 0..12 {
            history_record_capped(&db, "t1", 1_000 + i, i % 2 == 0, 5).unwrap();
        }
        let n: i64 = db.read(|c| c.query_row("SELECT COUNT(*) FROM play_history", [], |r| r.get(0))).unwrap();
        assert_eq!(n, 5);
        let oldest: i64 = db.read(|c| c.query_row("SELECT MIN(played_at) FROM play_history", [], |r| r.get(0))).unwrap();
        assert_eq!(oldest, 1_007, "the newest 5 rows (ms 1007..1011) are kept");
        history_clear(&db).unwrap();
        assert_eq!(history_recent_tracks(&db, 8).unwrap().total, 0);
    }

    #[test]
    fn history_recent_is_distinct_newest_first_and_skips_missing_tracks() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        history_record(&db, "t1", 100, true).unwrap();
        history_record(&db, "t3", 200, false).unwrap();
        history_record(&db, "gone", 250, true).unwrap();
        history_record(&db, "t1", 300, true).unwrap();
        let r = history_recent_tracks(&db, 8).unwrap();
        assert_eq!(r.items.iter().map(|e| (e.track.id.as_str(), e.played_at)).collect::<Vec<_>>(), vec![("t1", 300), ("t3", 200)]);
        assert_eq!(history_recent_tracks(&db, 1).unwrap().items.len(), 1);
    }

    #[test]
    fn layout_window_interleaves_headers_and_pages_across_days() {
        // day 20 has 3 plays, day 19 has 2: flat = H t t t H t t (7)
        let days = [(20, 3), (19, 2)];
        let (all, total) = layout_window(&days, 0, 100);
        assert_eq!(total, 7);
        assert_eq!(
            all,
            vec![
                Slot::Header { day: 20, count: 3 },
                Slot::Tracks { skip: 0, count: 3 },
                Slot::Header { day: 19, count: 2 },
                Slot::Tracks { skip: 3, count: 2 },
            ]
        );
        // A window starting mid-day: flat indices 2..6 = t(1) t(2) H t(0)
        let (mid, _) = layout_window(&days, 2, 4);
        assert_eq!(
            mid,
            vec![Slot::Tracks { skip: 1, count: 2 }, Slot::Header { day: 19, count: 2 }, Slot::Tracks { skip: 3, count: 1 }]
        );
        assert_eq!(layout_window(&days, 7, 10).0, vec![]);
    }

    #[test]
    fn history_page_groups_by_local_day_and_drops_vanished_tracks() {
        use chrono::TimeZone;
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        let local_day = |ms: i64| chrono::Local.timestamp_millis_opt(ms).unwrap().format("%Y-%m-%d").to_string();
        // 2026-01-02 06:00 / 06:01 UTC (same local day in any zone) and
        // 36 h earlier (a different local day in any zone, DST or not).
        let base = 20_455i64 * 86_400_000 + 6 * 3_600_000;
        history_record(&db, "t4", base - 36 * 3_600_000, false).unwrap();
        history_record(&db, "t1", base, true).unwrap();
        history_record(&db, "gone", base + 30_000, true).unwrap(); // no longer in `tracks`
        history_record(&db, "t3", base + 60_000, true).unwrap();
        let page = history_page(&db, 0, 50).unwrap();
        assert_eq!(page.total, 5, "the vanished track is neither counted nor listed");
        let shape: Vec<String> = page
            .items
            .iter()
            .map(|i| match i {
                HistoryItem::Day { day, count } => format!("{day}:{count}"),
                HistoryItem::Track { track, .. } => track.id.clone(),
            })
            .collect();
        let (d1, d0) = (local_day(base), local_day(base - 36 * 3_600_000));
        assert_eq!(shape, vec![format!("{d1}:2"), "t3".into(), "t1".into(), format!("{d0}:1"), "t4".into()]);
        // Paging mid-list returns the right slice (offset 3 = second header).
        let tail = history_page(&db, 2, 3).unwrap();
        let ids: Vec<String> = tail
            .items
            .iter()
            .map(|i| match i {
                HistoryItem::Day { day, .. } => day.clone(),
                HistoryItem::Track { track, .. } => track.id.clone(),
            })
            .collect();
        assert_eq!(ids, vec!["t1".to_string(), d0, "t4".into()]);
    }

    #[test]
    fn schema_reset_keeps_history_and_recreates_the_genre_index() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        history_record(&db, "t1", 1, true).unwrap();
        let has_idx = |db: &Db| -> bool {
            db.read(|c| c.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name = 'idx_tracks_genre'", [], |r| r.get::<_, i64>(0)))
                .unwrap()
                == 1
        };
        // A version bump (drop + recreate) as `Db::open` would run it.
        {
            let mut conn = db.lock_writer();
            conn.pragma_update(None, "user_version", 1).unwrap();
            super::super::schema::ensure(&mut conn).unwrap();
        }
        assert!(has_idx(&db), "idx_tracks_genre recreated with the new tracks table");
        let n: i64 = db.read(|c| c.query_row("SELECT COUNT(*) FROM play_history", [], |r| r.get(0))).unwrap();
        assert_eq!(n, 1, "history survives a schema rebuild");
        // Logout wipes it.
        db.clear_all().unwrap();
        assert!(has_idx(&db));
        let n: i64 = db.read(|c| c.query_row("SELECT COUNT(*) FROM play_history", [], |r| r.get(0))).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn listen_and_feedback_tables_survive_schema_reset_and_logout_wipes_them() {
        use crate::listen::{insert_session, record_feedback, EndReason, FeedbackKind, ListenOrigin, ListenSession};
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        let session = ListenSession {
            track_id: "t1".into(),
            started_at_ms: 1,
            ended_at_ms: 2,
            listened_ms: 1,
            duration_ms: 2,
            end_reason: EndReason::Next,
            source_kind: None,
            source_id: None,
            origin: ListenOrigin::User,
            counted: false,
            max_position_ms: 1,
        };
        insert_session(&db, &session).unwrap();
        record_feedback(&db, "t1", 1, FeedbackKind::Like, None).unwrap();
        let count = |db: &Db, table: &str| -> i64 { db.read(|c| c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))).unwrap() };
        {
            let mut conn = db.lock_writer();
            conn.pragma_update(None, "user_version", 1).unwrap();
            super::super::schema::ensure(&mut conn).unwrap();
        }
        assert_eq!((count(&db, "listen_events"), count(&db, "feedback_events")), (1, 1), "a schema rebuild keeps both tables");
        db.clear_all().unwrap();
        assert_eq!((count(&db, "listen_events"), count(&db, "feedback_events")), (0, 0));
    }

    #[test]
    fn artist_most_played_counts_local_plays() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        assert!(artist_most_played(&db, "ar1", 10).unwrap().is_empty());
        history_record(&db, "t1", 1, true).unwrap();
        history_record(&db, "t2", 2, true).unwrap();
        history_record(&db, "t2", 3, true).unwrap();
        history_record(&db, "t3", 4, true).unwrap();
        let top = artist_most_played(&db, "ar1", 10).unwrap();
        assert_eq!(top.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec!["t2", "t1"]);
    }

    #[test]
    fn cache_roundtrip_and_user_data_clear() {
        let db = Db::open_in_memory().unwrap();
        assert!(cache_get(&db, "info:ar1").unwrap().is_none());
        cache_put(&db, "info:ar1", "{}", 5).unwrap();
        cache_put(&db, "info:ar1", "{\"a\":1}", 9).unwrap();
        assert_eq!(cache_get(&db, "info:ar1").unwrap(), Some(("{\"a\":1}".to_string(), 9)));
        db.with_conn(clear_user_data).unwrap();
        assert!(cache_get(&db, "info:ar1").unwrap().is_none());
    }

    #[test]
    fn similar_artist_lookup_prefers_id_then_name() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        assert_eq!(find_artist_by_id_or_name(&db, Some("ar2"), "x").unwrap().unwrap().name, "Beta");
        assert_eq!(find_artist_by_id_or_name(&db, Some("zzz"), "alpha").unwrap().unwrap().id, "ar1");
        assert!(find_artist_by_id_or_name(&db, None, "Nobody").unwrap().is_none());
    }
}
