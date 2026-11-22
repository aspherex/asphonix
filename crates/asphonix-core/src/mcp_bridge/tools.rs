//! The MCP tools: synchronous handlers over the local library DB, the smart
//! playlist store and a [`Player`] port (production: the audio engine; tests:
//! a recorder, so no test ever touches an audio device).
//!
//! Every handler reuses the code the app's own commands use (`library::` queries,
//! the `*_source_cmd` builders, smart-list evaluation); this file only parses
//! arguments, enforces caps and projects results into compact JSON. Results
//! never contain file paths, cover-art ids, ReplayGain data or server URLs.

use std::collections::HashSet;

use chrono::{Local, LocalResult, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone};
use rand::seq::SliceRandom;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use asphonix_mcp_protocol::catalog::MAX_PAGE;

use crate::audio::output::VOLUME_RANGE_DB;
use crate::audio::{EngineCommand, EngineRequestError};
use crate::discovery::GenreSort;
use crate::ipc::{
    Album, Artist, EnqueuePosition, FlowArc, IpcError, Paged, PlayerState, Playlist, QueueState, SearchType, SortDir, Track, TrackOrder,
    TrackSortField,
};
use crate::library::{self, mcp_queries, Db};
use crate::smartlists::{self, MatchMode, Rule, SmartInput, SmartSort, SmartStore};

pub const CONTROL_DISABLED: &str =
    "Playback control is turned off. Ask the user to enable 'Allow playback control' in ASPHONIX > Settings > Claude (MCP).";
pub const UNKNOWN_TOOL_PREFIX: &str = "unknown tool";
/// Cap on ids accepted by `play_tracks` / `enqueue`.
pub const MAX_IDS: usize = 500;
/// Seek ceiling while the track length is unknown (still loading).
const MAX_SEEK_SEC: f64 = 24.0 * 3600.0;

/// What the tools need from the audio engine.
pub trait Player: Send + Sync {
    fn send(&self, cmd: EngineCommand);
    /// Last emitted state; never spawns the engine thread.
    fn state(&self) -> PlayerState;
    /// Blocking queue page (call from a blocking thread). Never starts the
    /// engine thread (`NotRunning` = nothing queued yet) and gives up after
    /// [`QUEUE_TIMEOUT`] (`NoAnswer`), so a stalled engine cannot pin
    /// blocking-pool threads.
    fn queue(&self, offset: u32, limit: u32) -> Result<QueueState, EngineRequestError>;
}

/// Upper bound for one queue read from the engine.
pub const QUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

impl Player for crate::audio::EngineHandle {
    fn send(&self, cmd: EngineCommand) {
        crate::audio::EngineHandle::send(self, cmd)
    }
    fn state(&self) -> PlayerState {
        self.last_player_state()
    }
    fn queue(&self, offset: u32, limit: u32) -> Result<QueueState, EngineRequestError> {
        self.request_blocking_if_running(|reply| EngineCommand::GetQueue { offset, limit, reply }, QUEUE_TIMEOUT)
    }
}

pub struct Ctx<'a> {
    pub db: &'a Db,
    pub smart: &'a SmartStore,
    pub player: &'a dyn Player,
    /// The logged-in username (playlist ownership flags).
    pub me: Option<String>,
    /// `Settings.mcpAllowControl`, read when the call arrived.
    pub allow_control: bool,
}

type ToolResult = Result<Value, String>;

fn ipc(e: IpcError) -> String {
    e.message
}

fn args_of<T: for<'de> Deserialize<'de>>(args: &Value) -> Result<T, String> {
    serde_json::from_value(args.clone()).map_err(|e| format!("invalid arguments: {e}"))
}

/// Runs one tool. Unknown names and gated control tools are errors.
pub fn call(ctx: &Ctx, tool: &str, args: &Value) -> ToolResult {
    let Some(def) = asphonix_mcp_protocol::catalog::find(tool) else {
        return Err(format!("{UNKNOWN_TOOL_PREFIX}: {tool}"));
    };
    if !args.is_object() {
        return Err("arguments must be a JSON object".into());
    }
    if def.control {
        if !ctx.allow_control {
            return Err(CONTROL_DISABLED.into());
        }
        control_tool(ctx, tool, args)
    } else {
        read_tool(ctx, tool, args)
    }
}

fn read_tool(ctx: &Ctx, tool: &str, args: &Value) -> ToolResult {
    match tool {
        "search" => search(ctx, args),
        "get_tracks" => get_tracks(ctx, args),
        "get_album" => get_album(ctx, args),
        "get_artist" => get_artist(ctx, args),
        "list_playlists" => list_playlists(ctx, args),
        "get_playlist" => get_playlist(ctx, args),
        "list_genres" => list_genres(ctx, args),
        "list_smart_playlists" => list_smart_playlists(ctx, args),
        "get_smart_playlist_tracks" => get_smart_playlist_tracks(ctx, args),
        "listening_history" => listening_history(ctx, args),
        "listen_events" => listen_events(ctx, args),
        "skip_stats" => skip_stats(ctx, args),
        "top_tracks" => top_tracks(ctx, args),
        "top_artists" => top_artists(ctx, args),
        "now_playing" => Ok(now_playing(&ctx.player.state())),
        "get_queue" => get_queue(ctx, args),
        "library_stats" => library_stats(ctx),
        other => Err(format!("{UNKNOWN_TOOL_PREFIX}: {other}")),
    }
}

fn control_tool(ctx: &Ctx, tool: &str, args: &Value) -> ToolResult {
    match tool {
        "play_pause" => {
            #[derive(Deserialize, Default)]
            struct A {
                action: Option<String>,
            }
            let a: A = args_of(args)?;
            let cmd = match a.action.as_deref().unwrap_or("toggle") {
                "toggle" => EngineCommand::PlayPause,
                "play" => EngineCommand::Play,
                "pause" => EngineCommand::Pause,
                other => return Err(format!("action must be toggle, play or pause (got '{other}')")),
            };
            ctx.player.send(cmd);
            Ok(ok())
        }
        "next" => {
            ctx.player.send(EngineCommand::Next);
            Ok(ok())
        }
        "previous" => {
            ctx.player.send(EngineCommand::Previous);
            Ok(ok())
        }
        "seek" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                position_sec: f64,
            }
            let a: A = args_of(args)?;
            if !a.position_sec.is_finite() || a.position_sec < 0.0 {
                return Err("positionSec must be a number >= 0".into());
            }
            // The app's seek bar never goes past the track; neither does Claude
            // (a value in ms instead of s would otherwise just skip the track).
            let duration = ctx.player.state().duration_sec;
            let max = if duration.is_finite() && duration > 0.0 { duration } else { MAX_SEEK_SEC };
            if a.position_sec > max {
                return Err(format!("positionSec must be <= {max:.1} (the track length, in seconds)"));
            }
            ctx.player.send(EngineCommand::Seek(a.position_sec));
            Ok(ok())
        }
        "set_volume_db" => {
            #[derive(Deserialize)]
            struct A {
                db: f64,
            }
            let a: A = args_of(args)?;
            let p = volume_for_db(a.db)?;
            ctx.player.send(EngineCommand::SetVolume(p));
            Ok(json!({ "ok": true, "volumeDb": if p <= 0.0 { Value::Null } else { json!(a.db) }, "muted": p <= 0.0 }))
        }
        "play_tracks" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                track_ids: Vec<String>,
                #[serde(default)]
                start_index: usize,
            }
            let a: A = args_of(args)?;
            let (ids, unknown, start) = known_ids(ctx.db, &a.track_ids, a.start_index)?;
            let queued = ids.len();
            ctx.player.send(EngineCommand::PlayQueue { track_ids: ids, start_index: start as u32 });
            let mut r = json!({ "ok": true, "queued": queued, "unknownIds": unknown });
            add_shuffle_note(ctx, &mut r);
            Ok(r)
        }
        "enqueue" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct A {
                track_ids: Vec<String>,
                position: Option<EnqueuePosition>,
            }
            let a: A = args_of(args)?;
            let (ids, unknown, _) = known_ids(ctx.db, &a.track_ids, 0)?;
            let queued = ids.len();
            ctx.player.send(EngineCommand::Enqueue { track_ids: ids, position: a.position.unwrap_or(EnqueuePosition::End) });
            Ok(json!({ "ok": true, "queued": queued, "unknownIds": unknown }))
        }
        "play_source" => play_source(ctx, args),
        other => Err(format!("{UNKNOWN_TOOL_PREFIX}: {other}")),
    }
}

