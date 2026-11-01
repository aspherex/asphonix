//! Discovery features: genre browsing, the richer artist page (popular
//! tracks, biography, similar artists) and listening history.
//!
//! Kept in its own module (types + `#[tauri::command]`s together) so the
//! shared `ipc.rs` only needed a one-line `SourceKind::Genre`. The TS
//! mirror lives in the "Discovery" blocks of `src/lib/ipc/types.ts`/
//! `commands.ts`; `docs/IPC.md` has the contract. DB work is in
//! `library/discovery.rs`.

use serde::{Deserialize, Serialize};

use crate::ipc::{Artist, FlowArc, IpcError, IpcErrorCode, IpcResult, Paged, SourceKind, Track};

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Genre {
    pub name: String,
    pub album_count: u32,
    pub track_count: u32,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GenreSort {
    #[default]
    Tracks,
    Alphabetical,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetGenresRequest {
    pub offset: u32,
    pub limit: u32,
    #[serde(default)]
    pub sort: GenreSort,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayGenreRequest {
    pub name: String,
    pub shuffle: bool,
    #[serde(default)]
    pub start_track_id: Option<String>,
    /// feat-flow: play as a Flow source with this arc (overrides `shuffle`).
    #[serde(default)]
    pub flow: Option<FlowArc>,
}

/// A played track and when (unix ms).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub track: Track,
    pub played_at: i64,
}

/// One row of the day-grouped history list: a day header or a played track.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[allow(clippy::large_enum_variant)] // short-lived serialization value, boxing would only add noise
pub enum HistoryItem {
    /// `day` is `YYYY-MM-DD`, the play's local date (OS time zone, DST
    /// applied per timestamp).
    Day { day: String, count: u32 },
    Track { track: Track, played_at: i64 },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetHistoryRequest {
    pub offset: u32,
    pub limit: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecentRequest {
    pub limit: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TopSource {
    /// Server `getTopSongs` (Last.fm + local plays).
    Popular,
    /// Local play-history counts (the honest fallback).
    MostPlayed,
}

/// `Paged<Track>` plus where the list came from, so the UI can label it
/// honestly. `items` may be empty (nothing popular, never played).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistTopTracks {
    pub source: TopSource,
    pub items: Vec<Track>,
    pub total: u64,
    pub offset: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistInfo {
    /// Plain text (HTML stripped by [`sanitize_bio`]), paragraphs separated
    /// by `\n`. Absent when the server has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub biography: Option<String>,
    /// Similar artists that exist in the local library, server order.
    pub similar: Vec<Artist>,
}

// ---------------------------------------------------------------------------
// Pure helpers (unit tested)
// ---------------------------------------------------------------------------

const TOP_SONGS_COUNT: u32 = 10;
const MAX_SIMILAR: usize = 12;
/// Cache lifetime of a non-empty server answer.
pub const CACHE_TTL_MS: i64 = 7 * 24 * 3600 * 1000;
/// An empty answer (no Last.fm agent, unknown artist) is retried sooner.
pub const CACHE_TTL_EMPTY_MS: i64 = 24 * 3600 * 1000;

pub fn cache_is_fresh(fetched_at: i64, now: i64, was_empty: bool) -> bool {
    let ttl = if was_empty { CACHE_TTL_EMPTY_MS } else { CACHE_TTL_MS };
    now >= fetched_at && now - fetched_at < ttl
}

/// Server order wins; ids the local mirror lacks are dropped (never
/// fabricated). If nothing survives, falls back to `most_played`.
pub fn resolve_top(server_ids: &[String], local: Vec<Track>, most_played: Vec<Track>) -> (TopSource, Vec<Track>) {
    let mut by_id: std::collections::HashMap<String, Track> = local.into_iter().map(|t| (t.id.clone(), t)).collect();
    let ordered: Vec<Track> = server_ids.iter().filter_map(|id| by_id.remove(id)).collect();
    if ordered.is_empty() {
        (TopSource::MostPlayed, most_played)
    } else {
        (TopSource::Popular, ordered)
    }
}

/// Server biographies are HTML with a trailing "Read more on Last.fm" link.
/// Returns plain text: tags stripped, `<br>`/`</p>` become newlines,
/// `<script>`/`<style>` bodies dropped, the read-more anchor removed (other
/// anchors keep their text), common entities decoded. `None` when nothing
/// readable is left. The external link is deliberately not surfaced (no
/// opener available; raw external URLs never go into the WebView).
pub fn sanitize_bio(html: &str) -> Option<String> {
    let mut out = String::with_capacity(html.len());
    let mut anchor: Option<String> = None;
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        push_text(&mut out, &mut anchor, &rest[..lt]);
        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else {
            rest = "";
            break;
        };
        let tag = after[..gt].trim();
        rest = &after[gt + 1..];
        let lower = tag.trim_start_matches('/').to_ascii_lowercase();
        let name: String = lower.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        let closing = tag.starts_with('/');
        match name.as_str() {
            "script" | "style" if !closing => {
                // Skip to the matching close tag.
                let close = format!("</{name}");
                match rest.to_ascii_lowercase().find(&close) {
                    Some(i) => {
                        let tail = &rest[i..];
                        rest = tail.find('>').map(|g| &tail[g + 1..]).unwrap_or("");
                    }
                    None => rest = "",
                }
            }
            "br" => push_text(&mut out, &mut anchor, "\n"),
            "p" | "div" | "li" if closing => push_text(&mut out, &mut anchor, "\n"),
            "a" if !closing => anchor = Some(String::new()),
            "a" if closing => {
                if let Some(text) = anchor.take() {
                    if !is_read_more(&text) {
                        out.push_str(&text);
                    }
                }
            }
            _ => {}
        }
    }
    push_text(&mut out, &mut anchor, rest);
    if let Some(text) = anchor.take() {
        if !is_read_more(&text) {
            out.push_str(&text);
        }
    }
    let decoded = decode_entities(&out);
    // Collapse whitespace per line, then limit blank-line runs to one.
    let mut lines: Vec<String> = Vec::new();
    for line in decoded.split('\n') {
        let l = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if l.is_empty() && lines.last().map(|p| p.is_empty()).unwrap_or(true) {
            continue;
        }
        lines.push(l);
    }
    let text = lines.join("\n").trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Last.fm's trailing "Read more on Last.fm" anchor (entities decoded first,
/// so `Read&nbsp;more` matches too).
fn is_read_more(anchor_text: &str) -> bool {
    decode_entities(anchor_text).split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase().starts_with("read more")
}

fn push_text(out: &mut String, anchor: &mut Option<String>, s: &str) {
    match anchor {
        Some(a) => a.push_str(s),
        None => out.push_str(s),
    }
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        match tail.find(';').filter(|&i| i <= 8) {
            Some(semi) => {
                let ent = &tail[1..semi];
                let rep = match ent {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    "hellip" => Some('\u{2026}'),
                    "ndash" => Some('\u{2013}'),
                    "mdash" => Some('\u{2014}'),
                    "lsquo" => Some('\u{2018}'),
                    "rsquo" => Some('\u{2019}'),
                    "ldquo" => Some('\u{201C}'),
                    "rdquo" => Some('\u{201D}'),
                    _ => ent
                        .strip_prefix('#')
                        .and_then(|n| match n.strip_prefix(['x', 'X']) {
                            Some(h) => u32::from_str_radix(h, 16).ok(),
                            None => n.parse().ok(),
                        })
                        .and_then(char::from_u32),
                };
                match rep {
                    Some(c) => {
                        out.push(c);
                        rest = &tail[semi + 1..];
                    }
                    None => {
                        out.push('&');
                        rest = &tail[1..];
                    }
                }
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------
// Cached payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Default)]
struct CachedTop {
    ids: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct CachedInfo {
    biography: Option<String>,
    /// `(artist id if the server gave one, name)`.
    similar: Vec<(Option<String>, String)>,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}


fn not_found(msg: String) -> IpcError {
    IpcError { code: IpcErrorCode::NotFound, message: msg }
}

// ---------------------------------------------------------------------------
// Genres
// ---------------------------------------------------------------------------

