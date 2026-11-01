//! SQLite schema for the local library mirror.
//!
//! The DB is a disposable cache of the server, so "migration" is simply:
//! if `PRAGMA user_version` doesn't match [`SCHEMA_VERSION`], drop every
//! object and recreate (the next background sync refills it). A matching
//! version means the schema is already there and opening the DB costs one
//! pragma read — no DDL on the startup path.
//!
//! ## Layout
//!
//! Every mirrored table has an explicit `rid INTEGER PRIMARY KEY` (a stable
//! rowid that `VACUUM` can never renumber) plus the server `id` as a
//! `UNIQUE` key used for UPSERTs, and a `gen` column holding the sync
//! generation that last saw the row (see `sync.rs`: rows whose `gen` is
//! older than the current sync's are deleted once that sync has fetched the
//! complete list).
//!
//! The FTS5 indexes are *external content* tables over those rowids, kept in
//! sync by triggers — so every write path (sync UPSERTs, local playlist
//! edits, deletes) keeps search consistent automatically, and the text is
//! not stored twice. The update triggers only touch the index when an
//! indexed column actually changed, so a resync that re-UPSERTs unchanged
//! rows (only bumping `gen`) does no FTS work.

/// History: 2 = integration/main; 3 was used by two diverging feature
/// branches with *different* layouts (context-menu: rating/size columns;
/// all-tracks: `tracks.created` + per-sort track indexes). 4 is their union,
/// so any DB that saw either v3 build is rebuilt (and resynced) rather than
/// trusted with a layout that's missing columns.
/// 5 (feat-smart-lists): `tracks.play_count`/`tracks.played` (server play
/// stats, for local smart-playlist rules) and `playlists.readonly`.
pub const SCHEMA_VERSION: i64 = 5;

/// Drops every object this module creates (and the pre-versioned Phase 1
/// layout's tables, which used the same names).
pub const DROP_SQL: &str = r#"
DROP TRIGGER IF EXISTS artists_ai; DROP TRIGGER IF EXISTS artists_ad; DROP TRIGGER IF EXISTS artists_au;
DROP TRIGGER IF EXISTS albums_ai; DROP TRIGGER IF EXISTS albums_ad; DROP TRIGGER IF EXISTS albums_au;
DROP TRIGGER IF EXISTS tracks_ai; DROP TRIGGER IF EXISTS tracks_ad; DROP TRIGGER IF EXISTS tracks_au;
DROP TRIGGER IF EXISTS playlists_ai; DROP TRIGGER IF EXISTS playlists_ad; DROP TRIGGER IF EXISTS playlists_au;
DROP TABLE IF EXISTS tracks_fts;
DROP TABLE IF EXISTS albums_fts;
DROP TABLE IF EXISTS artists_fts;
DROP TABLE IF EXISTS playlists_fts;
DROP TABLE IF EXISTS playlist_tracks;
DROP TABLE IF EXISTS playlists;
DROP TABLE IF EXISTS tracks;
DROP TABLE IF EXISTS albums;
DROP TABLE IF EXISTS artists;
DROP TABLE IF EXISTS home_sections;
DROP TABLE IF EXISTS meta;
"#;

pub const SCHEMA_SQL: &str = r#"
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT
);

