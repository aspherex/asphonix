//! Deterministic library generator, a bit-exact port of the generators in
//! SoniX `src/lib/ipc/mock.ts` (`mulberry32`, `rngFor`, `artistName`, `albumTitle`,
//! `trackTitle`, `genAlbum`, `genTrack`, `genTrackFormat`): the same ~54.5k tracks
//! with the same titles/artists/albums/years/formats as `npm run dev`, generated
//! on demand from a small per-album index and never held in memory as objects.
//!
//! Known, documented differences from the JS mock: `dateAdded` is relative to a
//! fixed epoch ([`EPOCH_MS`]) instead of `Date.now()` (so rows are reproducible
//! across runs); ReplayGain tags and star/rating state are not generated here.
use sonix_api::{Album, Artist, Track, TrackFormat};

pub const NUM_ARTISTS: usize = 640;
pub const NUM_ALBUMS: usize = 5200;
/// 2026-10-31T12:00:00Z, the stand-in for JS `Date.now()`.
pub const EPOCH_MS: i64 = 1_793_448_000_000;

const ADJ: [&str; 21] = [
    "Obsidian", "Lime", "Null", "Static", "Hollow", "Vector", "Cold", "Analog", "Digital", "Ferric", "Molten", "Subsonic", "Fractal",
    "Derelict", "Chrome", "Nocturnal", "Industrial", "Brutal", "Kinetic", "Glacial", "Synthetic",
];
const NOUN: [&str; 20] = [
    "Drift", "Engine", "Circuit", "Horizon", "Monolith", "Signal", "Vortex", "Chamber", "Reactor", "Echo", "Lattice", "Fathom", "Current",
    "Rift", "Cascade", "Spindle", "Axis", "Pulse", "Grid", "Static",
];
const ARTIST_FIRST: [&str; 10] = ["DJ", "Dr.", "", "", "", "", "The", "", "", ""];
const ARTIST_CORE: [&str; 30] = [
    "Monolith", "Overmono", "Bicep", "Skee", "Lorn", "Hopkins", "Burial", "Autechre", "Floating", "Hecker", "Seinfeld", "Modeselektor",
    "Objekt", "Helena", "Vex", "Kowton", "Batu", "Karenn", "Actress", "Pev", "Blawan", "Surgeon", "Shackleton", "Plastikman", "Container",
    "Lussuria", "Varg", "Ptaki", "Oake", "Alva",
];
const ARTIST_SUFFIX: [&str; 10] = ["", "", "", " Mask", " Noto", " Twin", " Jr.", " Collective", " Ensemble", ""];
pub const GENRES: [&str; 12] =
    ["Techno", "Ambient", "IDM", "Breaks", "Drum & Bass", "Dub", "Electro", "House", "Modular", "Industrial", "Noise", "Downtempo"];
const FORMATS: [(&str, f64); 5] = [("flac", 55.0), ("mp3", 20.0), ("alac", 10.0), ("wav", 8.0), ("aac", 7.0)];

/// JS `ToUint32` of a double.
fn to_uint32(x: f64) -> u32 {
    x.trunc().rem_euclid(4_294_967_296.0) as u32
}

/// `mulberry32(seed)`: `seed` is a JS number (a double; `a >>> 0`).
pub struct Rng(u32);

impl Rng {
    pub fn mulberry32(seed: f64) -> Self {
        Rng(to_uint32(seed))
    }
    /// `rngFor(seed)` = `mulberry32(seed * 2654435761 + 1)`, evaluated in f64 like JS
    /// (for large seeds the product is not exactly representable, and JS rounds it).
    pub fn for_seed(seed: u64) -> Self {
        Self::mulberry32(seed as f64 * 2_654_435_761.0 + 1.0)
    }
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6d2b_79f5);
        let a = self.0;
        let mut t = (a ^ (a >> 15)).wrapping_mul(1 | a);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        ((t ^ (t >> 14)) as f64) / 4_294_967_296.0
    }
    /// `pick(rng, arr)`
    pub fn pick<'a, T>(&mut self, arr: &'a [T]) -> &'a T {
        let i = (self.next() * arr.len() as f64).floor() as usize % arr.len();
        &arr[i]
    }
    pub fn floor_mul(&mut self, n: u64) -> u64 {
        (self.next() * n as f64).floor() as u64
    }
}

