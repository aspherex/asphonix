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

impl PlaylistsApi for RealBackend {
    fn playlists_list(&self) -> ApiResult<Vec<Playlist>> {
        r!(library::playlists_list(&self.0.db))
    }
    fn playlists_get(&self, id: &str) -> ApiResult<PlaylistDetail> {
        let d = library::playlist_detail(&self.0.db, id).map_err(err)?.ok_or_else(|| nf("playlist", id))?;
        conv(d)
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
    /// Cover at the display bucket size: disk cache first (`%LOCALAPPDATA%\dev.sonix.native\art`), else
    /// `getCoverArt?size=` (the server resizes), stored as received, decoded to RGBA.
    fn art_get(&self, cover_art: &str, size: u32) -> ApiResult<ArtPixels> {
        let size = bucket_for(size);
        let dir = crate::data_dir().join("art");
        let key: String = cover_art.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
        let path = dir.join(format!("{key}_{size}.img"));
        let bytes = match std::fs::read(&path) {
            Ok(b) if !b.is_empty() => b,
            _ => {
                let client = self.0.client.read().unwrap().clone().ok_or_else(|| ApiError::new(ErrorCode::NotConnected, "not connected"))?;
                let (b, _) = self.0.rt.block_on(client.get_cover_art(cover_art, Some(size))).map_err(err)?;
                let _ = std::fs::create_dir_all(&dir);
                let tmp = path.with_extension("tmp");
                if std::fs::write(&tmp, &b).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
                b
            }
        };
        let img = image::load_from_memory(&bytes).map_err(|e| ApiError::new(ErrorCode::Decode, format!("cover decode: {e}")))?;
        let img = if img.width() > size.max(img.height()) || img.height() > size { img.resize(size, size, image::imageops::FilterType::Triangle) } else { img };
        let rgba = img.to_rgba8();
        Ok(ArtPixels { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() })
    }
}