CREATE TABLE artists (
    rid         INTEGER PRIMARY KEY,
    id          TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL,
    album_count INTEGER NOT NULL DEFAULT 0,
    cover_art   TEXT,
    starred     TEXT,
    rating      INTEGER,
    gen         INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_artists_name ON artists(name COLLATE NOCASE);
CREATE INDEX idx_artists_starred ON artists(starred) WHERE starred IS NOT NULL;

CREATE TABLE albums (
    rid        INTEGER PRIMARY KEY,
    id         TEXT NOT NULL UNIQUE,
    name       TEXT NOT NULL,
    artist     TEXT NOT NULL,
    artist_id  TEXT,
    year       INTEGER,
    genre      TEXT,
    cover_art  TEXT,
    song_count INTEGER NOT NULL DEFAULT 0,
    duration   REAL NOT NULL DEFAULT 0,
    starred    TEXT,
    rating     INTEGER,
    play_count INTEGER,
    created    TEXT,
    gen        INTEGER NOT NULL DEFAULT 0
);
-- One index per paged sort order (`queries::get_albums`) so no sort ever
-- needs a temp B-tree over the whole table.
CREATE INDEX idx_albums_name ON albums(name COLLATE NOCASE);
CREATE INDEX idx_albums_artist_name ON albums(artist COLLATE NOCASE, name COLLATE NOCASE);
CREATE INDEX idx_albums_created_name ON albums(created DESC, name COLLATE NOCASE);
CREATE INDEX idx_albums_year_name ON albums(year DESC, name COLLATE NOCASE);
CREATE INDEX idx_albums_genre_name ON albums(genre, name COLLATE NOCASE);
CREATE INDEX idx_albums_artist_id ON albums(artist_id);
CREATE INDEX idx_albums_starred ON albums(starred) WHERE starred IS NOT NULL;

CREATE TABLE tracks (
    rid            INTEGER PRIMARY KEY,
    id             TEXT NOT NULL UNIQUE,
    title          TEXT NOT NULL,
    artist         TEXT NOT NULL,
    artist_id      TEXT,
    album          TEXT,
    album_id       TEXT,
    album_artist   TEXT,
    track_number   INTEGER,
    disc_number    INTEGER,
    year           INTEGER,
    genre          TEXT,
    duration       REAL NOT NULL DEFAULT 0,
    suffix         TEXT NOT NULL,
    bit_rate       INTEGER,
    bit_depth      INTEGER,
    sampling_rate  INTEGER NOT NULL DEFAULT 0,
    channel_count  INTEGER NOT NULL DEFAULT 0,
    cover_art      TEXT,
    starred        TEXT,
    rating         INTEGER,
    size           INTEGER,
    track_gain_db  REAL,
    album_gain_db  REAL,
    track_peak     REAL,
    album_peak     REAL,
    path           TEXT,
    -- When the file was added to the server library (Subsonic song `created`),
    -- for the all-tracks "date added" sort. Absent on servers/entries that
    -- don't expose it - never backfilled with a guess.
    created        TEXT,
    -- Server play stats (Subsonic song `playCount`/`played`) for the local
    -- smart-playlist rules. NULL = server doesn't report it / never played.
    play_count     INTEGER,
    played         TEXT,
    gen            INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_tracks_album_disc_track ON tracks(album_id, disc_number, track_number);
CREATE INDEX idx_tracks_artist_id ON tracks(artist_id);
CREATE INDEX idx_tracks_starred ON tracks(starred) WHERE starred IS NOT NULL;
-- One index per all-tracks sort order (`queries::get_tracks`/`get_all_track_ids`)
-- so a flat 50k-row browse of the whole library never needs a temp B-tree,
-- same rationale as the per-sort album indexes above.
CREATE INDEX idx_tracks_title ON tracks(title COLLATE NOCASE);
CREATE INDEX idx_tracks_artist_title ON tracks(artist COLLATE NOCASE, title COLLATE NOCASE);
CREATE INDEX idx_tracks_album_title ON tracks(album COLLATE NOCASE, disc_number ASC, track_number ASC);
CREATE INDEX idx_tracks_year_title ON tracks(year, title COLLATE NOCASE);
CREATE INDEX idx_tracks_created_title ON tracks(created, title COLLATE NOCASE);
CREATE INDEX idx_tracks_duration ON tracks(duration);
CREATE INDEX idx_tracks_suffix_title ON tracks(suffix COLLATE NOCASE, title COLLATE NOCASE);

CREATE TABLE playlists (
    rid        INTEGER PRIMARY KEY,
    id         TEXT NOT NULL UNIQUE,
    name       TEXT NOT NULL,
    comment    TEXT,
    owner      TEXT,
    public     INTEGER NOT NULL DEFAULT 0,
    song_count INTEGER NOT NULL DEFAULT 0,
    duration   REAL NOT NULL DEFAULT 0,
    cover_art  TEXT,
    created_at TEXT,
    updated_at TEXT,
    -- OpenSubsonic `readonly` (1/0): Navidrome sets it for smart (.nsp)
    -- playlists. NULL = the server didn't send the field (plain Subsonic).
    readonly   INTEGER
);
CREATE INDEX idx_playlists_name ON playlists(name COLLATE NOCASE);

CREATE TABLE playlist_tracks (
    playlist_id TEXT NOT NULL,
    position    INTEGER NOT NULL,
    track_id    TEXT NOT NULL,
    PRIMARY KEY (playlist_id, position)
) WITHOUT ROWID;

-- Cached server-ordered id lists for the four home sections, refreshed in
-- the background (never blocks startup).
CREATE TABLE home_sections (
    section      TEXT PRIMARY KEY,
    album_ids    TEXT NOT NULL, -- JSON array of album ids, server order preserved
    updated_at   TEXT NOT NULL
);

-- FTS5 prefix search, diacritic-insensitive, external content over the
-- stable `rid` of each table (see module docs).
CREATE VIRTUAL TABLE tracks_fts USING fts5(
    title, artist, album, content='tracks', content_rowid='rid',
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE VIRTUAL TABLE albums_fts USING fts5(
    name, artist, content='albums', content_rowid='rid',
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE VIRTUAL TABLE artists_fts USING fts5(
    name, content='artists', content_rowid='rid',
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE VIRTUAL TABLE playlists_fts USING fts5(
    name, comment, content='playlists', content_rowid='rid',
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TRIGGER tracks_ai AFTER INSERT ON tracks BEGIN
    INSERT INTO tracks_fts(rowid, title, artist, album) VALUES (new.rid, new.title, new.artist, new.album);
END;
CREATE TRIGGER tracks_ad AFTER DELETE ON tracks BEGIN
    INSERT INTO tracks_fts(tracks_fts, rowid, title, artist, album) VALUES ('delete', old.rid, old.title, old.artist, old.album);
END;
CREATE TRIGGER tracks_au AFTER UPDATE OF title, artist, album ON tracks
WHEN old.title IS NOT new.title OR old.artist IS NOT new.artist OR old.album IS NOT new.album BEGIN
    INSERT INTO tracks_fts(tracks_fts, rowid, title, artist, album) VALUES ('delete', old.rid, old.title, old.artist, old.album);
    INSERT INTO tracks_fts(rowid, title, artist, album) VALUES (new.rid, new.title, new.artist, new.album);
END;

CREATE TRIGGER albums_ai AFTER INSERT ON albums BEGIN
    INSERT INTO albums_fts(rowid, name, artist) VALUES (new.rid, new.name, new.artist);
END;
CREATE TRIGGER albums_ad AFTER DELETE ON albums BEGIN
    INSERT INTO albums_fts(albums_fts, rowid, name, artist) VALUES ('delete', old.rid, old.name, old.artist);
END;
CREATE TRIGGER albums_au AFTER UPDATE OF name, artist ON albums
WHEN old.name IS NOT new.name OR old.artist IS NOT new.artist BEGIN
    INSERT INTO albums_fts(albums_fts, rowid, name, artist) VALUES ('delete', old.rid, old.name, old.artist);
    INSERT INTO albums_fts(rowid, name, artist) VALUES (new.rid, new.name, new.artist);
END;

CREATE TRIGGER artists_ai AFTER INSERT ON artists BEGIN
    INSERT INTO artists_fts(rowid, name) VALUES (new.rid, new.name);
END;
CREATE TRIGGER artists_ad AFTER DELETE ON artists BEGIN
    INSERT INTO artists_fts(artists_fts, rowid, name) VALUES ('delete', old.rid, old.name);
END;
CREATE TRIGGER artists_au AFTER UPDATE OF name ON artists
WHEN old.name IS NOT new.name BEGIN
    INSERT INTO artists_fts(artists_fts, rowid, name) VALUES ('delete', old.rid, old.name);
    INSERT INTO artists_fts(rowid, name) VALUES (new.rid, new.name);
END;

CREATE TRIGGER playlists_ai AFTER INSERT ON playlists BEGIN
    INSERT INTO playlists_fts(rowid, name, comment) VALUES (new.rid, new.name, new.comment);
END;
CREATE TRIGGER playlists_ad AFTER DELETE ON playlists BEGIN
    INSERT INTO playlists_fts(playlists_fts, rowid, name, comment) VALUES ('delete', old.rid, old.name, old.comment);
END;
CREATE TRIGGER playlists_au AFTER UPDATE OF name, comment ON playlists
WHEN old.name IS NOT new.name OR old.comment IS NOT new.comment BEGIN
    INSERT INTO playlists_fts(playlists_fts, rowid, name, comment) VALUES ('delete', old.rid, old.name, old.comment);
    INSERT INTO playlists_fts(rowid, name, comment) VALUES (new.rid, new.name, new.comment);
END;
"#;

/// Ensures the schema is at [`SCHEMA_VERSION`]: a no-op (one pragma read)
/// when it already is, otherwise drop + recreate in one transaction.
///
/// The discovery extras (`play_history`, `artist_cache`, `idx_tracks_genre`)
/// live outside the versioned schema and are (re)created *last*, whatever
/// path ran before (no-op, reset, or any future in-place migration), so an
/// index on a just-recreated `tracks` table can never go missing.
pub fn ensure(conn: &mut rusqlite::Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != SCHEMA_VERSION {
        reset(conn)?;
    }
    super::discovery::ensure_extras(conn)
}

/// Drops and recreates every table (used for version changes and logout).
pub fn reset(conn: &mut rusqlite::Connection) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch(DROP_SQL)?;
    tx.execute_batch(SCHEMA_SQL)?;
    // `DROP TABLE tracks` dropped `idx_tracks_genre` with it; history and the
    // artist cache are untouched (never in DROP_SQL).
    super::discovery::ensure_extras(&tx)?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()
}
