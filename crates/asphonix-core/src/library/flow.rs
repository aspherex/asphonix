//! Flow sequencing data: the `track_features` store (created by
//! `discovery::ensure_extras`, so it survives schema bumps and resyncs) and
//! the loaders that feed `audio::sequence`.
//!
//! Sync writes **tag-derived** rows (`source = 'tag'`): `bpm` from the song
//! JSON, an estimated loudness from the track's real ReplayGain, and a key
//! only when a server sends one. A row whose source is anything else
//! (`'analysis'`, a future on-device scan) is never overwritten by sync, and
//! a tag row whose tags disappeared is deleted again. Nothing is ever
//! invented: an unknown value is NULL.

use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::{params, Connection};

use crate::sequence::{Camelot, FlowTrack};
use crate::ipc::IpcError;
use crate::subsonic::RawSong;

use super::Db;

pub const SOURCE_TAG: &str = "tag";
/// ReplayGain's reference level: `loudness_db = REFERENCE - trackGain`. Only
/// differences between tracks are ever used, never the absolute number.
const RG_REFERENCE_DB: f32 = -18.0;
const ID_CHUNK: usize = 500;
/// Above this many ids one whole-table scan beats that many `IN` chunks.
const SCAN_THRESHOLD: usize = 4000;

/// Sane tempo range; anything else (0 = untagged, garbage) is unknown.
pub fn valid_bpm(bpm: f64) -> Option<f64> {
    (bpm.is_finite() && (30.0..=300.0).contains(&bpm)).then_some(bpm)
}

/// Tag-derived features of one song, `None` when there is nothing real.
pub fn tag_features(s: &RawSong) -> Option<(Option<f64>, Option<String>, Option<f64>)> {
    let bpm = s.bpm.and_then(valid_bpm);
    let key = s.initial_key.as_deref().and_then(Camelot::parse).map(Camelot::label);
    let loudness = s
        .replay_gain
        .as_ref()
        .and_then(|rg| rg.track_gain)
        .filter(|g| g.is_finite())
        .map(|g| (RG_REFERENCE_DB - g) as f64);
    (bpm.is_some() || key.is_some() || loudness.is_some()).then_some((bpm, key, loudness))
}

/// Sync hook (called from `upsert_track`, inside the page transaction).
pub(super) fn upsert_tag_features(tx: &rusqlite::Transaction, s: &RawSong) -> rusqlite::Result<()> {
    match tag_features(s) {
        Some((bpm, key, loudness)) => {
            tx.prepare_cached(
                "INSERT INTO track_features (track_id, bpm, key, loudness_db, source, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'tag', ?5)
                 ON CONFLICT(track_id) DO UPDATE SET bpm=excluded.bpm, key=excluded.key,
                    loudness_db=excluded.loudness_db, updated_at=excluded.updated_at
                 WHERE track_features.source = 'tag'",
            )?
            .execute(params![s.id, bpm, key, loudness, now_ms()])?;
        }
        None => {
            tx.prepare_cached("DELETE FROM track_features WHERE track_id = ?1 AND source = 'tag'")?.execute([&s.id])?;
        }
    }
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Interns strings case-insensitively (first spelling wins) so a pool of
/// thousands of tracks shares a few hundred `Arc<str>`s.
#[derive(Default)]
struct Interner(HashMap<String, Arc<str>>);

impl Interner {
    fn get(&mut self, s: &str) -> Option<Arc<str>> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        Some(self.0.entry(s.to_ascii_lowercase()).or_insert_with(|| Arc::from(s)).clone())
    }
}

const SELECT: &str = "SELECT t.id, t.artist, t.genre, f.bpm, f.key, f.energy, f.loudness_db
                      FROM tracks t LEFT JOIN track_features f ON f.track_id = t.id";

fn row_to_flow(r: &rusqlite::Row, names: &mut Interner) -> rusqlite::Result<(String, FlowTrack)> {
    let id: String = r.get(0)?;
    let artist: Option<String> = r.get(1)?;
    let genre: Option<String> = r.get(2)?;
    let bpm: Option<f64> = r.get(3)?;
    let key: Option<String> = r.get(4)?;
    let energy: Option<f64> = r.get(5)?;
    let loud: Option<f64> = r.get(6)?;
    Ok((
        id,
        FlowTrack {
            bpm: bpm.map(|v| v as f32),
            key: key.as_deref().and_then(Camelot::parse),
            energy: energy.map(|v| v as f32),
            loudness: loud.map(|v| v as f32),
            genre: genre.as_deref().and_then(|g| names.get(g)),
            artist: artist.as_deref().and_then(|a| names.get(a)),
        },
    ))
}

