//! Structured logging for the SONIX binaries.
//!
//! Libraries (`sonix-core`, `sonix-real`, `sonix-ui`, `sonix-mcp`) only emit `tracing` events; the binaries call
//! [`init`] once at startup. Events go to `<dir>/<prefix>.YYYY-MM-DD.jsonl` (one JSON object per line, see
//! `json.rs` for the schema) through a lossy, bounded, non-blocking writer: a slow disk drops lines, it never
//! blocks the UI or the engine. Files rotate daily; at most [`MAX_LOG_FILES`] are kept.
//!
//! Filter: [`DEFAULT_FILTER`], or [`VERBOSE_FILTER`] while the "Verbose logging" setting is on ([`set_verbose`],
//! applied through a reload handle, no restart). `SONIX_LOG_LEVEL` (EnvFilter syntax, e.g.
//! `sonix_core::audio=trace,warn`) is a developer override and wins over the setting while it is set.
//! Crates that still use the `log` facade are bridged.
//!
//! Panics: with `panic = "abort"` the writer thread never flushes after a panic, so the binaries keep their
//! synchronous crash log; the event they emit here is best effort.
mod forward;
mod json;
mod rolling;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[cfg(test)]
mod tests;

pub use forward::LineSink;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::Subscriber;
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{EnvFilter, Registry, reload};

/// Normal operation: our crates at info, everything else at warn.
pub const DEFAULT_FILTER: &str = "sonix=info,warn";
/// "Verbose logging" on.
pub const VERBOSE_FILTER: &str = "sonix=debug,warn";
/// Daily files kept in the log dir (today's included).
pub const MAX_LOG_FILES: usize = 7;
/// Developer override (EnvFilter syntax); wins over the setting.
pub const ENV_FILTER_VAR: &str = "SONIX_LOG_LEVEL";
/// Queue between the emitting threads and the writer thread. tracing-appender's default (128k lines) is a
/// bounded crossbeam channel that initializes every slot up front: several MB of private working set. A burst
/// longer than this drops lines (lossy) instead of blocking.
const BUFFERED_LINES: usize = 1024;

/// What [`init`] sets up.
pub struct LogConfig {
    /// Log directory (created if missing); `None` = no file (events are still filtered and forwarded).
    pub dir: Option<PathBuf>,
    /// File name prefix: `sonix` -> `sonix.2026-11-22.jsonl`.
    pub file_prefix: String,
    /// The binary's version, recorded on the root span.
    pub version: &'static str,
    /// Start with [`VERBOSE_FILTER`] (the stored setting).
    pub verbose: bool,
    /// Optional line sink for info-and-up `sonix*` events (`[target] message k=v`), independent of the filter.
    pub forward: Option<LineSink>,
}

impl LogConfig {
    pub fn new(dir: Option<PathBuf>, file_prefix: &str, version: &'static str) -> Self {
        Self { dir, file_prefix: file_prefix.to_string(), version, verbose: false, forward: None }
    }
}

/// Keeps the writer thread alive; dropping it flushes the queued lines. Hold it until the process ends.
pub struct LogGuard {
    _worker: Option<WorkerGuard>,
    root: tracing::Span,
    file: Option<PathBuf>,
}

impl LogGuard {
    /// The process root span `app{run_id, version, os}`. Spans are per thread: enter it on the main thread
    /// (`let _root = guard.root().clone().entered();`); every line carries `run_id` regardless.
    pub fn root(&self) -> &tracing::Span {
        &self.root
    }

    /// Today's log file, if a file is written.
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }
}

/// Changes the file filter at runtime.
#[derive(Clone)]
pub struct LogControl {
    handle: reload::Handle<EnvFilter, Registry>,
    env: Option<String>,
}

impl LogControl {
    /// [`VERBOSE_FILTER`] on, [`DEFAULT_FILTER`] off; no effect while `SONIX_LOG_LEVEL` overrides.
    pub fn set_verbose(&self, on: bool) {
        self.apply(filter_for(on, self.env.as_deref()));
    }

    /// Any EnvFilter directive string (tests, diagnostics). Invalid directives are ignored.
    pub fn set_filter(&self, directives: &str) {
        self.apply(EnvFilter::new(directives));
    }

    fn apply(&self, f: EnvFilter) {
        let hint = f.max_level_hint();
        if self.handle.reload(f).is_ok() {
            sync_log_max_level(hint);
        }
    }
}

static CONTROL: OnceLock<LogControl> = OnceLock::new();

/// Applies the "Verbose logging" setting to the installed subscriber; a no-op before [`init`] (and in tests).
pub fn set_verbose(on: bool) {
    if let Some(c) = CONTROL.get() {
        c.set_verbose(on);
    }
}

fn env_override() -> Option<String> {
    std::env::var(ENV_FILTER_VAR).ok().filter(|s| !s.trim().is_empty())
}

/// The override wins when it parses; otherwise the setting decides.
fn filter_for(verbose: bool, env: Option<&str>) -> EnvFilter {
    if let Some(f) = env.and_then(|e| EnvFilter::try_new(e).ok()) {
        return f;
    }
    EnvFilter::new(if verbose { VERBOSE_FILTER } else { DEFAULT_FILTER })
}