/// A static queue is reshuffled by the engine while its global shuffle mode is
/// on (`Queue::set_queue`), so the order Claude sent is not what plays. Say so.
fn add_shuffle_note(ctx: &Ctx, result: &mut Value) {
    let shuffle_on = ctx.player.state().shuffle;
    if let Some(o) = result.as_object_mut() {
        o.insert("shuffleOn".into(), shuffle_on.into());
        if shuffle_on {
            o.insert(
                "note".into(),
                "ASPHONIX's shuffle mode is on, so this queue plays in shuffled order (the start track first). The user can turn shuffle off in the player.".into(),
            );
        }
    }
}

fn ok() -> Value {
    json!({ "ok": true, "note": "Command sent; ASPHONIX applies it asynchronously. Use now_playing to confirm." })
}

/// Inverse of `audio::output::volume_to_db`: slider position for `db`
/// (`0` = unity, `-VOLUME_RANGE_DB` = mute).
pub fn volume_for_db(db: f64) -> Result<f32, String> {
    if !db.is_finite() {
        return Err("db must be a finite number".into());
    }
    if db > 0.0 {
        return Err("db must be <= 0 (0 dB is full scale)".into());
    }
    let range = VOLUME_RANGE_DB as f64;
    Ok((1.0 + db / range).clamp(0.0, 1.0) as f32)
}

/// Splits `ids` into the ones the library knows (order and duplicates kept)
/// and the unknown rest; `start_index` (into the *given* list) is mapped to
/// the first known id at or after it.
fn known_ids(db: &Db, ids: &[String], start_index: usize) -> Result<(Vec<String>, Vec<String>, usize), String> {
    if ids.is_empty() {
        return Err("trackIds must not be empty".into());
    }
    if ids.len() > MAX_IDS {
        return Err(format!("at most {MAX_IDS} trackIds per call (got {})", ids.len()));
    }
    let uniq: Vec<String> = ids.iter().cloned().collect::<HashSet<_>>().into_iter().collect();
    let known: HashSet<String> = library::get_tracks_by_ids(db, &uniq).map_err(ipc)?.into_iter().map(|t| t.id).collect();
    let (mut kept, mut unknown, mut start) = (Vec::new(), Vec::new(), 0usize);
    for (i, id) in ids.iter().enumerate() {
        if known.contains(id) {
            if i < start_index {
                start += 1;
            }
            kept.push(id.clone());
        } else if !unknown.contains(id) {
            unknown.push(id.clone());
        }
    }
    if kept.is_empty() {
        return Err("none of the given trackIds exist in the library (use search / get_tracks for real ids)".into());
    }
    if start >= kept.len() {
        return Err("startIndex is past the last known track".into());
    }
    Ok((kept, unknown, start))
}

fn play_source(ctx: &Ctx, args: &Value) -> ToolResult {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct A {
        kind: String,
        id: Option<String>,
        #[serde(default)]
        shuffle: bool,
        /// Flow sequencing arc (`steady`/`build`/`peak`); excludes `shuffle`.
        flow: Option<FlowArc>,
        start_track_id: Option<String>,
    }
    let a: A = args_of(args)?;
    if a.flow.is_some() && a.shuffle {
        return Err("pass either shuffle or flow, not both".into());
    }
    if a.flow.is_some() && a.kind == "album" {
        return Err("flow is not available for albums (an album plays as a plain queue); use shuffle or neither".into());
    }
    let flow = a.flow;
    let need_id = |a: &A| a.id.clone().filter(|s| !s.is_empty()).ok_or_else(|| format!("kind '{}' needs an id", a.kind));
    let start = a.start_track_id.clone();
    let cmd = match a.kind.as_str() {
        "playlist" => super::sources::playlist_source_cmd(ctx.db, need_id(&a)?, a.shuffle, start, flow),
        "artist" => super::sources::artist_source_cmd(ctx.db, need_id(&a)?, a.shuffle, start, flow),
        "genre" => super::sources::genre_source_cmd(ctx.db, need_id(&a)?, a.shuffle, start, flow),
        "liked" => super::sources::liked_source_cmd(ctx.db, a.shuffle, start, flow),
        "smart" => super::sources::smart_source_cmd(ctx.db, ctx.smart, need_id(&a)?, a.shuffle, start, None, flow),
        "allTracks" => {
            let order = if a.shuffle {
                TrackOrder::Shuffle
            } else {
                TrackOrder::Sort { field: TrackSortField::Artist, dir: SortDir::Asc }
            };
            super::sources::play_all_cmd(ctx.db, order, None, start, flow)
        }
        "album" => {
            // Albums are plain static queues in the app too (never a rolling source).
            let id = need_id(&a)?;
            let detail = library::get_album_detail(ctx.db, &id)
                .map_err(ipc)?
                .ok_or_else(|| format!("album {id} not found"))?;
            let mut ids: Vec<String> = detail.tracks.into_iter().map(|t| t.id).collect();
            if ids.is_empty() {
                return Err("album has no tracks".into());
            }
            if a.shuffle {
                ids.shuffle(&mut rand::thread_rng());
            }
            let start_index = a.start_track_id.as_deref().and_then(|s| ids.iter().position(|i| i == s)).unwrap_or(0);
            let n = ids.len();
            ctx.player.send(EngineCommand::PlayQueue { track_ids: ids, start_index: start_index as u32 });
            let mut r = json!({ "ok": true, "kind": "album", "label": detail.album.name, "queued": n });
            add_shuffle_note(ctx, &mut r);
            return Ok(r);
        }
        other => return Err(format!("unknown kind '{other}'")),
    }
    .map_err(ipc)?;
    let (label, count, mode, arc) = match &cmd {
        EngineCommand::PlaySource { label, track_ids, mode, arc, .. } => (label.clone(), track_ids.len(), *mode, *arc),
        _ => (String::new(), 0, crate::ipc::SourceMode::InOrder, FlowArc::Steady),
    };
    ctx.player.send(cmd);
    let mut r = json!({ "ok": true, "kind": a.kind, "label": label, "poolSize": count, "mode": mode,
                        "note": "Plays as a rolling source: only the next few tracks are queued at once." });
    if mode == crate::ipc::SourceMode::Flow
        && let Some(o) = r.as_object_mut() {
            o.insert("arc".into(), json!(arc));
        }
    Ok(r)
}

// ---------------------------------------------------------------------------
// Projections
// ---------------------------------------------------------------------------

fn short_format(t: &Track) -> String {
    let f = &t.format;
    let khz = f.sampling_rate as f64 / 1000.0;
    let khz = if (khz - khz.round()).abs() < 0.05 { format!("{}", khz.round() as u32) } else { format!("{khz:.1}") };
    let suffix = f.suffix.to_uppercase();
    match (f.bit_depth, f.bit_rate) {
        (Some(d), _) if f.sampling_rate > 0 => format!("{suffix} {d}/{khz}"),
        (None, Some(b)) if b > 0 => format!("{suffix} {b}k"),
        _ if f.sampling_rate > 0 => format!("{suffix} {khz}kHz"),
        _ => suffix,
    }
}

fn put<T: Into<Value>>(m: &mut Map<String, Value>, k: &str, v: Option<T>) {
    if let Some(v) = v {
        m.insert(k.into(), v.into());
    }
}

pub fn track_json(t: &Track) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), t.id.clone().into());
    m.insert("title".into(), t.title.clone().into());
    m.insert("artist".into(), t.artist.clone().into());
    put(&mut m, "artistId", t.artist_id.clone());
    put(&mut m, "album", t.album.clone());
    put(&mut m, "albumId", t.album_id.clone());
    put(&mut m, "trackNumber", t.track_number);
    put(&mut m, "discNumber", t.disc_number);
    put(&mut m, "year", t.year);
    put(&mut m, "genre", t.genre.clone());
    m.insert("durationSec".into(), json!((t.duration * 10.0).round() / 10.0));
    m.insert("format".into(), short_format(t).into());
    put(&mut m, "rating", t.rating);
    m.insert("liked".into(), t.starred.is_some().into());
    put(&mut m, "dateAdded", t.date_added.clone());
    Value::Object(m)
}

