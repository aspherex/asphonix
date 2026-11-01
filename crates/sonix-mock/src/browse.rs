//! Library-browsing commands of the mock (`mock.ts`: `browse_get_albums/artists/album/artist/
//! artist_tracks/home_sections/genres/genre`, `browse_get_artist_top_tracks/info`, `history_recent`),
//! as pure functions over the generated [`Library`] and the star map. Orders, seeds and the
//! "popular / most played / similar" rules are the ones of the JS mock.
use crate::library::{self, EPOCH_MS, GENRES, Library, NUM_ALBUMS, NUM_ARTISTS, Rng};
use sonix_api::*;
use std::cmp::Ordering;
use std::collections::HashMap;

pub type Stars = HashMap<String, String>;

pub const MOCK_BIO: [&str; 3] = [
    "Formed in a converted warehouse, the project grew out of late-night sessions with modular synthesizers and a battered drum machine.",
    "Early releases circulated on cassette before a run of 12-inch records brought wider attention, with live sets built entirely from improvised patches.",
    "Later work moves toward longer, more spacious arrangements while keeping the rhythmic backbone of the early singles.",
];

fn rng_for(seed: u64) -> f64 {
    Rng::for_seed(seed).next()
}

/// `a.localeCompare(b)` for the ASCII names of the mock: case-insensitive primary order.
fn locale_cmp(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase())
}

pub fn album_s(lib: &Library, stars: &Stars, i: usize) -> Album {
    let mut a = lib.album(i);
    if let Some(s) = stars.get(&a.id) {
        a.starred = Some(s.clone());
    }
    a
}

pub fn artist_s(lib: &Library, stars: &Stars, i: usize) -> Artist {
    let mut a = lib.artist(i);
    if let Some(s) = stars.get(&a.id) {
        a.starred = Some(s.clone());
    }
    a
}

pub fn track_s(lib: &Library, stars: &Stars, ix: usize) -> Track {
    let mut t = lib.track(ix);
    if let Some(s) = stars.get(&t.id) {
        t.starred = Some(s.clone());
    }
    t
}

fn slice<T>(v: Vec<T>, offset: usize, limit: usize) -> Vec<T> {
    v.into_iter().skip(offset).take(limit).collect()
}

pub fn artists_page(lib: &Library, stars: &Stars, req: &GetArtistsRequest) -> Paged<Artist> {
    let mut ix: Vec<usize> = (0..NUM_ARTISTS).collect();
    if req.sort != Some(ArtistSort::Random) {
        ix.sort_by(|a, b| locale_cmp(&library::artist_name(*a), &library::artist_name(*b)));
    } else {
        ix.sort_by(|a, b| rng_for(*a as u64).partial_cmp(&rng_for(*b as u64)).unwrap_or(Ordering::Equal));
    }
    let items = slice(ix, req.offset, req.limit).into_iter().map(|i| artist_s(lib, stars, i)).collect();
    Paged { items, total: NUM_ARTISTS, offset: req.offset }
}

pub fn albums_page(lib: &Library, stars: &Stars, req: &GetAlbumsRequest) -> Paged<Album> {
    let mut ix: Vec<usize> = (0..NUM_ALBUMS).collect();
    if let Some(f) = &req.filter {
        if let Some(g) = &f.genre {
            ix.retain(|i| lib.album(*i).genre.as_deref() == Some(g.as_str()));
        }
        if let Some(y) = f.year {
            ix.retain(|i| lib.album(*i).year == Some(y));
        }
        if f.starred_only {
            ix.retain(|i| stars.contains_key(&format!("al_{i}")));
        }
    }
    match req.sort {
        AlbumSort::Alphabetical => ix.sort_by(|a, b| locale_cmp(&library::album_title(*a), &library::album_title(*b))),
        AlbumSort::Artist => ix.sort_by_key(|a| library::album_artist_index(*a)),
        AlbumSort::Year => ix.sort_by_key(|a| lib.album(*a).year),
        AlbumSort::Random => ix.sort_by(|a, b| rng_for(*a as u64 + 99).partial_cmp(&rng_for(*b as u64 + 99)).unwrap_or(Ordering::Equal)),
        // newest / recent / frequent: deterministic pseudo ordering seeded by `a * 3 + sort.length`
        s @ (AlbumSort::Newest | AlbumSort::Recent | AlbumSort::Frequent) => {
            let len = match s {
                AlbumSort::Frequent => 8,
                _ => 6,
            };
            ix.sort_by(|a, b| rng_for(*a as u64 * 3 + len).partial_cmp(&rng_for(*b as u64 * 3 + len)).unwrap_or(Ordering::Equal));
        }
    }
    let total = ix.len();
    let items = slice(ix, req.offset, req.limit).into_iter().map(|i| album_s(lib, stars, i)).collect();
    Paged { items, total, offset: req.offset }
}

