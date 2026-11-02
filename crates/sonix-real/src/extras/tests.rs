//! Tests of the read-only extras against a local mock HTTP server (127.0.0.1, random port) serving canned Subsonic JSON.
use super::*;
use crate::art::cache_path;
use sonix_core::subsonic::Client;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

type Resp = (u16, &'static str, Vec<u8>);

/// Records `endpoint?query` of every request and answers via `handler(endpoint, query)`.
struct Mock {
    url: String,
    log: Arc<Mutex<Vec<String>>>,
}

impl Mock {
    fn start(handler: impl Fn(&str, &str) -> Resp + Send + Sync + 'static) -> Mock {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let log = Arc::new(Mutex::new(Vec::new()));
        let lg = log.clone();
        let handler = Arc::new(handler);
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                let (lg, handler) = (lg.clone(), handler.clone());
                std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                let n = s.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let target = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                let ep = path.rsplit('/').next().unwrap_or("").trim_end_matches(".view").to_string();
                lg.lock().unwrap().push(format!("{ep}?{query}"));
                let (code, ct, body) = handler(&ep, query);
                let _ = write!(s, "HTTP/1.1 {code} X\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(&body);
                });
            }
        });
        Mock { url, log }
    }
    fn count(&self, ep: &str) -> usize {
        self.log.lock().unwrap().iter().filter(|l| l.starts_with(&format!("{ep}?"))).count()
    }
    fn total(&self) -> usize {
        self.log.lock().unwrap().len()
    }
    fn find(&self, ep: &str) -> String {
        self.log.lock().unwrap().iter().find(|l| l.starts_with(&format!("{ep}?"))).cloned().unwrap_or_default()
    }
    /// Every request must be a read endpoint (these features never write to the server).
    fn assert_read_only(&self) {
        const READ: [&str; 9] =
            ["getOpenSubsonicExtensions", "getLyricsBySongId", "getLyrics", "getTopSongs", "getArtistInfo2", "getSimilarSongs", "getSimilarSongs2", "getCoverArt", "stream"];
        for l in self.log.lock().unwrap().iter() {
            let ep = l.split('?').next().unwrap();
            assert!(READ.contains(&ep), "unexpected endpoint {ep}");
        }
    }
}


/// Temp dir on D: (C: is nearly full), removed on drop even when an assertion fails.
struct TmpDir(std::path::PathBuf);

