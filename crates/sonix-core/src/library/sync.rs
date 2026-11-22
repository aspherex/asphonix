//! Library sync: mirrors the server catalog into the local SQLite DB
//! **in place**, without ever emptying the live tables.
//!
//! ## Change detection (the common case does one cheap call)
//!
//! Subsonic has no "changes since X" endpoint, but Navidrome's
//! `getScanStatus` reports `lastScan` (+ the track `count`): the catalog can
//! only change when a scan completes. After every successful catalog sync we
//! store that fingerprint in `meta`; an incremental sync whose fingerprint
//! still matches skips the catalog entirely and only refreshes the small,
//! user-driven state that changes without a scan (stars via `getStarred2`,
//! playlists via `getPlaylists` + `getPlaylist` only for changed ones, and
//! the home-section id lists). The background connection monitor already
//! polls `getScanStatus` as its ping and hands the result in, so a
//! no-change startup costs no extra round-trip for the check itself.
//!
//! ## Catalog sync without an empty/partial library
//!
//! When the catalog did change (or a full sync is forced), every artist/
//! album/track is UPSERTed by id into the live tables with the current
//! *sync generation* (`meta.syncGen`, +1 per catalog sync). Only after the
//! complete list for a table has been fetched are rows with an older
//! generation (deleted on the server) removed. Each page is its own short
//! write transaction on the writer connection; readers (separate WAL
//! connection) see the old row or the new row, never a missing one, and the
//! FTS indexes follow via triggers (`schema.rs`). A sync that fails or is
//! cancelled half-way leaves the previous complete library plus some
//! already-updated rows — still consistent — and the next sync finishes the
//! job (its higher generation also sweeps anything stale).

use std::collections::{HashMap, HashSet};

use crate::ipc::{IpcError, IpcErrorCode, SyncPhase};
use crate::subsonic::{Client, RawAlbum, RawArtist, RawPlaylist, RawSong, ScanStatus};

use super::queries::HomeSectionKind;
use super::{set_meta_tx, Db};

/// Receives sync progress. The backend forwards it to the
/// UI as sync-progress events; a no-op implementation is used by
/// integration tests.
pub trait SyncProgressSink: Send + Sync {
    fn progress(&self, phase: SyncPhase, current: u64, total: u64, message: Option<&str>);

    /// Checked at the start of every sync write transaction (under the DB
    /// writer lock). Returning `true` (logout / account switch happened
    /// since this sync started) aborts the sync before it writes anything
    /// else, so a stale sync can never repopulate a just-cleared DB.
    fn cancelled(&self) -> bool {
        false
    }
}

/// Does nothing — used where a sink is required but progress isn't observed
/// (tests, `#[ignore]` integration tests).
#[allow(dead_code)] // used by tests
pub struct NullProgressSink;
impl SyncProgressSink for NullProgressSink {
    fn progress(&self, _phase: SyncPhase, _current: u64, _total: u64, _message: Option<&str>) {}
}

/// What a sync actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOutcome {
    /// Server catalog unchanged (or mid-scan): only stars/playlists/home
    /// sections were refreshed.
    CatalogUnchanged,
    /// Artists/albums/tracks were re-fetched and reconciled.
    CatalogSynced,
}

const ALBUM_LIST_TYPE: &str = "alphabeticalByName";
const PAGE: u32 = 500;

pub(crate) const META_LAST_SYNC_AT: &str = "lastSyncAt";
/// `"{lastScan}|{count}"` from `getScanStatus` as of the last complete
/// catalog sync.
pub(crate) const META_SERVER_FINGERPRINT: &str = "serverScanFingerprint";
const META_SYNC_GEN: &str = "syncGen";

/// User-initiated full sync (first login, or "Resync" in Settings): always
/// re-fetches the catalog (still in place, never truncating).
pub async fn run_full_sync(client: &Client, db: &Db, progress: &dyn SyncProgressSink) -> Result<SyncOutcome, IpcError> {
    run_sync_logged(client, db, progress, true, None).await
}

/// Background resync: skips the catalog when the server's scan fingerprint
/// is unchanged. `known_status` lets the caller pass a `getScanStatus`
/// result it already has (the connection monitor's ping) to avoid a second
/// round-trip.
pub async fn run_incremental_sync(
    client: &Client,
    db: &Db,
    progress: &dyn SyncProgressSink,
    known_status: Option<ScanStatus>,
) -> Result<SyncOutcome, IpcError> {
    run_sync_logged(client, db, progress, false, known_status).await
}

