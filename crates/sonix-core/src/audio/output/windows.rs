//! WASAPI output: exclusive, event-driven mode at the negotiated
//! container/rate (never resampled), falling back to shared mode (the
//! source's own rate/channels as 32-bit float, with WASAPI's
//! `AUTOCONVERTPCM` resampling to the mix format) when exclusive isn't
//! available.
//!
//! `negotiate()` and `spawn_render()` each do their own short-lived (or
//! thread-lifetime, for render) COM/MTA initialization and device open —
//! deliberately never sharing a `Device`/`AudioClient` across threads, which
//! sidesteps any question of whether those COM wrapper types are safe to
//! hand off. `negotiate()`'s result is a plain, `Copy` `NegotiatedFormat`
//! (including the exact WAVEFORMAT variant the driver accepted);
//! `spawn_render()` reopens the device itself from that value and reports
//! the `Initialize` outcome synchronously, so a failed exclusive open
//! (device in use, exclusive disallowed, ...) can fall back to shared mode
//! instead of playing silence.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rtrb::Consumer;
use wasapi::{
    calculate_period_100ns, initialize_mta, DeviceCollection, DeviceEnumerator, Direction,
    SampleType, ShareMode, StreamMode, WasapiError, WaveFormat,
};
use windows::core::PCWSTR;
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW,
};

use super::{DeviceInfo, MonoTap, Negotiation, RenderEvent, RenderHandle};
use crate::audio::format::{Container, NegotiatedFormat, SourceFormat};
use crate::ipc::OutputMode;

type WasapiRes<T> = Result<T, WasapiError>;

// AUDCLNT_E_* HRESULTs (audioclient.h) we explain to the user by name.
const AUDCLNT_E_DEVICE_INVALIDATED: u32 = 0x8889_0004;
const AUDCLNT_E_UNSUPPORTED_FORMAT: u32 = 0x8889_0008;
const AUDCLNT_E_DEVICE_IN_USE: u32 = 0x8889_000A;
const AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED: u32 = 0x8889_000E;
const AUDCLNT_E_ENDPOINT_CREATE_FAILED: u32 = 0x8889_000F;
const AUDCLNT_E_SERVICE_NOT_RUNNING: u32 = 0x8889_0010;
const AUDCLNT_E_BUFFER_SIZE_ERROR: u32 = 0x8889_0016;
const AUDCLNT_E_CPUUSAGE_EXCEEDED: u32 = 0x8889_0017;
const AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED: u32 = 0x8889_0019;
const AUDCLNT_E_INVALID_DEVICE_PERIOD: u32 = 0x8889_0020;
const E_INVALIDARG: u32 = 0x8007_0057;

/// Human-readable name for a WASAPI HRESULT (falls back to hex).
pub fn hresult_name(hr: u32) -> String {
    let name = match hr {
        AUDCLNT_E_DEVICE_INVALIDATED => "AUDCLNT_E_DEVICE_INVALIDATED (device removed/changed)",
        AUDCLNT_E_UNSUPPORTED_FORMAT => "AUDCLNT_E_UNSUPPORTED_FORMAT",
        AUDCLNT_E_DEVICE_IN_USE => "AUDCLNT_E_DEVICE_IN_USE (another app holds the device exclusively)",
        AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED => {
            "AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED (\"Allow applications to take exclusive control\" is off)"
        }
        AUDCLNT_E_ENDPOINT_CREATE_FAILED => "AUDCLNT_E_ENDPOINT_CREATE_FAILED",
        AUDCLNT_E_SERVICE_NOT_RUNNING => "AUDCLNT_E_SERVICE_NOT_RUNNING",
        AUDCLNT_E_BUFFER_SIZE_ERROR => "AUDCLNT_E_BUFFER_SIZE_ERROR",
        AUDCLNT_E_CPUUSAGE_EXCEEDED => "AUDCLNT_E_CPUUSAGE_EXCEEDED",
        AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED => "AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED",
        AUDCLNT_E_INVALID_DEVICE_PERIOD => "AUDCLNT_E_INVALID_DEVICE_PERIOD",
        E_INVALIDARG => "E_INVALIDARG",
        _ => return format!("HRESULT 0x{hr:08X}"),
    };
    name.to_string()
}

fn hresult_of(err: &WasapiError) -> Option<u32> {
    match err {
        WasapiError::Windows(e) => Some(e.code().0 as u32),
        _ => None,
    }
}

fn describe(err: &WasapiError) -> String {
    match hresult_of(err) {
        Some(hr) => hresult_name(hr),
        None => err.to_string(),
    }
}

fn is_device_level_hresult(hr: Option<u32>) -> bool {
    matches!(
        hr,
        Some(
            AUDCLNT_E_DEVICE_IN_USE
                | AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED
                | AUDCLNT_E_DEVICE_INVALIDATED
        )
    )
}

fn is_device_level_refusal(err: &WasapiError) -> bool {
    is_device_level_hresult(hresult_of(err))
}

fn open_device(device_id: Option<&str>) -> WasapiRes<wasapi::Device> {
    let enumerator = DeviceEnumerator::new()?;
    match device_id {
        Some(id) => enumerator.get_device(id),
        None => enumerator.get_default_device(&Direction::Render),
    }
}

fn container_to_waveformat(
    container: Container,
    sample_rate: u32,
    channels: u8,
    mask: Option<u32>,
) -> WaveFormat {
    let (storebits, validbits, sample_type) = match container {
        Container::I16 => (16, 16, SampleType::Int),
        Container::I24In32 => (32, 24, SampleType::Int),
        Container::I24Packed => (24, 24, SampleType::Int),
        Container::I32 => (32, 32, SampleType::Int),
        Container::F32 => (32, 32, SampleType::Float),
    };
    WaveFormat::new(
        storebits,
        validbits,
        &sample_type,
        sample_rate as usize,
        channels as usize,
        mask,
    )
}

/// The exact WAVEFORMAT structure for a negotiated format — including the
/// channel-mask / plain-WAVEFORMATEX quirk the driver accepted, so
/// `Initialize` sees byte-for-byte what `IsFormatSupported` approved.
fn negotiated_waveformat(format: &NegotiatedFormat) -> WaveFormat {
    let wf = container_to_waveformat(
        format.container,
        format.sample_rate,
        format.channels,
        format.channel_mask,
    );
    if format.plain_wfx {
        wf.to_waveformatex().unwrap_or(wf)
    } else {
        wf
    }
}

pub fn list_devices() -> Vec<DeviceInfo> {
    run_on_mta_thread("sonix-wasapi-enum", || {
        list_devices_inner().unwrap_or_default()
    })
    .unwrap_or_default()
}

/// Id of the default render endpoint (the one `open_device(None)` opens):
/// a single `GetDefaultAudioEndpoint` + `GetId`, no enumeration of all
/// endpoints or friendly-name reads. Same id format as `list_devices`.
pub fn default_device_id() -> Option<String> {
    run_on_mta_thread("sonix-wasapi-default", || {
        DeviceEnumerator::new()
            .and_then(|e| e.get_default_device(&Direction::Render))
            .and_then(|d| d.get_id())
            .ok()
    })
    .flatten()
}

