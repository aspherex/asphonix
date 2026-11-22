//! Local smart playlists: user-defined rule sets evaluated against the local
//! SQLite mirror. They exist only on this machine (never sent to the server).
//!
//! ## Storage
//! Definitions live in `smart_playlists.json` in the app **config** dir (next
//! to the settings file), *not* in the library DB: the DB is a disposable
//! cache that is dropped and re-synced on every schema bump and on logout,
//! which would silently delete the user's hand-built rules. The JSON file is
//! tiny, written atomically (tmp + rename), and survives resyncs, logout and
//! account switches (rules only reference local-mirror columns, never
//! account-specific ids).
//!
//! ## Evaluation
//! [`compile`] turns a definition into a `WHERE` fragment with **bound
//! parameters only**: fields, operators and sort orders are closed enums that
//! map to literal SQL snippets; every user-supplied value (numbers, text,
//! day counts -> ISO cut-off timestamps) is a `?` parameter, and text is
//! additionally LIKE-escaped for `contains`. Nothing user-controlled is ever
//! interpolated into the SQL string.
//!
//! Random order is *stable per seed* (`(rid * K + seed) % 2^32`), so paging
//! and a `LIMIT` stay consistent within one view; the UI picks a new
//! seed per page load.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use rusqlite::types::Value;
use serde::{Deserialize, Serialize};

use crate::ipc::{IpcError, IpcErrorCode, Paged, Track};
use crate::library::Db;

const FILE_NAME: &str = "smart_playlists.json";
pub const MAX_RULES: usize = 20;
pub const MAX_LIMIT: u32 = 10_000;
const MAX_TEXT: usize = 200;
const MAX_NAME: usize = 100;
const MAX_DAYS: f64 = 36_500.0;