/// [`run_sync`] with its outcome logged. An unchanged catalog (the connection monitor's routine check) and
/// a failed background sync stay at debug, so an idle or offline app does not log periodically.
async fn run_sync_logged(
    client: &Client,
    db: &Db,
    progress: &dyn SyncProgressSink,
    full: bool,
    known_status: Option<ScanStatus>,
) -> Result<SyncOutcome, IpcError> {
    let started = std::time::Instant::now();
    let r = run_sync(client, db, progress, full, known_status).await;
    let duration_ms = started.elapsed().as_millis() as u64;
    match &r {
        Ok(SyncOutcome::CatalogSynced) => tracing::info!(full, duration_ms, "sync: catalog synced"),
        Ok(SyncOutcome::CatalogUnchanged) => tracing::debug!(full, duration_ms, "sync: catalog unchanged"),
        Err(e) if full => tracing::warn!(full, duration_ms, error = %e.message, "sync failed"),
        Err(e) => tracing::debug!(full, duration_ms, error = %e.message, "sync failed"),
    }
    r
}

/// The fingerprint stored after a catalog sync; `None` while a scan is
/// running or when the server doesn't report `lastScan` (then nothing can be
/// skipped).
pub fn scan_fingerprint(status: &ScanStatus) -> Option<String> {
    if status.scanning {
        return None;
    }
    status
        .last_scan
        .as_ref()
        .map(|ls| format!("{ls}|{}", status.count.unwrap_or(0)))
}

async fn run_sync(
    client: &Client,
    db: &Db,
    progress: &dyn SyncProgressSink,
    force: bool,
    known_status: Option<ScanStatus>,
) -> Result<SyncOutcome, IpcError> {
    let result = sync_inner(client, db, progress, force, known_status).await;
    match &result {
        Ok(outcome) => {
            let msg = match outcome {
                SyncOutcome::CatalogUnchanged => "unchanged",
                SyncOutcome::CatalogSynced => "synced",
            };
            progress.progress(SyncPhase::Done, 0, 0, Some(msg));
        }
        Err(e) => progress.progress(SyncPhase::Error, 0, 0, Some(&e.message)),
    }
    result
}

