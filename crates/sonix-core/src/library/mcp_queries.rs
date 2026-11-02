//! Read-only aggregate queries over `play_history` for the MCP bridge
//! (`mcp_bridge`): history by time range, top tracks/artists, library stats.
//! Kept in its own file (not `discovery.rs`) so it merges cleanly with the
//! listening-history work elsewhere. Times are unix milliseconds; the caller
//! resolves local dates to those.

use rusqlite::params;

use super::{row_to_track, Db};
use crate::ipc::{IpcError, Paged, Track};

#[derive(Debug, Clone)]
pub struct PlayedTrack {
    pub played_at: i64,
    pub track: Track,
}

#[derive(Debug, Clone)]
pub struct TopTrack {
    pub track: Track,
    pub plays: u64,
    pub last_played: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopArtist {
    pub artist_id: Option<String>,
    pub name: String,
    pub plays: u64,
    pub distinct_tracks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryStats {
    pub tracks: u64,
    pub albums: u64,
    pub artists: u64,
    pub playlists: u64,
    pub liked_tracks: u64,
    pub recorded_plays: u64,
    pub first_recorded_play_ms: Option<i64>,
}

/// Plays in `[since, until)` (either bound optional), newest first. Plays of
/// tracks that no longer exist in the library are skipped (they can't be shown).
pub fn history_range(
    db: &Db,
    since: Option<i64>,
    until: Option<i64>,
    offset: u64,
    limit: u64,
) -> Result<Paged<PlayedTrack>, IpcError> {
    db.read(|conn| {
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM play_history h JOIN tracks t ON t.id = h.track_id
             WHERE (?1 IS NULL OR h.played_at >= ?1) AND (?2 IS NULL OR h.played_at < ?2)",
            params![since, until],
            |r| r.get(0),
        )?;
        let mut stmt = conn.prepare_cached(
            "SELECT t.*, h.played_at AS played_at FROM play_history h JOIN tracks t ON t.id = h.track_id
             WHERE (?1 IS NULL OR h.played_at >= ?1) AND (?2 IS NULL OR h.played_at < ?2)
             ORDER BY h.played_at DESC, h.id DESC LIMIT ?3 OFFSET ?4",
        )?;
        let items = stmt
            .query_map(params![since, until, limit as i64, offset as i64], |r| {
                Ok(PlayedTrack { played_at: r.get("played_at")?, track: row_to_track(r)? })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Paged { items, total: total.max(0) as u64, offset })
    })
}

/// Most-played tracks in `[since, until)`; `unliked_only` keeps tracks that
/// are not starred.
pub fn top_tracks(
    db: &Db,
    since: Option<i64>,
    until: Option<i64>,
    limit: u64,
    unliked_only: bool,
) -> Result<Vec<TopTrack>, IpcError> {
    db.read(|conn| {
        let mut stmt = conn.prepare_cached(
            "SELECT t.*, COUNT(*) AS plays, MAX(h.played_at) AS last_played
             FROM play_history h JOIN tracks t ON t.id = h.track_id
             WHERE (?1 IS NULL OR h.played_at >= ?1) AND (?2 IS NULL OR h.played_at < ?2)
               AND (?3 = 0 OR t.starred IS NULL)
             GROUP BY t.id ORDER BY plays DESC, last_played DESC, t.id LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(params![since, until, unliked_only as i64, limit as i64], |r| {
                Ok(TopTrack {
                    plays: r.get::<_, i64>("plays")?.max(0) as u64,
                    last_played: r.get("last_played")?,
                    track: row_to_track(r)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

/// Most-played artists in `[since, until)` (grouped by artist id, else name).
pub fn top_artists(db: &Db, since: Option<i64>, until: Option<i64>, limit: u64) -> Result<Vec<TopArtist>, IpcError> {
    db.read(|conn| {
        let mut stmt = conn.prepare_cached(
            "SELECT t.artist_id AS artist_id, t.artist AS name, COUNT(*) AS plays, COUNT(DISTINCT t.id) AS distinct_tracks
             FROM play_history h JOIN tracks t ON t.id = h.track_id
             WHERE (?1 IS NULL OR h.played_at >= ?1) AND (?2 IS NULL OR h.played_at < ?2)
             GROUP BY COALESCE(t.artist_id, 'name:' || lower(t.artist))
             ORDER BY plays DESC, name COLLATE NOCASE LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![since, until, limit as i64], |r| {
                Ok(TopArtist {
                    artist_id: r.get("artist_id")?,
                    name: r.get("name")?,
                    plays: r.get::<_, i64>("plays")?.max(0) as u64,
                    distinct_tracks: r.get::<_, i64>("distinct_tracks")?.max(0) as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

pub fn stats(db: &Db) -> Result<LibraryStats, IpcError> {
    db.read(|conn| {
        let n = |sql: &str| -> rusqlite::Result<u64> { conn.query_row(sql, [], |r| r.get::<_, i64>(0)).map(|v| v.max(0) as u64) };
        Ok(LibraryStats {
            tracks: n("SELECT COUNT(*) FROM tracks")?,
            albums: n("SELECT COUNT(*) FROM albums")?,
            artists: n("SELECT COUNT(*) FROM artists")?,
            playlists: n("SELECT COUNT(*) FROM playlists")?,
            liked_tracks: n("SELECT COUNT(*) FROM tracks WHERE starred IS NOT NULL")?,
            recorded_plays: n("SELECT COUNT(*) FROM play_history")?,
            first_recorded_play_ms: conn.query_row("SELECT MIN(played_at) FROM play_history", [], |r| r.get(0))?,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn seed(db: &Db) {
        db.with_tx(|tx| {
            tx.execute(
                "INSERT INTO tracks (id, title, artist, artist_id, album, duration, suffix, starred, genre) VALUES
                 ('t1','Alpha','Nova','ar1','A',100,'flac',NULL,'Ambient'),
                 ('t2','Beta','Nova','ar1','A',200,'flac','2024-01-01T00:00:00Z','Ambient'),
                 ('t3','Gamma','Comet','ar2','B',300,'mp3',NULL,'Rock')",
                [],
            )?;
            // t1 x3, t2 x2, t3 x1 + a play of a deleted track.
            for (track, at) in [("t1", 1000), ("t1", 2000), ("t1", 3000), ("t2", 4000), ("t2", 5000), ("t3", 6000), ("gone", 7000)] {
                tx.execute("INSERT INTO play_history (track_id, played_at, scrobbled) VALUES (?1, ?2, 0)", params![track, at])?;
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn history_is_newest_first_ranged_and_paged() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        let all = history_range(&db, None, None, 0, 50).unwrap();
        assert_eq!(all.total, 6, "the deleted track's play is not counted");
        assert_eq!(all.items.iter().map(|p| p.played_at).collect::<Vec<_>>(), vec![6000, 5000, 4000, 3000, 2000, 1000]);
        let r = history_range(&db, Some(2000), Some(5000), 0, 50).unwrap();
        assert_eq!(r.items.iter().map(|p| p.played_at).collect::<Vec<_>>(), vec![4000, 3000, 2000], "since inclusive, until exclusive");
        let page = history_range(&db, None, None, 1, 2).unwrap();
        assert_eq!((page.total, page.offset), (6, 1));
        assert_eq!(page.items.iter().map(|p| p.played_at).collect::<Vec<_>>(), vec![5000, 4000]);
    }

    #[test]
    fn top_tracks_count_rank_and_filter_unliked() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        let top = top_tracks(&db, None, None, 10, false).unwrap();
        assert_eq!(top.iter().map(|t| (t.track.id.as_str(), t.plays)).collect::<Vec<_>>(), vec![("t1", 3), ("t2", 2), ("t3", 1)]);
        assert_eq!(top[0].last_played, 3000);
        let unliked = top_tracks(&db, None, None, 10, true).unwrap();
        assert_eq!(unliked.iter().map(|t| t.track.id.as_str()).collect::<Vec<_>>(), vec!["t1", "t3"]);
        let ranged = top_tracks(&db, Some(3500), None, 1, false).unwrap();
        assert_eq!(ranged.len(), 1);
        assert_eq!(ranged[0].track.id, "t2");
    }

    #[test]
    fn top_artists_group_by_artist() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        let top = top_artists(&db, None, None, 10).unwrap();
        assert_eq!(
            top,
            vec![
                TopArtist { artist_id: Some("ar1".into()), name: "Nova".into(), plays: 5, distinct_tracks: 2 },
                TopArtist { artist_id: Some("ar2".into()), name: "Comet".into(), plays: 1, distinct_tracks: 1 },
            ]
        );
    }

    #[test]
    fn stats_count_everything() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        let s = stats(&db).unwrap();
        assert_eq!((s.tracks, s.liked_tracks, s.recorded_plays, s.first_recorded_play_ms), (3, 1, 7, Some(1000)));
    }
}