fn album_index(id: &str) -> Option<usize> {
    id.strip_prefix("al_")?.parse().ok().filter(|i| *i < NUM_ALBUMS)
}
fn artist_index(id: &str) -> Option<usize> {
    id.strip_prefix("ar_")?.parse().ok().filter(|i| *i < NUM_ARTISTS)
}

pub fn album_detail(lib: &Library, stars: &Stars, id: &str) -> ApiResult<AlbumDetail> {
    let i = album_index(id).ok_or_else(|| ApiError::not_found("album"))?;
    let album = album_s(lib, stars, i);
    let start = lib.album_first_track(i);
    let tracks = (0..album.song_count as usize).map(|j| track_s(lib, stars, start + j)).collect();
    Ok(AlbumDetail { album, tracks })
}

fn albums_by_artist(idx: usize) -> Vec<usize> {
    (0..NUM_ALBUMS).filter(|i| library::album_artist_index(*i) == idx).collect()
}

pub fn artist_detail(lib: &Library, stars: &Stars, id: &str) -> ApiResult<ArtistDetail> {
    let idx = artist_index(id).ok_or_else(|| ApiError::not_found("artist"))?;
    let albums: Vec<Album> = albums_by_artist(idx).into_iter().map(|i| album_s(lib, stars, i)).collect();
    let mut artist = artist_s(lib, stars, idx);
    artist.album_count = albums.len() as u32;
    Ok(ArtistDetail { artist, albums })
}

/// `artistTracksOrdered`: every track by an artist, album year then track number (stable).
pub fn artist_tracks(lib: &Library, stars: &Stars, idx: usize) -> Vec<Track> {
    let mut tracks = Vec::new();
    for ai in albums_by_artist(idx) {
        let start = lib.album_first_track(ai);
        for j in 0..lib.album(ai).song_count as usize {
            tracks.push(track_s(lib, stars, start + j));
        }
    }
    tracks.sort_by_key(|t| (t.year.map(u64::from).unwrap_or(u64::MAX), t.track_number.unwrap_or(0)));
    tracks
}

pub fn artist_tracks_for_id(lib: &Library, stars: &Stars, id: &str) -> ApiResult<Vec<Track>> {
    let idx = artist_index(id).ok_or_else(|| ApiError::not_found("artist"))?;
    Ok(artist_tracks(lib, stars, idx))
}

pub fn home_sections(lib: &Library, stars: &Stars) -> HomeSections {
    let sample = |seed_base: u64, n: u64| -> Vec<Album> {
        (0..n).map(|i| album_s(lib, stars, (rng_for(seed_base + i * 991) * NUM_ALBUMS as f64).floor() as usize)).collect()
    };
    HomeSections { recently_played: sample(1, 6), frequent: sample(2, 5), newest: sample(3, 8), random: sample(4, 5) }
}

/// `mockGenres()`: genres in order of first appearance over the albums, with album/track counts.
pub fn genres(lib: &Library) -> Vec<Genre> {
    let mut out: Vec<Genre> = Vec::new();
    for i in 0..NUM_ALBUMS {
        let a = lib.album(i);
        let Some(g) = a.genre else { continue };
        match out.iter_mut().find(|x| x.name == g) {
            Some(x) => {
                x.album_count += 1;
                x.track_count += a.song_count;
            }
            None => out.push(Genre { name: g, album_count: 1, track_count: a.song_count }),
        }
    }
    out
}