/// Runs `f` on a short-lived thread with COM initialized for MTA (so the
/// caller's own COM apartment, if any, is never touched), with a timeout.
fn run_on_mta_thread<T: Send + 'static>(
    name: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _ = initialize_mta();
            let _ = tx.send(f());
        })
        .ok()?;
    rx.recv_timeout(Duration::from_secs(5)).ok()
}

fn list_devices_inner() -> WasapiRes<Vec<DeviceInfo>> {
    let enumerator = DeviceEnumerator::new()?;
    let default_id = enumerator
        .get_default_device(&Direction::Render)
        .and_then(|d| d.get_id())
        .ok();
    let collection: DeviceCollection = enumerator.get_device_collection(&Direction::Render)?;
    let n = collection.get_nbr_devices()?;
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let Ok(dev) = collection.get_device_at_index(i) else {
            continue;
        };
        let (Ok(id), Ok(name)) = (dev.get_id(), dev.get_friendlyname()) else {
            continue;
        };
        let is_default = default_id.as_deref() == Some(id.as_str());
        out.push(DeviceInfo {
            id,
            name,
            is_default,
        });
    }
    Ok(out)
}

/// Asks the driver whether `wf` is supported in exclusive mode, also trying
/// the plain-WAVEFORMATEX form (mono/stereo, non-24-bit only) and the
/// alternative channel masks some drivers insist on. Returns the accepted
/// `(channel_mask, plain_wfx)` or the error of the *direct* query (the most
/// meaningful one, e.g. `AUDCLNT_E_DEVICE_IN_USE`).
fn probe_exclusive(
    client: &wasapi::AudioClient,
    wf: &WaveFormat,
) -> Result<(Option<u32>, bool), WasapiError> {
    let direct = match client.is_supported(wf, &ShareMode::Exclusive) {
        Ok(_) => return Ok((None, false)),
        Err(e) => e,
    };
    // Device-level refusals aren't format problems: no variant will help.
    if is_device_level_refusal(&direct) {
        return Err(direct);
    }
    if wf.get_nchannels() <= 2
        && let Ok(plain) = wf.to_waveformatex()
            && client.is_supported(&plain, &ShareMode::Exclusive).is_ok() {
                return Ok((None, true));
            }
    let default_mask = wf.get_dwchannelmask();
    for mask in wasapi::make_channelmasks(wf.get_nchannels() as usize) {
        if mask == default_mask {
            continue;
        }
        let alt = WaveFormat::new(
            wf.get_bitspersample() as usize,
            wf.get_validbitspersample() as usize,
            &wf.get_subformat().unwrap_or(SampleType::Int),
            wf.get_samplespersec() as usize,
            wf.get_nchannels() as usize,
            Some(mask),
        );
        if client.is_supported(&alt, &ShareMode::Exclusive).is_ok() {
            return Ok((Some(mask), false));
        }
    }
    Err(direct)
}

/// Finds the best exclusive-mode container the device accepts at the
/// source's native rate, trying candidates in `source.candidate_containers()`
/// order (exact bit depth first, then zero-padded wider containers, never a
/// smaller one). Falls back to shared mode if exclusive mode isn't
/// available in any candidate container, or if `requested_mode` is
/// `Shared`; the reason is returned in `Negotiation::exclusive_error`.
pub fn negotiate(
    device_id: Option<String>,
    source: SourceFormat,
    requested_mode: OutputMode,
) -> Negotiation {
    run_on_mta_thread("sonix-wasapi-negotiate", move || {
        negotiate_inner(device_id.as_deref(), source, requested_mode)
    })
    .unwrap_or_else(|| Negotiation {
        format: shared_format(source),
        exclusive_error: Some("device negotiation timed out".into()),
    })
}

/// Shared mode always gets the *source's* rate/channels as float; WASAPI's
/// `AUTOCONVERTPCM` + `SRC_DEFAULT_QUALITY` (set by `initialize_client`)
/// converts to the engine mix format. Declaring the mix format's rate here
/// instead would play the source at the wrong speed.
pub fn shared_format(source: SourceFormat) -> NegotiatedFormat {
    NegotiatedFormat::new(
        source.sample_rate,
        source.channels,
        Container::F32,
        OutputMode::Shared,
    )
}

fn negotiate_inner(
    device_id: Option<&str>,
    source: SourceFormat,
    requested_mode: OutputMode,
) -> Negotiation {
    if requested_mode == OutputMode::Shared {
        return Negotiation {
            format: shared_format(source),
            exclusive_error: None,
        };
    }
    let device = match open_device(device_id) {
        Ok(d) => d,
        Err(e) => {
            return Negotiation {
                format: shared_format(source),
                exclusive_error: Some(format!("could not open output device: {}", describe(&e))),
            }
        }
    };

    let mut last_err: Option<String> = None;
    let mut last_hr: Option<u32> = None;
    for &container in source.candidate_containers() {
        let wf = container_to_waveformat(container, source.sample_rate, source.channels, None);
        let client = match device.get_iaudioclient() {
            Ok(c) => c,
            Err(e) => {
                last_err = Some(describe(&e));
                continue;
            }
        };
        match probe_exclusive(&client, &wf) {
            Ok((channel_mask, plain_wfx)) => {
                return Negotiation {
                    format: NegotiatedFormat {
                        sample_rate: source.sample_rate,
                        channels: source.channels,
                        container,
                        mode: OutputMode::Exclusive,
                        channel_mask,
                        plain_wfx,
                    },
                    exclusive_error: None,
                };
            }
            Err(e) => {
                last_err = Some(describe(&e));
                last_hr = hresult_of(&e);
                if is_device_level_refusal(&e) {
                    break; // no other container will help
                }
            }
        }
    }
    let containers: Vec<&str> = source
        .candidate_containers()
        .iter()
        .map(|c| c.label())
        .collect();
    let reason = format!(
        "exclusive mode rejected at {} Hz/{} ch ({}): {}",
        source.sample_rate,
        source.channels,
        containers.join("/"),
        last_err.unwrap_or_else(|| "unknown error".into())
    );
    log::warn!("WASAPI: {reason}; falling back to shared mode");
    // The UI gets a plain-language version; the HRESULTs stay in the log.
    let device_name = device
        .get_friendlyname()
        .unwrap_or_else(|_| "This output device".into());
    let message = match last_hr {
        Some(AUDCLNT_E_UNSUPPORTED_FORMAT) => unsupported_format_message(
            &device_name,
            source.sample_rate,
            source.bit_depth,
            &exclusive_formats_cached(&device, source.channels),
        ),
        Some(AUDCLNT_E_DEVICE_IN_USE) => format!(
            "{device_name} is held in exclusive mode by another app, so this track plays in shared mode."
        ),
        Some(AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED) => format!(
            "Exclusive mode is turned off for {device_name} in Windows (Sound settings > device \
             properties > \"Allow applications to take exclusive control\"), so this track plays \
             in shared mode."
        ),
        _ => format!("Exclusive mode failed on {device_name}, so this track plays in shared mode. ({reason})"),
    };
    Negotiation {
        format: shared_format(source),
        exclusive_error: Some(message),
    }
}

