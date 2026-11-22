//! The file layer: one JSON object per event and line,
//! `{"ts","level","target","message","fields":{..},"spans":[..],"run_id","thread"}`.
//!
//! Written by hand instead of `tracing_subscriber::fmt::json` so the schema is ours (top-level `ts`/`message`,
//! structured `fields`, `run_id` on every line: the root span is per thread, the run id is per process).
use serde_json::{Number, Value};
use std::fmt::{self, Write as _};
use std::io::Write;
use std::sync::Mutex;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber, span};
use tracing_log::NormalizeEvent;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

/// Event or span fields, split into the message and the rest. `log.*` fields (added by the `log` bridge) are
/// dropped: the normalized metadata already carries target, file and line.
#[derive(Default)]
pub(crate) struct Fields {
    pub message: Option<String>,
    /// In recording order (not a `serde_json::Map`: its order depends on a feature another crate may enable).
    pub map: Vec<(String, Value)>,
}

impl Fields {
    fn put(&mut self, field: &Field, v: Value) {
        let name = field.name();
        if name == "message" {
            self.message = Some(match v {
                Value::String(s) => s,
                other => other.to_string(),
            });
        } else if !name.starts_with("log.") {
            set(&mut self.map, name, v);
        }
    }
}

impl Visit for Fields {
    fn record_f64(&mut self, f: &Field, v: f64) {
        self.put(f, Number::from_f64(v).map(Value::Number).unwrap_or_else(|| Value::String(v.to_string())));
    }
    fn record_i64(&mut self, f: &Field, v: i64) {
        self.put(f, Value::from(v));
    }
    fn record_u64(&mut self, f: &Field, v: u64) {
        self.put(f, Value::from(v));
    }
    fn record_bool(&mut self, f: &Field, v: bool) {
        self.put(f, Value::Bool(v));
    }
    fn record_str(&mut self, f: &Field, v: &str) {
        self.put(f, Value::String(v.to_string()));
    }
    fn record_error(&mut self, f: &Field, v: &(dyn std::error::Error + 'static)) {
        self.put(f, Value::String(v.to_string()));
    }
    fn record_debug(&mut self, f: &Field, v: &dyn fmt::Debug) {
        self.put(f, Value::String(format!("{v:?}")));
    }
}

/// A span's recorded fields, kept in the registry's span extensions.
struct SpanFields(Vec<(String, Value)>);

/// Inserts or replaces `k` (a span field recorded again later).
fn set(map: &mut Vec<(String, Value)>, k: &str, v: Value) {
    match map.iter_mut().find(|(n, _)| n == k) {
        Some(slot) => slot.1 = v,
        None => map.push((k.to_string(), v)),
    }
}

/// `{"k":v,...}` in order.
fn object(name: Option<&str>, fields: &[(String, Value)]) -> String {
    let mut s = String::from("{");
    if let Some(n) = name {
        let _ = write!(s, "\"name\":{}", json_str(n));
    }
    for (i, (k, v)) in fields.iter().enumerate() {
        if i > 0 || name.is_some() {
            s.push(',');
        }
        let _ = write!(s, "{}:{v}", json_str(k));
    }
    s.push('}');
    s
}

pub(crate) struct JsonLayer {
    out: Mutex<Box<dyn Write + Send>>,
    run_id: String,
}

impl JsonLayer {
    pub(crate) fn new(out: Box<dyn Write + Send>, run_id: String) -> Self {
        Self { out: Mutex::new(out), run_id }
    }
}

/// `2026-11-22T10:15:30.123Z` (UTC, milliseconds).
pub(crate) fn ts_now() -> String {
    let t = time::OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.millisecond()
    )
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

impl<S> Layer<S> for JsonLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        let mut f = Fields::default();
        attrs.record(&mut f);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanFields(f.map));
        }
    }

    fn on_record(&self, id: &span::Id, values: &span::Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut f = Fields::default();
        values.record(&mut f);
        let mut ext = span.extensions_mut();
        match ext.get_mut::<SpanFields>() {
            Some(SpanFields(m)) => {
                for (k, v) in f.map {
                    set(m, &k, v);
                }
            }
            None => ext.insert(SpanFields(f.map)),
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let normalized = event.normalized_metadata();
        let meta = normalized.as_ref().unwrap_or_else(|| event.metadata());
        let mut f = Fields::default();
        event.record(&mut f);
        let mut line = String::with_capacity(256);
        let _ = write!(
            line,
            "{{\"ts\":\"{}\",\"level\":\"{}\",\"target\":{},\"message\":{},\"fields\":{}",
            ts_now(),
            meta.level(),
            json_str(meta.target()),
            json_str(f.message.as_deref().unwrap_or("")),
            object(None, &f.map)
        );
        if let Some(scope) = ctx.event_scope(event) {
            let mut spans = Vec::new();
            for span in scope.from_root() {
                let ext = span.extensions();
                let fields = ext.get::<SpanFields>().map(|SpanFields(sf)| sf.as_slice()).unwrap_or(&[]);
                spans.push(object(Some(span.name()), fields));
            }
            if !spans.is_empty() {
                let _ = write!(line, ",\"spans\":[{}]", spans.join(","));
            }
        }
        let _ = write!(line, ",\"run_id\":{}", json_str(&self.run_id));
        if let Some(name) = std::thread::current().name() {
            let _ = write!(line, ",\"thread\":{}", json_str(name));
        }
        line.push_str("}\n");
        // One write per line: the non-blocking writer sends each `write` as one message.
        if let Ok(mut out) = self.out.lock() {
            let _ = out.write_all(line.as_bytes());
        }
    }
}
