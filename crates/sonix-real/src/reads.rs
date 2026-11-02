//! Library sync + all read-only browse/search/history/liked/playlist calls over the vendored SQLite mirror.
//! Wire types of `sonix-core::ipc` and `sonix-api` are the same JSON shape; values cross via serde.
use crate::{err, RealBackend};
use serde::{de::DeserializeOwned, Serialize};
use sonix_api::*;
use sonix_core::ipc::{IpcError, IpcErrorCode};
use sonix_core::library;

pub(crate) fn conv<A: Serialize, B: DeserializeOwned>(a: A) -> ApiResult<B> {
    let v = serde_json::to_value(a).map_err(|e| ApiError::new(ErrorCode::Decode, e.to_string()))?;
    serde_json::from_value(v).map_err(|e| ApiError::new(ErrorCode::Decode, e.to_string()))
}

fn nf(what: &str, id: &str) -> ApiError {
    ApiError::new(ErrorCode::NotFound, format!("{what} {id} not found"))
}

macro_rules! r {
    ($e:expr) => {
        conv($e.map_err(err)?)
    };
}

impl BrowseApi for RealBackend {
    fn session_check_server_queue(&self) -> ApiResult<Option<ServerQueueOffer>> {
        self.session_check_impl()
    }
    fn session_resume_from_server(&self) -> ApiResult<()> {
        self.session_resume_impl()
    }
    fn browse_get_tracks(&self, req: GetTracksRequest) -> ApiResult<Paged<Track>> {
        let q: sonix_core::ipc::GetTracksRequest = conv(req)?;
        r!(library::get_tracks(&self.0.db, q.offset, q.limit, q.sort, q.dir, q.filter.as_deref()))
    }
    fn browse_get_artists(&self, req: GetArtistsRequest) -> ApiResult<Paged<Artist>> {
        let q: sonix_core::ipc::GetArtistsRequest = conv(req)?;
        r!(library::get_artists(&self.0.db, q.offset, q.limit, q.sort))
    }
    fn browse_get_albums(&self, req: GetAlbumsRequest) -> ApiResult<Paged<Album>> {
        let q: sonix_core::ipc::GetAlbumsRequest = conv(req)?;
        r!(library::get_albums(&self.0.db, q.offset, q.limit, q.sort, q.filter))
    }
    fn browse_get_album(&self, id: &str) -> ApiResult<AlbumDetail> {
        let d = library::get_album_detail(&self.0.db, id).map_err(err)?.ok_or_else(|| nf("album", id))?;
        conv(d)
    }
    fn browse_get_artist(&self, id: &str) -> ApiResult<ArtistDetail> {
        let d = library::get_artist_detail(&self.0.db, id).map_err(err)?.ok_or_else(|| nf("artist", id))?;
        conv(d)
    }
    fn browse_get_artist_tracks(&self, id: &str) -> ApiResult<Vec<Track>> {
        r!(library::get_artist_tracks(&self.0.db, id))
    }
    fn browse_get_home_sections(&self) -> ApiResult<HomeSections> {
        r!(library::get_home_sections(&self.0.db))
    }
    fn browse_get_genres(&self, req: GetGenresRequest) -> ApiResult<Paged<Genre>> {
        let q: sonix_core::discovery::GetGenresRequest = conv(req)?;
        r!(library::discovery::get_genres(&self.0.db, q.offset, q.limit, q.sort))
    }
    fn browse_get_genre(&self, name: &str) -> ApiResult<Genre> {
        let g = library::discovery::get_genre(&self.0.db, name).map_err(err)?.ok_or_else(|| nf("genre", name))?;
        conv(g)
    }
    fn browse_get_artist_top_tracks(&self, id: &str) -> ApiResult<ArtistTopTracks> {
        self.top_tracks_impl(id)
    }
    fn browse_get_artist_info(&self, id: &str) -> ApiResult<ArtistInfo> {
        self.artist_info_impl(id)
    }
    fn history_recent(&self, req: HistoryRecentRequest) -> ApiResult<Paged<HistoryEntry>> {
        r!(library::discovery::history_recent_tracks(&self.0.db, req.limit as u32))
    }
}

