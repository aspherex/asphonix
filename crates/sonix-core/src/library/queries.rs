//! Paged read queries backing every `browse_*`/`search_*` IPC command, plus
//! the local-DB side of playlist CRUD and star/unstar (the server is always
//! written first — see `ipc.rs` — these functions mirror that write
//! locally so the UI reflects it without waiting for the next resync).
#![allow(dead_code)] // pins the full query surface; not all type aliases have an external caller yet

use rusqlite::params;

use crate::ipc::{
    track_order_by_sql, Album, AlbumDetail, AlbumFilter, AlbumSort, Artist, ArtistDetail, ArtistSort, HomeSections,
    IpcError, IpcErrorCode, Paged, Playlist, PlaylistDetail, SearchResults, SearchType, SortDir, Track, TrackSortField,
};

use super::{fts_prefix_query, row_to_album, row_to_artist, row_to_playlist, row_to_track, Db};

// Re-exported for documentation/discoverability from `library::`; not all
// are used outside this module today but they pin the public shape.
pub type AlbumPage = Paged<Album>;
pub type ArtistPage = Paged<Artist>;
pub type SearchPages = SearchResults;
/// `(id, artist name)` rows resolved for the engine's rolling-window
/// `Source` (`get_all_track_ids`/`get_artist_play_source`/
/// `get_playlist_play_source`) — never full `Track` rows, since starting
/// playback never needs more than this.
pub type PlaySourceIds = Vec<(String, String)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StarKindLocal {
    Track,
    Album,
    Artist,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RatingKindLocal {
    Track,
    Album,
    Artist,
}

#[derive(Debug, Clone)]
pub struct PlaylistWithTracks {
    pub playlist: Playlist,
    pub track_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeSectionKind {
    RecentlyPlayed,
    Frequent,
    Newest,
    Random,
}

impl HomeSectionKind {
    pub fn key(self) -> &'static str {
        match self {
            HomeSectionKind::RecentlyPlayed => "recentlyPlayed",
            HomeSectionKind::Frequent => "frequent",
            HomeSectionKind::Newest => "newest",
            HomeSectionKind::Random => "random",
        }
    }

    /// The Subsonic `getAlbumList2` `type` parameter used to populate this
    /// section's cache (see `ipc.rs`'s background home-section refresher).
    pub fn subsonic_list_type(self) -> &'static str {
        match self {
            HomeSectionKind::RecentlyPlayed => "recent",
            HomeSectionKind::Frequent => "frequent",
            HomeSectionKind::Newest => "newest",
            HomeSectionKind::Random => "random",
        }
    }
}

// ---------------------------------------------------------------------------
// Browse: artists
// ---------------------------------------------------------------------------

pub fn get_artists(db: &Db, offset: u64, limit: u64, sort: Option<ArtistSort>) -> Result<Paged<Artist>, IpcError> {
    let order_by = match sort {
        Some(ArtistSort::Random) => "RANDOM()",
        _ => "name COLLATE NOCASE ASC",
    };
    db.read(|conn| {
        let total: i64 = conn.query_row("SELECT COUNT(*) FROM artists", [], |r| r.get(0))?;
        let sql = format!("SELECT * FROM artists ORDER BY {order_by} LIMIT ?1 OFFSET ?2");
        let mut stmt = conn.prepare_cached(&sql)?;
        let items = stmt
            .query_map(params![limit as i64, offset as i64], row_to_artist)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Paged {
            items,
            total: total.max(0) as u64,
            offset,
        })
    })
}

pub fn get_artist_detail(db: &Db, id: &str) -> Result<Option<ArtistDetail>, IpcError> {
    db.read(|conn| {
        let artist = conn
            .query_row("SELECT * FROM artists WHERE id = ?1", [id], row_to_artist)
            .optional_opt()?;
        let Some(artist) = artist else { return Ok(None) };
        let mut stmt = conn.prepare_cached(
            "SELECT * FROM albums WHERE artist_id = ?1 ORDER BY year DESC, name COLLATE NOCASE ASC",
        )?;
        let albums = stmt.query_map([id], row_to_album)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(ArtistDetail { artist, albums }))
    })
}

// ---------------------------------------------------------------------------
// Browse: albums
// ---------------------------------------------------------------------------

pub fn get_albums(
    db: &Db,
    offset: u64,
    limit: u64,
    sort: AlbumSort,
    filter: Option<AlbumFilter>,
) -> Result<Paged<Album>, IpcError> {
    // `recent`/`frequent` have no stable local ordering column (Subsonic
    // doesn't expose a last-played timestamp per album in `getAlbumList2`'s
    // alphabetical listing) — they're served from the small cached id list
    // the home-section refresher already maintains (see `home_sections`).
    if matches!(sort, AlbumSort::Recent | AlbumSort::Frequent) {
        let kind = if matches!(sort, AlbumSort::Recent) {
            HomeSectionKind::RecentlyPlayed
        } else {
            HomeSectionKind::Frequent
        };
        return get_albums_from_home_cache(db, kind, offset, limit);
    }

    let order_by = match sort {
        AlbumSort::Alphabetical => "name COLLATE NOCASE ASC",
        AlbumSort::Artist => "artist COLLATE NOCASE ASC, name COLLATE NOCASE ASC",
        AlbumSort::Newest => "created DESC, name COLLATE NOCASE ASC",
        AlbumSort::Year => "year DESC, name COLLATE NOCASE ASC",
        AlbumSort::Random => "RANDOM()",
        AlbumSort::Recent | AlbumSort::Frequent => unreachable!(),
    };

    let filter = filter.unwrap_or_default();
    let mut where_clauses: Vec<String> = Vec::new();
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(genre) = &filter.genre {
        where_clauses.push("genre = ?".into());
        args.push(Box::new(genre.clone()));
    }
    if let Some(year) = filter.year {
        where_clauses.push("year = ?".into());
        args.push(Box::new(year));
    }
    if filter.starred_only.unwrap_or(false) {
        where_clauses.push("starred IS NOT NULL".into());
    }
    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_clauses.join(" AND "))
    };

    db.read(|conn| {
        let count_sql = format!("SELECT COUNT(*) FROM albums {where_sql}");
        let total: i64 = conn.query_row(&count_sql, rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())), |r| {
            r.get(0)
        })?;

        let sql = format!("SELECT * FROM albums {where_sql} ORDER BY {order_by} LIMIT ? OFFSET ?");
        let mut stmt = conn.prepare_cached(&sql)?;
        let mut all_args: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let limit_i = limit as i64;
        let offset_i = offset as i64;
        all_args.push(&limit_i);
        all_args.push(&offset_i);
        let items = stmt
            .query_map(rusqlite::params_from_iter(all_args), row_to_album)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Paged {
            items,
            total: total.max(0) as u64,
            offset,
        })
    })
}