pub fn artist_name(index: usize) -> String {
    let mut r = Rng::for_seed(index as u64 * 7 + 1);
    let first = *r.pick(&ARTIST_FIRST);
    let core = *r.pick(&ARTIST_CORE);
    let suffix = *r.pick(&ARTIST_SUFFIX);
    let name = format!("{}{core}{suffix}", if first.is_empty() { String::new() } else { format!("{first} ") });
    let name = name.trim().to_string();
    if index.is_multiple_of(7) { format!("{name} {}", 100 + (index % 900)) } else { name }
}

pub fn album_title(index: usize) -> String {
    let mut r = Rng::for_seed(index as u64 * 13 + 5);
    let a = *r.pick(&ADJ);
    let n = *r.pick(&NOUN);
    format!("{a} {n}")
}

pub fn track_title(album_index: usize, t: usize) -> String {
    let mut r = Rng::for_seed(album_index as u64 * 97 + t as u64 * 31 + 3);
    let a = *r.pick(&ADJ);
    let n = *r.pick(&NOUN);
    format!("{a} {n} ({} {})", ["Pt.", "Mix", "Dub", "Edit", "Reprise"][t % 5], (t % 3) + 1)
}

pub fn album_artist_index(album_index: usize) -> usize {
    (Rng::for_seed(album_index as u64 * 17 + 23).next() * NUM_ARTISTS as f64).floor() as usize
}

fn weighted_format(r: &mut Rng) -> &'static str {
    let total: f64 = FORMATS.iter().map(|f| f.1).sum();
    let mut x = r.next() * total;
    for (s, w) in FORMATS {
        if x < w {
            return s;
        }
        x -= w;
    }
    "flac"
}

pub fn track_format(seed: u64) -> TrackFormat {
    let mut r = Rng::for_seed(seed);
    let suffix = weighted_format(&mut r);
    let lossless = matches!(suffix, "flac" | "alac" | "wav");
    let rates: &[u32] = if lossless { &[44100, 48000, 88200, 96000, 192000] } else { &[44100, 48000] };
    let sampling_rate = *r.pick(rates);
    let bit_depth = if lossless { Some(*r.pick(&[16u32, 24, 24, 32])) } else { None };
    let bit_rate = if lossless {
        ((sampling_rate as f64 * bit_depth.unwrap_or(16) as f64 * 2.0) / 1000.0).round() as u32
    } else {
        *r.pick(&[128u32, 192, 256, 320])
    };
    TrackFormat { suffix: suffix.into(), bit_rate: Some(bit_rate), bit_depth, sampling_rate, channel_count: 2 }
}

/// ISO-8601 UTC with milliseconds (`Date.toISOString()` shape).
pub fn iso_from_ms(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    // Howard Hinnant's civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let (h, mi, s, milli) = (rem / 3_600_000, rem / 60_000 % 60, rem / 1000 % 60, rem % 1000);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z")
}

/// The small per-album index: track count per album plus prefix sums.
pub struct Library {
    counts: Vec<u32>,
    offsets: Vec<u32>,
    artist_album_counts: Vec<u32>,
}

impl Default for Library {
    fn default() -> Self {
        Self::new()
    }
}

impl Library {
    pub fn new() -> Self {
        let mut counts = Vec::with_capacity(NUM_ALBUMS);
        let mut offsets = Vec::with_capacity(NUM_ALBUMS + 1);
        let mut acc = 0u32;
        let mut artist_album_counts = vec![0u32; NUM_ARTISTS];
        for i in 0..NUM_ALBUMS {
            let n = 6 + Rng::for_seed(i as u64 * 3 + 11).floor_mul(10) as u32;
            counts.push(n);
            offsets.push(acc);
            acc += n;
            artist_album_counts[album_artist_index(i)] += 1;
        }
        offsets.push(acc);
        Self { counts, offsets, artist_album_counts }
    }

    pub fn total_tracks(&self) -> usize {
        self.offsets[NUM_ALBUMS] as usize
    }

    /// Global index of the first track of album `index` (the per-album prefix sum).
    pub fn album_first_track(&self, index: usize) -> usize {
        self.offsets[index] as usize
    }

    /// Binary search: (album index, track index within the album).
    pub fn album_for_track(&self, global: usize) -> (usize, usize) {
        let g = global as u32;
        let ix = (self.offsets.partition_point(|&o| o <= g) - 1).min(NUM_ALBUMS - 1);
        (ix, (g - self.offsets[ix]) as usize)
    }