pub fn genres_page(lib: &Library, req: &GetGenresRequest) -> Paged<Genre> {
    let mut all = genres(lib);
    if req.sort == GenreSort::Alphabetical {
        all.sort_by(|a, b| locale_cmp(&a.name, &b.name));
    } else {
        all.sort_by(|a, b| b.track_count.cmp(&a.track_count).then_with(|| locale_cmp(&a.name, &b.name)));
    }
    let total = all.len();
    Paged { items: slice(all, req.offset, req.limit), total, offset: req.offset }
}

pub fn genre(lib: &Library, name: &str) -> ApiResult<Genre> {
    genres(lib).into_iter().find(|g| g.name == name).ok_or_else(|| ApiError::not_found("genre"))
}

/// `mockGenreTrackIds`: every track of every album of the genre (global track indices).
pub fn genre_track_ixs(lib: &Library, name: &str) -> Vec<u32> {
    let mut ids = Vec::new();
    for ai in 0..NUM_ALBUMS {
        let a = lib.album(ai);
        if a.genre.as_deref() != Some(name) {
            continue;
        }
        let start = lib.album_first_track(ai);
        ids.extend((0..a.song_count as usize).map(|j| (start + j) as u32));
    }
    ids
}

pub fn artist_track_ixs(lib: &Library, stars: &Stars, id: &str) -> ApiResult<Vec<u32>> {
    Ok(artist_tracks_for_id(lib, stars, id)?.iter().filter_map(|t| library::track_index(&t.id)).map(|i| i as u32).collect())
}

/// `ensureMockHistory()`: ~100 plays, one every 3.1 h, newest first (relative to the epoch).
fn history(lib: &Library) -> Vec<(usize, i64)> {
    let total = lib.total_tracks();
    (0..100usize).map(|k| ((k * 137 + 11) % total, EPOCH_MS - (k as f64 * 3.1 * 3_600_000.0) as i64 - 60_000)).collect()
}

pub fn history_recent(lib: &Library, stars: &Stars, req: &HistoryRecentRequest) -> Paged<HistoryEntry> {
    let mut seen = std::collections::HashSet::new();
    let mut items = Vec::new();
    for (ix, at) in history(lib) {
        if !seen.insert(ix) {
            continue;
        }
        items.push(HistoryEntry { track: track_s(lib, stars, ix), played_at: at });
        if items.len() >= req.limit {
            break;
        }
    }
    let total = items.len();
    Paged { items, total, offset: 0 }
}

/// `browse_get_artist_top_tracks` (the JS mock waits 450 ms to look like a network call).
pub fn top_tracks(lib: &Library, stars: &Stars, id: &str) -> ApiResult<ArtistTopTracks> {
    let idx = artist_index(id).ok_or_else(|| ApiError::not_found("artist"))?;
    std::thread::sleep(std::time::Duration::from_millis(450));
    let all = artist_tracks(lib, stars, idx);
    if idx % 3 != 0 && !all.is_empty() {
        let items: Vec<Track> = all.into_iter().enumerate().filter(|(i, _)| i % 2 == 0).map(|(_, t)| t).take(8).collect();
        let total = items.len();
        return Ok(ArtistTopTracks { source: TopSource::Popular, items, total, offset: 0 });
    }
    let mut counts: Vec<(usize, u32)> = Vec::new();
    for (ix, _) in history(lib) {
        if lib.track(ix).artist_id.as_deref() == Some(id) {
            match counts.iter_mut().find(|c| c.0 == ix) {
                Some(c) => c.1 += 1,
                None => counts.push((ix, 1)),
            }
        }
    }
    counts.sort_by_key(|c| std::cmp::Reverse(c.1)); // stable, like Array.sort
    let items: Vec<Track> = counts.into_iter().take(10).map(|(ix, _)| track_s(lib, stars, ix)).collect();
    let total = items.len();
    Ok(ArtistTopTracks { source: TopSource::MostPlayed, items, total, offset: 0 })
}