fn album_json(a: &Album) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), a.id.clone().into());
    m.insert("name".into(), a.name.clone().into());
    m.insert("artist".into(), a.artist.clone().into());
    put(&mut m, "artistId", a.artist_id.clone());
    put(&mut m, "year", a.year);
    put(&mut m, "genre", a.genre.clone());
    m.insert("trackCount".into(), a.song_count.into());
    m.insert("durationSec".into(), json!(a.duration.round()));
    put(&mut m, "rating", a.rating);
    m.insert("liked".into(), a.starred.is_some().into());
    put(&mut m, "playCount", a.play_count);
    Value::Object(m)
}

fn artist_json(a: &Artist) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), a.id.clone().into());
    m.insert("name".into(), a.name.clone().into());
    m.insert("albumCount".into(), a.album_count.into());
    put(&mut m, "rating", a.rating);
    m.insert("liked".into(), a.starred.is_some().into());
    Value::Object(m)
}

fn playlist_json(p: &Playlist) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), p.id.clone().into());
    m.insert("name".into(), p.name.clone().into());
    put(&mut m, "comment", p.comment.clone().filter(|c| !c.is_empty()));
    put(&mut m, "owner", p.owner.clone());
    m.insert("trackCount".into(), p.song_count.into());
    m.insert("durationSec".into(), json!(p.duration.round()));
    m.insert("public".into(), p.public.into());
    m.insert("smart".into(), p.smart.into());
    m.insert("readOnly".into(), p.read_only.into());
    put(&mut m, "updatedAt", p.updated_at.clone());
    Value::Object(m)
}

fn paged<T>(p: &Paged<T>, f: impl Fn(&T) -> Value) -> Value {
    json!({ "total": p.total, "offset": p.offset, "items": p.items.iter().map(f).collect::<Vec<_>>() })
}

/// In-memory paging for lists the DB layer returns whole.
fn slice_page<T>(all: &[T], offset: u64, limit: u64, f: impl Fn(&T) -> Value) -> Value {
    let items: Vec<Value> = all.iter().skip(offset as usize).take(limit as usize).map(f).collect();
    json!({ "total": all.len(), "offset": offset, "items": items })
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PageArgs {
    offset: u64,
    limit: Option<u64>,
}

/// `(offset, limit)` with the hard cap applied.
fn page_of(p: &PageArgs, default_limit: u64) -> (u64, u64) {
    (p.offset.min(10_000_000), p.limit.unwrap_or(default_limit).clamp(1, MAX_PAGE))
}

// ---------------------------------------------------------------------------
// Times
// ---------------------------------------------------------------------------

/// Parses an ISO date / date-time into unix ms. Date-only = local midnight
/// (as an `until` bound: the *end* of that day, i.e. the next midnight).
pub fn parse_when(s: &str, is_until: bool) -> Result<i64, String> {
    let s = s.trim();
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(dt.timestamp_millis());
    }
    let local = |n: NaiveDateTime| match Local.from_local_datetime(&n) {
        LocalResult::Single(d) => Ok(d.timestamp_millis()),
        LocalResult::Ambiguous(a, _) => Ok(a.timestamp_millis()),
        LocalResult::None => Err(format!("'{s}' does not exist in the local time zone (DST gap)")),
    };
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, fmt) {
            return local(n);
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let day = if is_until { d.succ_opt().ok_or("date out of range")? } else { d };
        return local(day.and_hms_opt(0, 0, 0).ok_or("date out of range")?);
    }
    Err(format!("cannot parse '{s}': use an ISO 8601 date (2026-10-31) or date-time (2026-10-31T18:30:00)"))
}

fn range(since: &Option<String>, until: &Option<String>) -> Result<(Option<i64>, Option<i64>), String> {
    let s = since.as_deref().map(|v| parse_when(v, false)).transpose()?;
    let u = until.as_deref().map(|v| parse_when(v, true)).transpose()?;
    if let (Some(s), Some(u)) = (s, u)
        && s >= u {
            return Err("'since' must be before 'until'".into());
        }
    Ok((s, u))
}

fn iso_local(ms: i64) -> Value {
    match Local.timestamp_millis_opt(ms) {
        LocalResult::Single(d) | LocalResult::Ambiguous(d, _) => d.to_rfc3339_opts(SecondsFormat::Secs, false).into(),
        LocalResult::None => Value::Null,
    }
}

// ---------------------------------------------------------------------------
// Read tools
// ---------------------------------------------------------------------------

fn search(ctx: &Ctx, args: &Value) -> ToolResult {
    #[derive(Deserialize)]
    struct A {
        query: String,
        #[serde(rename = "type")]
        kind: Option<SearchType>,
        #[serde(flatten)]
        page: PageArgs,
    }
    let a: A = args_of(args)?;
    let q = a.query.trim();
    if q.is_empty() || q.chars().count() > 200 {
        return Err("query must be 1-200 characters".into());
    }
    let (offset, limit) = page_of(&a.page, 20);
    let r = library::search(ctx.db, q, a.kind.unwrap_or(SearchType::All), offset, limit).map_err(ipc)?;
    Ok(json!({
        "tracks": paged(&r.tracks, track_json),
        "albums": paged(&r.albums, album_json),
        "artists": paged(&r.artists, artist_json),
        "playlists": paged(&r.playlists, playlist_json),
    }))
}

fn sort_field(s: &str) -> Result<TrackSortField, String> {
    Ok(match s {
        "title" => TrackSortField::Title,
        "artist" => TrackSortField::Artist,
        "album" => TrackSortField::Album,
        "year" => TrackSortField::Year,
        "dateAdded" => TrackSortField::DateAdded,
        "duration" => TrackSortField::Duration,
        "format" => TrackSortField::Format,
        other => return Err(format!("unknown sort '{other}'")),
    })
}

fn get_tracks(ctx: &Ctx, args: &Value) -> ToolResult {
    #[derive(Deserialize)]
    struct A {
        sort: Option<String>,
        dir: Option<SortDir>,
        filter: Option<String>,
        #[serde(flatten)]
        page: PageArgs,
    }
    let a: A = args_of(args)?;
    let (offset, limit) = page_of(&a.page, 50);
    let sort = sort_field(a.sort.as_deref().unwrap_or("title"))?;
    let filter = a.filter.as_deref().map(str::trim).filter(|f| !f.is_empty());
    let r = library::get_tracks(ctx.db, offset, limit, sort, a.dir.unwrap_or(SortDir::Asc), filter).map_err(ipc)?;
    Ok(paged(&r, track_json))
}

#[derive(Deserialize)]
struct IdPageArg {
    id: String,
    #[serde(flatten)]
    page: PageArgs,
}

fn get_album(ctx: &Ctx, args: &Value) -> ToolResult {
    let a: IdPageArg = args_of(args)?;
    let (offset, limit) = page_of(&a.page, 100);
    let d = library::get_album_detail(ctx.db, &a.id).map_err(ipc)?.ok_or_else(|| format!("album {} not found", a.id))?;
    Ok(json!({ "album": album_json(&d.album), "tracks": slice_page(&d.tracks, offset, limit, track_json) }))
}

fn get_artist(ctx: &Ctx, args: &Value) -> ToolResult {
    let a: IdPageArg = args_of(args)?;
    let (offset, limit) = page_of(&a.page, 100);
    let d = library::get_artist_detail(ctx.db, &a.id).map_err(ipc)?.ok_or_else(|| format!("artist {} not found", a.id))?;
    Ok(json!({ "artist": artist_json(&d.artist), "albums": slice_page(&d.albums, offset, limit, album_json) }))
}

fn list_playlists(ctx: &Ctx, args: &Value) -> ToolResult {
    let p: PageArgs = args_of(args)?;
    let (offset, limit) = page_of(&p, 100);
    let mut lists = library::playlists_list(ctx.db).map_err(ipc)?;
    lists.iter_mut().for_each(|l| super::sources::apply_playlist_access(l, ctx.me.as_deref()));
    Ok(slice_page(&lists, offset, limit, playlist_json))
}