/// Looks up `ids` in the local library mirror, in no particular order
/// (callers that need the caller's own order re-sort, like
/// `get_albums_by_ids_ordered` does for home sections) — used by the audio
/// engine (`audio::engine::Engine::resolve_tracks`) to resolve queued track
/// ids to real metadata. An id with no local row (not yet synced, or from a
/// different account) is simply absent from the result, never fabricated.
pub fn get_tracks_by_ids(db: &Db, ids: &[String]) -> Result<Vec<Track>, IpcError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    db.read(|conn| {
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("SELECT * FROM tracks WHERE id IN ({placeholders})");
        let mut stmt = conn.prepare_cached(&sql)?;
        let items = stmt
            .query_map(rusqlite::params_from_iter(ids.iter()), row_to_track)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(items)
    })
}

/// Max bound parameters per `IN (...)` query in [`track_artists_for_ids`].
const ID_CHUNK: usize = 500;

/// The artist of each of `ids` that exists in the local mirror (an id with no
/// row is simply absent), in batched `WHERE id IN (...)` queries of
/// <= 500 ids — used once at session restore to drop saved queue ids that were
/// deleted from the library and to re-derive the "spread artists" map, which
/// isn't persisted. Reads only the ids asked for, not the whole tracks table.
pub fn track_artists_for_ids(
    db: &Db,
    ids: &[String],
) -> Result<std::collections::HashMap<String, String>, IpcError> {
    let mut out = std::collections::HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(ID_CHUNK) {
        db.read(|conn| {
            let placeholders = vec!["?"; chunk.len()].join(",");
            // `prepare`, not `prepare_cached`: the SQL text varies with the chunk length.
            let mut stmt = conn.prepare(&format!("SELECT id, artist FROM tracks WHERE id IN ({placeholders})"))?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (id, artist) = row?;
                out.insert(id, artist);
            }
            Ok(())
        })?;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Browse: all tracks (Tracks route) + play-all's base id order
// ---------------------------------------------------------------------------

/// Builds the (optional) FTS filter clause shared by `get_tracks` and
/// `get_all_track_ids`: `Some((join_sql, where_sql, fts_query))` when
/// `filter` is non-empty text, `None` for an unfiltered browse of the whole
/// table. Reuses `tracks_fts` (title/artist/album prefix match), same as
/// `search_tracks`.
fn tracks_fts_filter(filter: Option<&str>) -> Option<String> {
    filter.and_then(fts_prefix_query)
}

/// Paged, sorted (+ optionally filtered) listing of every track in the
/// library — backs `browse_get_tracks` (the all-tracks route). Each sort has
/// its own index (`schema.rs`) so a page, even deep in a 50k-row table,
/// never costs a full scan or a temp B-tree sort (see `library::perf_tests`).
pub fn get_tracks(
    db: &Db,
    offset: u64,
    limit: u64,
    sort: TrackSortField,
    dir: SortDir,
    filter: Option<&str>,
) -> Result<Paged<Track>, IpcError> {
    let fts = tracks_fts_filter(filter);

    db.read(|conn| {
        if let Some(q) = &fts {
            let order_by = track_order_by_sql(sort, dir, true);
            let total: i64 =
                conn.query_row("SELECT COUNT(*) FROM tracks_fts WHERE tracks_fts MATCH ?1", [q], |r| r.get(0))?;
            let sql = format!(
                "SELECT t.* FROM tracks_fts f JOIN tracks t ON t.rid = f.rowid
                 WHERE tracks_fts MATCH ?1 ORDER BY {order_by} LIMIT ?2 OFFSET ?3"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            let items = stmt
                .query_map(params![q, limit as i64, offset as i64], row_to_track)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Paged { items, total: total.max(0) as u64, offset })
        } else {
            let order_by = track_order_by_sql(sort, dir, false);
            let total: i64 = conn.query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))?;
            let sql = format!("SELECT * FROM tracks ORDER BY {order_by} LIMIT ?1 OFFSET ?2");
            let mut stmt = conn.prepare_cached(&sql)?;
            let items = stmt
                .query_map(params![limit as i64, offset as i64], row_to_track)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(Paged { items, total: total.max(0) as u64, offset })
        }
    })
}

