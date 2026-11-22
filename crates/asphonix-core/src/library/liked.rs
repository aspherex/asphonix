//! Liked Songs (= every starred track in the local mirror, newest like
//! first) and playlist read-only lookups. Separate file so the feature stays
//! a clearly delimited, additive block.

use rusqlite::params;

use crate::ipc::{IpcError, Paged, Track};

use super::{row_to_track, Db};

const LIKED_ORDER: &str = "starred DESC, title COLLATE NOCASE, rid";

/// One page of starred tracks, ordered by starred date descending (the
/// `starred` column holds the ISO timestamp the server/`star_add` recorded).
pub fn get_liked(db: &Db, offset: u64, limit: u64) -> Result<Paged<Track>, IpcError> {
    db.read(|conn| {
        let total: i64 = conn.query_row("SELECT COUNT(*) FROM tracks WHERE starred IS NOT NULL", [], |r| r.get(0))?;
        let sql = format!("SELECT * FROM tracks WHERE starred IS NOT NULL ORDER BY {LIKED_ORDER} LIMIT ?1 OFFSET ?2");
        let mut stmt = conn.prepare_cached(&sql)?;
        let items = stmt
            .query_map(params![limit as i64, offset as i64], row_to_track)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Paged { items, total: total.max(0) as u64, offset })
    })
}

/// `(count, total duration in seconds)` of all liked tracks (real sums).
pub fn get_liked_summary(db: &Db) -> Result<(u64, f64), IpcError> {
    db.read(|conn| {
        conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(duration), 0) FROM tracks WHERE starred IS NOT NULL",
            [],
            |r| Ok((r.get::<_, i64>(0)?.max(0) as u64, r.get::<_, f64>(1)?)),
        )
    })
}

/// `(id, artist)` of every liked track in display order, 1★ tracks left out
/// (same rule as every other play source) except `start_track_id`.
pub fn get_liked_play_source(db: &Db, start_track_id: Option<&str>) -> Result<Vec<(String, String)>, IpcError> {
    db.read(|conn| {
        let sql = format!(
            "SELECT id, artist FROM tracks WHERE starred IS NOT NULL AND (rating IS NOT 1 OR id = ?1) ORDER BY {LIKED_ORDER}"
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        let items = stmt
            .query_map(params![start_track_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(items)
    })
}

/// `(smart, read_only)` for a server playlist.
///
/// When the server sends OpenSubsonic `readonly` (Navidrome does, computed
/// per requesting user) it is trusted as-is for `read_only`: the server knows
/// who may edit (e.g. an admin may edit other users' lists), so a playlist the
/// server calls editable is never locked here. Only when the field is absent
/// (plain Subsonic servers) does "owned by someone else" stand in for it.
/// `smart` = flagged read-only *and* owned by the current user: Navidrome
/// marks its rule-based (`.nsp`) lists read-only even for their owner, so
/// read-only-but-mine can only be a smart list. Owner comparison is
/// case-insensitive (Navidrome user names are), and a missing owner or
/// current user counts as "mine" (never lock the user out on missing data).
pub fn playlist_access_flags(server_read_only: Option<bool>, owner: Option<&str>, me: Option<&str>) -> (bool, bool) {
    let mine = match (owner, me) {
        (Some(o), Some(m)) => o.trim().eq_ignore_ascii_case(m.trim()),
        _ => true,
    };
    let read_only = server_read_only.unwrap_or(!mine);
    (server_read_only == Some(true) && mine, read_only)
}

/// `(owner, server readonly flag)` as stored for a playlist.
pub type PlaylistAccess = (Option<String>, Option<bool>);

/// `(owner, server readonly flag)` of a mirrored playlist; `None` if unknown.
pub fn playlist_access(db: &Db, id: &str) -> Result<Option<PlaylistAccess>, IpcError> {
    db.read(|conn| {
        match conn.query_row("SELECT owner, readonly FROM playlists WHERE id = ?1", [id], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<i64>>(1)?.map(|v| v != 0)))
        }) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    })
}

