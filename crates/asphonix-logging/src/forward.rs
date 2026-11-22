//! `$ASPHONIX_LOG` forwarding: info-and-up events from `asphonix*` targets as `[target] message key=value` lines,
//! handed to a sink the binary supplies (the UI's perf-harness `log()`). Replaces the old `log::Log` impl in
//! `asphonix-ui/src/main.rs`, whose slot the `log` -> `tracing` bridge now takes.
use crate::json::Fields;
use serde_json::Value;
use std::fmt::Write as _;
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_log::NormalizeEvent;
use tracing_subscriber::filter::{FilterFn, LevelFilter, filter_fn};
use tracing_subscriber::layer::{Context, Layer};

/// Receives one finished line (no trailing newline).
pub type LineSink = Box<dyn Fn(&str) + Send + Sync>;

pub(crate) struct Forward {
    pub sink: LineSink,
}

/// Its own static filter, independent of the reloadable file filter, so `ASPHONIX_LOG_LEVEL=warn` or the
/// verbose toggle never change what the perf harness sees.
pub(crate) fn forward_filter() -> FilterFn<fn(&Metadata<'_>) -> bool> {
    let f: fn(&Metadata<'_>) -> bool = |m| *m.level() <= Level::INFO && m.target().starts_with("asphonix");
    filter_fn(f).with_max_level_hint(LevelFilter::INFO)
}

/// `[target] message k=v k2=v2` (string values unquoted).
pub(crate) fn format_line(target: &str, f: &Fields) -> String {
    let mut line = format!("[{target}] {}", f.message.as_deref().unwrap_or(""));
    for (k, v) in &f.map {
        match v {
            Value::String(s) => {
                let _ = write!(line, " {k}={s}");
            }
            other => {
                let _ = write!(line, " {k}={other}");
            }
        }
    }
    line
}

impl<S: Subscriber> Layer<S> for Forward {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let normalized = event.normalized_metadata();
        let meta = normalized.as_ref().unwrap_or_else(|| event.metadata());
        let mut f = Fields::default();
        event.record(&mut f);
        (self.sink)(&format_line(crate::json::target_of(meta), &f));
    }
}