/// Every matching track id (+ its artist name, for the optional "spread
/// artists in shuffle" post-process), in the requested base order — the
/// *whole* (filtered) result set, not a page: this is `player_play_all`'s
/// source of truth for what "all" means, resolved once in Rust from SQLite
/// so the frontend never builds or transmits a 50k-element array. Only two
/// narrow columns are selected (not full `Track` rows), so even a 50k-row
/// unfiltered result is a few MB at most.
///
/// Tracks rated 1★ are left out (a 1★ rating means "never play this" — on
/// the dev setup a nightly job even deletes those files), except
/// `keep_id`: the row the user explicitly started playback from is always
/// kept, whatever its rating.
pub fn get_all_track_ids(
    db: &Db,
    sort: TrackSortField,
    dir: SortDir,
    filter: Option<&str>,
    keep_id: Option<&str>,
) -> Result<Vec<(String, String)>, IpcError> {
    let fts = tracks_fts_filter(filter);
    db.read(|conn| {
        if let Some(q) = &fts {
            let order_by = track_order_by_sql(sort, dir, true);
            let sql = format!(
                "SELECT t.id, t.artist FROM tracks_fts f JOIN tracks t ON t.rid = f.rowid
                 WHERE tracks_fts MATCH ?1 AND (t.rating IS NOT 1 OR t.id = ?2) ORDER BY {order_by}"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            let items = stmt
                .query_map(params![q, keep_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        } else {
            let order_by = track_order_by_sql(sort, dir, false);
            let sql = format!("SELECT id, artist FROM tracks WHERE (rating IS NOT 1 OR id = ?1) ORDER BY {order_by}");
            let mut stmt = conn.prepare_cached(&sql)?;
            let items = stmt
                .query_map([keep_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        }
    })
}

/// `(label, [(id, artist)])` for an artist's "Play all"/"Shuffle"
/// (`ipc::player_play_artist`) — same two-narrow-columns shape as
/// `get_all_track_ids`, resolved straight from SQLite rather than reusing
/// `get_artist_tracks`'s full `Track` rows: playing an artist's catalog
/// (which can be hundreds of tracks) never needs more than ids + artist
/// names for the engine's rolling-window `Source` (`audio::queue`'s module
/// doc comment). Tracks rated 1★ are left out, same as `get_all_track_ids`
/// (`start_track_id`, an explicit pick, is always kept). `None` if the
/// artist doesn't exist locally.
pub fn get_artist_play_source(
    db: &Db,
    artist_id: &str,
    start_track_id: Option<&str>,
) -> Result<Option<(String, PlaySourceIds)>, IpcError> {
    db.read(|conn| {
        let name: Option<String> =
            conn.query_row("SELECT name FROM artists WHERE id = ?1", [artist_id], |r| r.get(0)).optional_opt()?;
        let Some(name) = name else { return Ok(None) };
        let mut stmt = conn.prepare_cached(
            "SELECT t.id, t.artist FROM tracks t JOIN albums a ON a.id = t.album_id
             WHERE t.artist_id = ?1 AND (t.rating IS NOT 1 OR t.id = ?2)
             ORDER BY (a.year IS NULL), a.year, a.name COLLATE NOCASE,
                      COALESCE(t.disc_number, 1), COALESCE(t.track_number, 0), t.title COLLATE NOCASE",
        )?;
        let items = stmt
            .query_map(params![artist_id, start_track_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some((name, items)))
    })
}

/// `(label, [(id, artist)])` for a playlist's "Play"/"Shuffle"
/// (`ipc::player_play_playlist`) — same reasoning as
/// `get_artist_play_source`: `playlists_get`/`PlaylistDetail` already fetch
/// every track for the detail page's own list, but play/shuffle resolves
/// fresh here so a 1,980-track playlist never needs its whole `Track` list
/// built just to start the engine's rolling-window `Source`. `None` if the
/// playlist doesn't exist locally. 1★ tracks are left out (see
/// `get_artist_play_source`).
pub fn get_playlist_play_source(
    db: &Db,
    id: &str,
    start_track_id: Option<&str>,
) -> Result<Option<(String, PlaySourceIds)>, IpcError> {
    db.read(|conn| {
        let name: Option<String> =
            conn.query_row("SELECT name FROM playlists WHERE id = ?1", [id], |r| r.get(0)).optional_opt()?;
        let Some(name) = name else { return Ok(None) };
        let mut stmt = conn.prepare_cached(
            "SELECT t.id, t.artist FROM playlist_tracks pt JOIN tracks t ON t.id = pt.track_id
             WHERE pt.playlist_id = ?1 AND (t.rating IS NOT 1 OR t.id = ?2) ORDER BY pt.position ASC",
        )?;
        let items = stmt
            .query_map(params![id, start_track_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some((name, items)))
    })
}

pub fn get_album_detail(db: &Db, id: &str) -> Result<Option<AlbumDetail>, IpcError> {
    db.read(|conn| {
        let album = conn
            .query_row("SELECT * FROM albums WHERE id = ?1", [id], row_to_album)
            .optional_opt()?;
        let Some(album) = album else { return Ok(None) };
        let mut stmt = conn.prepare_cached(
            "SELECT * FROM tracks WHERE album_id = ?1 ORDER BY COALESCE(disc_number, 1), COALESCE(track_number, 0), title COLLATE NOCASE",
        )?;
        let tracks = stmt.query_map([id], row_to_track)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(AlbumDetail { album, tracks }))
    })
}

/// Every track by an artist, across all their albums, ordered the way "Play
/// all"/"Shuffle"/radio fallbacks want it: album release year (oldest
/// first), then disc/track number. Albums the server didn't report a year
/// for sort last (`NULL`s last via the `IS NULL` tiebreak), not first.
pub fn get_artist_tracks(db: &Db, artist_id: &str) -> Result<Vec<Track>, IpcError> {
    db.read(|conn| {
        let mut stmt = conn.prepare_cached(
            "SELECT t.* FROM tracks t JOIN albums a ON a.id = t.album_id
             WHERE t.artist_id = ?1
             ORDER BY (a.year IS NULL), a.year, a.name COLLATE NOCASE,
                      COALESCE(t.disc_number, 1), COALESCE(t.track_number, 0), t.title COLLATE NOCASE",
        )?;
        let items = stmt.query_map([artist_id], row_to_track)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(items)
    })
}

/// Up to `limit` DISTINCT album cover-art ids used by a playlist's tracks,
/// in track order (first occurrence wins) — the input to the playlist cover
/// mosaic (`art::mosaic`). Tracks with no cover (`cover_art IS NULL`) are
/// skipped rather than producing a blank tile.
pub fn get_playlist_distinct_covers(db: &Db, playlist_id: &str, limit: usize) -> Result<Vec<String>, IpcError> {
    db.read(|conn| {
        let mut stmt = conn.prepare_cached(
            "SELECT t.cover_art FROM playlist_tracks pt JOIN tracks t ON t.id = pt.track_id
             WHERE pt.playlist_id = ?1 AND t.cover_art IS NOT NULL ORDER BY pt.position ASC",
        )?;
        let all = stmt
            .query_map([playlist_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::with_capacity(limit);
        for cover in all {
            if out.len() >= limit {
                break;
            }
            if seen.insert(cover.clone()) {
                out.push(cover);
            }
        }
        Ok(out)
    })
}

/// Local fallback for `radio_start` when the server has no/empty
/// `getSimilarSongs`/`getSimilarSongs2` result (not every server/library has
/// the similarity data): random tracks by the same artist, or failing that
/// (track/album seed with no local artist match) the same genre. `seed_id`
/// itself is excluded so a track/album seed's radio doesn't just replay it
/// first. Never fabricates relationships beyond "same artist"/"same genre".
pub fn radio_fallback_tracks(db: &Db, kind: StarKindLocal, seed_id: &str, limit: usize) -> Result<Vec<String>, IpcError> {
    db.read(|conn| {
        let (artist_id, genre): (Option<String>, Option<String>) = match kind {
            StarKindLocal::Artist => (Some(seed_id.to_string()), None),
            StarKindLocal::Album => conn
                .query_row("SELECT artist_id, genre FROM albums WHERE id = ?1", [seed_id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional_opt()?
                .unwrap_or((None, None)),
            StarKindLocal::Track => conn
                .query_row("SELECT artist_id, genre FROM tracks WHERE id = ?1", [seed_id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional_opt()?
                .unwrap_or((None, None)),
        };
        if let Some(aid) = artist_id {
            let mut stmt = conn.prepare_cached("SELECT id FROM tracks WHERE artist_id = ?1 AND id != ?2 ORDER BY RANDOM() LIMIT ?3")?;
            let ids = stmt
                .query_map(params![aid, seed_id, limit as i64], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if !ids.is_empty() {
                return Ok(ids);
            }
        }
        if let Some(g) = genre {
            let mut stmt = conn.prepare_cached("SELECT id FROM tracks WHERE genre = ?1 AND id != ?2 ORDER BY RANDOM() LIMIT ?3")?;
            return stmt
                .query_map(params![g, seed_id, limit as i64], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>();
        }
        Ok(Vec::new())
    })
}

// ---------------------------------------------------------------------------
// Home sections (cached server-ordered album id lists)
// ---------------------------------------------------------------------------

pub fn set_home_section(db: &Db, kind: HomeSectionKind, album_ids: &[String]) -> Result<(), IpcError> {
    let json = serde_json::to_string(album_ids).map_err(|e| IpcError {
        code: IpcErrorCode::Unknown,
        message: e.to_string(),
    })?;
    let now = chrono::Utc::now().to_rfc3339();
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO home_sections (section, album_ids, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(section) DO UPDATE SET album_ids = excluded.album_ids, updated_at = excluded.updated_at",
            params![kind.key(), json, now],
        )
        .map(|_| ())
    })
}

fn get_home_section_ids(db: &Db, kind: HomeSectionKind) -> Result<Vec<String>, IpcError> {
    let raw = db.read(|conn| {
        conn.query_row(
            "SELECT album_ids FROM home_sections WHERE section = ?1",
            [kind.key()],
            |r| r.get::<_, String>(0),
        )
        .optional_opt()
    })?;
    Ok(raw
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default())
}

fn get_albums_from_home_cache(db: &Db, kind: HomeSectionKind, offset: u64, limit: u64) -> Result<Paged<Album>, IpcError> {
    let ids = get_home_section_ids(db, kind)?;
    let total = ids.len() as u64;
    let page_ids: Vec<&String> = ids.iter().skip(offset as usize).take(limit as usize).collect();
    let albums = get_albums_by_ids_ordered(db, &page_ids.into_iter().cloned().collect::<Vec<_>>())?;
    Ok(Paged {
        items: albums,
        total,
        offset,
    })
}

fn get_albums_by_ids_ordered(db: &Db, ids: &[String]) -> Result<Vec<Album>, IpcError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    db.read(|conn| {
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("SELECT * FROM albums WHERE id IN ({placeholders})");
        let mut stmt = conn.prepare_cached(&sql)?;
        let found = stmt
            .query_map(rusqlite::params_from_iter(ids.iter()), row_to_album)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // Preserve the server's original ordering (IN () doesn't).
        let mut by_id: std::collections::HashMap<String, Album> = found.into_iter().map(|a| (a.id.clone(), a)).collect();
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    })
}

pub fn get_home_sections(db: &Db) -> Result<HomeSections, IpcError> {
    Ok(HomeSections {
        recently_played: get_albums_by_ids_ordered(db, &get_home_section_ids(db, HomeSectionKind::RecentlyPlayed)?)?,
        frequent: get_albums_by_ids_ordered(db, &get_home_section_ids(db, HomeSectionKind::Frequent)?)?,
        newest: get_albums_by_ids_ordered(db, &get_home_section_ids(db, HomeSectionKind::Newest)?)?,
        random: get_albums_by_ids_ordered(db, &get_home_section_ids(db, HomeSectionKind::Random)?)?,
    })
}

// ---------------------------------------------------------------------------
// Search (FTS5)
// ---------------------------------------------------------------------------

pub fn search(db: &Db, text: &str, search_type: SearchType, offset: u64, limit: u64) -> Result<SearchResults, IpcError> {
    let want_tracks = matches!(search_type, SearchType::All | SearchType::Track);
    let want_albums = matches!(search_type, SearchType::All | SearchType::Album);
    let want_artists = matches!(search_type, SearchType::All | SearchType::Artist);
    let want_playlists = matches!(search_type, SearchType::All | SearchType::Playlist);

    let tracks = if want_tracks {
        search_tracks(db, text, offset, limit)?
    } else {
        Paged { items: Vec::new(), total: 0, offset }
    };
    let albums = if want_albums {
        search_albums(db, text, offset, limit)?
    } else {
        Paged { items: Vec::new(), total: 0, offset }
    };
    let artists = if want_artists {
        search_artists(db, text, offset, limit)?
    } else {
        Paged { items: Vec::new(), total: 0, offset }
    };
    let playlists = if want_playlists {
        search_playlists(db, text, offset, limit)?
    } else {
        Paged { items: Vec::new(), total: 0, offset }
    };

    Ok(SearchResults { tracks, albums, artists, playlists })
}

fn search_tracks(db: &Db, text: &str, offset: u64, limit: u64) -> Result<Paged<Track>, IpcError> {
    let Some(q) = fts_prefix_query(text) else {
        return Ok(Paged { items: Vec::new(), total: 0, offset });
    };
    db.read(|conn| {
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tracks_fts WHERE tracks_fts MATCH ?1",
            [&q],
            |r| r.get(0),
        )?;
        let mut stmt = conn.prepare_cached(
            "SELECT t.* FROM tracks_fts f JOIN tracks t ON t.rid = f.rowid
             WHERE tracks_fts MATCH ?1 ORDER BY f.rank LIMIT ?2 OFFSET ?3",
        )?;
        let items = stmt
            .query_map(params![q, limit as i64, offset as i64], row_to_track)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Paged { items, total: total.max(0) as u64, offset })
    })
}

fn search_albums(db: &Db, text: &str, offset: u64, limit: u64) -> Result<Paged<Album>, IpcError> {
    let Some(q) = fts_prefix_query(text) else {
        return Ok(Paged { items: Vec::new(), total: 0, offset });
    };
    db.read(|conn| {
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM albums_fts WHERE albums_fts MATCH ?1",
            [&q],
            |r| r.get(0),
        )?;
        let mut stmt = conn.prepare_cached(
            "SELECT a.* FROM albums_fts f JOIN albums a ON a.rid = f.rowid
             WHERE albums_fts MATCH ?1 ORDER BY f.rank LIMIT ?2 OFFSET ?3",
        )?;
        let items = stmt
            .query_map(params![q, limit as i64, offset as i64], row_to_album)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Paged { items, total: total.max(0) as u64, offset })
    })
}

fn search_artists(db: &Db, text: &str, offset: u64, limit: u64) -> Result<Paged<Artist>, IpcError> {
    let Some(q) = fts_prefix_query(text) else {
        return Ok(Paged { items: Vec::new(), total: 0, offset });
    };
    db.read(|conn| {
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM artists_fts WHERE artists_fts MATCH ?1",
            [&q],
            |r| r.get(0),
        )?;
        let mut stmt = conn.prepare_cached(
            "SELECT a.* FROM artists_fts f JOIN artists a ON a.rid = f.rowid
             WHERE artists_fts MATCH ?1 ORDER BY f.rank LIMIT ?2 OFFSET ?3",
        )?;
        let items = stmt
            .query_map(params![q, limit as i64, offset as i64], row_to_artist)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Paged { items, total: total.max(0) as u64, offset })
    })
}

fn search_playlists(db: &Db, text: &str, offset: u64, limit: u64) -> Result<Paged<Playlist>, IpcError> {
    let Some(q) = fts_prefix_query(text) else {
        return Ok(Paged { items: Vec::new(), total: 0, offset });
    };
    db.read(|conn| {
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM playlists_fts WHERE playlists_fts MATCH ?1",
            [&q],
            |r| r.get(0),
        )?;
        let mut stmt = conn.prepare_cached(
            "SELECT p.* FROM playlists_fts f JOIN playlists p ON p.rid = f.rowid
             WHERE playlists_fts MATCH ?1 ORDER BY f.rank LIMIT ?2 OFFSET ?3",
        )?;
        let items = stmt
            .query_map(params![q, limit as i64, offset as i64], row_to_playlist)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Paged { items, total: total.max(0) as u64, offset })
    })
}

// ---------------------------------------------------------------------------
// Playlists (local mirror; server is always written first by `ipc.rs`)
// ---------------------------------------------------------------------------

pub fn playlists_list(db: &Db) -> Result<Vec<Playlist>, IpcError> {
    db.read(|conn| {
        let mut stmt = conn.prepare_cached("SELECT * FROM playlists ORDER BY name COLLATE NOCASE ASC")?;
        let items = stmt.query_map([], row_to_playlist)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(items)
    })
}

pub fn playlist_detail(db: &Db, id: &str) -> Result<Option<PlaylistDetail>, IpcError> {
    db.read(|conn| {
        let playlist = conn
            .query_row("SELECT * FROM playlists WHERE id = ?1", [id], row_to_playlist)
            .optional_opt()?;
        let Some(playlist) = playlist else { return Ok(None) };
        let mut stmt = conn.prepare_cached(
            "SELECT t.* FROM playlist_tracks pt JOIN tracks t ON t.id = pt.track_id
             WHERE pt.playlist_id = ?1 ORDER BY pt.position ASC",
        )?;
        let tracks = stmt.query_map([id], row_to_track)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(PlaylistDetail { playlist, tracks }))
    })
}

pub fn upsert_playlist_meta(db: &Db, playlist: &Playlist) -> Result<(), IpcError> {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO playlists (id, name, comment, owner, public, song_count, duration, cover_art, created_at, updated_at, readonly)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, comment=excluded.comment, owner=excluded.owner,
                public=excluded.public, song_count=excluded.song_count, duration=excluded.duration,
                cover_art=excluded.cover_art, updated_at=excluded.updated_at,
                readonly=COALESCE(excluded.readonly, playlists.readonly)",
            params![
                playlist.id,
                playlist.name,
                playlist.comment,
                playlist.owner,
                playlist.public as i64,
                playlist.song_count,
                playlist.duration,
                playlist.cover_art,
                playlist.created_at,
                playlist.updated_at,
                playlist.server_read_only.map(i64::from),
            ],
        )?;
        // playlists_fts follows via triggers (schema.rs).
        Ok(())
    })
}

