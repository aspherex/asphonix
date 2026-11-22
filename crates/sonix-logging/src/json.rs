//! The file layer: one JSON object per event and line,
//! `{"ts","level","target","message","fields":{..},"spans":[..],"run_id","thread"}`.
//!
//! Written by hand instead of `tracing_subscriber::fmt::json` so the schema is ours (top-level `ts`/`message`,
//! structured `fields`, `run_id` on every line: the root span is per thread, the run id is per process).
use serde_json::{Map, Number, Value};
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
    pub map: Map<String, Value>,
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
            self.map.insert(name.to_string(), v);
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
struct SpanFields(Map<String, Value>);

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
            Some(SpanFields(m)) => m.extend(f.map),
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
            Value::Object(f.map)
        );
        if let Some(scope) = ctx.event_scope(event) {
            let mut spans = Vec::new();
            for span in scope.from_root() {
                let mut m = Map::new();
                m.insert("name".into(), Value::String(span.name().into()));
                if let Some(SpanFields(sf)) = span.extensions().get::<SpanFields>() {
                    m.extend(sf.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
                spans.push(Value::Object(m));
            }
            if !spans.is_empty() {
                let _ = write!(line, ",\"spans\":{}", Value::Array(spans));
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