/// `browse_get_artist_info` (700 ms simulated network).
pub fn artist_info(lib: &Library, stars: &Stars, id: &str) -> ApiResult<ArtistInfo> {
    let idx = artist_index(id).ok_or_else(|| ApiError::not_found("artist"))?;
    std::thread::sleep(std::time::Duration::from_millis(700));
    let mut similar = Vec::new();
    let mut k = 1;
    while similar.len() < 6 && k < 40 {
        let j = (idx + k * 17) % NUM_ARTISTS;
        if j != idx {
            similar.push(artist_s(lib, stars, j));
        }
        k += 1;
    }
    Ok(ArtistInfo {
        biography: if idx % 4 == 3 { None } else { Some(MOCK_BIO.join("\n")) },
        similar: if idx % 5 == 4 { Vec::new() } else { similar },
    })
}

#[allow(dead_code)]
pub fn genre_names() -> &'static [&'static str] {
    &GENRES
}

// ------------------------------------------------------------ the simulated server-side play queue (`mock.ts: serverQueue`)
/// "listened on the phone an hour ago": 8 tracks `tr_{(4242 + j*29) % 50}`, the 4th current, at 83 s.
pub const SERVER_QUEUE_CURRENT_INDEX: usize = 3;
pub const SERVER_QUEUE_CURRENT: &str = "tr_29";
pub const SERVER_QUEUE_POSITION: f64 = 83.0;

pub fn server_queue_ids() -> Vec<String> {
    (0..8).map(|j| format!("tr_{}", (4242 + j * 29) % 50)).collect()
}

pub fn server_queue_offer(lib: &Library, stars: &Stars) -> ServerQueueOffer {
    let ids = server_queue_ids();
    let ix = library::track_index(&ids[SERVER_QUEUE_CURRENT_INDEX]).unwrap_or(0);
    ServerQueueOffer {
        track: track_s(lib, stars, ix),
        position_sec: SERVER_QUEUE_POSITION,
        track_count: ids.len(),
        changed_at: Some(library::iso_from_ms(EPOCH_MS - 3_600_000)),
        changed_by: Some("SONIX Mobile".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genres_cover_every_album_and_sort_both_ways() {
        let lib = Library::new();
        let g = genres(&lib);
        assert_eq!(g.len(), 12);
        assert_eq!(g.iter().map(|x| x.album_count as usize).sum::<usize>(), NUM_ALBUMS);
        let by_tracks = genres_page(&lib, &GetGenresRequest { offset: 0, limit: 1000, sort: GenreSort::Tracks });
        assert!(by_tracks.items.windows(2).all(|w| w[0].track_count >= w[1].track_count));
        let az = genres_page(&lib, &GetGenresRequest { offset: 0, limit: 1000, sort: GenreSort::Alphabetical });
        assert_eq!(az.items[0].name, "Ambient");
    }

    #[test]
    fn album_detail_lists_its_tracks_and_artist_detail_its_albums() {
        let lib = Library::new();
        let stars = Stars::new();
        let d = album_detail(&lib, &stars, "al_5").unwrap();
        assert_eq!(d.tracks.len() as u32, d.album.song_count);
        assert!(d.tracks.iter().all(|t| t.album_id.as_deref() == Some("al_5")));
        let a = artist_detail(&lib, &stars, d.album.artist_id.as_deref().unwrap()).unwrap();
        assert!(a.albums.iter().any(|x| x.id == "al_5"));
        assert_eq!(a.artist.album_count as usize, a.albums.len());
    }

    #[test]
    fn starred_only_filter_and_home_sections() {
        let lib = Library::new();
        let mut stars = Stars::new();
        stars.insert("al_3".into(), "x".into());
        let req = GetAlbumsRequest { offset: 0, limit: 60, sort: AlbumSort::Alphabetical, filter: Some(AlbumFilter { starred_only: true, ..Default::default() }) };
        let p = albums_page(&lib, &stars, &req);
        assert_eq!((p.total, p.items[0].id.as_str()), (1, "al_3"));
        let h = home_sections(&lib, &stars);
        assert_eq!((h.recently_played.len(), h.frequent.len(), h.newest.len(), h.random.len()), (6, 5, 8, 5));
    }
}