fn get_playlist(ctx: &Ctx, args: &Value) -> ToolResult {
    #[derive(Deserialize)]
    struct A {
        id: String,
        #[serde(flatten)]
        page: PageArgs,
    }
    let a: A = args_of(args)?;
    let (offset, limit) = page_of(&a.page, 100);
    let mut d = library::playlist_detail(ctx.db, &a.id).map_err(ipc)?.ok_or_else(|| format!("playlist {} not found", a.id))?;
    super::sources::apply_playlist_access(&mut d.playlist, ctx.me.as_deref());
    Ok(json!({ "playlist": playlist_json(&d.playlist), "tracks": slice_page(&d.tracks, offset, limit, track_json) }))
}

fn list_genres(ctx: &Ctx, args: &Value) -> ToolResult {
    #[derive(Deserialize)]
    struct A {
        sort: Option<GenreSortArg>,
        #[serde(flatten)]
        page: PageArgs,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    enum GenreSortArg {
        Tracks,
        Alphabetical,
    }
    let a: A = args_of(args)?;
    let (offset, limit) = page_of(&a.page, 100);
    let sort = match a.sort {
        Some(GenreSortArg::Alphabetical) => GenreSort::Alphabetical,
        _ => GenreSort::Tracks,
    };
    let r = library::discovery::get_genres(ctx.db, offset as u32, limit as u32, sort).map_err(ipc)?;
    Ok(paged(&r, |g| json!({ "name": g.name, "albumCount": g.album_count, "trackCount": g.track_count })))
}

fn list_smart_playlists(ctx: &Ctx, args: &Value) -> ToolResult {
    let p: PageArgs = args_of(args)?;
    let (offset, limit) = page_of(&p, 100);
    let all = ctx.smart.list().map_err(ipc)?;
    Ok(slice_page(&all, offset, limit, |p| serde_json::to_value(p).unwrap_or(Value::Null)))
}

fn get_smart_playlist_tracks(ctx: &Ctx, args: &Value) -> ToolResult {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Adhoc {
        match_mode: Option<MatchMode>,
        rules: Vec<Rule>,
        sort: Option<SmartSort>,
        limit: Option<u32>,
    }
    #[derive(Deserialize)]
    struct A {
        id: Option<String>,
        rules: Option<Adhoc>,
        seed: Option<u64>,
        #[serde(flatten)]
        page: PageArgs,
    }
    let a: A = args_of(args)?;
    let (offset, limit) = page_of(&a.page, 50);
    let def: SmartInput = match (a.id, a.rules) {
        (Some(id), None) => ctx.smart.get(&id).map_err(ipc)?.def,
        (None, Some(r)) => {
            if r.rules.len() > 20 {
                return Err("at most 20 rules".into());
            }
            SmartInput {
                name: "Ad hoc".into(),
                match_mode: r.match_mode.unwrap_or(MatchMode::All),
                rules: r.rules,
                sort: r.sort.unwrap_or(SmartSort::Title),
                limit: r.limit,
            }
        }
        _ => return Err("pass exactly one of `id` or `rules`".into()),
    };
    smartlists::validate(&def).map_err(ipc)?;
    let r = smartlists::evaluate_page(ctx.db, &def, offset, limit, a.seed.unwrap_or(0)).map_err(ipc)?;
    Ok(paged(&r, track_json))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RangeArgs {
    since: Option<String>,
    until: Option<String>,
}

fn listening_history(ctx: &Ctx, args: &Value) -> ToolResult {
    #[derive(Deserialize)]
    struct A {
        #[serde(flatten)]
        range: RangeArgs,
        #[serde(flatten)]
        page: PageArgs,
    }
    let a: A = args_of(args)?;
    let (offset, limit) = page_of(&a.page, 50);
    let (since, until) = range(&a.range.since, &a.range.until)?;
    let r = mcp_queries::history_range(ctx.db, since, until, offset, limit).map_err(ipc)?;
    Ok(paged(&r, |p| json!({ "playedAt": iso_local(p.played_at), "track": track_json(&p.track) })))
}

/// Compact tracks for the ids on one page (one batched query); ids no longer
/// in the library map to nothing (the row then carries only `trackId`).
fn tracks_for(db: &Db, ids: impl Iterator<Item = String>) -> Result<std::collections::HashMap<String, Value>, String> {
    let uniq: Vec<String> = ids.collect::<HashSet<_>>().into_iter().collect();
    Ok(library::get_tracks_by_ids(db, &uniq).map_err(ipc)?.iter().map(|t| (t.id.clone(), track_json(t))).collect())
}

fn listen_req(window: &RangeArgs, page: &PageArgs) -> Result<crate::listen::ListenRangeRequest, String> {
    let (offset, limit) = page_of(page, 50);
    let (since, until) = range(&window.since, &window.until)?;
    Ok(crate::listen::ListenRangeRequest { offset: offset.min(u32::MAX as u64) as u32, limit: limit as u32, since, until })
}

fn listen_events(ctx: &Ctx, args: &Value) -> ToolResult {
    #[derive(Deserialize)]
    struct A {
        #[serde(flatten)]
        range: RangeArgs,
        #[serde(flatten)]
        page: PageArgs,
    }
    let a: A = args_of(args)?;
    let req = listen_req(&a.range, &a.page)?;
    let p = crate::listen::listen_events_page(ctx.db, &req).map_err(ipc)?;
    let tracks = tracks_for(ctx.db, p.items.iter().map(|e| e.track_id.clone()))?;
    Ok(paged(&p, |e| {
        let mut m = Map::new();
        m.insert("trackId".into(), e.track_id.clone().into());
        put(&mut m, "track", tracks.get(&e.track_id).cloned());
        m.insert("startedAt".into(), iso_local(e.started_at));
        m.insert("endedAt".into(), iso_local(e.ended_at));
        m.insert("listenedSec".into(), json!((e.listened_ms as f64 / 100.0).round() / 10.0));
        m.insert("durationSec".into(), json!((e.duration_ms as f64 / 100.0).round() / 10.0));
        m.insert("endReason".into(), e.end_reason.clone().into());
        put(&mut m, "sourceKind", e.source_kind.clone());
        put(&mut m, "sourceId", e.source_id.clone());
        m.insert("origin".into(), e.origin.clone().into());
        m.insert("countedAsPlay".into(), e.counted.into());
        Value::Object(m)
    }))
}

fn skip_stats(ctx: &Ctx, args: &Value) -> ToolResult {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct A {
        #[serde(flatten)]
        range: RangeArgs,
        #[serde(flatten)]
        page: PageArgs,
        min_sessions: Option<u32>,
    }
    let a: A = args_of(args)?;
    let r = listen_req(&a.range, &a.page)?;
    let req = crate::listen::SkipStatsRequest {
        offset: r.offset,
        limit: r.limit,
        since: r.since,
        until: r.until,
        min_sessions: a.min_sessions.map(|n| n.clamp(1, 1_000_000)),
    };
    let p = crate::listen::skip_stats_page(ctx.db, &req).map_err(ipc)?;
    let tracks = tracks_for(ctx.db, p.items.iter().map(|s| s.track_id.clone()))?;
    Ok(paged(&p, |s| {
        let mut m = Map::new();
        m.insert("trackId".into(), s.track_id.clone().into());
        put(&mut m, "track", tracks.get(&s.track_id).cloned());
        m.insert("sessions".into(), s.sessions.into());
        m.insert("skips".into(), s.skips.into());
        m.insert("countedPlays".into(), s.counted.into());
        put(&mut m, "avgListenedFraction", s.avg_listened_fraction.map(|f| (f * 1000.0).round() / 1000.0));
        m.insert("lastListenedAt".into(), iso_local(s.last_listened_at));
        Value::Object(m)
    }))
}

fn top_tracks(ctx: &Ctx, args: &Value) -> ToolResult {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct A {
        #[serde(flatten)]
        range: RangeArgs,
        #[serde(default)]
        unliked_only: bool,
        limit: Option<u64>,
    }
    let a: A = args_of(args)?;
    let (since, until) = range(&a.range.since, &a.range.until)?;
    let limit = a.limit.unwrap_or(25).clamp(1, MAX_PAGE);
    let rows = mcp_queries::top_tracks(ctx.db, since, until, limit, a.unliked_only).map_err(ipc)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| json!({ "plays": r.plays, "lastPlayed": iso_local(r.last_played), "track": track_json(&r.track) }))
        .collect();
    Ok(json!({ "items": items }))
}

fn top_artists(ctx: &Ctx, args: &Value) -> ToolResult {
    #[derive(Deserialize)]
    struct A {
        #[serde(flatten)]
        range: RangeArgs,
        limit: Option<u64>,
    }
    let a: A = args_of(args)?;
    let (since, until) = range(&a.range.since, &a.range.until)?;
    let limit = a.limit.unwrap_or(25).clamp(1, MAX_PAGE);
    let rows = mcp_queries::top_artists(ctx.db, since, until, limit).map_err(ipc)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut m = Map::new();
            put(&mut m, "artistId", r.artist_id.clone());
            m.insert("name".into(), r.name.clone().into());
            m.insert("plays".into(), r.plays.into());
            m.insert("distinctTracks".into(), r.distinct_tracks.into());
            Value::Object(m)
        })
        .collect();
    Ok(json!({ "items": items }))
}

