//! Non-Windows shared-mode output fallback (`cpal`). macOS CoreAudio hog
//! mode is not implemented yet; this keeps the engine compiling and
//! playable on non-Windows platforms in the meantime. Always reports
//! `OutputMode::Shared` — truthfully, since `cpal` has no exclusive mode —
//! and always negotiates an `F32` container at the *source's* rate/channels
//! (the decoder never resamples; opening the stream at another rate would
//! play at the wrong speed, so an unsupported rate fails loudly instead).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::Consumer;

use super::{DeviceInfo, MonoTap, Negotiation, RenderEvent, RenderHandle};
use crate::audio::format::{Container, NegotiatedFormat, SourceFormat};
use crate::ipc::OutputMode;

/// Largest callback buffer we pre-size the mono scratch for (frames); larger
/// callbacks just skip the spectrum tap for the excess.
const MAX_CALLBACK_FRAMES: usize = 16_384;

fn host() -> cpal::Host {
    cpal::default_host()
}

fn find_device(device_id: Option<&str>) -> Option<cpal::Device> {
    let h = host();
    match device_id {
        Some(id) => h
            .output_devices()
            .ok()?
            .find(|d| d.name().map(|n| n == id).unwrap_or(false)),
        None => h.default_output_device(),
    }
}

pub fn list_devices() -> Vec<DeviceInfo> {
    let h = host();
    let default_name = h.default_output_device().and_then(|d| d.name().ok());
    let Ok(devices) = h.output_devices() else {
        return Vec::new();
    };
    devices
        .filter_map(|d| {
            let name = d.name().ok()?;
            let is_default = default_name.as_deref() == Some(name.as_str());
            Some(DeviceInfo {
                id: name.clone(),
                name,
                is_default,
            })
        })
        .collect()
}

/// Default output device id (its name, as in `list_devices`).
pub fn default_device_id() -> Option<String> {
    host().default_output_device().and_then(|d| d.name().ok())
}

pub fn shared_format(source: SourceFormat) -> NegotiatedFormat {
    NegotiatedFormat::new(
        source.sample_rate,
        source.channels,
        Container::F32,
        OutputMode::Shared,
    )
}

pub fn negotiate(
    _device_id: Option<String>,
    source: SourceFormat,
    requested_mode: OutputMode,
) -> Negotiation {
    Negotiation {
        format: shared_format(source),
        exclusive_error: (requested_mode == OutputMode::Exclusive)
            .then(|| "exclusive/hog mode is not implemented on this platform yet".to_string()),
    }
}

