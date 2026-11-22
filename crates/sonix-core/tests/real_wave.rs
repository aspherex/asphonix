//! Read-only probe of the waveform data path against the server saved in the app's connection:
//! `cargo test -p sonix-core --test real_wave -- --ignored --nocapture`.
//! Credentials come from the saved connection + OS keychain (never printed). Only `stream` GETs, no writes,
//! no audio device (the decoder thread is driven into a ring nobody drains).
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sonix_core::audio::decode::{spawn_decoder, DecodeCommand, DecodeEvent};
use sonix_core::audio::format::Container;
use sonix_core::audio::ring::new_pcm_ring;
use sonix_core::audio::waveform::{WaveformStore, BUCKETS};
use sonix_core::audio::{SubsonicTrackSource, TrackSource};

#[test]
#[ignore]
fn real_tracks_get_a_waveform_from_the_playback_download() {
    sonix_core::secrets::set_service_name("dev.sonix.app");
    let dir = std::path::PathBuf::from(std::env::var("APPDATA").unwrap()).join("dev.sonix.app");
    let cfg = sonix_core::secrets::load_config(&dir).expect("saved connection");
    let pw = sonix_core::secrets::load_profile_password(&cfg.url, &cfg.username).unwrap().expect("keychain password");
    let client = sonix_core::subsonic::Client::new(&cfg.url, &cfg.username, &pw).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let mut songs = Vec::new();
    for page in 0..12 {
        let p = rt.block_on(client.search_songs_page(page * 500, 500)).unwrap();
        if p.is_empty() {
            break;
        }
        songs.extend(p);
    }
    let mut picks: Vec<(String, String, f64)> = Vec::new();
    for s in &songs {
        if !picks.iter().any(|p| p.1 == s.suffix) {
            picks.push((s.id.clone(), s.suffix.clone(), s.duration.unwrap_or(0.0)));
        }
    }
    println!("{} songs; formats: {:?}", songs.len(), picks.iter().map(|p| &p.1).collect::<Vec<_>>());
    let shared = Arc::new(std::sync::RwLock::new(Some(client)));
    let source: Arc<dyn TrackSource> = Arc::new(SubsonicTrackSource::new(shared));
    for resume in [false, true] {
        for (id, suffix, dur) in &picks {
            let (id, dur) = (id.as_str(), *dur);
            let store = WaveformStore::new(source.clone());
            let before = store.get(id, dur);
            // Emulate the engine: decoder probes, then decodes into a paced ring; `resume` = restored-position seek.
            let playback = source.open(id).unwrap();
            let (cmd_tx, cmd_rx) = channel();
            let (evt_tx, evt_rx) = channel();
            spawn_decoder(playback, None, cmd_rx, evt_tx);
            let probed = matches!(evt_rx.recv_timeout(Duration::from_secs(30)), Ok(DecodeEvent::FormatReady(_)));
            let (prod, _cons) = new_pcm_ring(44_100, 4);
            let _ = cmd_tx.send(DecodeCommand::Begin { container: Container::I16, producer: prod });
            if resume {
                let (prod, _cons) = new_pcm_ring(44_100, 4);
                let _ = cmd_tx.send(DecodeCommand::Seek { seconds: dur * 0.6, producer: prod });
            }
            let t0 = Instant::now();
            let mut last = store.get(id, dur);
            while !last.done && t0.elapsed() < Duration::from_secs(90) {
                std::thread::sleep(Duration::from_millis(200));
                last = store.get(id, dur);
            }
            let max = last.buckets.iter().max().copied().unwrap_or(0);
            println!(
                "{suffix} resume={resume} {id}: probed={probed} before={}/{} after: filled={}/{BUCKETS} done={} max={max} in {:?}",
                before.filled, before.done, last.filled, last.done, t0.elapsed()
            );
            let _ = cmd_tx.send(DecodeCommand::Stop);
        }
    }
}