fn now_playing(s: &PlayerState) -> Value {
    let mut m = Map::new();
    m.insert("status".into(), serde_json::to_value(s.status).unwrap_or(Value::Null));
    m.insert("track".into(), s.track.as_ref().map(track_json).unwrap_or(Value::Null));
    m.insert("positionSec".into(), json!((s.position_sec * 10.0).round() / 10.0));
    m.insert("durationSec".into(), json!((s.duration_sec * 10.0).round() / 10.0));
    let mut out = Map::new();
    out.insert("mode".into(), serde_json::to_value(s.output_mode).unwrap_or(Value::Null));
    put(&mut out, "sampleRate", s.sample_rate);
    put(&mut out, "bitDepth", s.bit_depth);
    put(&mut out, "codec", s.format_label.clone());
    out.insert("bitPerfect".into(), s.bit_perfect.into());
    m.insert("output".into(), Value::Object(out));
    put(&mut m, "volumeDb", s.volume_db);
    m.insert("shuffle".into(), s.shuffle.into());
    m.insert("repeat".into(), serde_json::to_value(s.repeat).unwrap_or(Value::Null));
    m.insert("queueLength".into(), s.queue_length.into());
    put(&mut m, "error", s.error.clone());
    Value::Object(m)
}

fn get_queue(ctx: &Ctx, args: &Value) -> ToolResult {
    let p: PageArgs = args_of(args)?;
    let (offset, limit) = page_of(&p, 50);
    let q = match ctx.player.queue(offset as u32, limit as u32) {
        Ok(q) => q,
        // Never started this run (no session restored, nothing played): empty.
        Err(EngineRequestError::NotRunning) => QueueState { items: vec![], total: 0, current_index: -1, source: None },
        Err(EngineRequestError::NoAnswer) => {
            return Err(format!("the audio engine did not answer within {} s; try again", QUEUE_TIMEOUT.as_secs()))
        }
    };
    let items: Vec<Value> = q
        .items
        .iter()
        .map(|i| {
            let mut v = track_json(&i.track);
            if let Some(o) = v.as_object_mut() {
                o.insert("origin".into(), serde_json::to_value(i.origin).unwrap_or(Value::Null));
            }
            v
        })
        .collect();
    let source = q.source.as_ref().map(|s| serde_json::to_value(s).unwrap_or(Value::Null));
    Ok(json!({ "total": q.total, "offset": offset, "currentIndex": q.current_index, "source": source, "items": items }))
}

