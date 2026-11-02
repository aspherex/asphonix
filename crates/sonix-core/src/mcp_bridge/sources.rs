//! `PlaySource` builders the MCP tools share with the app's own Play buttons. Vendored from the reference
//! (`ipc.rs` `play_all_cmd` / `playlist_source_cmd` / `artist_source_cmd` / `apply_playlist_access` / `play_mode`,
//! `discovery.rs` `genre_source_cmd`, `smart_ipc.rs` `liked_source_cmd` / `smart_source_cmd`), kept in this
//! module so the vendored `ipc.rs` / `discovery.rs` stay untouched. Bodies are verbatim (Tauri commands omitted).
use crate::audio::EngineCommand;
use crate::ipc::{
    FlowArc, IpcError, IpcErrorCode, IpcResult, Playlist, SortDir, SourceKind, SourceMode, TrackOrder, TrackSortField,
};
use crate::library;
use crate::smartlists;

fn not_found(message: String) -> IpcError {
    IpcError { code: IpcErrorCode::NotFound, message }
}

fn fresh_seed() -> u64 {
    use rand::Rng;
    rand::thread_rng().gen_range(0..4_294_967_296u64)
}

fn split_ids(rows: Vec<(String, String)>) -> (Vec<String>, Vec<Option<String>>) {
    rows.into_iter().map(|(id, artist)| (id, (!artist.is_empty()).then_some(artist))).unzip()
}

pub(crate) fn play_mode(shuffle: bool, flow: Option<FlowArc>) -> (SourceMode, FlowArc) {
    match flow {
        Some(arc) => (SourceMode::Flow, arc),
        None if shuffle => (SourceMode::Shuffle, FlowArc::Steady),
        None => (SourceMode::InOrder, FlowArc::Steady),
    }
}

pub(crate) fn apply_playlist_access(p: &mut Playlist, me: Option<&str>) {
    let (smart, read_only) = library::playlist_access_flags(p.server_read_only, p.owner.as_deref(), me);
    p.smart = smart;
    p.read_only = read_only;
}

pub(crate) fn play_all_cmd(
    db: &library::Db,
    order: TrackOrder,
    filter: Option<&str>,
    start_track_id: Option<String>,
    flow: Option<FlowArc>,
) -> IpcResult<EngineCommand> {
    let (base_sort, base_dir, shuffle) = match order {
        TrackOrder::Shuffle => (TrackSortField::Title, SortDir::Asc, true),
        TrackOrder::Sort { field, dir } => (field, dir, false),
    };
    let rows = library::get_all_track_ids(db, base_sort, base_dir, filter, start_track_id.as_deref())?;
    let (track_ids, artists) = split_ids(rows);
    let (mode, arc) = play_mode(shuffle, flow);
    Ok(EngineCommand::PlaySource {
        kind: SourceKind::AllTracks,
        label: "All Tracks".into(),
        source_id: None,
        mode,
        arc,
        track_ids,
        artists,
        start_track_id,
    })
}

pub(crate) fn playlist_source_cmd(db: &library::Db, id: String, shuffle: bool, start_track_id: Option<String>, flow: Option<FlowArc>) -> IpcResult<EngineCommand> {
    let Some((label, rows)) = library::get_playlist_play_source(db, &id, start_track_id.as_deref())? else {
        return Err(not_found(format!("playlist {id} not found")));
    };
    if rows.is_empty() {
        return Err(not_found("playlist has no tracks".into()));
    }
    let (track_ids, artists) = split_ids(rows);
    let (mode, arc) = play_mode(shuffle, flow);
    Ok(EngineCommand::PlaySource { kind: SourceKind::Playlist, label, source_id: Some(id), mode, arc, track_ids, artists, start_track_id })
}

pub(crate) fn artist_source_cmd(db: &library::Db, id: String, shuffle: bool, start_track_id: Option<String>, flow: Option<FlowArc>) -> IpcResult<EngineCommand> {
    let Some((label, rows)) = library::get_artist_play_source(db, &id, start_track_id.as_deref())? else {
        return Err(not_found(format!("artist {id} not found")));
    };
    if rows.is_empty() {
        return Err(not_found("artist has no tracks".into()));
    }
    let (track_ids, artists) = split_ids(rows);
    let (mode, arc) = play_mode(shuffle, flow);
    Ok(EngineCommand::PlaySource { kind: SourceKind::Artist, label, source_id: Some(id), mode, arc, track_ids, artists, start_track_id })
}

pub(crate) fn genre_source_cmd(db: &library::Db, name: String, shuffle: bool, start_track_id: Option<String>, flow: Option<FlowArc>) -> IpcResult<EngineCommand> {
    let Some((label, rows)) = library::discovery::get_genre_play_source(db, &name, start_track_id.as_deref())? else {
        return Err(not_found(format!("no playable tracks in genre {name}")));
    };
    let (track_ids, artists) = split_ids(rows);
    let (mode, arc) = play_mode(shuffle, flow);
    Ok(EngineCommand::PlaySource { kind: SourceKind::Genre, label, source_id: Some(name), mode, arc, track_ids, artists, start_track_id })
}

pub(crate) fn liked_source_cmd(db: &library::Db, shuffle: bool, start_track_id: Option<String>, flow: Option<FlowArc>) -> IpcResult<EngineCommand> {
    let rows = library::get_liked_play_source(db, start_track_id.as_deref())?;
    if rows.is_empty() {
        return Err(not_found("no liked songs".into()));
    }
    let (track_ids, artists) = split_ids(rows);
    let (mode, arc) = play_mode(shuffle, flow);
    Ok(EngineCommand::PlaySource { kind: SourceKind::Liked, label: "Liked Songs".into(), mode, arc, track_ids, artists, start_track_id, source_id: None })
}

pub(crate) fn smart_source_cmd(
    db: &library::Db,
    store: &smartlists::SmartStore,
    id: String,
    shuffle: bool,
    start_track_id: Option<String>,
    seed: Option<u64>,
    flow: Option<FlowArc>,
) -> IpcResult<EngineCommand> {
    let p = store.get(&id)?;
    let rows = smartlists::play_ids(db, &p.def, start_track_id.as_deref(), seed.unwrap_or_else(fresh_seed))?;
    if rows.is_empty() {
        return Err(not_found("smart playlist has no matching tracks".into()));
    }
    let (track_ids, artists) = split_ids(rows);
    let (mode, arc) = play_mode(shuffle, flow);
    Ok(EngineCommand::PlaySource { kind: SourceKind::Smart, label: p.def.name.clone(), mode, arc, track_ids, artists, start_track_id, source_id: Some(id) })
}
