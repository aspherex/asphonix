//! Tests of the read-only extras against a local mock HTTP server (127.0.0.1, random port) serving canned Subsonic JSON.
use super::*;
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
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
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

/// One sequential test: `RealBackend::open` reads process-global env (`APPDATA`/`LOCALAPPDATA`) and the client is shared.
#[test]
fn extras_against_a_local_mock_server() {
    let tmp = std::env::temp_dir().join(format!("sonix-extras-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    unsafe { std::env::set_var("APPDATA", tmp.join("roaming")) };
    unsafe { std::env::set_var("LOCALAPPDATA", tmp.join("local")) };
    let b = RealBackend::open();
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
        _ => not_found(),
    });
    use_server(&b, &m.url);
    let client = b.client_now().unwrap();
    let (ids, artists) = b.0.rt.block_on(extras::radio_pool(&client, &b.0.db, core::StarTargetType::Track, "t1")).unwrap();
    assert!(m.find("getSimilarSongs").contains("count=50") && m.find("getSimilarSongs").contains("id=t1"));
    assert_eq!(&ids[..2], ["t2", "t1"]);
    assert!(!ids.contains(&"t3".to_string()) && !ids.contains(&"ghost".to_string()));
    assert_eq!(artists.len(), ids.len());
    assert!(artists.iter().all(|a| a.as_deref() == Some("Band")));
    let (ids, _) = b.0.rt.block_on(extras::radio_pool(&client, &b.0.db, core::StarTargetType::Artist, "ar1")).unwrap();
    assert!(m.count("getSimilarSongs2") == 1 && !ids.is_empty(), "artist seeds use getSimilarSongs2 plus the local fallback");
    let e = b.radio_start(RadioStartRequest { seed: RadioSeed { kind: RadioSeedKind::Artist, id: "none".into() } }).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    // the engine path: silent (volume 0), the mock has no stream so nothing plays; stop right away
    b.player_set_volume(SetVolumeRequest { volume: 0.0 }).unwrap();
    b.radio_start(RadioStartRequest { seed: RadioSeed { kind: RadioSeedKind::Track, id: "t1".into() } }).unwrap();
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
    assert!(m.find("getCoverArt").contains("size=160"));
    assert_eq!(b.art_get("mosaic/p1", 160).unwrap(), a);
    assert_eq!(b.art_get("p1", 160).unwrap(), a, "a bare id of a mirrored playlist gives the same mosaic");
    assert_eq!(m.count("getCoverArt"), 3, "mosaic and tiles are served from the disk cache");
    assert_eq!(b.art_get("c1", 160).unwrap().rgba[0], 255, "a real cover still resolves (cached tile)");
    assert_eq!(b.art_get("mosaic/p0", 160).unwrap_err().code, ErrorCode::NotFound, "no covered tracks");
    assert_eq!(b.art_get("mosaic/missing", 64).unwrap_err().code, ErrorCode::NotFound);
    m.assert_read_only();

    // --- waveform: a track that playback does not hold is never downloaded for a scan
    let n = m.total();
    let w = b.player_get_waveform("t1").unwrap();
    assert_eq!((w.track_id.as_str(), w.buckets.len(), w.filled, w.done), ("t1", WAVEFORM_BUCKETS, 0, true));
    assert_eq!(m.count("stream"), 0);
    assert_eq!(m.total(), n);

    // --- A-B loop: command accepted; loop markers reach PlayerState events
    b.player_loop_cycle().unwrap();
    assert_eq!(b.player_get_state().unwrap().loop_a, None, "nothing audible: no marker");
    let (tx, rx) = std::sync::mpsc::channel();
    let mut st = b.0._state_rx.borrow().clone();
    st.loop_a = Some(12.5);
    st.loop_b = Some(30.0);
    sonix_core::audio::EngineEvents::player_state(&crate::player::Events(tx), &st);
    match rx.recv().unwrap() {
        Event::PlayerState(s) => assert_eq!((s.loop_a, s.loop_b), (Some(12.5), Some(30.0))),
        _ => panic!("expected a PlayerState event"),
    }

    drop(b);
    let _ = std::fs::remove_dir_all(&tmp);
}