    pub fn artist(&self, index: usize) -> Artist {
        let id = format!("ar_{index}");
        Artist { id: id.clone(), name: artist_name(index), album_count: self.artist_album_counts[index], cover_art: Some(id), starred: None, rating: None }
    }

    pub fn album(&self, index: usize) -> Album {
        let id = format!("al_{index}");
        let mut r = Rng::for_seed(index as u64 * 41 + 2);
        let artist_idx = album_artist_index(index);
        let year = 1990 + r.floor_mul(35) as u32;
        let genre = (*r.pick(&GENRES)).to_string();
        let per = 180 + r.floor_mul(180);
        // mock.ts draws `playCount` before `created` (object literal order)
        let play_count = r.floor_mul(400) as u32;
        let created = EPOCH_MS - r.floor_mul(10_000_000_000) as i64;
        Album {
            id: id.clone(),
            name: album_title(index),
            artist: artist_name(artist_idx),
            artist_id: Some(format!("ar_{artist_idx}")),
            year: Some(year),
            genre: Some(genre),
            cover_art: Some(id),
            song_count: self.counts[index],
            duration: (self.counts[index] as u64 * per) as f64,
            starred: None,
            rating: None,
            play_count: Some(play_count),
            created: Some(iso_from_ms(created)),
        }
    }

    /// Cheap fields only (to sort/filter 54k rows without building full `Track`s).
    pub fn sort_fields(&self, global: usize) -> SortFields {
        let (ai, ti) = self.album_for_track(global);
        let mut r = Rng::for_seed(global as u64 * 59 + 7);
        let duration = 90 + r.floor_mul(360);
        let added = EPOCH_MS - r.floor_mul(10_000_000_000) as i64;
        let year = 1990 + Rng::for_seed(ai as u64 * 41 + 2).floor_mul(35) as u32;
        SortFields {
            title: track_title(ai, ti),
            artist: artist_name(album_artist_index(ai)),
            album: album_title(ai),
            year,
            added,
            duration,
            format: track_format(global as u64 * 3 + 1).suffix,
        }
    }

    pub fn track(&self, global: usize) -> Track {
        let (ai, ti) = self.album_for_track(global);
        let album = self.album(ai);
        let mut r = Rng::for_seed(global as u64 * 59 + 7);
        let duration = 90 + r.floor_mul(360);
        let format = track_format(global as u64 * 3 + 1);
        let added = EPOCH_MS - r.floor_mul(10_000_000_000) as i64;
        let size = (duration as f64 * format.bit_rate.map(|b| b as f64).unwrap_or(900.0) * 1000.0 / 8.0).round() as u64;
        Track {
            id: format!("tr_{global}"),
            title: track_title(ai, ti),
            artist: album.artist.clone(),
            artist_id: album.artist_id.clone(),
            album: Some(album.name.clone()),
            album_id: Some(album.id.clone()),
            album_artist: Some(album.artist.clone()),
            track_number: Some(ti as u32 + 1),
            disc_number: Some(1),
            year: album.year,
            genre: album.genre.clone(),
            duration: duration as f64,
            format,
            cover_art: album.cover_art.clone(),
            starred: None,
            rating: None,
            size: Some(size),
            replay_gain: None,
            path: None,
            date_added: Some(iso_from_ms(added)),
        }
    }

    /// Lower-case substring match over title/artist/album, like `trackMatchesFilter`.
    pub fn matches(&self, global: usize, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }
        let (ai, ti) = self.album_for_track(global);
        track_title(ai, ti).to_lowercase().contains(needle)
            || artist_name(album_artist_index(ai)).to_lowercase().contains(needle)
            || album_title(ai).to_lowercase().contains(needle)
    }
}

pub struct SortFields {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: u32,
    pub added: i64,
    pub duration: u64,
    pub format: String,
}