pub fn rename_playlist_local(db: &Db, id: &str, name: &str) -> Result<(), IpcError> {
    db.with_conn(|conn| {
        conn.execute("UPDATE playlists SET name = ?1 WHERE id = ?2", params![name, id])
            .map(|_| ())
    })
}

pub fn delete_playlist_local(db: &Db, id: &str) -> Result<(), IpcError> {
    db.with_tx(|tx| {
        tx.execute("DELETE FROM playlists WHERE id = ?1", [id])?;
        tx.execute("DELETE FROM playlist_tracks WHERE playlist_id = ?1", [id])?;
        Ok(())
    })
}

/// Overwrites the local track list for `id` with `track_ids` in order
/// (used after add/remove/reorder round-trip to the server, which is the
/// source of truth for final ordering).
pub fn set_playlist_tracks_local(db: &Db, id: &str, track_ids: &[String]) -> Result<(), IpcError> {
    db.with_tx(|tx| {
        tx.execute("DELETE FROM playlist_tracks WHERE playlist_id = ?1", [id])?;
        for (position, track_id) in track_ids.iter().enumerate() {
            tx.execute(
                "INSERT INTO playlist_tracks (playlist_id, position, track_id) VALUES (?1, ?2, ?3)",
                params![id, position as i64, track_id],
            )?;
        }
        tx.execute(
            "UPDATE playlists SET song_count = ?2 WHERE id = ?1",
            params![id, track_ids.len() as i64],
        )?;
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Stars
// ---------------------------------------------------------------------------

pub fn set_starred_local(db: &Db, id: &str, kind: StarKindLocal, starred_at: Option<String>) -> Result<(), IpcError> {
    let table = match kind {
        StarKindLocal::Track => "tracks",
        StarKindLocal::Album => "albums",
        StarKindLocal::Artist => "artists",
    };
    db.with_conn(|conn| {
        conn.execute(
            &format!("UPDATE {table} SET starred = ?1 WHERE id = ?2"),
            params![starred_at, id],
        )
        .map(|_| ())
    })
}

// ---------------------------------------------------------------------------
// Ratings
// ---------------------------------------------------------------------------

/// `rating` of `0` clears the rating (stored as `NULL`, same convention as
/// `rating_from_raw`/`non_zero_u8`: "0 stars" and "unrated" are the same
/// thing, never rendered as a filled star).
pub fn set_rating_local(db: &Db, id: &str, kind: RatingKindLocal, rating: u8) -> Result<(), IpcError> {
    let table = match kind {
        RatingKindLocal::Track => "tracks",
        RatingKindLocal::Album => "albums",
        RatingKindLocal::Artist => "artists",
    };
    let value: Option<i64> = (rating > 0).then_some(i64::from(rating.min(5)));
    db.with_conn(|conn| {
        conn.execute(&format!("UPDATE {table} SET rating = ?1 WHERE id = ?2"), params![value, id])
            .map(|_| ())
    })
}

// ---------------------------------------------------------------------------
// small helper: `query_row` that maps "no rows" to `Ok(None)` instead of the
// `rusqlite::Error::QueryReturnedNoRows` that `Result<T,_>::optional()` also
// handles, but spelled out here to avoid importing `OptionalExtension` into
// every call site above.
// ---------------------------------------------------------------------------

trait OptionalRow<T> {
    fn optional_opt(self) -> rusqlite::Result<Option<T>>;
}

impl<T> OptionalRow<T> for rusqlite::Result<T> {
    fn optional_opt(self) -> rusqlite::Result<Option<T>> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{AlbumSort, ArtistSort, SearchType};
    use crate::library::sync::{run_full_sync, NullProgressSink};

    fn seed(db: &Db) {
        db.with_tx(|tx| {
            tx.execute(
                "INSERT INTO artists (id, name, album_count) VALUES ('ar1','Alpha Band',1)",
                [],
            )?;
            tx.execute(
                "INSERT INTO albums (id, name, artist, artist_id, song_count, duration, year, created)
                 VALUES ('al1','First Light','Alpha Band','ar1',2,300,2020,'2020-01-01')",
                [],
            )?;
            tx.execute(
                "INSERT INTO tracks (id, title, artist, artist_id, album, album_id, duration, suffix, sampling_rate, channel_count, track_number, disc_number)
                 VALUES ('tr1','Opening','Alpha Band','ar1','First Light','al1',150,'flac',44100,2,1,1)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn get_artists_pages_and_sorts() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        let page = get_artists(&db, 0, 10, Some(ArtistSort::Alphabetical)).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].name, "Alpha Band");
    }

    /// `get_tracks_by_ids` backs the audio engine's track-metadata resolution
    /// (`audio::engine::Engine::resolve_tracks`): queued track ids must come
    /// back with real metadata, and an id with no local row must simply be
    /// absent (never a fabricated placeholder row).
    #[test]
    fn get_tracks_by_ids_returns_known_rows_and_skips_unknown() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        let found = get_tracks_by_ids(&db, &["tr1".to_string(), "does-not-exist".to_string()]).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "tr1");
        assert_eq!(found[0].title, "Opening");
        assert_eq!(found[0].album.as_deref(), Some("First Light"));
        assert_eq!(found[0].format.suffix, "flac");
        assert_eq!(found[0].format.sampling_rate, 44100);
    }

    #[test]
    fn track_artists_for_ids_spans_chunks_and_skips_unknown() {
        let db = Db::open_in_memory().unwrap();
        db.with_tx(|tx| {
            for i in 0..1_100 {
                tx.execute(
                    "INSERT INTO tracks (id, title, artist, duration, suffix) VALUES (?1, 't', ?2, 1, 'flac')",
                    rusqlite::params![format!("t{i}"), format!("a{}", i % 7)],
                )?;
            }
            Ok(())
        })
        .unwrap();
        // 1,600 ids = 4 chunks; the last 500 are unknown.
        let ids: Vec<String> = (0..1_600).map(|i| format!("t{i}")).collect();
        let got = track_artists_for_ids(&db, &ids).unwrap();
        assert_eq!(got.len(), 1_100, "only ids with a row come back");
        assert_eq!(got["t0"], "a0");
        assert_eq!(got["t1099"], format!("a{}", 1099 % 7));
        assert!(!got.contains_key("t1100"));
        assert!(track_artists_for_ids(&db, &[]).unwrap().is_empty());
    }

    #[test]
    fn get_tracks_by_ids_empty_input_short_circuits() {
        let db = Db::open_in_memory().unwrap();
        assert!(get_tracks_by_ids(&db, &[]).unwrap().is_empty());
    }

    #[test]
    fn get_album_detail_includes_ordered_tracks() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        let detail = get_album_detail(&db, "al1").unwrap().unwrap();
        assert_eq!(detail.album.name, "First Light");
        assert_eq!(detail.tracks.len(), 1);
        assert_eq!(detail.tracks[0].id, "tr1");
    }

    #[test]
    fn get_albums_filters_by_genre_and_year() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        let page = get_albums(
            &db,
            0,
            10,
            AlbumSort::Alphabetical,
            Some(AlbumFilter { genre: None, year: Some(1999), starred_only: None }),
        )
        .unwrap();
        assert_eq!(page.total, 0);
    }

    #[test]
    fn search_prefix_matches_track_title() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        let results = search(&db, "open", SearchType::All, 0, 20).unwrap();
        assert_eq!(results.tracks.total, 1);
        assert_eq!(results.tracks.items[0].id, "tr1");
        assert_eq!(results.albums.total, 0, "query doesn't prefix-match the album name");
    }

    #[test]
    fn search_specific_type_leaves_others_empty() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        let results = search(&db, "alpha", SearchType::Artist, 0, 20).unwrap();
        assert_eq!(results.artists.total, 1);
        assert_eq!(results.albums.total, 0);
        assert_eq!(results.tracks.total, 0);
    }

    #[test]
    fn star_and_unstar_round_trip_locally() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        set_starred_local(&db, "tr1", StarKindLocal::Track, Some("2026-01-01T00:00:00Z".into())).unwrap();
        let detail = get_album_detail(&db, "al1").unwrap().unwrap();
        assert_eq!(detail.tracks[0].starred.as_deref(), Some("2026-01-01T00:00:00Z"));
        set_starred_local(&db, "tr1", StarKindLocal::Track, None).unwrap();
        let detail = get_album_detail(&db, "al1").unwrap().unwrap();
        assert_eq!(detail.tracks[0].starred, None);
    }

    #[test]
    fn playlist_tracks_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        upsert_playlist_meta(
            &db,
            &Playlist {
                id: "pl1".into(),
                name: "Test".into(),
                comment: None,
                owner: None,
                public: false,
                song_count: 0,
                duration: 0.0,
                cover_art: None,
                created_at: None,
                updated_at: None,
                smart: false,
                read_only: false,
                server_read_only: None,
            },
        )
        .unwrap();
        set_playlist_tracks_local(&db, "pl1", &["tr1".to_string()]).unwrap();
        let detail = playlist_detail(&db, "pl1").unwrap().unwrap();
        assert_eq!(detail.tracks.len(), 1);
        assert_eq!(detail.tracks[0].id, "tr1");

        rename_playlist_local(&db, "pl1", "Renamed").unwrap();
        let detail = playlist_detail(&db, "pl1").unwrap().unwrap();
        assert_eq!(detail.playlist.name, "Renamed");
        // FTS follows create + rename + delete via triggers.
        assert_eq!(search(&db, "renam", SearchType::Playlist, 0, 10).unwrap().playlists.total, 1);
        assert_eq!(search(&db, "test", SearchType::Playlist, 0, 10).unwrap().playlists.total, 0);

        delete_playlist_local(&db, "pl1").unwrap();
        assert!(playlist_detail(&db, "pl1").unwrap().is_none());
        assert_eq!(search(&db, "renam", SearchType::Playlist, 0, 10).unwrap().playlists.total, 0);
    }

    #[test]
    fn get_artist_tracks_orders_by_album_year_then_track_number() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        db.with_tx(|tx| {
            tx.execute(
                "INSERT INTO albums (id, name, artist, artist_id, song_count, duration, year)
                 VALUES ('al0','Earlier','Alpha Band','ar1',1,120,2010)",
                [],
            )?;
            tx.execute(
                "INSERT INTO tracks (id, title, artist, artist_id, album, album_id, duration, suffix, sampling_rate, channel_count, track_number, disc_number)
                 VALUES ('tr0','Prelude','Alpha Band','ar1','Earlier','al0',120,'flac',44100,2,1,1)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let tracks = get_artist_tracks(&db, "ar1").unwrap();
        assert_eq!(tracks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec!["tr0", "tr1"]);
    }

    #[test]
    fn set_rating_local_round_trips_and_zero_clears() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        set_rating_local(&db, "tr1", RatingKindLocal::Track, 4).unwrap();
        assert_eq!(get_tracks_by_ids(&db, &["tr1".into()]).unwrap()[0].rating, Some(4));
        set_rating_local(&db, "tr1", RatingKindLocal::Track, 0).unwrap();
        assert_eq!(get_tracks_by_ids(&db, &["tr1".into()]).unwrap()[0].rating, None);
    }

    #[test]
    fn playlist_distinct_covers_dedupes_in_track_order() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        db.with_tx(|tx| {
            tx.execute(
                "INSERT INTO tracks (id, title, artist, artist_id, album, album_id, duration, suffix, sampling_rate, channel_count, cover_art)
                 VALUES ('tr2','Second','Alpha Band','ar1','First Light','al1',150,'flac',44100,2,'al1')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        db.with_conn(|conn| conn.execute("UPDATE tracks SET cover_art = 'al1' WHERE id = 'tr1'", [])).unwrap();
        upsert_playlist_meta(
            &db,
            &Playlist {
                id: "pl1".into(),
                name: "Test".into(),
                comment: None,
                owner: None,
                public: false,
                song_count: 0,
                duration: 0.0,
                cover_art: None,
                created_at: None,
                updated_at: None,
                smart: false,
                read_only: false,
                server_read_only: None,
            },
        )
        .unwrap();
        set_playlist_tracks_local(&db, "pl1", &["tr1".to_string(), "tr2".to_string()]).unwrap();
        let covers = get_playlist_distinct_covers(&db, "pl1", 4).unwrap();
        assert_eq!(covers, vec!["al1".to_string()]);
    }

    /// A handful of tracks spanning distinct titles/artists/albums/years/
    /// durations/formats/dates-added, for exercising every `TrackSortField`.
    fn seed_tracks(db: &Db) {
        db.with_tx(|tx| {
            tx.execute(
                "INSERT INTO tracks (id, title, artist, album, year, duration, suffix, sampling_rate, channel_count, created)
                 VALUES
                 ('t1','Zebra Crossing','Nova','Red Album',2019,120,'mp3',44100,2,'2021-06-01'),
                 ('t2','Apple Pie','Comet','Blue Album',2021,300,'flac',96000,2,'2023-01-15'),
                 ('t3','Midnight Run','Nova','Blue Album',2020,210,'flac',44100,2,'2022-03-10')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn get_tracks_sorts_every_field_asc_and_desc() {
        let db = Db::open_in_memory().unwrap();
        seed_tracks(&db);

        let by_title = get_tracks(&db, 0, 10, TrackSortField::Title, SortDir::Asc, None).unwrap();
        assert_eq!(by_title.total, 3);
        assert_eq!(by_title.items.iter().map(|t| &t.id).collect::<Vec<_>>(), vec!["t2", "t3", "t1"]);

        let by_title_desc = get_tracks(&db, 0, 10, TrackSortField::Title, SortDir::Desc, None).unwrap();
        assert_eq!(by_title_desc.items.iter().map(|t| &t.id).collect::<Vec<_>>(), vec!["t1", "t3", "t2"]);

        // Artist ties (two Nova tracks) break by title ascending.
        let by_artist = get_tracks(&db, 0, 10, TrackSortField::Artist, SortDir::Asc, None).unwrap();
        assert_eq!(by_artist.items.iter().map(|t| &t.id).collect::<Vec<_>>(), vec!["t2", "t3", "t1"]);

        let by_duration = get_tracks(&db, 0, 10, TrackSortField::Duration, SortDir::Asc, None).unwrap();
        assert_eq!(by_duration.items.iter().map(|t| &t.id).collect::<Vec<_>>(), vec!["t1", "t3", "t2"]);

        let by_year_desc = get_tracks(&db, 0, 10, TrackSortField::Year, SortDir::Desc, None).unwrap();
        assert_eq!(by_year_desc.items.iter().map(|t| &t.id).collect::<Vec<_>>(), vec!["t2", "t3", "t1"]);

        let by_date_added_desc = get_tracks(&db, 0, 10, TrackSortField::DateAdded, SortDir::Desc, None).unwrap();
        assert_eq!(by_date_added_desc.items.iter().map(|t| &t.id).collect::<Vec<_>>(), vec!["t2", "t3", "t1"]);

        let by_format = get_tracks(&db, 0, 10, TrackSortField::Format, SortDir::Asc, None).unwrap();
        assert_eq!(by_format.items.iter().map(|t| &t.id).collect::<Vec<_>>(), vec!["t2", "t3", "t1"]);
    }

    #[test]
    fn get_tracks_pages_within_a_sort() {
        let db = Db::open_in_memory().unwrap();
        seed_tracks(&db);
        let page = get_tracks(&db, 1, 1, TrackSortField::Title, SortDir::Asc, None).unwrap();
        assert_eq!(page.total, 3);
        assert_eq!(page.offset, 1);
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, "t3"); // middle of Apple/Midnight/Zebra
    }

    #[test]
    fn get_tracks_filters_by_title_artist_or_album_prefix() {
        let db = Db::open_in_memory().unwrap();
        seed_tracks(&db);
        let by_title = get_tracks(&db, 0, 10, TrackSortField::Title, SortDir::Asc, Some("zebra")).unwrap();
        assert_eq!(by_title.items.iter().map(|t| &t.id).collect::<Vec<_>>(), vec!["t1"]);

        let by_artist = get_tracks(&db, 0, 10, TrackSortField::Title, SortDir::Asc, Some("nova")).unwrap();
        assert_eq!(by_artist.total, 2);

        let by_album = get_tracks(&db, 0, 10, TrackSortField::Title, SortDir::Asc, Some("blue")).unwrap();
        assert_eq!(by_album.total, 2);

        let no_match = get_tracks(&db, 0, 10, TrackSortField::Title, SortDir::Asc, Some("nonexistent")).unwrap();
        assert_eq!(no_match.total, 0);
    }

    #[test]
    fn get_all_track_ids_returns_every_id_with_its_artist_in_sorted_order() {
        let db = Db::open_in_memory().unwrap();
        seed_tracks(&db);
        let all = get_all_track_ids(&db, TrackSortField::Title, SortDir::Asc, None, None).unwrap();
        assert_eq!(all, vec![
            ("t2".to_string(), "Comet".to_string()),
            ("t3".to_string(), "Nova".to_string()),
            ("t1".to_string(), "Nova".to_string()),
        ]);
    }

    #[test]
    fn get_all_track_ids_respects_the_filter() {
        let db = Db::open_in_memory().unwrap();
        seed_tracks(&db);
        let filtered = get_all_track_ids(&db, TrackSortField::Title, SortDir::Asc, Some("nova"), None).unwrap();
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().all(|(_, artist)| artist == "Nova"));
    }

    #[test]
    fn get_all_track_ids_skips_one_star_tracks_unless_explicitly_started() {
        let db = Db::open_in_memory().unwrap();
        seed_tracks(&db);
        set_rating_local(&db, "t3", RatingKindLocal::Track, 1).unwrap();
        set_rating_local(&db, "t2", RatingKindLocal::Track, 3).unwrap();
        let ids = |v: Vec<(String, String)>| v.into_iter().map(|(id, _)| id).collect::<Vec<_>>();
        assert_eq!(ids(get_all_track_ids(&db, TrackSortField::Title, SortDir::Asc, None, None).unwrap()), vec!["t2", "t1"]);
        assert_eq!(ids(get_all_track_ids(&db, TrackSortField::Title, SortDir::Asc, Some("nova"), None).unwrap()), vec!["t1"]);
        assert_eq!(
            ids(get_all_track_ids(&db, TrackSortField::Title, SortDir::Asc, None, Some("t3")).unwrap()),
            vec!["t2", "t3", "t1"]
        );
    }

    #[test]
    fn playlist_play_source_skips_one_star_tracks_unless_explicitly_started() {
        let db = Db::open_in_memory().unwrap();
        seed_tracks(&db);
        db.with_tx(|tx| {
            tx.execute("INSERT INTO playlists (id, name) VALUES ('p1', 'Mix')", [])?;
            Ok(())
        })
        .unwrap();
        set_playlist_tracks_local(&db, "p1", &["t3".into(), "t1".into(), "t2".into()]).unwrap();
        set_rating_local(&db, "t1", RatingKindLocal::Track, 1).unwrap();
        let ids = |v: Option<(String, PlaySourceIds)>| v.unwrap().1.into_iter().map(|(id, _)| id).collect::<Vec<_>>();
        assert_eq!(ids(get_playlist_play_source(&db, "p1", None).unwrap()), vec!["t3", "t2"]);
        assert_eq!(ids(get_playlist_play_source(&db, "p1", Some("t1")).unwrap()), vec!["t3", "t1", "t2"]);
        assert!(get_playlist_play_source(&db, "nope", None).unwrap().is_none());
    }

    // silence "unused" for run_full_sync/NullProgressSink imports used only
    // by the ignored integration tests in `subsonic`/`library` top-level
    // test modules; kept here so `cargo test` in this module alone compiles
    // even if those are feature-gated differently in the future.
    #[allow(dead_code)]
    fn _reference(_: &dyn Fn()) {
        let _ = run_full_sync;
        let _ = NullProgressSink;
    }
}