/// `(rate, bits)` pairs the device accepts in exclusive mode at `channels`,
/// asked once per device per run (IsFormatSupported only, never grabs the
/// device): the answer only feeds an error message, and a track change must
/// not re-run ~24 probes.
fn exclusive_formats_cached(device: &wasapi::Device, channels: u8) -> Vec<(u32, u8)> {
    type Cache = std::sync::Mutex<std::collections::HashMap<(String, u8), Vec<(u32, u8)>>>;
    static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    let key = (device.get_id().unwrap_or_default(), channels);
    let cache = CACHE.get_or_init(Default::default);
    if let Some(found) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return found;
    }
    let mut accepted = Vec::new();
    for &rate in &PROBE_RATES {
        for container in [Container::I16, Container::I24In32, Container::I24Packed, Container::I32] {
            let bits = container.valid_bits();
            if accepted.contains(&(rate, bits)) {
                continue;
            }
            let wf = container_to_waveformat(container, rate, channels, None);
            let Ok(client) = device.get_iaudioclient() else { continue };
            if probe_exclusive(&client, &wf).is_ok() {
                accepted.push((rate, bits));
            }
        }
    }
    if let Ok(mut c) = cache.lock() {
        c.insert(key, accepted.clone());
    }
    accepted
}

fn khz(rate: u32) -> String {
    if rate.is_multiple_of(1000) {
        (rate / 1000).to_string()
    } else {
        format!("{:.1}", rate as f64 / 1000.0)
    }
}