pub fn track_index(id: &str) -> Option<usize> {
    id.strip_prefix("tr_")?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Golden values produced by running the original JS (mock.ts lines 89-287,
    // untouched) under node: `rngFor(seed)()` incl. seeds whose product exceeds 2^53,
    // and the first/boundary tracks. This is what "bit-exact" means here.
    #[test]
    fn rng_matches_js() {
        for (seed, want) in [
            (0u64, 0.6270739405881613),
            (1, 0.314902942860499),
            (2, 0.5179393014404923),
            (4_850_000, 0.47108772676438093),
            (3_000_000, 0.9394550679717213),
            (123_456_789, 0.19137616525404155),
        ] {
            assert_eq!(Rng::for_seed(seed).next(), want, "seed {seed}");
        }
    }

    #[test]
    fn to_uint32_is_js_semantics() {
        assert_eq!(to_uint32(4_294_967_296.0), 0);
        assert_eq!(to_uint32(4_294_967_297.0), 1);
        assert_eq!(to_uint32(-1.0), u32::MAX);
    }

    #[test]
    fn iso_dates() {
        assert_eq!(iso_from_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_from_ms(EPOCH_MS), "2026-10-31T12:00:00.000Z");
        assert_eq!(iso_from_ms(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn library_matches_the_js_mock_row_for_row() {
        let l = Library::new();
        assert_eq!(l.total_tracks(), 54_515);
        // (global index, album, track in album, title, artist, album title, suffix, bitRate, bitDepth, rate)
        type Golden = (usize, usize, usize, &'static str, &'static str, &'static str, &'static str, u32, Option<u32>, u32);
        let golden: [Golden; 8] = [
            (0, 0, 0, "Ferric Monolith (Pt. 1)", "Actress Jr.", "Industrial Horizon", "flac", 6144, Some(32), 96000),
            (1, 0, 1, "Chrome Static (Mix 2)", "Actress Jr.", "Industrial Horizon", "flac", 1536, Some(16), 48000),
            (2, 0, 2, "Obsidian Fathom (Dub 3)", "Actress Jr.", "Industrial Horizon", "aac", 128, None, 48000),
            (7, 0, 7, "Lime Engine (Dub 2)", "Actress Jr.", "Industrial Horizon", "flac", 9216, Some(24), 192000),
            (999, 91, 10, "Hollow Echo (Pt. 2)", "Oake Collective", "Digital Reactor", "flac", 4234, Some(24), 88200),
            (25_000, 2372, 8, "Fractal Current (Edit 3)", "DJ Skee", "Null Fathom", "flac", 12288, Some(32), 192000),
            (49_999, 4766, 9, "Synthetic Signal (Reprise 1)", "Shackleton Noto 212", "Digital Chamber", "mp3", 128, None, 44100),
            (54_514, 5199, 5, "Kinetic Horizon (Pt. 3)", "Varg Collective", "Molten Engine", "mp3", 320, None, 44100),
        ];
        for (i, ai, ti, title, artist, album, suffix, br, bd, sr) in golden {
            assert_eq!(l.album_for_track(i), (ai, ti), "index {i}");
            let t = l.track(i);
            assert_eq!(t.title, title);
            assert_eq!(t.artist, artist);
            assert_eq!(t.album.as_deref(), Some(album));
            assert_eq!(t.format.suffix, suffix);
            assert_eq!(t.format.bit_rate, Some(br));
            assert_eq!(t.format.bit_depth, bd);
            assert_eq!(t.format.sampling_rate, sr);
            assert_eq!(t.id, format!("tr_{i}"));
            let f = l.sort_fields(i);
            assert_eq!((f.title.as_str(), f.artist.as_str(), f.album.as_str()), (title, artist, album));
            assert_eq!(f.duration as f64, t.duration);
            assert_eq!(Some(f.year), t.year);
        }
        for (i, name) in [(0usize, "Blawan Ensemble 100"), (7, "Overmono Jr. 107"), (14, "The Overmono Jr. 114"), (639, "Monolith")] {
            assert_eq!(artist_name(i), name);
        }
    }

    #[test]
    fn album_boundaries_map_back() {
        let l = Library::new();
        for a in [1usize, 77, 2600, 5199] {
            assert_eq!(l.album_for_track(l.offsets[a] as usize), (a, 0));
            assert_eq!(l.album_for_track(l.offsets[a] as usize - 1).0, a - 1);
        }
        let (ai, ti) = l.album_for_track(l.total_tracks() - 1);
        assert_eq!((ai, ti as u32), (NUM_ALBUMS - 1, l.counts[NUM_ALBUMS - 1] - 1));
    }

    #[test]
    fn tracks_are_deterministic() {
        let l = Library::new();
        for i in [0usize, 1, 999, 25_000, l.total_tracks() - 1] {
            assert_eq!(l.track(i), l.track(i));
        }
        assert!(l.matches(0, "ferric"));
        assert!(l.matches(0, "actress"));
        assert!(!l.matches(0, "zzzz-no"));
    }
}