async fn sync_inner(
    client: &Client,
    db: &Db,
    progress: &dyn SyncProgressSink,
    force: bool,
    known_status: Option<ScanStatus>,
) -> Result<SyncOutcome, IpcError> {
    let status = match known_status {
        Some(s) => Some(s),
        None => match client.get_scan_status().await {
            Ok((s, _)) => Some(s),
            Err(e) if e.code == IpcErrorCode::Network => return Err(e),
            // Not fatal: without a fingerprint we just can't skip.
            Err(_) => None,
        },
    };
    let fingerprint = status.as_ref().and_then(scan_fingerprint);
    let scanning = status.as_ref().is_some_and(|s| s.scanning);
    let have_catalog = db.track_count()? > 0;
    let stored = db.get_meta(META_SERVER_FINGERPRINT)?;

    let unchanged = fingerprint.is_some() && fingerprint == stored;
    // Mid-scan the server catalog is in flux; keep the current complete
    // mirror and let the monitor trigger a resync once `lastScan` moves.
    if !force && have_catalog && (unchanged || scanning) {
        refresh_user_state(client, db, progress).await?;
        let now = chrono::Utc::now().to_rfc3339();
        write(db, progress, |tx| set_meta_tx(tx, META_LAST_SYNC_AT, &now))?;
        return Ok(SyncOutcome::CatalogUnchanged);
    }

    let r#gen: i64 = db
        .get_meta(META_SYNC_GEN)?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
        + 1;
    write(db, progress, |tx| set_meta_tx(tx, META_SYNC_GEN, &r#gen.to_string()))?;

    let track_total_hint = status.as_ref().and_then(|s| s.count).unwrap_or(0);
    sync_artists(client, db, progress, r#gen).await?;
    sync_albums(client, db, progress, r#gen).await?;
    sync_tracks(client, db, progress, r#gen, track_total_hint).await?;
    sync_playlists(client, db, progress).await?;
    // Best-effort: home section caching failing shouldn't fail the whole
    // sync (the UI keeps the previous lists until the next sync).
    if let Err(e) = refresh_home_sections(client, db, progress).await {
        tracing::warn!(error = %e.message, "home section refresh failed");
    }

    let now = chrono::Utc::now().to_rfc3339();
    write(db, progress, |tx| {
        set_meta_tx(tx, META_LAST_SYNC_AT, &now)?;
        match &fingerprint {
            // Fingerprint taken *before* the catalog fetch: if a scan
            // finished meanwhile, the next check sees a newer one and syncs.
            Some(fp) => set_meta_tx(tx, META_SERVER_FINGERPRINT, fp),
            None => tx
                .execute("DELETE FROM meta WHERE key = ?1", [META_SERVER_FINGERPRINT])
                .map(|_| ()),
        }
    })?;
    Ok(SyncOutcome::CatalogSynced)
}

/// One sync write transaction, aborted (without writing) if the sync was
/// cancelled — the check runs under the writer lock, so it's ordered with
/// `Db::clear_all`.
fn write<T>(
    db: &Db,
    progress: &dyn SyncProgressSink,
    f: impl FnOnce(&rusqlite::Transaction) -> rusqlite::Result<T>,
) -> Result<T, IpcError> {
    let mut was_cancelled = false;
    let result = db.with_tx(|tx| {
        if progress.cancelled() {
            was_cancelled = true;
            return Err(rusqlite::Error::InvalidQuery);
        }
        f(tx)
    });
    if was_cancelled {
        return Err(IpcError {
            code: IpcErrorCode::Unknown,
            message: "sync cancelled".into(),
        });
    }
    result
}

/// Stars, playlists and home sections: state that changes without a server
/// scan. Run concurrently (all are small single requests, except
/// `getPlaylist` for playlists that actually changed).
async fn refresh_user_state(client: &Client, db: &Db, progress: &dyn SyncProgressSink) -> Result<(), IpcError> {
    let (stars, playlists, home) = futures_util::join!(
        refresh_stars(client, db, progress),
        sync_playlists(client, db, progress),
        refresh_home_sections(client, db, progress)
    );
    stars?;
    playlists?;
    if let Err(e) = home {
        tracing::warn!(error = %e.message, "home section refresh failed");
    }
    Ok(())
}

async fn sync_artists(client: &Client, db: &Db, progress: &dyn SyncProgressSink, r#gen: i64) -> Result<(), IpcError> {
    let artists = client.get_artists().await?;
    let total = artists.len() as u64;
    progress.progress(SyncPhase::Artists, 0, total, None);
    // `getArtists` is a single response, so upsert + stale sweep fit in one
    // transaction.
    write(db, progress, |tx| {
        for a in &artists {
            upsert_artist(tx, a, r#gen)?;
        }
        tx.execute("DELETE FROM artists WHERE gen < ?1", [r#gen])?;
        Ok(())
    })?;
    progress.progress(SyncPhase::Artists, total, total, None);
    Ok(())
}

async fn sync_albums(client: &Client, db: &Db, progress: &dyn SyncProgressSink, r#gen: i64) -> Result<(), IpcError> {
    let mut offset: u32 = 0;
    let mut current: u64 = 0;
    loop {
        let page: Vec<RawAlbum> = client.get_album_list2(ALBUM_LIST_TYPE, offset, PAGE).await?;
        let n = page.len();
        if n > 0 {
            write(db, progress, |tx| {
                for a in &page {
                    upsert_album(tx, a, r#gen)?;
                }
                Ok(())
            })?;
        }
        current += n as u64;
        progress.progress(SyncPhase::Albums, current, current, None);
        if n < PAGE as usize {
            break;
        }
        offset += PAGE;
    }
    write(db, progress, |tx| tx.execute("DELETE FROM albums WHERE gen < ?1", [r#gen]).map(|_| ()))?;
    Ok(())
}

async fn sync_tracks(
    client: &Client,
    db: &Db,
    progress: &dyn SyncProgressSink,
    r#gen: i64,
    total_hint: u64,
) -> Result<(), IpcError> {
    let mut offset: u32 = 0;
    let mut current: u64 = 0;
    progress.progress(SyncPhase::Tracks, 0, total_hint, None);
    loop {
        let page: Vec<RawSong> = client.search_songs_page(offset, PAGE).await?;
        let n = page.len();
        if n > 0 {
            write(db, progress, |tx| {
                for s in &page {
                    upsert_track(tx, s, r#gen)?;
                }
                Ok(())
            })?;
        }
        current += n as u64;
        progress.progress(SyncPhase::Tracks, current, total_hint.max(current), None);
        if n < PAGE as usize {
            break;
        }
        offset += PAGE;
    }
    write(db, progress, |tx| tx.execute("DELETE FROM tracks WHERE gen < ?1", [r#gen]).map(|_| ()))?;
    Ok(())
}

/// Applies `getStarred2` in one transaction: clears every local star, then
/// sets the server's (readers see the before or after state, never a mix).
async fn refresh_stars(client: &Client, db: &Db, progress: &dyn SyncProgressSink) -> Result<(), IpcError> {
    let starred = client.get_starred2().await?;
    write(db, progress, |tx| {
        tx.execute("UPDATE tracks SET starred = NULL WHERE starred IS NOT NULL", [])?;
        tx.execute("UPDATE albums SET starred = NULL WHERE starred IS NOT NULL", [])?;
        tx.execute("UPDATE artists SET starred = NULL WHERE starred IS NOT NULL", [])?;
        {
            let mut st = tx.prepare_cached("UPDATE tracks SET starred = ?2 WHERE id = ?1")?;
            for s in &starred.song {
                st.execute(rusqlite::params![s.id, s.starred])?;
            }
        }
        {
            let mut st = tx.prepare_cached("UPDATE albums SET starred = ?2 WHERE id = ?1")?;
            for a in &starred.album {
                st.execute(rusqlite::params![a.id, a.starred])?;
            }
        }
        let mut st = tx.prepare_cached("UPDATE artists SET starred = ?2 WHERE id = ?1")?;
        for a in &starred.artist {
            st.execute(rusqlite::params![a.id, a.starred])?;
        }
        Ok(())
    })
}

/// Refreshes the small cached id lists backing the four home-page sections
/// (`browse_get_home_sections`): four concurrent `getAlbumList2` calls
/// (~50 ids each), written in one transaction. Never on the startup
/// critical path.
pub async fn refresh_home_sections(client: &Client, db: &Db, progress: &dyn SyncProgressSink) -> Result<(), IpcError> {
    const SECTION_SIZE: u32 = 50;
    let kinds = [
        HomeSectionKind::RecentlyPlayed,
        HomeSectionKind::Frequent,
        HomeSectionKind::Newest,
        HomeSectionKind::Random,
    ];
    let (a, b, c, d) = futures_util::join!(
        client.get_album_list2(kinds[0].subsonic_list_type(), 0, SECTION_SIZE),
        client.get_album_list2(kinds[1].subsonic_list_type(), 0, SECTION_SIZE),
        client.get_album_list2(kinds[2].subsonic_list_type(), 0, SECTION_SIZE),
        client.get_album_list2(kinds[3].subsonic_list_type(), 0, SECTION_SIZE)
    );
    let lists = [a?, b?, c?, d?];
    let now = chrono::Utc::now().to_rfc3339();
    write(db, progress, |tx| {
        let mut st = tx.prepare_cached(
            "INSERT INTO home_sections (section, album_ids, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(section) DO UPDATE SET album_ids = excluded.album_ids, updated_at = excluded.updated_at",
        )?;
        for (kind, albums) in kinds.iter().zip(lists.iter()) {
            let ids: Vec<&str> = albums.iter().map(|a| a.id.as_str()).collect();
            let json = serde_json::to_string(&ids).unwrap_or_else(|_| "[]".into());
            st.execute(rusqlite::params![kind.key(), json, now])?;
        }
        Ok(())
    })
}

/// Playlists: `getPlaylists` every time (one small call), `getPlaylist` only
/// for playlists that are new or whose `changed`/`songCount` differ from the
/// local copy; playlists gone from the server are removed. All local writes
/// happen in one transaction at the end.
async fn sync_playlists(client: &Client, db: &Db, progress: &dyn SyncProgressSink) -> Result<(), IpcError> {
    let remote: Vec<RawPlaylist> = client.get_playlists().await?;
    let total = remote.len() as u64;
    progress.progress(SyncPhase::Playlists, 0, total, None);

    let local: HashMap<String, (Option<String>, i64)> = db.read(|conn| {
        let mut st = conn.prepare("SELECT id, updated_at, song_count FROM playlists")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?))))?;
        rows.collect()
    })?;

    let mut changed_tracks: Vec<(String, Vec<String>)> = Vec::new();
    for (i, pl) in remote.iter().enumerate() {
        // Smart (.nsp) playlists change server-side without the
        // `changed`/`songCount` pair necessarily moving (same count, different
        // tracks), and Navidrome only re-evaluates one when `getPlaylist` asks
        // for it, so they're re-fetched on every sync. Bounded to the current
        // user's own smart lists (normally a handful): other users' read-only
        // lists move `changed` like any plain playlist. This runs in the
        // background sync task, never on the startup critical path.
        let smart = super::liked::playlist_access_flags(pl.readonly, pl.owner.as_deref(), Some(client.username())).0;
        let up_to_date = !smart
            && local
                .get(&pl.id)
                .is_some_and(|(updated, count)| *updated == pl.changed && *count == i64::from(pl.song_count.unwrap_or(0)));
        if !up_to_date {
            let detail = client.get_playlist(&pl.id).await?;
            changed_tracks.push((pl.id.clone(), detail.entry.into_iter().map(|s| s.id).collect()));
        }
        progress.progress(SyncPhase::Playlists, (i + 1) as u64, total, None);
    }

    write(db, progress, |tx| {
        for pl in &remote {
            upsert_playlist(tx, pl)?;
        }
        for (id, track_ids) in &changed_tracks {
            tx.execute("DELETE FROM playlist_tracks WHERE playlist_id = ?1", [id])?;
            let mut st =
                tx.prepare_cached("INSERT INTO playlist_tracks (playlist_id, position, track_id) VALUES (?1, ?2, ?3)")?;
            for (position, track_id) in track_ids.iter().enumerate() {
                st.execute(rusqlite::params![id, position as i64, track_id])?;
            }
        }
        let remote_ids: HashSet<&str> = remote.iter().map(|p| p.id.as_str()).collect();
        for id in local.keys().filter(|id| !remote_ids.contains(id.as_str())) {
            tx.execute("DELETE FROM playlists WHERE id = ?1", [id])?;
            tx.execute("DELETE FROM playlist_tracks WHERE playlist_id = ?1", [id])?;
        }
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Row UPSERTs (FTS follows via triggers, see schema.rs)
// ---------------------------------------------------------------------------

pub(super) fn upsert_artist(tx: &rusqlite::Transaction, a: &RawArtist, r#gen: i64) -> rusqlite::Result<()> {
    let rating = a.user_rating.filter(|&v| v > 0).map(|v| v.min(5));
    tx.prepare_cached(
        "INSERT INTO artists (id, name, album_count, cover_art, starred, rating, gen) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET name=excluded.name, album_count=excluded.album_count,
            cover_art=excluded.cover_art, starred=excluded.starred, rating=excluded.rating, gen=excluded.gen",
    )?
    .execute(rusqlite::params![a.id, a.name, a.album_count.unwrap_or(0), a.cover_art, a.starred, rating, r#gen])?;
    Ok(())
}

pub(super) fn upsert_album(tx: &rusqlite::Transaction, a: &RawAlbum, r#gen: i64) -> rusqlite::Result<()> {
    let rating = a.user_rating.filter(|&v| v > 0).map(|v| v.min(5));
    tx.prepare_cached(
        "INSERT INTO albums (id, name, artist, artist_id, year, genre, cover_art, song_count, duration, starred, rating, play_count, created, gen)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(id) DO UPDATE SET name=excluded.name, artist=excluded.artist, artist_id=excluded.artist_id,
            year=excluded.year, genre=excluded.genre, cover_art=excluded.cover_art, song_count=excluded.song_count,
            duration=excluded.duration, starred=excluded.starred, rating=excluded.rating, play_count=excluded.play_count,
            created=excluded.created, gen=excluded.gen",
    )?
    .execute(rusqlite::params![
        a.id,
        a.name,
        a.artist.as_deref().unwrap_or_default(),
        a.artist_id,
        a.year,
        a.genre,
        a.cover_art,
        a.song_count.unwrap_or(0),
        a.duration.unwrap_or(0.0),
        a.starred,
        rating,
        a.play_count.map(|v| v as i64),
        a.created,
        r#gen,
    ])?;
    Ok(())
}

pub(super) fn upsert_track(tx: &rusqlite::Transaction, s: &RawSong, r#gen: i64) -> rusqlite::Result<()> {
    let bit_depth = s.bit_depth.filter(|&v| v > 0);
    // Unknown is stored as 0 (the IPC field is non-optional) rather than a
    // made-up "44100 Hz / stereo": real telemetry only.
    let sampling_rate = s.sampling_rate.unwrap_or(0);
    let channel_count = s.channel_count.unwrap_or(0);
    let (track_gain, album_gain, track_peak, album_peak) = match &s.replay_gain {
        Some(rg) => (rg.track_gain, rg.album_gain, rg.track_peak, rg.album_peak),
        None => (None, None, None, None),
    };
    let rating = s.user_rating.filter(|&v| v > 0).map(|v| v.min(5));
    tx.prepare_cached(
        "INSERT INTO tracks (id, title, artist, artist_id, album, album_id, album_artist, track_number, disc_number,
            year, genre, duration, suffix, bit_rate, bit_depth, sampling_rate, channel_count, cover_art, starred,
            rating, size, track_gain_db, album_gain_db, track_peak, album_peak, path, created, gen, play_count, played)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30)
         ON CONFLICT(id) DO UPDATE SET title=excluded.title, artist=excluded.artist, artist_id=excluded.artist_id,
            album=excluded.album, album_id=excluded.album_id, album_artist=excluded.album_artist,
            track_number=excluded.track_number, disc_number=excluded.disc_number, year=excluded.year,
            genre=excluded.genre, duration=excluded.duration, suffix=excluded.suffix, bit_rate=excluded.bit_rate,
            bit_depth=excluded.bit_depth, sampling_rate=excluded.sampling_rate, channel_count=excluded.channel_count,
            cover_art=excluded.cover_art, starred=excluded.starred, rating=excluded.rating, size=excluded.size,
            track_gain_db=excluded.track_gain_db, album_gain_db=excluded.album_gain_db, track_peak=excluded.track_peak,
            album_peak=excluded.album_peak, path=excluded.path, created=excluded.created, gen=excluded.gen,
            play_count=excluded.play_count, played=excluded.played",
    )?
    .execute(rusqlite::params![
        s.id,
        s.title,
        s.artist.as_deref().unwrap_or_default(),
        s.artist_id,
        s.album,
        s.album_id,
        s.display_album_artist,
        s.track,
        s.disc_number,
        s.year,
        s.genre,
        s.duration.unwrap_or(0.0),
        s.suffix,
        s.bit_rate,
        bit_depth,
        sampling_rate,
        channel_count,
        s.cover_art,
        s.starred,
        rating,
        s.size.map(|v| v as i64),
        track_gain,
        album_gain,
        track_peak,
        album_peak,
        s.path,
        s.created,
        r#gen,
        s.play_count,
        s.played,
    ])?;
    // flow block: tag-derived sequencing features (never touches 'analysis' rows).
    super::flow::upsert_tag_features(tx, s)?;
    Ok(())
}

pub(super) fn upsert_playlist(tx: &rusqlite::Transaction, pl: &RawPlaylist) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "INSERT INTO playlists (id, name, comment, owner, public, song_count, duration, cover_art, created_at, updated_at, readonly)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
         ON CONFLICT(id) DO UPDATE SET name=excluded.name, comment=excluded.comment, owner=excluded.owner,
            public=excluded.public, song_count=excluded.song_count, duration=excluded.duration,
            cover_art=excluded.cover_art, created_at=excluded.created_at, updated_at=excluded.updated_at,
            readonly=excluded.readonly",
    )?
    .execute(rusqlite::params![
        pl.id,
        pl.name,
        pl.comment,
        pl.owner,
        pl.public.unwrap_or(false) as i64,
        pl.song_count.unwrap_or(0),
        pl.duration.unwrap_or(0.0),
        pl.cover_art,
        pl.created,
        pl.changed,
        pl.readonly.map(i64::from),
    ])?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn song(id: &str, title: &str, album_id: &str) -> RawSong {
        RawSong {
            id: id.into(),
            title: title.into(),
            artist: Some("Artist".into()),
            artist_id: Some("ar1".into()),
            album: Some("Album".into()),
            album_id: Some(album_id.into()),
            display_album_artist: None,
            track: Some(1),
            disc_number: Some(1),
            year: Some(2020),
            genre: None,
            duration: Some(100.0),
            suffix: "flac".into(),
            bit_rate: Some(900),
            bit_depth: Some(16),
            sampling_rate: Some(44100),
            channel_count: Some(2),
            cover_art: None,
            starred: None,
            replay_gain: None,
            path: None,
            user_rating: None,
            size: None,
            created: None,
            play_count: None,
            played: None,
            bpm: None,
            moods: Vec::new(),
            initial_key: None,
        }
    }

    fn fts_count(db: &Db, table: &str, q: &str) -> i64 {
        db.read(|c| c.query_row(&format!("SELECT COUNT(*) FROM {table} WHERE {table} MATCH ?1"), [q], |r| r.get(0)))
            .unwrap()
    }

    #[test]
    fn upsert_artist_and_album_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        db.with_tx(|tx| {
            upsert_artist(
                tx,
                &RawArtist {
                    id: "a1".into(),
                    name: "Test Artist".into(),
                    album_count: Some(2),
                    cover_art: None,
                    starred: None,
                    user_rating: None,
                },
                1,
            )?;
            upsert_album(
                tx,
                &RawAlbum {
                    id: "al1".into(),
                    name: "Test Album".into(),
                    artist: Some("Test Artist".into()),
                    artist_id: Some("a1".into()),
                    year: Some(2020),
                    genre: None,
                    cover_art: None,
                    song_count: Some(10),
                    duration: Some(1234.5),
                    starred: None,
                    play_count: None,
                    created: None,
                    user_rating: None,
                },
                1,
            )
        })
        .unwrap();

        let count: i64 = db.read(|c| c.query_row("SELECT COUNT(*) FROM artists", [], |r| r.get(0))).unwrap();
        assert_eq!(count, 1);
        assert_eq!(fts_count(&db, "artists_fts", "\"Test\"*"), 1);
        assert_eq!(fts_count(&db, "albums_fts", "\"Test\"*"), 1);
    }

    #[test]
    fn upsert_track_treats_zero_bit_depth_as_absent() {
        let db = Db::open_in_memory().unwrap();
        let mut s = song("t1", "Song", "al1");
        s.bit_depth = Some(0);
        db.with_tx(|tx| upsert_track(tx, &s, 1)).unwrap();
        let bit_depth: Option<i64> = db
            .read(|c| c.query_row("SELECT bit_depth FROM tracks WHERE id = 't1'", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(bit_depth, None);
    }

    /// The generation scheme: re-upserting keeps rid (so FTS stays valid),
    /// renames re-index FTS, and the stale sweep removes exactly the rows a
    /// newer sync didn't see — FTS included.
    #[test]
    fn generation_upsert_and_stale_sweep_keep_fts_consistent() {
        let db = Db::open_in_memory().unwrap();
        db.with_tx(|tx| {
            upsert_track(tx, &song("t1", "Alpha Song", "al1"), 1)?;
            upsert_track(tx, &song("t2", "Beta Song", "al1"), 1)?;
            upsert_track(tx, &song("t3", "Gamma Song", "al1"), 1)
        })
        .unwrap();
        assert_eq!(fts_count(&db, "tracks_fts", "\"song\"*"), 3);
        let rid_before: i64 = db.read(|c| c.query_row("SELECT rid FROM tracks WHERE id='t1'", [], |r| r.get(0))).unwrap();

        // Gen 2: t1 unchanged, t2 renamed, t3 deleted on the server.
        db.with_tx(|tx| {
            upsert_track(tx, &song("t1", "Alpha Song", "al1"), 2)?;
            upsert_track(tx, &song("t2", "Delta Tune", "al1"), 2)
        })
        .unwrap();
        // Before the sweep, readers still see the complete (old) library.
        assert_eq!(db.track_count().unwrap(), 3);
        db.with_tx(|tx| tx.execute("DELETE FROM tracks WHERE gen < ?1", [2]).map(|_| ())).unwrap();

        assert_eq!(db.track_count().unwrap(), 2);
        let rid_after: i64 = db.read(|c| c.query_row("SELECT rid FROM tracks WHERE id='t1'", [], |r| r.get(0))).unwrap();
        assert_eq!(rid_before, rid_after);
        assert_eq!(fts_count(&db, "tracks_fts", "\"song\"*"), 1);
        assert_eq!(fts_count(&db, "tracks_fts", "\"beta\"*"), 0);
        assert_eq!(fts_count(&db, "tracks_fts", "\"delta\"*"), 1);
        assert_eq!(fts_count(&db, "tracks_fts", "\"gamma\"*"), 0);
        // FTS5's own consistency check against the content table.
        db.with_conn(|c| c.execute("INSERT INTO tracks_fts(tracks_fts) VALUES('integrity-check')", []))
            .unwrap();
    }

    #[test]
    fn scan_fingerprint_none_while_scanning_or_unknown() {
        let mut s = ScanStatus {
            scanning: false,
            count: Some(10),
            last_scan: Some("2026-01-01T00:00:00Z".into()),
        };
        assert_eq!(scan_fingerprint(&s).as_deref(), Some("2026-01-01T00:00:00Z|10"));
        s.scanning = true;
        assert_eq!(scan_fingerprint(&s), None);
        s.scanning = false;
        s.last_scan = None;
        assert_eq!(scan_fingerprint(&s), None);
    }

    #[test]
    fn cancelled_sink_blocks_writes() {
        struct Cancelled;
        impl SyncProgressSink for Cancelled {
            fn progress(&self, _: SyncPhase, _: u64, _: u64, _: Option<&str>) {}
            fn cancelled(&self) -> bool {
                true
            }
        }
        let db = Db::open_in_memory().unwrap();
        let err = write(&db, &Cancelled, |tx| upsert_track(tx, &song("t1", "x", "a"), 1)).unwrap_err();
        assert!(err.message.contains("cancelled"));
        assert_eq!(db.track_count().unwrap(), 0);
    }

    // -----------------------------------------------------------------
    // Integration against a real Subsonic server (`SONIX_DEV_*` env vars). Run with:
    //   cargo test -- --ignored --nocapture integration_
    // Read-only against the server (sync never writes); writes only to a
    // throwaway temp SQLite DB, which is removed afterwards.
    // -----------------------------------------------------------------

    #[tokio::test]
    #[ignore]
    async fn integration_full_sync_against_dev_server_and_bench() {
        let (url, user, pass) = crate::subsonic::tests::dev_server_env();
        let client = Client::new(&url, &user, &pass).unwrap();

        let dir = std::env::temp_dir().join(format!("sonix-sync-bench-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = Db::open(&dir).unwrap();

        let started = std::time::Instant::now();
        assert_eq!(run_full_sync(&client, &db, &NullProgressSink).await.unwrap(), SyncOutcome::CatalogSynced);
        let elapsed = started.elapsed();

        let track_count = db.track_count().unwrap();
        let artist_count: i64 = db.read(|c| c.query_row("SELECT COUNT(*) FROM artists", [], |r| r.get(0))).unwrap();
        let album_count: i64 = db.read(|c| c.query_row("SELECT COUNT(*) FROM albums", [], |r| r.get(0))).unwrap();
        let playlist_count: i64 = db.read(|c| c.query_row("SELECT COUNT(*) FROM playlists", [], |r| r.get(0))).unwrap();

        // Second full sync over a populated DB (UPSERT path, no truncation).
        let started2 = std::time::Instant::now();
        run_full_sync(&client, &db, &NullProgressSink).await.unwrap();
        let elapsed_resync = started2.elapsed();
        assert_eq!(db.track_count().unwrap(), track_count, "in-place resync must keep the same rows");
        db.with_conn(|c| c.execute("INSERT INTO tracks_fts(tracks_fts) VALUES('integrity-check')", []))
            .unwrap();

        // Simulated no-change startup: the monitor's first probe
        // (getScanStatus, doubling as the ping) hands its result to the
        // incremental sync, which must skip the catalog.
        crate::subsonic::tests::take_requests();
        let started3 = std::time::Instant::now();
        let (status, ping_ms) = client.get_scan_status().await.unwrap();
        let outcome = run_incremental_sync(&client, &db, &NullProgressSink, Some(status)).await.unwrap();
        let elapsed_incr = started3.elapsed();
        assert_eq!(outcome, SyncOutcome::CatalogUnchanged);
        let requests = crate::subsonic::tests::take_requests();
        println!(
            "NO-CHANGE STARTUP: {} requests in {elapsed_incr:?} (probe rtt {ping_ms}ms): {requests:?}",
            requests.len()
        );
        assert!(!requests.iter().any(|r| r == "search3" || r == "getArtists" || r == "getPlaylist"));
        assert_eq!(db.track_count().unwrap(), track_count);

        let db_path = Db::db_file_path(&dir);
        db.with_conn(|c| c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")).unwrap();
        let db_size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);

        let secs = elapsed.as_secs_f64().max(0.001);
        let tracks_per_sec = track_count as f64 / secs;
        println!(
            "SYNC BENCH: {track_count} tracks, {artist_count} artists, {album_count} albums, {playlist_count} playlists; \
             first full sync {elapsed:?} ({tracks_per_sec:.0} tracks/s, ~{:.0}s extrapolated to 50k); \
             in-place full resync {elapsed_resync:?}; unchanged incremental {elapsed_incr:?}; \
             db {:.2} MB (~{:.0} MB at 50k)",
            50_000.0 / tracks_per_sec,
            db_size as f64 / 1_048_576.0,
            db_size as f64 / track_count.max(1) as f64 * 50_000.0 / 1_048_576.0
        );

        assert!(track_count > 0);
        assert!(artist_count > 0);
        assert!(album_count > 0);

        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    #[ignore]
    async fn integration_browse_and_search_after_sync() {
        let (url, user, pass) = crate::subsonic::tests::dev_server_env();
        let client = Client::new(&url, &user, &pass).unwrap();

        let dir = std::env::temp_dir().join(format!("sonix-browse-bench-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = Db::open(&dir).unwrap();
        run_full_sync(&client, &db, &NullProgressSink).await.unwrap();

        let artists_page = crate::library::get_artists(&db, 0, 20, None).unwrap();
        assert!(!artists_page.items.is_empty());
        assert!(artists_page.total > 0);

        let albums_page = crate::library::get_albums(&db, 0, 20, crate::ipc::AlbumSort::Alphabetical, None).unwrap();
        assert!(!albums_page.items.is_empty());

        let first_album_id = albums_page.items[0].id.clone();
        let detail = crate::library::get_album_detail(&db, &first_album_id).unwrap().unwrap();
        assert!(!detail.tracks.is_empty());
        println!(
            "album '{}' has {} tracks, first track format: {:?}",
            detail.album.name,
            detail.tracks.len(),
            detail.tracks[0].format
        );

        // A prefix search on the first word of a real track title should
        // find that track.
        if let Some(first_word) = detail.tracks[0].title.split_whitespace().next() {
            let results = crate::library::search(&db, first_word, crate::ipc::SearchType::Track, 0, 10).unwrap();
            assert!(results.tracks.total > 0, "expected FTS prefix search for {first_word:?} to find at least the seeded track");
        }

        let home = crate::library::get_home_sections(&db).unwrap();
        println!(
            "home sections: recentlyPlayed={} frequent={} newest={} random={}",
            home.recently_played.len(),
            home.frequent.len(),
            home.newest.len(),
            home.random.len()
        );

        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