impl SearchApi for RealBackend {
    fn search_run(&self, req: SearchRequest) -> ApiResult<SearchResults> {
        let q: sonix_core::ipc::SearchRequest = conv(req)?;
        r!(library::search(&self.0.db, &q.text, q.search_type, q.offset, q.limit))
    }
}

impl LibraryApi for RealBackend {
    fn library_sync_start(&self, req: SyncStartRequest) -> ApiResult<()> {
        self.start_sync(req.full);
        Ok(())
    }
    fn liked_count(&self) -> ApiResult<u32> {
        Ok(library::get_liked_summary(&self.0.db).map_err(err)?.0 as u32)
    }
    fn liked_get(&self, req: PageRequest) -> ApiResult<Paged<Track>> {
        r!(library::get_liked(&self.0.db, req.offset as u64, req.limit as u64))
    }
    fn liked_summary(&self) -> ApiResult<LikedSummary> {
        let (count, duration) = library::get_liked_summary(&self.0.db).map_err(err)?;
        Ok(LikedSummary { count: count as usize, duration })
    }
    fn history_get(&self, req: PageRequest) -> ApiResult<Paged<HistoryItem>> {
        r!(library::discovery::history_page(&self.0.db, req.offset as u32, req.limit as u32))
    }
    fn history_clear(&self) -> ApiResult<()> {
        library::discovery::history_clear(&self.0.db).map_err(err)
    }
    fn smart_list(&self) -> ApiResult<Vec<SmartSummary>> {
        self.smart_list_impl()
    }
    fn smart_get(&self, id: &str, seed: u32) -> ApiResult<SmartSummary> {
        self.smart_get_impl(id, seed)
    }
    fn smart_create(&self, input: SmartInput) -> ApiResult<SmartSummary> {
        self.smart_create_impl(input)
    }
    fn smart_update(&self, id: &str, input: SmartInput) -> ApiResult<SmartSummary> {
        self.smart_update_impl(id, input)
    }
    fn smart_delete(&self, id: &str) -> ApiResult<()> {
        self.smart_delete_impl(id)
    }
    fn smart_duplicate(&self, id: &str) -> ApiResult<SmartSummary> {
        self.smart_duplicate_impl(id)
    }
    fn smart_presets(&self) -> ApiResult<Vec<SmartPreset>> {
        conv(sonix_core::smartlists::presets())
    }
    fn smart_tracks(&self, id: &str, seed: u32, req: PageRequest) -> ApiResult<Paged<Track>> {
        self.smart_tracks_impl(id, seed, req)
    }
    fn smart_preview(&self, input: SmartInput, seed: u32, req: PageRequest) -> ApiResult<Paged<Track>> {
        self.smart_preview_impl(input, seed, req)
    }
}

pub(crate) struct Sink(pub RealBackend, pub u64);

impl library::SyncProgressSink for Sink {
    fn progress(&self, phase: sonix_core::ipc::SyncPhase, current: u64, total: u64, message: Option<&str>) {
        let phase: SyncPhase = conv(phase).unwrap_or(SyncPhase::Error);
        self.0.emit(Event::SyncProgress(SyncProgressEvent { phase, current, total, message: message.map(str::to_string) }));
    }
    fn cancelled(&self) -> bool {
        self.0 .0.epoch.load(std::sync::atomic::Ordering::SeqCst) != self.1
    }
}

#[allow(dead_code)]
fn _unused(_: IpcError, _: IpcErrorCode) {}

impl ArtApi for RealBackend {
    /// Real covers and playlist mosaics: see `art.rs`.
    fn art_get(&self, cover_art: &str, size: u32) -> ApiResult<ArtPixels> {
        self.art_pixels(cover_art, size)
    }
}
