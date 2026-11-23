//! Wire-type fixtures. Every builder returns a complete, valid value; tests change single fields with struct update
//! syntax (`Track { title: "X".into(), ..track("t1") }`) instead of a builder method per field.
use asphonix_api::*;

/// A 3-minute 44.1 kHz / 16-bit stereo FLAC track `id` by "Artist" (`ar1`) on "Album" (`al1`).
pub fn track(id: &str) -> Track {
    Track {
        id: id.into(),
        title: format!("Track {id}"),
        artist: "Artist".into(),
        artist_id: Some("ar1".into()),
        album: Some("Album".into()),
        album_id: Some("al1".into()),
        album_artist: None,
        track_number: None,
        disc_number: None,
        year: None,
        genre: None,
        duration: 180.0,
        format: TrackFormat { suffix: "flac".into(), bit_rate: None, bit_depth: Some(16), sampling_rate: 44_100, channel_count: 2 },
        cover_art: None,
        starred: None,
        rating: None,
        size: None,
        replay_gain: None,
        path: None,
        date_added: None,
    }
}

/// `track(id)` by artist `artist` (id `ar-<artist>`) on album `album` (id `al-<album>`).
pub fn track_by(id: &str, artist: &str, album: &str) -> Track {
    Track {
        artist: artist.into(),
        artist_id: Some(format!("ar-{artist}")),
        album: Some(album.into()),
        album_id: Some(format!("al-{album}")),
        ..track(id)
    }
}

/// `n` tracks `t0000`, `t0001`, ... with titles that sort in id order.
pub fn tracks(n: usize) -> Vec<Track> {
    (0..n).map(|i| Track { track_number: Some(i as u32 + 1), ..track(&format!("t{i:04}")) }).collect()
}

/// An album `id` by "Artist" with `song_count` songs of 3 minutes.
pub fn album(id: &str, song_count: u32) -> Album {
    Album {
        id: id.into(),
        name: format!("Album {id}"),
        artist: "Artist".into(),
        artist_id: Some("ar1".into()),
        year: None,
        genre: None,
        cover_art: None,
        song_count,
        duration: 180.0 * song_count as f64,
        starred: None,
        rating: None,
        play_count: None,
        created: None,
    }
}

/// `n` albums `al0000`, `al0001`, ... with 10 songs each.
pub fn albums(n: usize) -> Vec<Album> {
    (0..n).map(|i| album(&format!("al{i:04}"), 10)).collect()
}

pub fn artist(id: &str, album_count: u32) -> Artist {
    Artist { id: id.into(), name: format!("Artist {id}"), album_count, cover_art: None, starred: None, rating: None }
}

/// `n` artists `ar0000`, `ar0001`, ... with one album each.
pub fn artists(n: usize) -> Vec<Artist> {
    (0..n).map(|i| artist(&format!("ar{i:04}"), 1)).collect()
}

/// A writable playlist owned by "me" holding `tracks`.
pub fn playlist(id: &str, name: &str, tracks: Vec<Track>) -> PlaylistDetail {
    let playlist = Playlist {
        id: id.into(),
        name: name.into(),
        comment: None,
        owner: Some("me".into()),
        public: false,
        song_count: tracks.len() as u32,
        duration: tracks.iter().map(|t| t.duration).sum(),
        cover_art: None,
        created_at: None,
        updated_at: None,
        smart: false,
        read_only: false,
    };
    PlaylistDetail { playlist, tracks }
}

pub fn queue_item(t: Track) -> QueueItem {
    QueueItem { track: t, origin: QueueItemOrigin::Source, flow: None }
}

/// Player state with `t` loaded in `status` at `position_sec`, the queue `queue_length` long.
pub fn player_state(t: Option<Track>, status: PlaybackStatus, position_sec: f64) -> PlayerState {
    let duration_sec = t.as_ref().map(|t| t.duration).unwrap_or(0.0);
    PlayerState { status, track: t, position_sec, duration_sec, ..PlayerState::idle() }
}

/// A connected session (so the UI does not open the login screen) with `track_count` tracks.
pub fn session_connected(track_count: u64) -> SessionStatus {
    SessionStatus {
        state: ConnectionState::Connected,
        server_url: Some("http://mock.invalid".into()),
        username: Some("me".into()),
        server_version: Some("0.0.0".into()),
        api_version: Some("1.16.1".into()),
        ping_ms: None,
        last_sync_at: None,
        track_count: Some(track_count),
    }
}

/// The backend's defaults (everything opt-in off), as `settings_get` returns them on a fresh profile.
pub fn settings() -> Settings {
    Settings {
        scrobbling_enabled: false,
        auto_check_updates: false,
        verbose_logging: false,
        spread_artists_in_shuffle: false,
        queue_window_size: 20,
        autoplay_enabled: false,
        sync_play_queue: false,
        play_threshold_sec: 30,
        free_memory_when_minimized: true,
        replay_gain_mode: ReplayGainMode::Off,
        track_overlay: true,
        track_overlay_corner: OverlayCorner::TopLeft,
    }
}

/// `items[offset..offset+limit]` with the true total (what every paged backend call returns).
pub fn page<T: Clone>(items: &[T], offset: usize, limit: usize) -> Paged<T> {
    let end = offset.saturating_add(limit).min(items.len());
    let items_page = if offset < items.len() { items[offset..end].to_vec() } else { Vec::new() };
    Paged { items: items_page, total: items.len(), offset }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_slices_and_reports_the_true_total() {
        let v: Vec<u32> = (0..250).collect();
        let p = page(&v, 200, 100);
        assert_eq!((p.items.len(), p.total, p.offset, p.items[0]), (50, 250, 200, 200));
        assert!(page(&v, 300, 100).items.is_empty());
        assert_eq!(page(&v, 0, usize::MAX).items.len(), 250);
    }

    #[test]
    fn fixtures_are_distinct_and_valid() {
        let t = tracks(3);
        assert_eq!(t.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["t0000", "t0001", "t0002"]);
        let pl = playlist("p1", "Mix", t);
        assert_eq!((pl.playlist.song_count, pl.playlist.duration), (3, 540.0));
        assert_eq!(track_by("x", "A", "B").album_id.as_deref(), Some("al-B"));
    }
}