/// The `log` facade checks its own max level before calling the bridge; keep it at what the filter can let
/// through, so disabled `log::debug!` calls in dependencies (symphonia on the decode path) stay a single compare.
fn sync_log_max_level(hint: Option<tracing_subscriber::filter::LevelFilter>) {
    use tracing_subscriber::filter::LevelFilter as L;
    log::set_max_level(match hint {
        Some(L::OFF) => log::LevelFilter::Off,
        Some(L::ERROR) => log::LevelFilter::Error,
        Some(L::WARN) => log::LevelFilter::Warn,
        Some(L::INFO) => log::LevelFilter::Info,
        Some(L::DEBUG) => log::LevelFilter::Debug,
        _ => log::LevelFilter::Trace,
    });
}

/// The subscriber without installing it: the file layer (reloadable filter) plus the optional forward layer.
pub(crate) fn build(
    out: Box<dyn io::Write + Send>,
    verbose: bool,
    env: Option<String>,
    forward: Option<LineSink>,
    run_id: String,
) -> (impl Subscriber + Send + Sync + for<'a> LookupSpan<'a>, LogControl) {
    let (filter, handle) = reload::Layer::new(filter_for(verbose, env.as_deref()));
    let sub = Registry::default()
        .with(json::JsonLayer::new(out, run_id).with_filter(filter))
        .with(forward.map(|sink| forward::Forward { sink }.with_filter(forward::forward_filter())));
    (sub, LogControl { handle, env })
}

/// A random id per process run, so lines of one run can be grouped across rotated files.
fn new_run_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u32(std::process::id());
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    format!("{:016x}", h.finish())
}

/// Installs the global subscriber. Never panics: an unusable directory or an invalid `SONIX_LOG_LEVEL` falls
/// back (no file / default filter) and is reported as a warning in whatever output remains.
pub fn init(cfg: LogConfig) -> LogGuard {
    let env = env_override();
    let mut problems: Vec<String> = Vec::new();
    if let Some(e) = &env
        && let Err(err) = EnvFilter::try_new(e)
    {
        problems.push(format!("{ENV_FILTER_VAR}={e:?} is not a valid filter ({err}); using the default"));
    }
    let (out, worker, file): (Box<dyn io::Write + Send>, _, _) = match cfg.dir.as_deref().map(|d| open_file(d, &cfg.file_prefix)) {
        Some(Ok((nb, guard, path))) => (Box::new(nb), Some(guard), Some(path)),
        Some(Err(e)) => {
            problems.push(format!("log file unavailable: {e}"));
            (Box::new(io::sink()), None, None)
        }
        None => (Box::new(io::sink()), None, None),
    };
    let run_id = new_run_id();
    let (sub, control) = build(out, cfg.verbose, env.clone(), cfg.forward, run_id.clone());
    let hint = filter_for(cfg.verbose, env.as_deref()).max_level_hint();
    if tracing::subscriber::set_global_default(sub).is_ok() {
        // `log` records from dependencies -> tracing events.
        let _ = tracing_log::LogTracer::init();
        sync_log_max_level(hint);
        let _ = CONTROL.set(control);
    }
    let root = tracing::info_span!(target: "sonix_logging", "app", run_id = %run_id, version = cfg.version, os = std::env::consts::OS);
    tracing::info!(
        target: "sonix_logging",
        parent: &root,
        pid = std::process::id(),
        file = file.as_ref().map(|p| p.display().to_string()),
        filter_override = env.as_deref(),
        verbose = cfg.verbose,
        "logging started"
    );
    for p in problems {
        tracing::warn!(target: "sonix_logging", parent: &root, "{p}");
    }
    LogGuard { _worker: worker, root, file }
}

/// Daily file (pruned on open and on every day change) behind the non-blocking writer thread.
fn open_file(dir: &Path, prefix: &str) -> io::Result<(tracing_appender::non_blocking::NonBlocking, WorkerGuard, PathBuf)> {
    let file = rolling::DailyFile::open(dir.to_path_buf(), prefix, MAX_LOG_FILES, Box::new(ts_date))?;
    let today = file.path();
    let (nb, guard) = NonBlockingBuilder::default().lossy(true).buffered_lines_limit(BUFFERED_LINES).thread_name("sonix-log").finish(file);
    Ok((nb, guard, today))
}

/// UTC date `YYYY-MM-DD` (the appender rotates on UTC days).
fn ts_date() -> String {
    json::ts_now()[..10].to_string()
}

/// `<prefix>.<date>.jsonl`
pub fn file_name(prefix: &str, date: &str) -> String {
    format!("{prefix}.{date}.jsonl")
}

/// The date of a `<prefix>.YYYY-MM-DD.jsonl` file name (exact prefix: `sonix` never matches `sonix-mcp.*`).
fn dated<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
    let date = name.strip_prefix(prefix)?.strip_prefix('.')?.strip_suffix(".jsonl")?;
    let b = date.as_bytes();
    let ok = b.len() == 10 && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() });
    ok.then_some(date)
}

/// Deletes the oldest `<prefix>.YYYY-MM-DD.jsonl` files in `dir` until at most `keep` remain; other files
/// (crash.log, the other binary's logs) are never touched. Returns the deleted paths.
pub fn prune(dir: &Path, prefix: &str, keep: usize) -> io::Result<Vec<PathBuf>> {
    let mut files: Vec<(String, PathBuf)> = std::fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            let date = dated(&name, prefix)?.to_string();
            Some((date, e.path()))
        })
        .collect();
    files.sort();
    let excess = files.len().saturating_sub(keep);
    let mut removed = Vec::new();
    for (_, p) in files.into_iter().take(excess) {
        if std::fs::remove_file(&p).is_ok() {
            removed.push(p);
        }
    }
    Ok(removed)
}