fn library_stats(ctx: &Ctx) -> ToolResult {
    let s = mcp_queries::stats(ctx.db).map_err(ipc)?;
    let last_sync = ctx.db.get_meta(library::META_LAST_SYNC_AT).ok().flatten();
    let now = Local::now();
    Ok(json!({
        "tracks": s.tracks, "albums": s.albums, "artists": s.artists, "playlists": s.playlists,
        "likedTracks": s.liked_tracks, "recordedPlays": s.recorded_plays,
        "firstRecordedPlay": s.first_recorded_play_ms.map(iso_local),
        "lastSyncAt": last_sync,
        "nowLocal": now.to_rfc3339_opts(SecondsFormat::Secs, false),
    }))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ipc::{OutputMode, PlaybackStatus, RepeatMode};
    use std::sync::Mutex;

    #[derive(Default)]
    pub(crate) struct Recorder {
        pub cmds: Mutex<Vec<EngineCommand>>,
        /// What `queue` fails with (None = a normal answer).
        pub queue_reply: Mutex<Option<EngineRequestError>>,
    }

    impl Player for Recorder {
        fn send(&self, cmd: EngineCommand) {
            self.cmds.lock().unwrap().push(cmd);
        }
        fn state(&self) -> PlayerState {
            PlayerState {
                status: PlaybackStatus::Paused,
                track: None,
                position_sec: 12.34,
                duration_sec: 200.0,
                output_mode: OutputMode::Shared,
                sample_rate: Some(48000),
                bit_depth: Some(32),
                float_mix: true,
                format_label: Some("FLAC".into()),
                volume: 0.5,
                volume_db: Some(-30.0),
                bit_perfect: false,
                shuffle: false,
                repeat: RepeatMode::Off,
                queue_length: 3,
                error: None,
                replay_gain_db: None,
                loop_a: None,
                loop_b: None,
            }
        }
        fn queue(&self, _offset: u32, _limit: u32) -> Result<QueueState, EngineRequestError> {
            match *self.queue_reply.lock().unwrap() {
                Some(e) => Err(e),
                None => Ok(QueueState { items: vec![], total: 7, current_index: 2, source: None }),
            }
        }
    }

    pub(crate) fn seed(db: &Db) {
        db.with_tx(|tx| {
            tx.execute(
                "INSERT INTO artists (id, name, album_count) VALUES ('ar1','Nova',1),('ar2','Comet',1)",
                [],
            )?;
            tx.execute(
                "INSERT INTO albums (id, name, artist, artist_id, year, genre, song_count, duration) VALUES
                 ('al1','Red Album','Nova','ar1',2019,'Ambient',2,300),('al2','Blue Album','Comet','ar2',2021,'Rock',1,300)",
                [],
            )?;
            tx.execute(
                "INSERT INTO tracks (id, title, artist, artist_id, album, album_id, track_number, year, genre, duration, suffix, bit_depth, sampling_rate, channel_count, starred, path, cover_art, created) VALUES
                 ('t1','Alpha','Nova','ar1','Red Album','al1',1,2019,'Ambient',100,'flac',24,96000,2,NULL,'/secret/nas/path/a.flac','cov1','2026-10-01T00:00:00Z'),
                 ('t2','Beta','Nova','ar1','Red Album','al1',2,2019,'Ambient',200,'flac',16,44100,2,'2024-01-01T00:00:00Z','/secret/nas/path/b.flac','cov1','2026-10-02T00:00:00Z'),
                 ('t3','Gamma','Comet','ar2','Blue Album','al2',1,2021,'Rock',300,'mp3',NULL,44100,2,NULL,'/secret/nas/path/c.mp3','cov2','2026-10-03T00:00:00Z')",
                [],
            )?;
            tx.execute("UPDATE tracks SET bit_rate = 320 WHERE id = 't3'", [])?;
            tx.execute(
                "INSERT INTO playlists (id, name, song_count, duration, owner, public) VALUES ('pl1','Run mix',2,300,'me',0)",
                [],
            )?;
            tx.execute("INSERT INTO playlist_tracks (playlist_id, position, track_id) VALUES ('pl1',0,'t3'),('pl1',1,'t1')", [])?;
            for (track, at) in [("t1", 1_000i64), ("t1", 2_000), ("t1", 3_000), ("t2", 4_000), ("t2", 5_000), ("t3", 6_000)] {
                tx.execute("INSERT INTO play_history (track_id, played_at, scrobbled) VALUES (?1, ?2, 0)", rusqlite::params![track, at])?;
            }
            Ok(())
        })
        .unwrap();
    }

    pub(crate) struct Fixture {
        pub db: Db,
        pub smart: SmartStore,
        pub player: Recorder,
        pub _dir: tempfile::TempDir,
    }

    impl Fixture {
        pub fn new() -> Self {
            let db = Db::open_in_memory().unwrap();
            seed(&db);
            let dir = tempfile::tempdir().unwrap();
            Self { smart: SmartStore::new(dir.path()), db, player: Recorder::default(), _dir: dir }
        }
        pub fn call(&self, allow_control: bool, tool: &str, args: Value) -> ToolResult {
            let ctx = Ctx { db: &self.db, smart: &self.smart, player: &self.player, me: Some("me".into()), allow_control };
            call(&ctx, tool, &args)
        }
        pub fn sent(&self) -> usize {
            self.player.cmds.lock().unwrap().len()
        }
    }

    #[test]
    fn every_catalog_tool_has_a_handler() {
        let f = Fixture::new();
        for t in asphonix_mcp_protocol::catalog::tools() {
            // `{}` is invalid for some tools (missing args) but never "unknown tool".
            let r = f.call(true, t.name, json!({}));
            if let Err(e) = &r {
                assert!(!e.starts_with(UNKNOWN_TOOL_PREFIX), "{} has no handler: {e}", t.name);
            }
        }
        assert!(f.call(true, "bogus", json!({})).unwrap_err().starts_with(UNKNOWN_TOOL_PREFIX));
    }

    #[test]
    fn control_tools_are_refused_without_the_setting_and_send_nothing() {
        let f = Fixture::new();
        let args = json!({ "positionSec": 5, "db": -10, "trackIds": ["t1"], "kind": "liked" });
        for t in asphonix_mcp_protocol::catalog::tools().into_iter().filter(|t| t.control) {
            let e = f.call(false, t.name, args.clone()).unwrap_err();
            assert_eq!(e, CONTROL_DISABLED, "{}", t.name);
        }
        assert_eq!(f.sent(), 0);
        // Read tools still work with control off.
        assert!(f.call(false, "now_playing", json!({})).is_ok());
    }

    #[test]
    fn arguments_must_be_an_object_and_are_validated() {
        let f = Fixture::new();
        assert!(f.call(true, "search", json!("x")).is_err());
        assert!(f.call(true, "search", json!({})).unwrap_err().contains("invalid arguments"));
        assert!(f.call(true, "search", json!({ "query": "  " })).is_err());
        assert!(f.call(true, "get_tracks", json!({ "sort": "bogus" })).is_err());
        assert!(f.call(true, "get_tracks", json!({ "limit": "ten" })).is_err());
    }

    #[test]
    fn paging_is_capped_at_the_hard_maximum() {
        assert_eq!(page_of(&PageArgs { offset: 5, limit: Some(100_000) }, 50), (5, MAX_PAGE));
        assert_eq!(page_of(&PageArgs { offset: 0, limit: Some(0) }, 50), (0, 1));
        assert_eq!(page_of(&PageArgs::default(), 50), (0, 50));
        let f = Fixture::new();
        let r = f.call(false, "get_tracks", json!({ "limit": 100000, "offset": 1 })).unwrap();
        assert_eq!(r["total"], 3);
        assert_eq!(r["offset"], 1);
        assert_eq!(r["items"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn track_projection_is_compact_and_leaks_no_paths() {
        let f = Fixture::new();
        let r = f.call(false, "get_tracks", json!({ "sort": "dateAdded", "dir": "desc" })).unwrap();
        let text = r.to_string();
        assert!(!text.contains("secret"), "file paths must not be exposed");
        assert!(!text.contains("cov1") && !text.contains("coverArt"));
        let items = r["items"].as_array().unwrap();
        assert_eq!(items[0]["id"], "t3");
        assert_eq!(items[0]["format"], "MP3 320k");
        assert_eq!(items[2]["format"], "FLAC 24/96");
        assert_eq!(items[1]["liked"], true);
        assert_eq!(items[0]["liked"], false);
        assert_eq!(items[2]["dateAdded"], "2026-10-01T00:00:00Z");
    }

    #[test]
    fn search_and_browse_tools() {
        let f = Fixture::new();
        let r = f.call(false, "search", json!({ "query": "nov", "type": "artist" })).unwrap();
        assert_eq!(r["artists"]["items"][0]["id"], "ar1");
        assert_eq!(r["tracks"]["items"].as_array().unwrap().len(), 0);
        let r = f.call(false, "search", json!({ "query": "alph" })).unwrap();
        assert_eq!(r["tracks"]["items"][0]["id"], "t1");
        let r = f.call(false, "get_album", json!({ "id": "al1" })).unwrap();
        assert_eq!(r["album"]["name"], "Red Album");
        assert_eq!(r["tracks"]["items"].as_array().unwrap().len(), 2);
        let r = f.call(false, "get_album", json!({ "id": "al1", "offset": 1, "limit": 1 })).unwrap();
        assert_eq!((r["tracks"]["total"].clone(), r["tracks"]["items"][0]["id"].clone()), (json!(2), json!("t2")));
        assert!(f.call(false, "get_album", json!({ "id": "nope" })).unwrap_err().contains("not found"));
        let r = f.call(false, "get_artist", json!({ "id": "ar2" })).unwrap();
        assert_eq!(r["albums"]["items"][0]["id"], "al2");
        assert_eq!(f.call(false, "get_artist", json!({ "id": "ar2", "offset": 1 })).unwrap()["albums"]["items"].as_array().unwrap().len(), 0);
        let r = f.call(false, "list_genres", json!({})).unwrap();
        assert_eq!(r["items"][0], json!({ "name": "Ambient", "albumCount": 1, "trackCount": 2 }));
    }

    #[test]
    fn playlists_and_smart_playlists() {
        let f = Fixture::new();
        let r = f.call(false, "list_playlists", json!({})).unwrap();
        assert_eq!(r["total"], 1);
        assert_eq!(r["items"][0]["name"], "Run mix");
        assert_eq!(r["items"][0]["readOnly"], false);
        let r = f.call(false, "get_playlist", json!({ "id": "pl1", "limit": 1 })).unwrap();
        assert_eq!(r["tracks"]["total"], 2);
        assert_eq!(r["tracks"]["items"][0]["id"], "t3", "playlist order");
        assert_eq!(f.call(false, "list_smart_playlists", json!({})).unwrap()["total"], 0);
        assert!(f.call(false, "list_smart_playlists", json!({ "limit": 0 })).is_ok(), "limits are clamped, not rejected");
        // Ad-hoc rules: Ambient tracks over 150 s, not liked.
        let r = f
            .call(
                false,
                "get_smart_playlist_tracks",
                json!({ "rules": { "rules": [
                    { "field": "genre", "op": "is", "value": "Ambient" },
                    { "field": "duration", "op": "gte", "value": 150 },
                ] } }),
            )
            .unwrap();
        assert_eq!(r["total"], 1);
        assert_eq!(r["items"][0]["id"], "t2");
        // Saved one.
        let sp = f
            .smart
            .create(SmartInput {
                name: "Liked".into(),
                match_mode: MatchMode::All,
                rules: vec![Rule { field: smartlists::RuleField::Starred, op: smartlists::RuleOp::Yes, value: None }],
                sort: SmartSort::Title,
                limit: None,
            })
            .unwrap();
        assert_eq!(f.call(false, "list_smart_playlists", json!({})).unwrap()["items"][0]["id"], sp.id);
        assert_eq!(f.call(false, "list_smart_playlists", json!({ "offset": 1 })).unwrap()["items"].as_array().unwrap().len(), 0);
        let r = f.call(false, "get_smart_playlist_tracks", json!({ "id": sp.id })).unwrap();
        assert_eq!(r["items"][0]["id"], "t2");
        // Exactly one of id/rules; invalid rules are rejected, not executed.
        assert!(f.call(false, "get_smart_playlist_tracks", json!({})).is_err());
        assert!(f.call(false, "get_smart_playlist_tracks", json!({ "id": "x", "rules": { "rules": [] } })).is_err());
        let bad = json!({ "rules": { "rules": [{ "field": "genre", "op": "gte", "value": "x" }] } });
        assert!(f.call(false, "get_smart_playlist_tracks", bad).is_err());
        let inj = json!({ "rules": { "rules": [{ "field": "artist", "op": "contains", "value": "'; DROP TABLE tracks;--" }] } });
        assert_eq!(f.call(false, "get_smart_playlist_tracks", inj).unwrap()["total"], 0);
        assert_eq!(f.call(false, "library_stats", json!({})).unwrap()["tracks"], 3);
    }

    #[test]
    fn history_tools_respect_ranges() {
        let f = Fixture::new();
        let r = f.call(false, "listening_history", json!({ "limit": 2 })).unwrap();
        assert_eq!(r["total"], 6);
        assert_eq!(r["items"][0]["track"]["id"], "t3");
        let r = f
            .call(false, "listening_history", json!({ "since": "1970-01-01T00:00:02Z", "until": "1970-01-01T00:00:05Z" }))
            .unwrap();
        assert_eq!(r["total"], 3, "[2s,5s): plays at 2000, 3000, 4000");
        let r = f.call(false, "top_tracks", json!({ "unlikedOnly": true })).unwrap();
        let ids: Vec<_> = r["items"].as_array().unwrap().iter().map(|i| i["track"]["id"].as_str().unwrap().to_string()).collect();
        assert_eq!(ids, vec!["t1", "t3"]);
        assert_eq!(r["items"][0]["plays"], 3);
        let r = f.call(false, "top_artists", json!({ "limit": 1 })).unwrap();
        assert_eq!(r["items"][0]["name"], "Nova");
        assert_eq!(r["items"][0]["plays"], 5);
        assert!(f.call(false, "top_tracks", json!({ "since": "2026-02-01", "until": "2026-01-01" })).is_err());
        assert!(f.call(false, "top_tracks", json!({ "since": "last week" })).is_err());
    }

    #[test]
    fn when_parsing() {
        assert_eq!(parse_when("1970-01-01T00:00:01Z", false).unwrap(), 1000);
        assert_eq!(parse_when("1970-01-01T01:00:00+01:00", false).unwrap(), 0);
        let since = parse_when("2026-10-31", false).unwrap();
        let until = parse_when("2026-10-31", true).unwrap();
        let day = until - since;
        assert!((23 * 3_600_000..=25 * 3_600_000).contains(&day), "a date-only `until` includes the whole day");
        assert!(parse_when("2026-10-31T18:30:00", false).unwrap() > since);
        assert!(parse_when("2026-10-31 18:30", false).is_ok());
        assert!(parse_when("tomorrow", false).is_err());
        assert!(parse_when("2026-13-45", false).is_err());
    }

    #[test]
    fn now_playing_and_queue_and_stats() {
        let f = Fixture::new();
        let r = f.call(false, "now_playing", json!({})).unwrap();
        assert_eq!(r["status"], "paused");
        assert_eq!(r["positionSec"], 12.3);
        assert_eq!(r["output"]["sampleRate"], 48000);
        assert_eq!(r["output"]["bitPerfect"], false);
        assert_eq!(r["volumeDb"], -30.0);
        let q = f.call(false, "get_queue", json!({})).unwrap();
        assert_eq!((q["currentIndex"].clone(), q["total"].clone()), (json!(2), json!(7)));
        // Engine never started: an empty queue, not an error (and not a spawn).
        *f.player.queue_reply.lock().unwrap() = Some(EngineRequestError::NotRunning);
        let q = f.call(false, "get_queue", json!({})).unwrap();
        assert_eq!((q["currentIndex"].clone(), q["total"].clone()), (json!(-1), json!(0)));
        // Stalled engine: a bounded, clear error.
        *f.player.queue_reply.lock().unwrap() = Some(EngineRequestError::NoAnswer);
        assert!(f.call(false, "get_queue", json!({})).unwrap_err().contains("did not answer"));
        let s = f.call(false, "library_stats", json!({})).unwrap();
        assert_eq!((s["albums"].clone(), s["likedTracks"].clone(), s["recordedPlays"].clone()), (json!(2), json!(1), json!(6)));
        assert!(s["nowLocal"].as_str().unwrap().contains('T'));
    }

    #[test]
    fn volume_db_inverts_the_engine_taper() {
        use crate::audio::output::volume_to_db;
        for db in [0.0, -1.0, -20.0, -45.5, -59.0] {
            let p = volume_for_db(db).unwrap();
            let back = volume_to_db(p).unwrap() as f64;
            assert!((back - db).abs() < 0.01, "{db} -> {p} -> {back}");
        }
        assert_eq!(volume_for_db(0.0).unwrap(), 1.0);
        assert_eq!(volume_for_db(-60.0).unwrap(), 0.0);
        assert_eq!(volume_for_db(-500.0).unwrap(), 0.0);
        assert!(volume_for_db(3.0).is_err());
        assert!(volume_for_db(f64::NAN).is_err());
    }

    fn only_cmd(f: &Fixture) -> EngineCommand {
        let mut g = f.player.cmds.lock().unwrap();
        assert_eq!(g.len(), 1);
        g.pop().unwrap()
    }

    #[test]
    fn transport_tools_send_commands_when_allowed() {
        let f = Fixture::new();
        f.call(true, "play_pause", json!({})).unwrap();
        assert!(matches!(only_cmd(&f), EngineCommand::PlayPause));
        f.call(true, "play_pause", json!({ "action": "pause" })).unwrap();
        assert!(matches!(only_cmd(&f), EngineCommand::Pause));
        assert!(f.call(true, "play_pause", json!({ "action": "dance" })).is_err());
        f.call(true, "next", json!({})).unwrap();
        assert!(matches!(only_cmd(&f), EngineCommand::Next));
        f.call(true, "previous", json!({})).unwrap();
        assert!(matches!(only_cmd(&f), EngineCommand::Previous));
        f.call(true, "seek", json!({ "positionSec": 42.5 })).unwrap();
        assert!(matches!(only_cmd(&f), EngineCommand::Seek(s) if s == 42.5));
        assert!(f.call(true, "seek", json!({ "positionSec": -1 })).is_err());
        // Never past the track (the recorder reports 200 s), never absurd values.
        assert!(f.call(true, "seek", json!({ "positionSec": 200.5 })).unwrap_err().contains("<= 200.0"));
        assert!(f.call(true, "seek", json!({ "positionSec": 1e308 })).is_err());
        f.call(true, "seek", json!({ "positionSec": 200 })).unwrap();
        assert!(matches!(only_cmd(&f), EngineCommand::Seek(s) if s == 200.0));
        f.call(true, "set_volume_db", json!({ "db": -30 })).unwrap();
        assert!(matches!(only_cmd(&f), EngineCommand::SetVolume(p) if (p - 0.5).abs() < 1e-6));
        assert!(f.call(true, "set_volume_db", json!({ "db": 6 })).is_err());
        assert_eq!(f.sent(), 0, "rejected calls send nothing");
    }

    #[test]
    fn play_tracks_and_enqueue_filter_unknown_ids_and_cap_the_count() {
        let f = Fixture::new();
        let r = f.call(true, "play_tracks", json!({ "trackIds": ["t3", "ghost", "t1"], "startIndex": 1 })).unwrap();
        assert_eq!(r["queued"], 2);
        assert_eq!(r["unknownIds"], json!(["ghost"]));
        assert_eq!(r["shuffleOn"], false, "the recorder's player has shuffle off");
        assert!(r.get("note").is_none());
        match only_cmd(&f) {
            EngineCommand::PlayQueue { track_ids, start_index } => {
                assert_eq!(track_ids, vec!["t3", "t1"]);
                assert_eq!(start_index, 1, "start maps to the first known id at/after startIndex");
            }
            _ => panic!("expected PlayQueue"),
        }
        assert!(f.call(true, "play_tracks", json!({ "trackIds": ["ghost"] })).is_err());
        assert!(f.call(true, "play_tracks", json!({ "trackIds": [] })).is_err());
        assert!(f.call(true, "play_tracks", json!({ "trackIds": ["t1"], "startIndex": 5 })).is_err());
        let many: Vec<String> = (0..=MAX_IDS).map(|i| format!("x{i}")).collect();
        assert!(f.call(true, "enqueue", json!({ "trackIds": many })).unwrap_err().contains("at most"));
        f.call(true, "enqueue", json!({ "trackIds": ["t2", "t1"], "position": "next" })).unwrap();
        match only_cmd(&f) {
            EngineCommand::Enqueue { track_ids, position } => {
                assert_eq!(track_ids, vec!["t2", "t1"], "order kept");
                assert_eq!(position, EnqueuePosition::Next);
            }
            _ => panic!("expected Enqueue"),
        }
    }

    #[test]
    fn play_source_builds_the_same_commands_as_the_app() {
        use crate::ipc::{SourceKind, SourceMode};
        let f = Fixture::new();
        f.call(true, "play_source", json!({ "kind": "playlist", "id": "pl1", "shuffle": true })).unwrap();
        match only_cmd(&f) {
            EngineCommand::PlaySource { kind, mode, track_ids, source_id, .. } => {
                assert_eq!(kind, SourceKind::Playlist);
                assert_eq!(mode, SourceMode::Shuffle);
                assert_eq!(track_ids, vec!["t3", "t1"]);
                assert_eq!(source_id.as_deref(), Some("pl1"));
            }
            _ => panic!("expected PlaySource"),
        }
        let r = f.call(true, "play_source", json!({ "kind": "genre", "id": "Ambient" })).unwrap();
        assert_eq!(r["poolSize"], 2);
        assert!(matches!(only_cmd(&f), EngineCommand::PlaySource { kind: SourceKind::Genre, mode: SourceMode::InOrder, .. }));
        f.call(true, "play_source", json!({ "kind": "liked" })).unwrap();
        assert!(matches!(only_cmd(&f), EngineCommand::PlaySource { kind: SourceKind::Liked, .. }));
        f.call(true, "play_source", json!({ "kind": "allTracks", "shuffle": true })).unwrap();
        assert!(matches!(only_cmd(&f), EngineCommand::PlaySource { kind: SourceKind::AllTracks, mode: SourceMode::Shuffle, .. }));
        f.call(true, "play_source", json!({ "kind": "artist", "id": "ar1", "startTrackId": "t2" })).unwrap();
        assert!(matches!(only_cmd(&f), EngineCommand::PlaySource { kind: SourceKind::Artist, start_track_id: Some(s), .. } if s == "t2"));
        f.call(true, "play_source", json!({ "kind": "album", "id": "al1", "startTrackId": "t2" })).unwrap();
        match only_cmd(&f) {
            EngineCommand::PlayQueue { track_ids, start_index } => {
                assert_eq!(track_ids, vec!["t1", "t2"]);
                assert_eq!(start_index, 1);
            }
            _ => panic!("expected PlayQueue"),
        }
        assert!(f.call(true, "play_source", json!({ "kind": "playlist" })).unwrap_err().contains("needs an id"));
        assert!(f.call(true, "play_source", json!({ "kind": "playlist", "id": "nope" })).is_err());
        assert!(f.call(true, "play_source", json!({ "kind": "smart", "id": "nope" })).is_err());
        assert!(f.call(true, "play_source", json!({ "kind": "radio" })).is_err());
        assert_eq!(f.sent(), 0);
    }

    #[test]
    fn play_source_flow_maps_to_the_flow_mode_and_arc() {
        use crate::ipc::{SourceKind, SourceMode};
        let f = Fixture::new();
        let r = f.call(true, "play_source", json!({ "kind": "playlist", "id": "pl1", "flow": "build" })).unwrap();
        assert_eq!((r["mode"].clone(), r["arc"].clone()), (json!("flow"), json!("build")));
        assert!(matches!(
            only_cmd(&f),
            EngineCommand::PlaySource { kind: SourceKind::Playlist, mode: SourceMode::Flow, arc: FlowArc::Build, .. }
        ));
        for (kind, id) in [("artist", Some("ar1")), ("genre", Some("Ambient")), ("liked", None), ("allTracks", None)] {
            let mut args = json!({ "kind": kind, "flow": "peak" });
            if let Some(id) = id {
                args["id"] = json!(id);
            }
            f.call(true, "play_source", args).unwrap();
            assert!(matches!(only_cmd(&f), EngineCommand::PlaySource { mode: SourceMode::Flow, arc: FlowArc::Peak, .. }), "{kind}");
        }
        // Without flow: unchanged shuffle / in-order modes, no arc in the result.
        let r = f.call(true, "play_source", json!({ "kind": "liked", "shuffle": true })).unwrap();
        assert_eq!(r["mode"], "shuffle");
        assert!(r.get("arc").is_none());
        assert!(matches!(only_cmd(&f), EngineCommand::PlaySource { mode: SourceMode::Shuffle, .. }));
        // Mutually exclusive with shuffle, not for albums, closed set of arcs.
        assert!(f.call(true, "play_source", json!({ "kind": "liked", "flow": "steady", "shuffle": true })).unwrap_err().contains("not both"));
        assert!(f.call(true, "play_source", json!({ "kind": "album", "id": "al1", "flow": "steady" })).unwrap_err().contains("albums"));
        assert!(f.call(true, "play_source", json!({ "kind": "liked", "flow": "chaos" })).is_err());
        assert_eq!(f.sent(), 0);
    }

    fn seed_listens(db: &Db) {
        db.with_tx(|tx| {
            // (track, started_at, listened_ms, duration_ms, end_reason, counted)
            for (track, at, heard, dur, reason, counted) in [
                ("t1", 10_000i64, 20_000i64, 100_000i64, "next", 0i64),
                ("t1", 20_000, 100_000, 100_000, "finished", 1),
                ("t2", 30_000, 5_000, 200_000, "jump", 0),
                ("gone", 40_000, 1_000, 0, "stopped", 0),
            ] {
                tx.execute(
                    "INSERT INTO listen_events (track_id, started_at, ended_at, listened_ms, duration_ms, end_reason,
                                                source_kind, source_id, origin, counted, max_position_ms)
                     VALUES (?1, ?2, ?2 + ?3, ?3, ?4, ?5, 'playlist', 'pl1', 'source', ?6, ?3)",
                    rusqlite::params![track, at, heard, dur, reason, counted],
                )?;
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn listen_events_and_skip_stats_tools() {
        let f = Fixture::new();
        seed_listens(&f.db);
        let r = f.call(false, "listen_events", json!({})).unwrap();
        assert_eq!(r["total"], 4);
        let items = r["items"].as_array().unwrap();
        assert_eq!(items[0]["trackId"], "gone", "newest first");
        assert!(items[0].get("track").is_none(), "a track no longer in the library has no projection");
        assert_eq!(items[1]["track"]["title"], "Beta");
        assert_eq!((items[1]["endReason"].clone(), items[1]["listenedSec"].clone()), (json!("jump"), json!(5.0)));
        assert_eq!(items[3]["countedAsPlay"], false);
        assert_eq!(items[2]["countedAsPlay"], true);
        assert!(items[0]["startedAt"].as_str().unwrap().contains('T'));
        assert!(!r.to_string().contains("secret"), "no paths");
        // Range [15 s, 35 s) on startedAt, paging.
        let r = f
            .call(false, "listen_events", json!({ "since": "1970-01-01T00:00:15Z", "until": "1970-01-01T00:00:35Z", "limit": 1 }))
            .unwrap();
        assert_eq!((r["total"].clone(), r["items"].as_array().unwrap().len()), (json!(2), 1));
        assert_eq!(f.call(false, "listen_events", json!({ "limit": 100000 })).unwrap()["items"].as_array().unwrap().len(), 4);
        assert!(f.call(false, "listen_events", json!({ "since": "yesterday" })).is_err());

        let r = f.call(false, "skip_stats", json!({})).unwrap();
        let t1 = r["items"].as_array().unwrap().iter().find(|i| i["trackId"] == "t1").unwrap().clone();
        assert_eq!((t1["sessions"].clone(), t1["skips"].clone(), t1["countedPlays"].clone()), (json!(2), json!(1), json!(1)));
        assert_eq!(t1["avgListenedFraction"], 0.6);
        assert_eq!(t1["track"]["title"], "Alpha");
        let r = f.call(false, "skip_stats", json!({ "minSessions": 2 })).unwrap();
        assert_eq!(r["total"], 1);
    }
}