// ---------------------------------------------------------------------------
// Definition types (wire shape == stored shape)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MatchMode {
    /// Every rule must match (AND).
    All,
    /// Any rule may match (OR).
    Any,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RuleField {
    Rating,
    Starred,
    PlayCount,
    LastPlayed,
    DateAdded,
    Genre,
    Year,
    Artist,
    /// Track length in seconds.
    Duration,
    /// File suffix (`flac`, `mp3`, ...).
    Format,
    BitDepth,
    /// Sampling rate in Hz.
    SampleRate,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RuleOp {
    Eq,
    Gte,
    Lte,
    Unrated,
    Yes,
    No,
    Never,
    OlderThanDays,
    WithinDays,
    Is,
    IsNot,
    Contains,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum RuleValue {
    Num(f64),
    Text(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub field: RuleField,
    pub op: RuleOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<RuleValue>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SmartSort {
    Random,
    RecentlyAdded,
    MostPlayed,
    RecentlyPlayed,
    Title,
    Artist,
    Rating,
}

/// What the user edits (create/update payload and preset body).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SmartInput {
    pub name: String,
    pub match_mode: MatchMode,
    pub rules: Vec<Rule>,
    pub sort: SmartSort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SmartPlaylist {
    pub id: String,
    #[serde(flatten)]
    pub def: SmartInput,
    pub created_at: String,
    pub updated_at: String,
}

/// A stored definition plus its live result size (real counts from the local
/// mirror, evaluated on demand).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SmartSummary {
    #[serde(flatten)]
    pub playlist: SmartPlaylist,
    pub track_count: u64,
    pub duration: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SmartPreset {
    pub key: String,
    pub description: String,
    #[serde(flatten)]
    pub input: SmartInput,
}

fn bad(message: impl Into<String>) -> IpcError {
    IpcError { code: IpcErrorCode::Unknown, message: message.into() }
}

// ---------------------------------------------------------------------------
// Validation + compilation
// ---------------------------------------------------------------------------

fn allowed_ops(field: RuleField) -> &'static [RuleOp] {
    use RuleOp::*;
    match field {
        RuleField::Rating => &[Eq, Gte, Lte, Unrated],
        RuleField::Starred => &[Yes, No],
        RuleField::PlayCount => &[Eq, Gte, Lte],
        RuleField::LastPlayed => &[Never, OlderThanDays, WithinDays],
        RuleField::DateAdded => &[OlderThanDays, WithinDays],
        RuleField::Genre | RuleField::Artist => &[Is, IsNot, Contains],
        RuleField::Format => &[Is, IsNot],
        RuleField::Year | RuleField::BitDepth | RuleField::SampleRate => &[Eq, Gte, Lte],
        RuleField::Duration => &[Gte, Lte],
    }
}

fn is_text_field(field: RuleField) -> bool {
    matches!(field, RuleField::Genre | RuleField::Artist | RuleField::Format)
}

fn no_value(op: RuleOp) -> bool {
    matches!(op, RuleOp::Unrated | RuleOp::Yes | RuleOp::No | RuleOp::Never)
}

fn cmp_sql(op: RuleOp) -> &'static str {
    match op {
        RuleOp::Eq => "=",
        RuleOp::Gte => ">=",
        _ => "<=",
    }
}

/// `%`/`_`/`\` are escaped so user text is matched literally by `LIKE`.
fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub fn validate(input: &SmartInput) -> Result<(), IpcError> {
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(bad(format!("smart playlist name must be 1-{MAX_NAME} characters")));
    }
    if input.rules.len() > MAX_RULES {
        return Err(bad(format!("at most {MAX_RULES} rules")));
    }
    if let Some(l) = input.limit
        && (l == 0 || l > MAX_LIMIT) {
            return Err(bad(format!("limit must be 1-{MAX_LIMIT}")));
        }
    for r in &input.rules {
        if !allowed_ops(r.field).contains(&r.op) {
            return Err(bad(format!("operator {:?} is not valid for field {:?}", r.op, r.field)));
        }
        if no_value(r.op) {
            continue;
        }
        match (&r.value, is_text_field(r.field)) {
            (Some(RuleValue::Text(t)), true) => {
                if t.trim().is_empty() || t.chars().count() > MAX_TEXT {
                    return Err(bad(format!("{:?}: text must be 1-{MAX_TEXT} characters", r.field)));
                }
            }
            (Some(RuleValue::Num(n)), false) => {
                if !n.is_finite() {
                    return Err(bad("numeric rule value must be finite"));
                }
                let whole = n.fract() == 0.0;
                let ok = match (r.field, r.op) {
                    (RuleField::Rating, _) => whole && (1.0..=5.0).contains(n),
                    (RuleField::Duration, _) => *n >= 0.0 && *n <= 1.0e7,
                    (_, RuleOp::OlderThanDays | RuleOp::WithinDays) => whole && (0.0..=MAX_DAYS).contains(n),
                    (RuleField::Year, _) => whole && (0.0..=3000.0).contains(n),
                    _ => whole && (0.0..=1.0e9).contains(n),
                };
                if !ok {
                    return Err(bad(format!("{:?}: value {n} out of range", r.field)));
                }
            }
            _ => return Err(bad(format!("{:?}: missing or wrongly-typed value", r.field))),
        }
    }
    Ok(())
}

/// A compiled `WHERE` body (never empty: `1=1` when there are no rules).
#[derive(Debug, Clone, PartialEq)]
pub struct Compiled {
    pub where_sql: String,
    pub params: Vec<Value>,
}

/// Cut-off for the day rules. Compared via SQLite `julianday()` on both sides,
/// not as strings: servers report RFC 3339 with or without fractional
/// seconds (Navidrome: `...05.123456789Z`), possibly with a `+hh:mm` offset,
/// and locally recorded plays use another variant; `julianday` parses all of
/// them to the same instant (an unparseable value yields NULL = no match).
fn iso_cutoff(now: chrono::DateTime<chrono::Utc>, days: i64) -> String {
    (now - chrono::Duration::days(days)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn num_param(v: &Option<RuleValue>, float: bool) -> Value {
    match v {
        Some(RuleValue::Num(n)) if float => Value::Real(*n),
        Some(RuleValue::Num(n)) => Value::Integer(*n as i64),
        _ => Value::Null,
    }
}

fn text_param(v: &Option<RuleValue>) -> Value {
    match v {
        Some(RuleValue::Text(t)) => Value::Text(t.trim().to_string()),
        _ => Value::Null,
    }
}

/// Compiles the rules to SQL over the `tracks` table. `now` is injected so
/// "within N days" is testable.
pub fn compile(input: &SmartInput, now: chrono::DateTime<chrono::Utc>) -> Result<Compiled, IpcError> {
    validate(input)?;
    if input.rules.is_empty() {
        return Ok(Compiled { where_sql: "1=1".into(), params: vec![] });
    }
    let mut parts: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    for r in &input.rules {
        use RuleField as F;
        use RuleOp as O;
        let frag: String = match (r.field, r.op) {
            (F::Rating, O::Unrated) => "rating IS NULL".into(),
            (F::Rating, op) => {
                params.push(num_param(&r.value, false));
                format!("(rating IS NOT NULL AND rating {} ?)", cmp_sql(op))
            }
            (F::Starred, O::Yes) => "starred IS NOT NULL".into(),
            (F::Starred, _) => "starred IS NULL".into(),
            (F::PlayCount, op) => {
                params.push(num_param(&r.value, false));
                format!("COALESCE(play_count, 0) {} ?", cmp_sql(op))
            }
            (F::LastPlayed, O::Never) => "(COALESCE(play_count, 0) = 0 AND played IS NULL)".into(),
            (F::LastPlayed, op) => {
                let days = num_param(&r.value, false);
                let Value::Integer(days) = days else { return Err(bad("days required")) };
                params.push(Value::Text(iso_cutoff(now, days)));
                let cmp = if op == O::WithinDays { ">=" } else { "<" };
                format!("(played IS NOT NULL AND julianday(played) {cmp} julianday(?))")
            }
            (F::DateAdded, op) => {
                let Value::Integer(days) = num_param(&r.value, false) else { return Err(bad("days required")) };
                params.push(Value::Text(iso_cutoff(now, days)));
                let cmp = if op == O::WithinDays { ">=" } else { "<" };
                format!("(created IS NOT NULL AND julianday(created) {cmp} julianday(?))")
            }
            (F::Genre, op) | (F::Artist, op) | (F::Format, op) => {
                let col = match r.field {
                    F::Genre => "genre",
                    F::Artist => "artist",
                    _ => "suffix",
                };
                match op {
                    O::Is => {
                        params.push(text_param(&r.value));
                        format!("{col} = ? COLLATE NOCASE")
                    }
                    O::IsNot => {
                        params.push(text_param(&r.value));
                        format!("({col} IS NULL OR {col} <> ? COLLATE NOCASE)")
                    }
                    _ => {
                        let Value::Text(t) = text_param(&r.value) else { return Err(bad("text required")) };
                        params.push(Value::Text(format!("%{}%", like_escape(&t))));
                        format!("{col} LIKE ? ESCAPE '\\'")
                    }
                }
            }
            (F::Year, op) => {
                params.push(num_param(&r.value, false));
                format!("(year IS NOT NULL AND year {} ?)", cmp_sql(op))
            }
            (F::Duration, op) => {
                params.push(num_param(&r.value, true));
                format!("duration {} ?", cmp_sql(op))
            }
            (F::BitDepth, op) => {
                params.push(num_param(&r.value, false));
                format!("(bit_depth IS NOT NULL AND bit_depth {} ?)", cmp_sql(op))
            }
            (F::SampleRate, op) => {
                params.push(num_param(&r.value, false));
                // 0 = unknown rate: never matches a rate comparison.
                format!("(sampling_rate > 0 AND sampling_rate {} ?)", cmp_sql(op))
            }
        };
        parts.push(format!("({frag})"));
    }
    let joiner = if input.match_mode == MatchMode::All { " AND " } else { " OR " };
    Ok(Compiled { where_sql: parts.join(joiner), params })
}

fn order_sql(sort: SmartSort) -> &'static str {
    match sort {
        // The two `?` are seed-derived a, b (see `seed_params`): order by the
        // quadratic hash (a*r^2 + b*r) mod p, p = 2^31-1, r = rid mod p, ties
        // by rid. Deterministic per seed (stable paging/limit), different per
        // seed. Quadratic on purpose: a linear (r*m) mod p sorts into a
        // fixed-stride walk through rid order (three-gap theorem), i.e. the
        // "random" list would step through the library in sync order.
        // Overflow-free in i64: every product is < 2^62.
        SmartSort::Random => {
            "((((rid % 2147483647) * (rid % 2147483647)) % 2147483647) * ? + (rid % 2147483647) * ?) % 2147483647, rid"
        }
        SmartSort::RecentlyAdded => "(created IS NULL), created DESC, title COLLATE NOCASE, rid",
        SmartSort::MostPlayed => "COALESCE(play_count, 0) DESC, title COLLATE NOCASE, rid",
        SmartSort::RecentlyPlayed => "(played IS NULL), played DESC, title COLLATE NOCASE, rid",
        SmartSort::Title => "title COLLATE NOCASE, rid",
        SmartSort::Artist => {
            "artist COLLATE NOCASE, album COLLATE NOCASE, COALESCE(disc_number, 1), COALESCE(track_number, 0), rid"
        }
        SmartSort::Rating => "COALESCE(rating, 0) DESC, title COLLATE NOCASE, rid",
    }
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn seed_params(sort: SmartSort, seed: u64) -> Vec<Value> {
    if sort == SmartSort::Random {
        const P: u64 = 2_147_483_647;
        // a in [1, p-1] (never 0: the order must stay non-linear), b in [0, p-1].
        let a = splitmix64(seed) % (P - 1) + 1;
        let b = splitmix64(seed ^ 0xA5A5_A5A5_A5A5_A5A5) % P;
        vec![Value::Integer(a as i64), Value::Integer(b as i64)]
    } else {
        vec![]
    }
}

// ---------------------------------------------------------------------------
// Evaluation against the library DB
// ---------------------------------------------------------------------------

/// One page of the result (honours the definition's `limit` as a cap on the
/// total, so paging past it returns nothing and `total <= limit`).
pub fn evaluate_page(db: &Db, def: &SmartInput, offset: u64, limit: u64, seed: u64) -> Result<Paged<Track>, IpcError> {
    let c = compile(def, chrono::Utc::now())?;
    let cap = def.limit.map(u64::from);
    let (total, _) = summarize_compiled(db, def, &c, seed)?;
    let page_limit = match cap {
        Some(cap) => limit.min(cap.saturating_sub(offset)),
        None => limit,
    };
    if page_limit == 0 {
        return Ok(Paged { items: vec![], total, offset });
    }
    let sql = format!(
        "SELECT * FROM tracks WHERE {} ORDER BY {} LIMIT ? OFFSET ?",
        c.where_sql,
        order_sql(def.sort)
    );
    let mut params = c.params.clone();
    params.extend(seed_params(def.sort, seed));
    params.push(Value::Integer(page_limit.min(i64::MAX as u64) as i64));
    params.push(Value::Integer(offset.min(i64::MAX as u64) as i64));
    let items = db.read(|conn| {
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), crate::library::row_to_track)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })?;
    Ok(Paged { items, total, offset })
}

fn summarize_compiled(db: &Db, def: &SmartInput, c: &Compiled, seed: u64) -> Result<(u64, f64), IpcError> {
    db.read(|conn| match def.limit {
        None => {
            let sql = format!("SELECT COUNT(*), COALESCE(SUM(duration), 0) FROM tracks WHERE {}", c.where_sql);
            let mut stmt = conn.prepare_cached(&sql)?;
            stmt.query_row(rusqlite::params_from_iter(c.params.iter()), |r| {
                Ok((r.get::<_, i64>(0)?.max(0) as u64, r.get::<_, f64>(1)?))
            })
        }
        Some(limit) => {
            let sql = format!(
                "SELECT COUNT(*), COALESCE(SUM(d), 0) FROM
                 (SELECT duration AS d FROM tracks WHERE {} ORDER BY {} LIMIT ?)",
                c.where_sql,
                order_sql(def.sort)
            );
            let mut params = c.params.clone();
            params.extend(seed_params(def.sort, seed));
            params.push(Value::Integer(i64::from(limit)));
            let mut stmt = conn.prepare_cached(&sql)?;
            stmt.query_row(rusqlite::params_from_iter(params.iter()), |r| {
                Ok((r.get::<_, i64>(0)?.max(0) as u64, r.get::<_, f64>(1)?))
            })
        }
    })
}

/// `(count, total duration seconds)` of the (limit-capped) result.
pub fn summarize(db: &Db, def: &SmartInput, seed: u64) -> Result<(u64, f64), IpcError> {
    let c = compile(def, chrono::Utc::now())?;
    summarize_compiled(db, def, &c, seed)
}

/// `(id, artist)` of every result row in play order, for the engine's
/// rolling-window `Source`. 1★ tracks are left out (same rule as every other
/// play source) except `keep_id`.
pub fn play_ids(db: &Db, def: &SmartInput, keep_id: Option<&str>, seed: u64) -> Result<Vec<(String, String)>, IpcError> {
    let c = compile(def, chrono::Utc::now())?;
    let sql = format!(
        "SELECT id, artist FROM tracks WHERE ({}) AND (rating IS NOT 1 OR id = ?) ORDER BY {} LIMIT ?",
        c.where_sql,
        order_sql(def.sort)
    );
    let mut params = c.params.clone();
    params.push(keep_id.map_or(Value::Null, |k| Value::Text(k.to_string())));
    params.extend(seed_params(def.sort, seed));
    params.push(Value::Integer(def.limit.map_or(-1, i64::from)));
    db.read(|conn| {
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}

// ---------------------------------------------------------------------------
// Presets
// ---------------------------------------------------------------------------

fn rule(field: RuleField, op: RuleOp, value: Option<RuleValue>) -> Rule {
    Rule { field, op, value }
}

fn num(n: f64) -> Option<RuleValue> {
    Some(RuleValue::Num(n))
}

pub fn presets() -> Vec<SmartPreset> {
    let mk = |key: &str, name: &str, description: &str, rules: Vec<Rule>, sort: SmartSort, limit: Option<u32>| SmartPreset {
        key: key.into(),
        description: description.into(),
        input: SmartInput { name: name.into(), match_mode: MatchMode::All, rules, sort, limit },
    };
    vec![
        mk(
            "hi-res",
            "Hi-Res only",
            "Sampling rate of 88.2 kHz or higher.",
            vec![rule(RuleField::SampleRate, RuleOp::Gte, num(88_200.0))],
            SmartSort::Artist,
            None,
        ),
        mk(
            "24-bit",
            "24-bit only",
            "Tracks with a 24-bit or deeper sample format.",
            vec![rule(RuleField::BitDepth, RuleOp::Gte, num(24.0))],
            SmartSort::Artist,
            None,
        ),
        mk(
            "top-rated",
            "Top rated (4\u{2605}+)",
            "Rated four or five stars, best first.",
            vec![rule(RuleField::Rating, RuleOp::Gte, num(4.0))],
            SmartSort::Rating,
            None,
        ),
        mk(
            "stale",
            "Not played in 6 months",
            "Played before, but not within the last 180 days. 200 random picks.",
            vec![rule(RuleField::LastPlayed, RuleOp::OlderThanDays, num(180.0))],
            SmartSort::Random,
            Some(200),
        ),
        mk(
            "recent",
            "Recently added",
            "Added to the library in the last 30 days, newest first.",
            vec![rule(RuleField::DateAdded, RuleOp::WithinDays, num(30.0))],
            SmartSort::RecentlyAdded,
            Some(500),
        ),
        mk(
            "never-played",
            "Never played",
            "No recorded plays. 500 random picks.",
            vec![rule(RuleField::LastPlayed, RuleOp::Never, None)],
            SmartSort::Random,
            Some(500),
        ),
    ]
}

// ---------------------------------------------------------------------------
// Store (JSON file in the config dir)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    items: Vec<SmartPlaylist>,
}

pub struct SmartStore {
    path: PathBuf,
    /// `None` until first use (nothing is read on the startup path).
    items: Mutex<Option<Vec<SmartPlaylist>>>,
}

static ID_SEQ: AtomicU64 = AtomicU64::new(0);

fn new_id() -> String {
    let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    format!("sp_{:x}{:x}", nanos, ID_SEQ.fetch_add(1, Ordering::Relaxed))
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl SmartStore {
    pub fn new(dir: &Path) -> Self {
        Self { path: dir.join(FILE_NAME), items: Mutex::new(None) }
    }

    fn with<T>(&self, f: impl FnOnce(&mut Vec<SmartPlaylist>) -> Result<T, IpcError>) -> Result<T, IpcError> {
        let mut guard = self.items.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            *guard = Some(self.load());
        }
        f(guard.as_mut().expect("loaded above"))
    }

    /// Reads the store. A file that exists but doesn't parse (hand-edited,
    /// truncated, or written by a newer build with a rule this one doesn't
    /// know) is moved aside to `smart_playlists.json.bad-<unix secs>` rather
    /// than silently treated as empty: the next save would otherwise
    /// overwrite the user's rules for good. (A crafted file can't inject
    /// SQL either way: fields/ops/sorts are closed enums and every value is
    /// re-validated and bound by `compile`.)
    fn load(&self) -> Vec<SmartPlaylist> {
        let body = match std::fs::read_to_string(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                tracing::warn!("smart playlists: cannot read {}: {e}", self.path.display());
                return Vec::new();
            }
        };
        match serde_json::from_str::<StoreFile>(&body) {
            Ok(f) => f.items,
            Err(e) => {
                let aside = self.path.with_extension(format!("json.bad-{}", chrono::Utc::now().timestamp()));
                tracing::warn!("smart playlists: {} is unreadable ({e}); moved to {}", self.path.display(), aside.display());
                if let Err(e) = std::fs::rename(&self.path, &aside) {
                    tracing::warn!(error = %e, "smart playlists: could not move the unreadable file aside");
                }
                Vec::new()
            }
        }
    }

    fn persist(&self, items: &[SmartPlaylist]) -> Result<(), IpcError> {
        let io = |e: std::io::Error| IpcError { code: IpcErrorCode::Io, message: format!("smart playlists: {e}") };
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let body = serde_json::to_string_pretty(&StoreFile { items: items.to_vec() })
            .map_err(|e| bad(format!("smart playlists: {e}")))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, body).map_err(io)?;
        std::fs::rename(&tmp, &self.path).map_err(io)
    }

    pub fn list(&self) -> Result<Vec<SmartPlaylist>, IpcError> {
        self.with(|items| {
            let mut v = items.clone();
            v.sort_by_key(|p| p.def.name.to_lowercase());
            Ok(v)
        })
    }

    pub fn get(&self, id: &str) -> Result<SmartPlaylist, IpcError> {
        self.with(|items| {
            items.iter().find(|p| p.id == id).cloned().ok_or_else(|| IpcError {
                code: IpcErrorCode::NotFound,
                message: format!("smart playlist {id} not found"),
            })
        })
    }

    pub fn create(&self, mut input: SmartInput) -> Result<SmartPlaylist, IpcError> {
        validate(&input)?;
        input.name = input.name.trim().to_string();
        self.with(|items| {
            let now = now_iso();
            let p = SmartPlaylist { id: new_id(), def: input, created_at: now.clone(), updated_at: now };
            items.push(p.clone());
            self.persist(items)?;
            Ok(p)
        })
    }

    pub fn update(&self, id: &str, mut input: SmartInput) -> Result<SmartPlaylist, IpcError> {
        validate(&input)?;
        input.name = input.name.trim().to_string();
        self.with(|items| {
            let p = items.iter_mut().find(|p| p.id == id).ok_or_else(|| IpcError {
                code: IpcErrorCode::NotFound,
                message: format!("smart playlist {id} not found"),
            })?;
            p.def = input;
            p.updated_at = now_iso();
            let out = p.clone();
            self.persist(items)?;
            Ok(out)
        })
    }

    pub fn delete(&self, id: &str) -> Result<(), IpcError> {
        self.with(|items| {
            items.retain(|p| p.id != id);
            self.persist(items)
        })
    }

    pub fn duplicate(&self, id: &str) -> Result<SmartPlaylist, IpcError> {
        let src = self.get(id)?;
        let mut def = src.def;
        def.name = format!("{} (copy)", def.name);
        self.create(def)
    }
}

#[cfg(test)]
mod tests;