/// Mirrors a play the server is about to count (a scrobble was submitted or
/// queued) into the local `play_count`/`played` columns, so smart-playlist
/// rules ("Never played", "Not played in 6 months", most played) see it
/// without waiting for the next catalog sync (which only re-reads tracks
/// when the server catalog changes). The next catalog sync overwrites both
/// with the server's own values. Unknown id = no-op.
pub fn record_local_play(db: &Db, id: &str, played_at: &str) -> Result<(), IpcError> {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE tracks SET play_count = COALESCE(play_count, 0) + 1,
                played = CASE WHEN played IS NULL OR julianday(played) < julianday(?2) THEN ?2 ELSE played END
             WHERE id = ?1",
            params![id, played_at],
        )
        .map(|_| ())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(db: &Db, id: &str, starred: Option<&str>, rating: Option<i64>, dur: f64) {
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO tracks (id, title, artist, suffix, duration, starred, rating) VALUES (?1,?1,'Art','flac',?2,?3,?4)",
                params![id, dur, starred, rating],
            )
            .map(|_| ())
        })
        .unwrap();
    }

    #[test]
    fn liked_is_starred_newest_first_with_real_totals() {
        let db = Db::open_in_memory().unwrap();
        put(&db, "old", Some("2025-01-01T00:00:00Z"), None, 100.0);
        put(&db, "new", Some("2026-03-01T00:00:00Z"), None, 200.0);
        put(&db, "mid", Some("2025-06-01T00:00:00Z"), Some(1), 50.0);
        put(&db, "none", None, None, 999.0);
        let page = get_liked(&db, 0, 2).unwrap();
        assert_eq!(page.total, 3);
        assert_eq!(page.items.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["new", "mid"]);
        assert_eq!(get_liked(&db, 2, 2).unwrap().items[0].id, "old");
        assert_eq!(get_liked_summary(&db).unwrap(), (3, 350.0));
    }

    #[test]
    fn liked_play_source_filters_one_star_but_keeps_explicit_start() {
        let db = Db::open_in_memory().unwrap();
        put(&db, "a", Some("2026-01-01T00:00:00Z"), None, 1.0);
        put(&db, "b", Some("2026-02-01T00:00:00Z"), Some(1), 1.0);
        put(&db, "c", None, None, 1.0);
        let ids = |v: Vec<(String, String)>| v.into_iter().map(|(i, _)| i).collect::<Vec<_>>();
        assert_eq!(ids(get_liked_play_source(&db, None).unwrap()), ["a"]);
        assert_eq!(ids(get_liked_play_source(&db, Some("b")).unwrap()), ["b", "a"]);
        let artists = get_liked_play_source(&db, None).unwrap();
        assert_eq!(artists[0].1, "Art");
    }

    #[test]
    fn server_readonly_flag_round_trips_through_sync_upsert() {
        use crate::subsonic::RawPlaylist;
        let db = Db::open_in_memory().unwrap();
        let raw = |id: &str, ro: Option<bool>| RawPlaylist {
            id: id.into(),
            name: id.into(),
            comment: None,
            owner: Some("me".into()),
            public: None,
            song_count: Some(3),
            duration: None,
            cover_art: None,
            created: None,
            changed: Some("2026-01-01T00:00:00Z".into()),
            readonly: ro,
        };
        db.with_tx(|tx| {
            super::super::sync::upsert_playlist(tx, &raw("smart", Some(true)))?;
            super::super::sync::upsert_playlist(tx, &raw("plain", None))
        })
        .unwrap();
        let lists = crate::library::playlists_list(&db).unwrap();
        let ro = |id: &str| lists.iter().find(|p| p.id == id).unwrap().read_only;
        assert!(ro("smart") && !ro("plain"));
        // A server-side change (list became editable) is picked up by the next upsert.
        db.with_tx(|tx| super::super::sync::upsert_playlist(tx, &raw("smart", Some(false)))).unwrap();
        assert!(!crate::library::playlists_list(&db).unwrap().iter().find(|p| p.id == "smart").unwrap().read_only);
    }

    #[test]
    fn record_local_play_bumps_count_and_keeps_the_latest_played() {
        let db = Db::open_in_memory().unwrap();
        put(&db, "a", None, None, 1.0);
        let stats = |db: &Db| {
            db.read(|c| {
                c.query_row("SELECT play_count, played FROM tracks WHERE id = 'a'", [], |r| {
                    Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<String>>(1)?))
                })
            })
            .unwrap()
        };
        assert_eq!(stats(&db), (None, None));
        record_local_play(&db, "a", "2026-10-01T10:00:00.000Z").unwrap();
        assert_eq!(stats(&db), (Some(1), Some("2026-10-01T10:00:00.000Z".into())));
        // A late retry of an older play counts but doesn't move "last played" back.
        record_local_play(&db, "a", "2026-09-01T10:00:00.000Z").unwrap();
        assert_eq!(stats(&db), (Some(2), Some("2026-10-01T10:00:00.000Z".into())));
        record_local_play(&db, "missing", "2026-10-01T10:00:00.000Z").unwrap();
    }

    #[test]
    fn playlist_access_reads_owner_and_readonly_flag() {
        let db = Db::open_in_memory().unwrap();
        db.with_conn(|c| {
            c.execute("INSERT INTO playlists (id, name, owner, readonly) VALUES ('s','Smart','me',1)", []).map(|_| ())
        })
        .unwrap();
        assert_eq!(playlist_access(&db, "s").unwrap(), Some((Some("me".into()), Some(true))));
        assert_eq!(playlist_access(&db, "x").unwrap(), None);
    }
}