/// "44.1 / 48 kHz at 16 or 24-bit; 96 kHz at 24-bit": rates in order, with
/// neighbouring rates that accept the same bit depths merged.
fn describe_formats(accepted: &[(u32, u8)]) -> String {
    let mut rates: Vec<u32> = accepted.iter().map(|f| f.0).collect();
    rates.sort_unstable();
    rates.dedup();
    let mut groups: Vec<(Vec<u32>, Vec<u8>)> = Vec::new();
    for rate in rates {
        let mut bits: Vec<u8> = accepted.iter().filter(|f| f.0 == rate).map(|f| f.1).collect();
        bits.sort_unstable();
        bits.dedup();
        match groups.last_mut() {
            Some((rs, b)) if *b == bits => rs.push(rate),
            _ => groups.push((vec![rate], bits)),
        }
    }
    groups
        .iter()
        .map(|(rs, bs)| {
            let rates: Vec<String> = rs.iter().map(|&r| khz(r)).collect();
            let bits: Vec<String> = bs.iter().map(|b| b.to_string()).collect();
            format!("{} kHz at {}-bit", rates.join(" / "), bits.join(" or "))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The Settings text for an exclusive refusal because of the track's format.
fn unsupported_format_message(device: &str, rate: u32, bit_depth: Option<u8>, accepted: &[(u32, u8)]) -> String {
    let track = match bit_depth {
        Some(b) => format!("{} kHz / {b}-bit", khz(rate)),
        None => format!("{} kHz", khz(rate)),
    };
    if accepted.is_empty() {
        return format!(
            "{device} accepts none of the common PCM formats in exclusive mode, so this {track} track plays in shared mode."
        );
    }
    let mut msg = format!(
        "{device} only accepts {} in exclusive mode. This track is {track}, so it plays in shared mode.",
        describe_formats(accepted)
    );
    // Right rate, too few bits: say why SONIX doesn't just use it.
    if let Some(b) = bit_depth {
        let same_rate: Vec<u8> = accepted.iter().filter(|f| f.0 == rate).map(|f| f.1).collect();
        if !same_rate.is_empty() && same_rate.iter().all(|&sb| sb < b) {
            msg.push_str(" SONIX never cuts a track down to fewer bits.");
        }
    }
    msg
}

/// Sets the calling thread's MMCSS characteristics to "Pro Audio" (elevated,
/// glitch-resistant scheduling for the render loop). Best-effort: playback
/// continues at normal thread priority if this fails.
fn set_mmcss_pro_audio() -> Option<windows::Win32::Foundation::HANDLE> {
    let name: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
    let mut task_index: u32 = 0;
    // SAFETY: `name` is a NUL-terminated UTF-16 buffer that outlives the call; `task_index` is a valid out-pointer.
    match unsafe { AvSetMmThreadCharacteristicsW(PCWSTR(name.as_ptr()), &mut task_index) } {
        Ok(h) => Some(h),
        Err(e) => {
            log::warn!(
                "AvSetMmThreadCharacteristicsW(\"Pro Audio\") failed ({e}); default priority"
            );
            None
        }
    }
}

/// An initialized (not yet started) WASAPI render stream.
struct OpenStream {
    client: wasapi::AudioClient,
    render: wasapi::AudioRenderClient,
    event: wasapi::Handle,
    buffer_frames: usize,
}

/// Exclusive mode asks for this many default device periods (~30 ms when the
/// default is 10 ms): far fewer wake-ups and more headroom against scheduling
/// hiccups than the default period, still well under the volume/seek latency
/// target. Refused periods fall back to the default one (`period_candidates`).
const EXCLUSIVE_PERIOD_FACTOR: i64 = 3;

/// Periods (100 ns units) to try for an exclusive open, in order: the enlarged
/// one, then the plain default; never below the device minimum, no duplicates.
fn period_candidates(default_hns: i64, min_hns: i64) -> Vec<i64> {
    let mut out = Vec::with_capacity(2);
    for p in [default_hns.saturating_mul(EXCLUSIVE_PERIOD_FACTOR), default_hns] {
        let p = p.max(min_hns).max(1);
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// Whether a failed exclusive `Initialize` is worth retrying with the default
/// period: any failure except a device-level refusal (in use / exclusive not
/// allowed / invalidated), which no period can fix. Drivers report an
/// unsupported enlarged period in many ways (INVALID_DEVICE_PERIOD,
/// BUFFER_SIZE_ERROR, UNSUPPORTED_FORMAT, ENDPOINT_CREATE_FAILED, ...), and
/// giving up there would drop to shared mode where the default period works.
fn retry_with_default_period(hr: Option<u32>) -> bool {
    !is_device_level_hresult(hr)
}

/// One exclusive-mode `Initialize` attempt at (about) `period_hns`, including
/// the `AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED` dance (re-activate the client and
/// initialize again with a period computed from the aligned buffer size the
/// failed call reported).
fn init_exclusive(
    device: &wasapi::Device,
    wf: &WaveFormat,
    sample_rate: u32,
    period_hns: i64,
) -> Result<wasapi::AudioClient, WasapiError> {
    let mut client = device.get_iaudioclient()?;
    let aligned = client.calculate_aligned_period_near(period_hns, Some(128), wf).unwrap_or(period_hns);
    match client.initialize_client(wf, &Direction::Render, &StreamMode::EventsExclusive { period_hns: aligned }) {
        Ok(()) => Ok(client),
        Err(e) if hresult_of(&e) == Some(AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED) => {
            let frames = client.get_buffer_size()?;
            let period = calculate_period_100ns(frames as i64, sample_rate as i64);
            log::info!("WASAPI: buffer not aligned; retrying with {frames} frames ({period} hns)");
            client = device.get_iaudioclient()?;
            client.initialize_client(wf, &Direction::Render, &StreamMode::EventsExclusive { period_hns: period })?;
            Ok(client)
        }
        Err(e) => Err(e),
    }
}

/// Opens + initializes the device for `format`. Exclusive tries a ~30 ms
/// period first and the default period if the device refuses it.
fn open_stream(device_id: Option<&str>, format: &NegotiatedFormat) -> Result<OpenStream, String> {
    let device = open_device(device_id).map_err(|e| format!("open device: {}", describe(&e)))?;
    let wf = negotiated_waveformat(format);
    let client = device.get_iaudioclient().map_err(|e| describe(&e))?;
    let (def_period, min_period) = client.get_device_period().map_err(|e| describe(&e))?;

    let client = match format.mode {
        OutputMode::Exclusive => {
            let candidates = period_candidates(def_period, min_period);
            let mut result = None;
            for (i, period) in candidates.iter().enumerate() {
                match init_exclusive(&device, &wf, format.sample_rate, *period) {
                    Ok(c) => {
                        log::info!("WASAPI exclusive period {} hns (device default {def_period})", period);
                        result = Some(Ok(c));
                        break;
                    }
                    Err(e) if retry_with_default_period(hresult_of(&e)) && i + 1 < candidates.len() => {
                        log::info!("WASAPI: exclusive period {period} hns failed ({}); trying the default period", describe(&e));
                    }
                    Err(e) => {
                        result = Some(Err(describe(&e)));
                        break;
                    }
                }
            }
            result.unwrap_or_else(|| Err("no exclusive period candidates".to_string()))?
        }
        OutputMode::Shared => {
            let mut client = client;
            client
                .initialize_client(
                    &wf,
                    &Direction::Render,
                    &StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: def_period },
                )
                .map_err(|e| describe(&e))?;
            client
        }
    };

    let event = client.set_get_eventhandle().map_err(|e| describe(&e))?;
    let render = client.get_audiorenderclient().map_err(|e| describe(&e))?;
    let buffer_frames = client.get_buffer_size().map_err(|e| describe(&e))? as usize;
    Ok(OpenStream {
        client,
        render,
        event,
        buffer_frames,
    })
}

/// Spawns the render thread and waits for it to open the device. Returns
/// `Err(reason)` if `Initialize` fails (e.g. the device is held exclusively
/// by another app) so the caller can fall back to shared mode.
pub fn spawn_render(
    device_id: Option<String>,
    format: NegotiatedFormat,
    consumer: Consumer<u8>,
    mono_tap: Option<MonoTap>,
    events: Sender<RenderEvent>,
    start_paused: bool,
    initial_volume: f32,
) -> Result<RenderHandle, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(start_paused));
    let played_frames = Arc::new(AtomicU64::new(0));
    let gap_frames = Arc::new(AtomicU64::new(0));
    let volume_bits = Arc::new(AtomicU32::new(initial_volume.to_bits()));
    let (seek_tx, seek_rx) = std::sync::mpsc::channel::<Consumer<u8>>();
    let seeks_applied = Arc::new(AtomicU64::new(0));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let ctx = RenderCtx {
        stop: stop.clone(),
        paused: paused.clone(),
        played_frames: played_frames.clone(),
        gap_frames: gap_frames.clone(),
        volume_bits: volume_bits.clone(),
        seeks_applied: seeks_applied.clone(),
        events,
    };
    let join = std::thread::Builder::new()
        .name("sonix-render".into())
        .spawn(move || render_thread(device_id, format, consumer, mono_tap, ctx, seek_rx, ready_tx))
        .map_err(|e| format!("spawn render thread: {e}"))?;
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

struct RenderCtx {
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    played_frames: Arc<AtomicU64>,
    gap_frames: Arc<AtomicU64>,
    volume_bits: Arc<AtomicU32>,
    seeks_applied: Arc<AtomicU64>,
    events: Sender<RenderEvent>,
}

fn render_thread(
    device_id: Option<String>,
    format: NegotiatedFormat,
    mut consumer: Consumer<u8>,
    mut mono_tap: Option<MonoTap>,
    ctx: RenderCtx,
    seek_rx: std::sync::mpsc::Receiver<Consumer<u8>>,
    ready: Sender<Result<(), String>>,
) {
    let _ = initialize_mta();
    let stream = match open_stream(device_id.as_deref(), &format) {
        Ok(s) => s,
        Err(reason) => {
            log::warn!("WASAPI {:?} open failed: {reason}", format.mode);
            let _ = ready.send(Err(reason));
            return;
        }
    };
    log::info!(
        "WASAPI {:?} stream open: {} Hz, {} ch, {}, buffer {} frames",
        format.mode,
        format.sample_rate,
        format.channels,
        format.container.label(),
        stream.buffer_frames
    );
    let mmcss = set_mmcss_pro_audio();
    // Pre-allocated once: the render loop below never allocates again.
    let bytes_per_frame = format.bytes_per_frame();
    let mut scratch = vec![0u8; stream.buffer_frames * bytes_per_frame];
    let mut mono_scratch: Vec<f32> = vec![0.0; stream.buffer_frames];
    let _ = ready.send(Ok(()));

    let result = render_loop(
        &stream,
        format,
        &mut consumer,
        &mut mono_tap,
        &ctx,
        &seek_rx,
        &mut scratch,
        &mut mono_scratch,
    );
    let _ = stream.client.stop_stream();
    if let Some(h) = mmcss {
        // SAFETY: `h` is the handle returned by AvSetMmThreadCharacteristicsW on this same thread and is reverted exactly once.
        let _ = unsafe { AvRevertMmThreadCharacteristics(h) };
    }
    if let Err(e) = result {
        log::warn!("WASAPI render thread exiting: {e:?}");
        let _ = ctx.events.send(e);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_loop(
    stream: &OpenStream,
    format: NegotiatedFormat,
    consumer: &mut Consumer<u8>,
    mono_tap: &mut Option<MonoTap>,
    ctx: &RenderCtx,
    seek_rx: &std::sync::mpsc::Receiver<Consumer<u8>>,
    scratch: &mut [u8],
    mono_scratch: &mut [f32],
) -> Result<(), RenderEvent> {
    let lost = |e: WasapiError| RenderEvent::DeviceLost(describe(&e));
    let mut tracker = PlayedTracker::default();
    let mut gaps = GapTracker::default();
    let mut grace = PauseGrace::default();

    let prefill = stream.client.get_available_space_in_frames().map_err(lost)? as usize;
    fill_output(stream, consumer, mono_tap, prefill, format, ctx, &mut tracker, &mut gaps, scratch, mono_scratch)
        .map_err(lost)?;
    stream.client.start_stream().map_err(lost)?;

    while !ctx.stop.load(Ordering::Relaxed) {
        if stream.event.wait_for_event(1000).is_err() {
            return Err(RenderEvent::DeviceLost("no buffer event from the device for 1 s".into()));
        }
        if ctx.stop.load(Ordering::Relaxed) {
            break;
        }
        // A seek landed (`RenderHandle::seek_tx`, sent by `engine::seek` once
        // it's created a fresh, already-seeked ring): flush the device's
        // internal buffer and resume from the new ring on this *same* open
        // stream — `stop_stream`/`reset_stream`/`start_stream` only flush
        // and restart, they never touch format negotiation, so this avoids
        // the ~25-70ms (and possible click) of a full device reopen on every
        // seek. `position_sec`'s frame-counting restarts from 0 to match
        // `engine::Playback::current.start_frame` being reset to 0 too.
        if let Ok(new_consumer) = seek_rx.try_recv() {
            *consumer = new_consumer;
            stream.client.stop_stream().map_err(lost)?;
            stream.client.reset_stream().map_err(lost)?;
            tracker = PlayedTracker::default();
            gaps = GapTracker::default();
            ctx.played_frames.store(0, Ordering::Relaxed);
            ctx.seeks_applied.fetch_add(1, Ordering::Release);
            let prefill = stream.client.get_available_space_in_frames().map_err(lost)? as usize;
            fill_output(stream, consumer, mono_tap, prefill, format, ctx, &mut tracker, &mut gaps, scratch, mono_scratch)
                .map_err(lost)?;
            stream.client.start_stream().map_err(lost)?;
            grace.reset();
            continue;
        }
        if grace.update(ctx.paused.load(Ordering::Relaxed), Instant::now()) {
            // Paused long enough: stop the device stream and sleep until
            // resume / a ring swap / shutdown (no per-period wake-ups).
            park_stopped(stream, format, consumer, mono_tap, ctx, seek_rx, &mut tracker, &mut gaps, scratch, mono_scratch)?;
            grace.reset();
            continue;
        }
        let avail = stream.client.get_available_space_in_frames().map_err(lost)? as usize;
        fill_output(stream, consumer, mono_tap, avail, format, ctx, &mut tracker, &mut gaps, scratch, mono_scratch)
            .map_err(lost)?;
        let queued = stream.client.get_current_padding().map(u64::from).unwrap_or(0);
        ctx.played_frames.store(tracker.real_played(queued), Ordering::Relaxed);
    }
    Ok(())
}

/// A pause shorter than this keeps writing silence every period (quick
/// pause/resume taps stay on the plain path); a longer one stops the stream.
const PAUSE_STOP_GRACE: Duration = Duration::from_secs(2);
/// How often the stopped render thread wakes by itself, only to notice a
/// removed device (every real event — resume, ring swap, stop — unparks it).
const STOPPED_POLL: Duration = Duration::from_millis(500);

/// Tracks how long the pause flag has been set without interruption.
#[derive(Default)]
struct PauseGrace {
    since: Option<Instant>,
}

impl PauseGrace {
    /// Feeds the current flag; true once it has been set for
    /// [`PAUSE_STOP_GRACE`] (and keeps saying so until [`Self::reset`]).
    fn update(&mut self, paused: bool, now: Instant) -> bool {
        if !paused {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(now);
        now.saturating_duration_since(since) >= PAUSE_STOP_GRACE
    }

    fn reset(&mut self) {
        self.since = None;
    }
}

/// The long-pause state: stops the device stream (it stays initialized, so
/// exclusive ownership and the negotiated format are kept) and blocks on
/// `thread::park_timeout` — `RenderHandle::set_paused`/`send_ring`/
/// `stop_and_join` unpark it — until the pause flag clears or the engine
/// stops it. A ring swap arriving meanwhile is applied (flush, accounting
/// reset, `seeks_applied`) without restarting the stream. On resume the
/// device buffer is flushed and prefilled from the ring like after a seek.
/// `played_frames` needs no update: after the grace period every real frame
/// has long been played, and `reset_stream` zeroes the queued padding the
/// tracker subtracts.
#[allow(clippy::too_many_arguments)]
fn park_stopped(
    stream: &OpenStream,
    format: NegotiatedFormat,
    consumer: &mut Consumer<u8>,
    mono_tap: &mut Option<MonoTap>,
    ctx: &RenderCtx,
    seek_rx: &std::sync::mpsc::Receiver<Consumer<u8>>,
    tracker: &mut PlayedTracker,
    gaps: &mut GapTracker,
    scratch: &mut [u8],
    mono_scratch: &mut [f32],
) -> Result<(), RenderEvent> {
    let lost = |e: WasapiError| RenderEvent::DeviceLost(describe(&e));
    stream.client.stop_stream().map_err(lost)?;
    log::debug!("WASAPI: paused > {PAUSE_STOP_GRACE:?}, device stream stopped");
    loop {
        if ctx.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        if let Ok(new_consumer) = seek_rx.try_recv() {
            *consumer = new_consumer;
            stream.client.reset_stream().map_err(lost)?;
            *tracker = PlayedTracker::default();
            *gaps = GapTracker::default();
            ctx.played_frames.store(0, Ordering::Relaxed);
            ctx.seeks_applied.fetch_add(1, Ordering::Release);
            continue; // re-check stop/pause/further swaps before sleeping
        }
        if !ctx.paused.load(Ordering::Relaxed) {
            break;
        }
        std::thread::park_timeout(STOPPED_POLL);
        // A removed device raises no buffer event while stopped: ask it.
        stream.client.get_current_padding().map_err(lost)?;
    }
    stream.client.reset_stream().map_err(lost)?;
    let prefill = stream.client.get_available_space_in_frames().map_err(lost)? as usize;
    fill_output(stream, consumer, mono_tap, prefill, format, ctx, tracker, gaps, scratch, mono_scratch).map_err(lost)?;
    stream.client.start_stream().map_err(lost)?;
    log::debug!("WASAPI: resumed, device stream restarted");
    Ok(())
}

/// Underrun accounting: silence written *between* real frames (not while
/// paused, not before the first or after the last real frame) is an audible
/// gap/dropout, reported via `RenderHandle::gap_frames`. Reset on every
/// in-place seek (a fresh "stream" of frames for accounting purposes).
#[derive(Default)]
struct GapTracker {
    silence_run: u64,
    had_real: bool,
}

/// Pops/writes one device buffer's worth of frames (or silence while
/// paused), updating `tracker` (played-frames accounting) and `gaps`
/// (underrun diagnostics). Pulled out of `render_loop` as a plain function
/// (rather than a closure capturing `consumer`/`mono_tap` by unique
/// reference) specifically so the seek branch above can reassign `*consumer`
/// to a freshly-seeked ring between calls.
#[allow(clippy::too_many_arguments)]
fn fill_output(
    stream: &OpenStream,
    consumer: &mut Consumer<u8>,
    mono_tap: &mut Option<MonoTap>,
    frames: usize,
    format: NegotiatedFormat,
    ctx: &RenderCtx,
    tracker: &mut PlayedTracker,
    gaps: &mut GapTracker,
    scratch: &mut [u8],
    mono_scratch: &mut [f32],
) -> WasapiRes<()> {
    let bytes_per_frame = format.bytes_per_frame();
    let paused = ctx.paused.load(Ordering::Relaxed);
    let real = if paused {
        // Keep the stream alive with silence, without touching the ring —
        // decode's producer side simply blocks on a full ring.
        write_silence(&stream.render, frames, bytes_per_frame, scratch)?;
        0
    } else {
        let volume = f32::from_bits(ctx.volume_bits.load(Ordering::Relaxed));
        write_frames(&stream.render, consumer, mono_tap, frames, format, volume, scratch, mono_scratch)?
    };
    tracker.record(frames as u64, real);
    if real > 0 {
        if gaps.had_real && gaps.silence_run > 0 {
            ctx.gap_frames.fetch_add(gaps.silence_run, Ordering::Relaxed);
        }
        gaps.silence_run = 0;
        gaps.had_real = true;
    }
    if !paused {
        gaps.silence_run += frames as u64 - real;
    }
    Ok(())
}

/// Maps "frames the device has played" (written minus still queued, which
/// includes any silence written on underrun/pause) to "*real* ring frames
/// played", by remembering the last few writes. Each write is real frames
/// first, then silence (see `fill_from_ring`). Fixed-size, no allocation.
#[derive(Default)]
pub(crate) struct PlayedTracker {
    /// (total frames written before this write, real frames written before
    /// this write, real frames in this write), newest at `head - 1`.
    segments: [(u64, u64, u64); 16],
    head: usize,
    len: usize,
    total_written: u64,
    real_written: u64,
}

impl PlayedTracker {
    pub(crate) fn record(&mut self, frames: u64, real: u64) {
        if frames == 0 {
            return;
        }
        self.segments[self.head] = (self.total_written, self.real_written, real);
        self.head = (self.head + 1) % self.segments.len();
        self.len = (self.len + 1).min(self.segments.len());
        self.total_written += frames;
        self.real_written += real;
    }

    pub(crate) fn real_played(&self, queued: u64) -> u64 {
        let played_total = self.total_written.saturating_sub(queued);
        let n = self.segments.len();
        let mut oldest_real = self.real_written;
        for i in 1..=self.len {
            let (total_start, real_start, real) = self.segments[(self.head + n - i) % n];
            if total_start <= played_total {
                return real_start + (played_total - total_start).min(real);
            }
            oldest_real = real_start;
        }
        oldest_real
    }
}

/// Pops up to `frames` frames from the ring into `scratch` (padding any
/// shortfall with silence so an underrun is quiet, not garbage), writes them
/// to the device verbatim, and — if a spectrum tap is attached — downmixes
/// the same frames to mono for the analyzer thread. Returns the number of
/// real (ring) frames written. No allocation: the scratch buffers are sized
/// once by the caller and reused.
#[allow(clippy::too_many_arguments)]
fn write_frames(
    render_client: &wasapi::AudioRenderClient,
    consumer: &mut Consumer<u8>,
    mono_tap: &mut Option<MonoTap>,
    frames: usize,
    format: NegotiatedFormat,
    volume: f32,
    scratch: &mut [u8],
    mono_scratch: &mut [f32],
) -> WasapiRes<u64> {
    if frames == 0 {
        return Ok(0);
    }
    let bytes_per_frame = format.bytes_per_frame();
    let buf = &mut scratch[..frames * bytes_per_frame];
    let popped_len = fill_from_ring(consumer, buf, bytes_per_frame);
    let valid_frames = popped_len / bytes_per_frame;

    // Spectrum tap before the gain stage: the analyzer shows the music, not
    // the volume knob (with the dB taper a quiet listening level would
    // otherwise flatten the display). The peak meter instead reports what
    // reaches the device: pre-gain peak x gain (linear, clamped at full scale
    // like the gain stage itself).
    if let Some(tap) = mono_tap.as_mut().filter(|t| t.active.load(Ordering::Relaxed)) {
        let mono = &mut mono_scratch[..valid_frames];
        let (pl, pr) = downmix_to_mono(&buf[..popped_len], format, mono);
        let _ = tap.mono.push_partial_slice(mono);
        tap.peaks.record((pl * volume).min(1.0), (pr * volume).min(1.0));
    }

    // Unity volume (the common case) never touches a byte here, so the
    // output stays bit-perfect. Applied to the whole popped region (the padded silence
    // tail, if any, is already zero and unaffected by a multiply).
    if (volume - 1.0).abs() > f32::EPSILON {
        super::apply_gain_in_place(&mut buf[..popped_len], format.container, volume);
    }

    render_client.write_to_device(frames, buf, None)?;
    Ok(valid_frames as u64)
}

/// Copies as many *whole frames* as the ring holds (up to `buf.len()`) into
/// `buf` verbatim — this is the bit-perfect hand-off — and zero-fills the
/// rest (underrun: silence, not garbage). Returns the bytes taken from the
/// ring. Never takes a partial frame, so channel alignment can't slip even
/// if the decoder is mid-push.
pub(crate) fn fill_from_ring(
    consumer: &mut Consumer<u8>,
    buf: &mut [u8],
    bytes_per_frame: usize,
) -> usize {
    let available = consumer.slots();
    let take = (available.min(buf.len()) / bytes_per_frame) * bytes_per_frame;
    let (head, tail) = buf.split_at_mut(take);
    let (popped, _) = consumer.pop_partial_slice(head);
    debug_assert_eq!(popped.len(), take);
    tail.fill(0);
    take
}

/// Writes `frames` of silence without touching the ring buffer at all.
fn write_silence(
    render_client: &wasapi::AudioRenderClient,
    frames: usize,
    bytes_per_frame: usize,
    scratch: &mut [u8],
) -> WasapiRes<()> {
    if frames == 0 {
        return Ok(());
    }
    let buf = &mut scratch[..frames * bytes_per_frame];
    buf.fill(0);
    render_client.write_to_device(frames, buf, None)
}

/// Mono downmix into `out`, returning the (left, right) linear peaks of the
/// same frames (channel 0 and 1; a mono source reports the same value twice).
fn downmix_to_mono(bytes: &[u8], format: NegotiatedFormat, out: &mut [f32]) -> (f32, f32) {
    let channels = format.channels as usize;
    let bps = format.container.bytes_per_sample();
    let (mut peak_l, mut peak_r) = (0f32, 0f32);
    for (frame, out_sample) in bytes
        .chunks_exact(format.bytes_per_frame())
        .zip(out.iter_mut())
    {
        let mut sum = 0f32;
        for ch in 0..channels {
            let v = sample_to_f32(&frame[ch * bps..], format.container);
            sum += v;
            if ch == 0 {
                peak_l = peak_l.max(v.abs());
            }
            if ch == 1 || channels == 1 {
                peak_r = peak_r.max(v.abs());
            }
        }
        *out_sample = sum / channels as f32;
    }
    (peak_l, peak_r)
}

fn sample_to_f32(bytes: &[u8], container: Container) -> f32 {
    match container {
        Container::I16 => i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 32_768.0,
        Container::I24Packed => {
            let v = i32::from_le_bytes([0, bytes[0], bytes[1], bytes[2]]);
            (v as f64 / 2_147_483_648.0) as f32
        }
        Container::I24In32 | Container::I32 => {
            let v = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            (v as f64 / 2_147_483_648.0) as f32
        }
        Container::F32 => f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
    }
}

// ---------------------------------------------------------------------------
// Diagnostics (examples/probe_formats.rs)
// ---------------------------------------------------------------------------

pub struct ProbeRow {
    pub rate: u32,
    pub container: &'static str,
    pub result: String,
}

pub struct DeviceProbe {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub mix_format: String,
    pub period_default: i64,
    pub period_min: i64,
    pub rows: Vec<ProbeRow>,
}

pub const PROBE_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];
pub const PROBE_CONTAINERS: [Container; 5] = [
    Container::I16,
    Container::I24In32,
    Container::I24Packed,
    Container::I32,
    Container::F32,
];

/// Queries every active render device's exclusive-mode support for
/// `PROBE_RATES` x `PROBE_CONTAINERS` (stereo). With `do_init`, each
/// accepted format is also really opened (`Initialize`, incl. the
/// aligned-period retry) and closed again, which is what surfaces
/// device-in-use / exclusive-not-allowed errors.
pub fn probe_all_devices(do_init: bool) -> Vec<DeviceProbe> {
    let _ = initialize_mta();
    let Ok(devices) = list_devices_inner() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for d in devices {
        let Ok(device) = open_device(Some(&d.id)) else {
            continue;
        };
        let Ok(client) = device.get_iaudioclient() else {
            continue;
        };
        let mix_format = client
            .get_mixformat()
            .map(|m| {
                format!(
                    "{} Hz, {} ch, {}-in-{} bit {:?}, mask 0x{:X}",
                    m.get_samplespersec(),
                    m.get_nchannels(),
                    m.get_validbitspersample(),
                    m.get_bitspersample(),
                    m.get_subformat().ok(),
                    m.get_dwchannelmask()
                )
            })
            .unwrap_or_else(|e| describe(&e));
        let (period_default, period_min) = client.get_device_period().unwrap_or((0, 0));
        let mut rows = Vec::new();
        for rate in PROBE_RATES {
            for container in PROBE_CONTAINERS {
                let wf = container_to_waveformat(container, rate, 2, None);
                let result = match device.get_iaudioclient().map(|c| probe_exclusive(&c, &wf)) {
                    Ok(Ok((mask, plain))) => {
                        let mut s = "OK".to_string();
                        if plain {
                            s.push_str(" (as WAVEFORMATEX)");
                        }
                        if let Some(m) = mask {
                            s.push_str(&format!(" (mask 0x{m:X})"));
                        }
                        if do_init {
                            let mut f =
                                NegotiatedFormat::new(rate, 2, container, OutputMode::Exclusive);
                            f.channel_mask = mask;
                            f.plain_wfx = plain;
                            match open_stream(Some(&d.id), &f) {
                                Ok(st) => s.push_str(&format!(
                                    " | INIT OK, buffer {} frames",
                                    st.buffer_frames
                                )),
                                Err(e) => s.push_str(&format!(" | INIT FAILED: {e}")),
                            }
                        }
                        s
                    }
                    Ok(Err(e)) => format!("rejected: {}", describe(&e)),
                    Err(e) => format!("no client: {}", describe(&e)),
                };
                rows.push(ProbeRow {
                    rate,
                    container: container.label(),
                    result,
                });
            }
        }
        out.push(DeviceProbe {
            id: d.id,
            name: d.name,
            is_default: d.is_default,
            mix_format,
            period_default,
            period_min,
            rows,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_format_message_names_what_the_device_takes() {
        // A USB adapter that only takes 48 kHz / 16-bit.
        let beyer = [(48_000, 16)];
        assert_eq!(
            unsupported_format_message("Speakers (beyerdynamic LL Adapter)", 44_100, Some(16), &beyer),
            "Speakers (beyerdynamic LL Adapter) only accepts 48 kHz at 16-bit in exclusive mode. \
             This track is 44.1 kHz / 16-bit, so it plays in shared mode."
        );
        // Right rate, too many bits: explain the refusal to truncate.
        assert!(unsupported_format_message("X", 48_000, Some(24), &beyer)
            .ends_with("SONIX never cuts a track down to fewer bits."));
        // Lossy source (no fixed bit depth).
        assert!(unsupported_format_message("X", 44_100, None, &beyer).contains("This track is 44.1 kHz,"));
        assert_eq!(
            unsupported_format_message("X", 44_100, Some(16), &[]),
            "X accepts none of the common PCM formats in exclusive mode, so this 44.1 kHz / 16-bit track plays in shared mode."
        );
    }

    #[test]
    fn describe_formats_merges_neighbouring_rates_with_the_same_depths() {
        // Realtek Digital Output on the dev machine.
        let realtek = [
            (44_100, 16), (44_100, 24), (48_000, 16), (48_000, 24), (88_200, 16), (88_200, 24),
            (96_000, 24), (96_000, 16), (192_000, 16), (192_000, 24),
        ];
        assert_eq!(describe_formats(&realtek), "44.1 / 48 / 88.2 / 96 / 192 kHz at 16 or 24-bit");
        assert_eq!(
            describe_formats(&[(44_100, 16), (48_000, 16), (48_000, 24), (176_400, 32)]),
            "44.1 kHz at 16-bit; 48 kHz at 16 or 24-bit; 176.4 kHz at 32-bit"
        );
    }

    #[test]
    fn pause_grace_stops_only_after_an_uninterrupted_two_seconds() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut g = PauseGrace::default();
        assert!(!g.update(false, t0));
        assert!(!g.update(true, t0), "first paused sample starts the clock");
        assert!(!g.update(true, t0 + ms(1999)));
        assert!(g.update(true, t0 + ms(2000)));
        assert!(g.update(true, t0 + ms(5000)), "stays true until reset");
        // A resume in between restarts the grace period.
        let mut g = PauseGrace::default();
        assert!(!g.update(true, t0));
        assert!(!g.update(false, t0 + ms(1500)));
        assert!(!g.update(true, t0 + ms(1600)));
        assert!(!g.update(true, t0 + ms(3500)), "only 1.9 s since the new pause");
        assert!(g.update(true, t0 + ms(3600)));
        g.reset();
        assert!(!g.update(true, t0 + ms(9000)));
    }

    #[test]
    fn period_candidates_triple_the_default_then_fall_back_within_the_minimum() {
        assert_eq!(period_candidates(100_000, 30_000), vec![300_000, 100_000]);
        // Minimum above the default (odd device): both clamp up, deduplicated.
        assert_eq!(period_candidates(100_000, 400_000), vec![400_000]);
        assert_eq!(period_candidates(100_000, 150_000), vec![300_000, 150_000]);
    }

    #[test]
    fn any_non_device_level_error_retries_the_default_period() {
        for hr in [
            AUDCLNT_E_INVALID_DEVICE_PERIOD,
            AUDCLNT_E_BUFFER_SIZE_ERROR,
            AUDCLNT_E_UNSUPPORTED_FORMAT,
            AUDCLNT_E_ENDPOINT_CREATE_FAILED,
            AUDCLNT_E_CPUUSAGE_EXCEEDED,
            E_INVALIDARG,
            0x8007_000E, // E_OUTOFMEMORY
        ] {
            assert!(retry_with_default_period(Some(hr)), "{hr:#x}");
        }
        assert!(retry_with_default_period(None), "non-HRESULT errors too");
        for hr in [AUDCLNT_E_DEVICE_IN_USE, AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED, AUDCLNT_E_DEVICE_INVALIDATED] {
            assert!(!retry_with_default_period(Some(hr)), "{hr:#x}");
        }
    }

    fn fmt(container: Container, channels: u8) -> NegotiatedFormat {
        NegotiatedFormat {
            sample_rate: 48_000,
            channels,
            container,
            mode: OutputMode::Exclusive,
            channel_mask: None,
            plain_wfx: false,
        }
    }

    /// Interleaved little-endian bytes for `samples` (-1.0..1.0) in `container`.
    fn pack(samples: &[f32], container: Container) -> Vec<u8> {
        let mut out = Vec::new();
        for &s in samples {
            match container {
                Container::I16 => out.extend_from_slice(&((s * 32_767.0).round() as i16).to_le_bytes()),
                Container::I24Packed => out.extend_from_slice(&((s * 8_388_607.0).round() as i32).to_le_bytes()[..3]),
                Container::I24In32 => out.extend_from_slice(&(((s * 8_388_607.0).round() as i32) << 8).to_le_bytes()),
                Container::I32 => out.extend_from_slice(&((s as f64 * 2_147_483_647.0).round() as i32).to_le_bytes()),
                Container::F32 => out.extend_from_slice(&s.to_le_bytes()),
            }
        }
        out
    }

    #[test]
    fn peak_meter_reads_front_left_right_in_every_container() {
        let containers = [Container::I16, Container::I24Packed, Container::I24In32, Container::I32, Container::F32];
        for c in containers {
            // Stereo: L peaks at -0.5 (the negative half counts), R at 0.25.
            let bytes = pack(&[0.1, 0.0, -0.5, 0.25, 0.3, -0.2], c);
            let mut mono = [0f32; 3];
            let (l, r) = downmix_to_mono(&bytes, fmt(c, 2), &mut mono);
            assert!((l - 0.5).abs() < 1e-3 && (r - 0.25).abs() < 1e-3, "{c:?}: {l} {r}");
            assert!((mono[1] + 0.125).abs() < 1e-3, "{c:?}: the FFT downmix is unchanged");

            // Mono: the one channel feeds both meters.
            let bytes = pack(&[0.2, -0.7], c);
            let (l, r) = downmix_to_mono(&bytes, fmt(c, 1), &mut mono);
            assert!((l - 0.7).abs() < 1e-3 && l == r, "{c:?} mono: {l} {r}");

            // 5.1: front L/R only; a loud centre/LFE does not light them.
            let bytes = pack(&[0.1, 0.2, 0.9, 0.9, 0.9, 0.9], c);
            let (l, r) = downmix_to_mono(&bytes, fmt(c, 6), &mut mono);
            assert!((l - 0.1).abs() < 1e-3 && (r - 0.2).abs() < 1e-3, "{c:?} 5.1: {l} {r}");
        }
        // Full-scale negative 16-bit is exactly 0 dBFS, not above it.
        let (l, _) = downmix_to_mono(&i16::MIN.to_le_bytes(), fmt(Container::I16, 1), &mut [0f32; 1]);
        assert_eq!(l, 1.0);
    }

    #[test]
    fn played_counter_ignores_trailing_silence() {
        // 480-frame device buffer; 1000 real frames then the ring runs dry.
        let mut t = PlayedTracker::default();
        t.record(480, 480); // prefill
        t.record(480, 480);
        t.record(480, 40); // underrun: 40 real + 440 silence
        t.record(480, 0); // pure silence
        // device has played everything but the last write
        assert_eq!(t.real_played(480), 1000);
        // ...and partway into the 40-real/440-silence write
        assert_eq!(t.real_played(480 + 460), 980);
        // nothing played yet beyond the first write
        assert_eq!(t.real_played(1920 - 100), 100);
        // pause (silence) then resume: real frames resume counting
        t.record(480, 480);
        assert_eq!(t.real_played(0), 1480);
        assert_eq!(t.real_played(80), 1400);
    }

    #[test]
    fn ring_bytes_reach_the_device_buffer_verbatim() {
        // The render hand-off must be a pure byte copy: what decode packed
        // is exactly what lands in the device buffer, whole frames only,
        // with silence (zeros) only for an underrun shortfall.
        let (mut prod, mut cons) = rtrb::RingBuffer::<u8>::new(64);
        let data: Vec<u8> = (0..30u8)
            .map(|i| i.wrapping_mul(37).wrapping_add(1))
            .collect();
        let _ = prod.push_partial_slice(&data);
        let mut buf = [0xAAu8; 48];
        let n = fill_from_ring(&mut cons, &mut buf, 4); // 4-byte frames: 7 whole frames
        assert_eq!(n, 28);
        assert_eq!(&buf[..28], &data[..28]);
        assert!(buf[28..].iter().all(|&b| b == 0));
        // the partial frame stays in the ring for next time
        assert_eq!(cons.slots(), 2);
    }

    #[test]
    fn hresult_names_are_explained() {
        assert!(hresult_name(AUDCLNT_E_DEVICE_IN_USE).contains("DEVICE_IN_USE"));
        assert!(hresult_name(AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED).contains("exclusive control"));
        assert_eq!(hresult_name(0x8000_4005), "HRESULT 0x80004005");
    }

    #[test]
    fn packed_24_downmix_reads_sign_correctly() {
        let fmt = NegotiatedFormat::new(48_000, 2, Container::I24Packed, OutputMode::Exclusive);
        // L = -1 LSB (0xFFFFFF), R = +0x400000 (half scale)
        let bytes = [0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x40];
        let mut out = [0f32; 1];
        downmix_to_mono(&bytes, fmt, &mut out);
        assert!((out[0] - 0.25).abs() < 1e-3, "{}", out[0]);
    }
}