impl TmpDir {
    fn new() -> Self {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::path::PathBuf::from("<build-dir>/tmp").join(format!("sonix-extras-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TmpDir(p)
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        // The closed backend's threads may hold the SQLite files for a moment: retry for up to 5 s.
        let t = std::time::Instant::now();
        while std::fs::remove_dir_all(&self.0).is_err() && self.0.exists() && t.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

/// `secs` of 16-bit mono 8 kHz silence as a WAV file.
fn silent_wav(secs: u32) -> Vec<u8> {
    let data = 8000 * 2 * secs;
    let mut v = Vec::with_capacity(44 + data as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&1u16.to_le_bytes()); // mono
    v.extend_from_slice(&8000u32.to_le_bytes());
    v.extend_from_slice(&16000u32.to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data.to_le_bytes());
    v.resize(44 + data as usize, 0);
    v
}

fn wait_for<T>(secs: u64, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < end {
        if let Some(v) = f() {
            return Some(v);
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    None
}
fn json(body: &str) -> Resp {
    (200, "application/json", format!(r#"{{"subsonic-response":{{"status":"ok","version":"1.16.1",{body}}}}}"#).into_bytes())
}

fn not_found() -> Resp {
    (404, "text/plain", Vec::new())
}

fn png(rgb: [u8; 3]) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(64, 64, image::Rgb(rgb));
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(img).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
    out
}

fn song(id: &str) -> String {
    format!(r#"{{"id":"{id}","title":"{id}","artist":"Band","suffix":"flac"}}"#)
}

fn use_server(b: &RealBackend, url: &str) {
    *b.0.client.write().unwrap() = Some(Client::new(url, "u", "p").unwrap());
}

fn seed(b: &RealBackend) {
    b.0.db
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO artists (id, name, album_count) VALUES ('ar1','Band & Co',1),('ar2','Other',1);
                 INSERT INTO tracks (id, title, artist, artist_id, genre, suffix, cover_art) VALUES
                   ('t1','Song One','Band','ar1','Rock','flac','c1'),
                   ('t2','Song Two','Band','ar1','Rock','flac','c2'),
                   ('t3','Song Three','Band','ar1','Rock','flac','c3'),
                   ('t4','Song Four','Band','ar1','Rock','flac','c1'),
                   ('lonely','Lonely','',NULL,NULL,'flac',NULL);
                 UPDATE tracks SET rating = 1 WHERE id = 't3';
                 INSERT INTO playlists (id, name, song_count) VALUES ('p1','Mix',4),('p0','Empty',0);
                 INSERT INTO playlist_tracks (playlist_id, position, track_id) VALUES ('p1',0,'t1'),('p1',1,'t4'),('p1',2,'t2'),('p1',3,'t3');",
            )
        })
        .unwrap();
}

fn near(p: &[u8], w: u32, x: u32, y: u32, rgb: [u8; 3]) {
    let i = ((y * w + x) * 4) as usize;
    for k in 0..3 {
        assert!((i32::from(p[i + k]) - i32::from(rgb[k])).abs() <= 16, "pixel ({x},{y}) = {:?}, want {rgb:?}", &p[i..i + 3]);
    }
}

/// One sequential test over its own temp dirs (`open_in`); the mock server is swapped per section.
#[test]
fn extras_against_a_local_mock_server() {
    let tmp = TmpDir::new();
    let b = RealBackend::open_in(tmp.0.join("roaming"), tmp.0.join("local"));
    seed(&b);

    // --- lyrics: OpenSubsonic first (songLyrics advertised), cached, "none" cached, no classic fallback when advertised
    let m = Mock::start(|ep, q| match ep {
        "getOpenSubsonicExtensions" => json(r#""openSubsonicExtensions":[{"name":"songLyrics","versions":[1]}]"#),
        "getLyricsBySongId" if q.contains("id=t1") => json(r#""lyricsList":{"structuredLyrics":[{"synced":true,"line":[{"start":1000,"value":"Hello"},{"start":2500,"value":"World"}]}]}"#),
        "getLyricsBySongId" => json(r#""lyricsList":{"structuredLyrics":[]}"#),
        _ => not_found(),
    });
    use_server(&b, &m.url);
    let l = b.lyrics_get("t1").unwrap().unwrap();
    assert!(l.synced);
    assert_eq!(l.source, LyricsSource::Opensubsonic);
    assert_eq!(l.lines.iter().map(|x| (x.start_ms, x.text.as_str())).collect::<Vec<_>>(), vec![(Some(1000), "Hello"), (Some(2500), "World")]);
    assert!(m.find("getLyricsBySongId").contains("id=t1"));
    let n = m.total();
    assert_eq!(b.lyrics_get("t1").unwrap(), Some(l), "second call is a cache hit");
    assert_eq!(m.total(), n);
    assert_eq!(b.lyrics_get("t2").unwrap(), None);
    assert_eq!(m.count("getLyrics"), 0, "songLyrics advertised and empty: no classic fallback");
    let n = m.total();
    assert_eq!(b.lyrics_get("t2").unwrap(), None);
    assert_eq!(b.lyrics_get("nope").unwrap(), None);
    assert_eq!(m.total(), n, "cached none / unknown track: no requests");
    m.assert_read_only();

    // --- lyrics: classic getLyrics when the server has no songLyrics
    let m = Mock::start(|ep, _| match ep {
        "getOpenSubsonicExtensions" => json(r#""openSubsonicExtensions":[]"#),
        "getLyrics" => json(r#""lyrics":{"artist":"Band","title":"Song Three","value":"line one\nline two"}"#),
        _ => not_found(),
    });
    use_server(&b, &m.url);
    let l = b.lyrics_get("t3").unwrap().unwrap();
    assert!(!l.synced);
    assert_eq!(l.source, LyricsSource::Classic);
    assert_eq!(l.lines.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), vec!["line one", "line two"]);
    assert!(l.lines.iter().all(|x| x.start_ms.is_none()));
    assert_eq!(m.count("getLyricsBySongId"), 0);
    assert!(m.find("getLyrics").contains("artist=Band") && m.find("getLyrics").contains("title=Song+Three"));
    m.assert_read_only();

    // --- artist top tracks + info
    let m = Mock::start(|ep, q| match ep {
        "getTopSongs" if q.contains("artist=Other") => json(r#""topSongs":{}"#),
        "getTopSongs" => json(&format!(r#""topSongs":{{"song":[{},{},{}]}}"#, song("t2"), song("t1"), song("zz"))),
        "getArtistInfo2" => json(
            r#""artistInfo2":{"biography":"Formed in 1990. <a target=\"_blank\" href=\"https://www.last.fm/music/Band\">Read more on Last.fm</a>","similarArtist":[{"id":"ar2","name":"Other"},{"id":"x9","name":"Ghost"}]}"#,
        ),
        _ => not_found(),
    });
    use_server(&b, &m.url);
    let top = b.browse_get_artist_top_tracks("ar1").unwrap();
    assert_eq!(top.source, TopSource::Popular);
    assert_eq!(top.items.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec!["t2", "t1"], "server order, unknown ids dropped");
    assert_eq!((top.total, top.offset), (2, 0));
    let q = m.find("getTopSongs");
    assert!(q.contains("artist=Band+%26+Co") && q.contains("count=10"), "{q}");
    assert_eq!(b.browse_get_artist_top_tracks("ar1").unwrap(), top);
    assert_eq!(m.count("getTopSongs"), 1, "7-day cache");
    let top2 = b.browse_get_artist_top_tracks("ar2").unwrap();
    assert_eq!((top2.source, top2.items.len()), (TopSource::MostPlayed, 0), "nothing popular, never played: honest empty fallback");
    let info = b.browse_get_artist_info("ar1").unwrap();
    assert_eq!(info.biography.as_deref(), Some("Formed in 1990."));
    assert_eq!(info.similar.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), vec!["ar2"], "only artists in the local library");
    assert!(m.find("getArtistInfo2").contains("id=ar1"));
    assert_eq!(b.browse_get_artist_info("ar1").unwrap(), info);
    assert_eq!(m.count("getArtistInfo2"), 1, "7-day cache");
    m.assert_read_only();

    // --- radio: pool (server order, 1-star and unknown ids removed, local top-up), NotFound when nothing
    let m = Mock::start(|ep, _| match ep {
        "getSimilarSongs" => json(&format!(r#""similarSongs":{{"song":[{},{},{},{}]}}"#, song("t2"), song("t3"), song("ghost"), song("t1"))),
        "getSimilarSongs2" => json(r#""similarSongs2":{}"#),
        "stream" => {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            not_found()
        }
        _ => not_found(),
    });
    use_server(&b, &m.url);
    let client = b.client_now().unwrap();
    let (ids, artists) = b.0.rt.block_on(sonix_core::continuity::radio_pool(&client, &b.0.db, core::StarTargetType::Track, "t1")).unwrap();
    assert!(m.find("getSimilarSongs").contains("count=50") && m.find("getSimilarSongs").contains("id=t1"));
    assert_eq!(&ids[..2], ["t2", "t1"]);
    assert!(!ids.contains(&"t3".to_string()) && !ids.contains(&"ghost".to_string()));
    assert_eq!(artists.len(), ids.len());
    assert!(artists.iter().all(|a| a.as_deref() == Some("Band")));
    let (ids, _) = b.0.rt.block_on(sonix_core::continuity::radio_pool(&client, &b.0.db, core::StarTargetType::Artist, "ar1")).unwrap();
    assert!(m.count("getSimilarSongs2") == 1 && !ids.is_empty(), "artist seeds use getSimilarSongs2 plus the local fallback");
    let e = b.radio_start(RadioStartRequest { seed: RadioSeed { kind: RadioSeedKind::Artist, id: "none".into() } }).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    // the engine path: silent (volume 0); the mock's stream hangs and then 404s, so the queue is observed before anything fails
    b.player_set_volume(SetVolumeRequest { volume: 0.0 }).unwrap();
    b.radio_start(RadioStartRequest { seed: RadioSeed { kind: RadioSeedKind::Track, id: "t1".into() } }).unwrap();
    let q = wait_for(3, || b.player_get_queue(PageRequest { offset: 0, limit: 50 }).ok().filter(|q| q.source.is_some())).expect("radio queue");
    let src = q.source.unwrap();
    assert_eq!((src.kind, src.mode, src.label.as_str()), (SourceKind::Radio, SourceMode::InOrder, "Radio"));
    b.0.engine.send(EngineCommand::Stop);
    m.assert_read_only();

    // --- playlist mosaic: 3 distinct covers (c1, c2, c3; the 4th track repeats c1), cached, real covers unaffected
    let m = Mock::start(|ep, q| match ep {
        "getCoverArt" => {
            let rgb = if q.contains("id=c1") {
                [255, 0, 0]
            } else if q.contains("id=c2") {
                [0, 255, 0]
            } else {
                [0, 0, 255]
            };
            (200, "image/png", png(rgb))
        }
        _ => not_found(),
    });
    use_server(&b, &m.url);
    let a = b.art_get("mosaic/p1", 160).unwrap();
    assert_eq!((a.width, a.height), (160, 160));
    near(&a.rgba, 160, 40, 80, [255, 0, 0]); // large left pane
    near(&a.rgba, 160, 130, 30, [0, 255, 0]); // top right
    near(&a.rgba, 160, 130, 130, [0, 0, 255]); // bottom right
    near(&a.rgba, 160, 96, 80, [0x0A, 0x0A, 0x0C]); // gutter seam (left pane 96 px, 2 px seam)
    assert_eq!(m.count("getCoverArt"), 3);
    assert!(m.find("getCoverArt").contains("size=640"));
    // cached (lossy JPEG) re-reads, also through the web's cache-busting key
    for id in ["mosaic/p1", "mosaic/p1?v=4-2026-01-01T00:00:00Z"] {
        let again = b.art_get(id, 160).unwrap();
        near(&again.rgba, 160, 40, 80, [255, 0, 0]);
        near(&again.rgba, 160, 130, 30, [0, 255, 0]);
        near(&again.rgba, 160, 130, 130, [0, 0, 255]);
    }
    assert_eq!(m.count("getCoverArt"), 3, "mosaic and tiles are served from the disk cache");
    b.art_get("p1", 160).unwrap();
    assert_eq!(m.count("getCoverArt"), 4, "a bare playlist id is an ordinary cover id (no mosaic)");
    // a torn cache file is rejected and rebuilt
    let tile = cache_path(&b.art_dir(), "c2", 160);
    std::fs::write(&tile, b"not an image").unwrap();
    assert_eq!(b.art_get("c2", 160).unwrap().rgba[1], 255);
    assert_eq!(m.count("getCoverArt"), 4, "a torn bucket file is re-derived from the cached master, no refetch");
    assert_eq!(b.art_get("c1", 160).unwrap().rgba[0], 255, "a real cover still resolves (cached tile)");
    assert_eq!(b.art_get("mosaic/p0", 160).unwrap_err().code, ErrorCode::NotFound, "no covered tracks");
    assert_eq!(b.art_get("mosaic/missing", 64).unwrap_err().code, ErrorCode::NotFound);
    m.assert_read_only();

    // --- waveform: a track that playback does not hold (radio started other tracks) is never downloaded for a scan
    let n = m.total();
    let w = b.player_get_waveform("lonely").unwrap();
    assert_eq!((w.track_id.as_str(), w.buckets.len(), w.filled, w.done), ("lonely", WAVEFORM_BUCKETS, 0, true));
    assert_eq!(m.count("stream"), 0);
    assert_eq!(m.total(), n);

    // --- A-B loop through the real engine: silent (volume 0) playback of a generated WAV from the mock
    let m = Mock::start(|ep, _| match ep {
        "stream" => (200, "audio/wav", silent_wav(20)),
        _ => not_found(),
    });
    use_server(&b, &m.url);
    let events = b.subscribe();
    b.player_set_volume(SetVolumeRequest { volume: 0.0 }).unwrap();
    b.player_play_queue(PlayQueueRequest { track_ids: vec!["t1".into()], start_index: 0 }).unwrap();
    let playing = wait_for(6, || (b.player_get_state().ok()?.status == PlaybackStatus::Playing).then_some(()));
    if playing.is_none() {
        // No usable audio output device on this machine: the engine cannot hold a track, nothing to test further.
        eprintln!("SKIP A-B loop / live waveform: playback did not start ({:?})", b.player_get_state().map(|s| (s.status, s.error)));
    } else {
        // the waveform of the playing (live) track is scanned from playback's own download
        let w = wait_for(6, || b.player_get_waveform("t1").ok().filter(|w| w.done)).expect("waveform done");
        assert_eq!((w.buckets.len(), w.filled), (WAVEFORM_BUCKETS, WAVEFORM_BUCKETS));
        assert_eq!(m.count("stream"), 1, "the scan reuses playback's download");
        // press 1: A at the current position; press 2: B (after A); press 3: clear
        std::thread::sleep(std::time::Duration::from_millis(900));
        b.player_loop_cycle().unwrap();
        let a = wait_for(3, || b.player_get_state().ok().and_then(|s| s.loop_a)).expect("loop A set by the engine");
        assert!(b.player_get_state().unwrap().loop_b.is_none());
        std::thread::sleep(std::time::Duration::from_millis(900));
        b.player_loop_cycle().unwrap();
        let (la, lb) = wait_for(3, || b.player_get_state().ok().and_then(|s| Some((s.loop_a?, s.loop_b?)))).expect("loop B set by the engine");
        assert!(la == a && lb > la, "A={la} B={lb}");
        // the markers arrive as PlayerState events
        let mut seen = false;
        while let Ok(ev) = events.try_recv() {
            if let Event::PlayerState(s) = ev {
                seen |= s.loop_a == Some(la) && s.loop_b == Some(lb);
            }
        }
        assert!(seen, "a PlayerState event carried loopA/loopB");
        b.player_loop_cycle().unwrap();
        wait_for(3, || b.player_get_state().ok().filter(|s| s.loop_a.is_none() && s.loop_b.is_none())).expect("cleared");
    }
    b.0.engine.send(EngineCommand::Stop);
    m.assert_read_only();

    drop(b);
}