fn load_with(conn: &Connection, ids: &[String]) -> rusqlite::Result<HashMap<String, FlowTrack>> {
    let mut names = Interner::default();
    let mut out = HashMap::with_capacity(ids.len());
    if ids.len() > SCAN_THRESHOLD {
        let wanted: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
        let mut stmt = conn.prepare(SELECT)?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let (id, t) = row_to_flow(r, &mut names)?;
            if wanted.contains(id.as_str()) {
                out.insert(id, t);
            }
        }
        return Ok(out);
    }
    for chunk in ids.chunks(ID_CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let mut stmt = conn.prepare(&format!("{SELECT} WHERE t.id IN ({placeholders})"))?;
        let mut rows = stmt.query(rusqlite::params_from_iter(chunk.iter()))?;
        while let Some(r) = rows.next()? {
            let (id, t) = row_to_flow(r, &mut names)?;
            out.insert(id, t);
        }
    }
    Ok(out)
}

/// Sequencing data for `ids` (tracks the library no longer has are absent).
pub fn load_flow_features(db: &Db, ids: &[String]) -> Result<HashMap<String, FlowTrack>, IpcError> {
    db.read(|conn| load_with(conn, ids))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::sync::tests::song;
    use crate::subsonic::RawReplayGain;

    type Row = (Option<f64>, Option<String>, Option<f64>, String);

    fn features(db: &Db, id: &str) -> Option<Row> {
        db.read(|c| {
            let mut st = c.prepare("SELECT bpm, key, loudness_db, source FROM track_features WHERE track_id = ?1")?;
            let mut rows = st.query([id])?;
            match rows.next()? {
                Some(r) => Ok(Some((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))),
                None => Ok(None),
            }
        })
        .unwrap()
    }

    fn sync_song(db: &Db, s: &RawSong) {
        db.with_tx(|tx| crate::library::sync::upsert_track(tx, s, 1)).unwrap();
    }

    #[test]
    fn sync_fills_tag_rows_from_bpm_and_replaygain_only_when_real() {
        let db = Db::open_in_memory().unwrap();
        let mut a = song("a", "A", "al");
        a.bpm = Some(124.0);
        a.replay_gain = Some(RawReplayGain { track_gain: Some(-6.0), album_gain: None, track_peak: None, album_peak: None });
        sync_song(&db, &a);
        let (bpm, key, loud, src) = features(&db, "a").unwrap();
        assert_eq!(bpm, Some(124.0));
        assert_eq!(key, None);
        assert_eq!(loud, Some(-12.0), "reference -18 minus gain -6");
        assert_eq!(src, "tag");

        // Untagged: no row. 0 bpm and an empty replayGain object are unknown.
        let mut b = song("b", "B", "al");
        b.bpm = Some(0.0);
        b.replay_gain = Some(RawReplayGain { track_gain: None, album_gain: None, track_peak: None, album_peak: None });
        sync_song(&db, &b);
        assert!(features(&db, "b").is_none());
        // Out-of-range tempo is not trusted either.
        let mut c = song("c", "C", "al");
        c.bpm = Some(9000.0);
        sync_song(&db, &c);
        assert!(features(&db, "c").is_none());
    }

    #[test]
    fn a_key_is_stored_only_when_a_server_sends_one() {
        let db = Db::open_in_memory().unwrap();
        let mut a = song("a", "A", "al");
        a.initial_key = Some("Am".into());
        sync_song(&db, &a);
        assert_eq!(features(&db, "a").unwrap().1.as_deref(), Some("8A"));
        let mut b = song("b", "B", "al");
        b.initial_key = Some("nonsense".into());
        sync_song(&db, &b);
        assert!(features(&db, "b").is_none());
    }

    #[test]
    fn resync_does_not_clobber_analysis_rows_but_refreshes_and_clears_tag_rows() {
        let db = Db::open_in_memory().unwrap();
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO track_features (track_id, bpm, energy, source, updated_at) VALUES ('x', 100, 0.7, 'analysis', 1)",
                [],
            )
        })
        .unwrap();
        let mut x = song("x", "X", "al");
        x.bpm = Some(140.0);
        sync_song(&db, &x);
        let (bpm, _, _, src) = features(&db, "x").unwrap();
        assert_eq!((bpm, src.as_str()), (Some(100.0), "analysis"), "analysis wins over tags");
        // The same song without tags must not delete an analysis row either.
        sync_song(&db, &song("x", "X", "al"));
        assert!(features(&db, "x").is_some());

        // Tag rows follow the tags: updated, then removed.
        let mut t = song("t", "T", "al");
        t.bpm = Some(90.0);
        sync_song(&db, &t);
        t.bpm = Some(95.0);
        sync_song(&db, &t);
        assert_eq!(features(&db, "t").unwrap().0, Some(95.0));
        sync_song(&db, &song("t", "T", "al"));
        assert!(features(&db, "t").is_none());
    }

    #[test]
    fn features_survive_a_schema_reset_and_are_wiped_by_logout() {
        let db = Db::open_in_memory().unwrap();
        let mut a = song("a", "A", "al");
        a.bpm = Some(124.0);
        sync_song(&db, &a);
        db.with_conn(|c| {
            c.execute("INSERT INTO track_features (track_id, energy, source, updated_at) VALUES ('z', 0.5, 'analysis', 1)", [])
        })
        .unwrap();
        // A schema bump drops and recreates the mirror (what `schema::ensure` does on a version change).
        db.with_conn(|c| c.execute_batch("PRAGMA user_version = 1")).unwrap();
        {
            let mut conn = db.lock_writer();
            crate::library::schema::ensure(&mut conn).unwrap();
        }
        assert!(features(&db, "a").is_some() && features(&db, "z").is_some(), "extras survive the reset");
        assert!(db.read(|c| c.query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get::<_, i64>(0))).unwrap() == 0);
        db.clear_all().unwrap();
        assert!(features(&db, "a").is_none() && features(&db, "z").is_none(), "logout wipes them");
    }

    #[test]
    fn loader_joins_features_with_artist_and_genre_and_both_paths_agree() {
        let db = Db::open_in_memory().unwrap();
        for i in 0..30 {
            let mut s = song(&format!("t{i}"), "T", "al");
            s.genre = Some(if i % 2 == 0 { "Techno".into() } else { "techno".into() });
            if i % 3 == 0 {
                s.bpm = Some(120.0 + i as f64);
            }
            sync_song(&db, &s);
        }
        let ids: Vec<String> = (0..30).map(|i| format!("t{i}")).collect();
        let m = db.read(|c| load_with(c, &ids)).unwrap();
        assert_eq!(m.len(), 30);
        assert_eq!(m["t3"].bpm, Some(123.0));
        assert_eq!(m["t4"].bpm, None);
        assert_eq!(m["t0"].artist.as_deref(), Some("Artist"));
        assert!(Arc::ptr_eq(m["t0"].genre.as_ref().unwrap(), m["t1"].genre.as_ref().unwrap()), "case-insensitive interning");
        // Whole-table scan path (above SCAN_THRESHOLD) returns the same rows.
        let mut many = ids.clone();
        many.extend((0..SCAN_THRESHOLD).map(|i| format!("missing{i}")));
        let m2 = db.read(|c| load_with(c, &many)).unwrap();
        assert_eq!(m2.len(), 30);
        assert_eq!(m2["t3"], m["t3"]);
    }

    #[test]
    fn songs_with_surprising_flow_fields_still_parse() {
        let j = r#"{"id":"1","title":"t","suffix":"flac","bpm":"128","moods":"Dark","initialKey":7}"#;
        let s: RawSong = serde_json::from_str(j).unwrap();
        assert_eq!(s.bpm, Some(128.0));
        assert_eq!(s.moods, vec!["Dark".to_string()]);
        assert_eq!(s.initial_key, None);
        let j = r#"{"id":"1","title":"t","suffix":"flac","bpm":null,"moods":{"a":1}}"#;
        let s: RawSong = serde_json::from_str(j).unwrap();
        assert_eq!((s.bpm, s.moods.len()), (None, 0));
        let j = r#"{"id":"1","title":"t","suffix":"flac","bpm":0,"moods":["Dark","Calm"]}"#;
        let s: RawSong = serde_json::from_str(j).unwrap();
        assert_eq!((s.bpm, s.moods.len()), (Some(0.0), 2));
    }
}