pub fn spawn_render(
    device_id: Option<String>,
    format: NegotiatedFormat,
    mut consumer: Consumer<u8>,
    mut mono_tap: Option<MonoTap>,
    events: Sender<RenderEvent>,
    start_paused: bool,
    initial_volume: f32,
) -> Result<RenderHandle, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(start_paused));
    let played_frames = Arc::new(AtomicU64::new(0));
    let gap_frames = Arc::new(AtomicU64::new(0)); // not tracked by the cpal fallback
    let volume_bits = Arc::new(AtomicU32::new(initial_volume.to_bits()));
    let (seek_tx, seek_rx) = std::sync::mpsc::channel::<Consumer<u8>>();
    let seeks_applied = Arc::new(AtomicU64::new(0));
    let thread_seeks = seeks_applied.clone();
    let thread_stop = stop.clone();
    let thread_paused = paused.clone();
    let thread_played = played_frames.clone();
    let thread_volume = volume_bits.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let join = std::thread::Builder::new()
        .name("asphonix-render-cpal".into())
        .spawn(move || {
            let Some(device) = find_device(device_id.as_deref()) else {
                let _ = ready_tx.send(Err("no output device".into()));
                return;
            };
            let config = cpal::StreamConfig {
                channels: format.channels as u16,
                sample_rate: cpal::SampleRate(format.sample_rate),
                buffer_size: cpal::BufferSize::Default,
            };
            let channels = (format.channels as usize).max(1);
            let stream_events = events.clone();
            let callback_paused = thread_paused.clone();
            // Pre-allocated: the callback never allocates.
            let mut mono_scratch: Vec<f32> = vec![0.0; MAX_CALLBACK_FRAMES];
            let mut played: u64 = 0;
            let stream = device.build_output_stream(
                &config,
                move |data: &mut [f32], _| {
                    // A seek landed: swap in the fresh (already-seeked) ring
                    // without rebuilding the `cpal` stream — see
                    // `RenderHandle::seek_tx`. `cpal` has no device-buffer
                    // flush primitive, so a few already-queued frames from
                    // just before the seek may still play out, but the
                    // stream itself stays open (no re-negotiation/reopen).
                    if let Ok(new_consumer) = seek_rx.try_recv() {
                        consumer = new_consumer;
                        thread_played.store(0, Ordering::Relaxed);
                        thread_seeks.fetch_add(1, Ordering::Release);
                        played = 0;
                    }
                    if callback_paused.load(Ordering::Relaxed) {
                        data.fill(0.0);
                        return;
                    }
                    // Loaded once per callback (a few ms), not per sample:
                    // see `RenderHandle::volume_bits` — gain is applied here,
                    // at render time, never at decode time, so a volume
                    // change is audible almost immediately instead of after
                    // the whole ring buffer drains.
                    let volume = f32::from_bits(thread_volume.load(Ordering::Relaxed));
                    let unity = (volume - 1.0).abs() <= f32::EPSILON;
                    let mut real_frames = 0usize;
                    let (mut peak_l, mut peak_r) = (0f32, 0f32);
                    // Downmix/peak math only while something shows the spectrum.
                    let tap_on = mono_tap.as_ref().is_some_and(|t| t.active.load(Ordering::Relaxed));
                    for (frame_idx, frame) in data.chunks_mut(channels).enumerate() {
                        if consumer.slots() < frame.len() * 4 {
                            frame.fill(0.0);
                            continue;
                        }
                        let mut acc = 0f32;
                        let mut ch = 0usize;
                        for sample in frame.iter_mut() {
                            let mut bytes = [0u8; 4];
                            let _ = consumer.pop_partial_slice(&mut bytes);
                            *sample = f32::from_le_bytes(bytes);
                            if tap_on {
                                acc += *sample; // spectrum is pre-gain (see windows.rs)
                            }
                            if !unity {
                                *sample = (*sample * volume).clamp(-1.0, 1.0);
                            }
                            if tap_on {
                                // Peak meter: post-gain, i.e. what reaches the device.
                                let level = sample.abs();
                                if ch == 0 {
                                    peak_l = peak_l.max(level);
                                }
                                if ch == 1 || channels == 1 {
                                    peak_r = peak_r.max(level);
                                }
                            }
                            ch += 1;
                        }
                        if tap_on && frame_idx < mono_scratch.len() {
                            mono_scratch[frame_idx] = acc / channels as f32;
                        }
                        real_frames += 1;
                    }
                    played += real_frames as u64;
                    thread_played.store(played, Ordering::Relaxed);
                    if let (true, Some(tap)) = (tap_on, mono_tap.as_mut()) {
                        let _ = tap.mono.push_partial_slice(
                            &mono_scratch[..real_frames.min(mono_scratch.len())],
                        );
                        tap.peaks.record(peak_l, peak_r);
                    }
                },
                move |err| {
                    log::error!("cpal output stream error: {err}");
                    let _ = stream_events.send(RenderEvent::DeviceLost(err.to_string()));
                },
                None,
            );
            let stream = match stream {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("failed to build output stream: {e}")));
                    return;
                }
            };
            if let Err(e) = stream.play() {
                let _ = ready_tx.send(Err(format!("failed to start output stream: {e}")));
                return;
            }
            let _ = ready_tx.send(Ok(()));
            while !thread_stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(20));
            }
        })
        .map_err(|e| format!("spawn cpal render thread: {e}"))?;
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(RenderHandle {
            stop,
            paused,
            played_frames,
            gap_frames,
            volume_bits,
            seek_tx,
            seeks_applied,
            join,
        }),
        Ok(Err(reason)) => {
            let _ = join.join();
            Err(reason)
        }
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            Err("render thread did not open the device in time".into())
        }
    }
}
