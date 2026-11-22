//! The static MCP tool table. `tools/list` is answered from here even when
//! SONIX is not running (clients list tools at startup). The app side
//! (`sonix-core/src/mcp_bridge`) validates every argument itself and has a test
//! that every name below has a handler, so the two cannot drift.

use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    /// Controls playback: refused unless "Allow playback control" is on.
    pub control: bool,
    pub schema: Value,
}

/// Hard cap on rows per page across every list tool.
pub const MAX_PAGE: u64 = 200;

fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required, "additionalProperties": false })
}

fn page_props(default_limit: u64) -> Value {
    json!({
        "offset": { "type": "integer", "minimum": 0, "default": 0 },
        "limit": { "type": "integer", "minimum": 1, "maximum": MAX_PAGE, "default": default_limit,
                   "description": "Rows per page (hard maximum 200)." }
    })
}

fn with_page(mut props: Value, default_limit: u64) -> Value {
    if let (Some(p), Some(extra)) = (props.as_object_mut(), page_props(default_limit).as_object()) {
        for (k, v) in extra {
            p.insert(k.clone(), v.clone());
        }
    }
    props
}

const WHEN: &str = "ISO 8601 date (YYYY-MM-DD, local midnight; as `until` it includes that whole day) \
or date-time (with offset, or local time without one).";

pub fn tools() -> Vec<ToolDef> {
    let rules_schema = json!({
        "type": "object",
        "description": "Ad-hoc smart-playlist rules (nothing is saved). Same model as SONIX smart playlists.",
        "properties": {
            "matchMode": { "enum": ["all", "any"], "default": "all" },
            "rules": { "type": "array", "maxItems": 20, "items": { "type": "object", "properties": {
                "field": { "enum": ["rating", "starred", "playCount", "lastPlayed", "dateAdded", "genre", "year",
                                    "artist", "duration", "format", "bitDepth", "sampleRate"] },
                "op": { "enum": ["eq", "gte", "lte", "unrated", "yes", "no", "never", "olderThanDays",
                                 "withinDays", "is", "isNot", "contains"] },
                "value": { "type": ["number", "string"],
                           "description": "duration is in seconds; day ops take a number of days." }
            }, "required": ["field", "op"] } },
            "sort": { "enum": ["random", "recentlyAdded", "mostPlayed", "recentlyPlayed", "title", "artist", "rating"],
                      "default": "title" },
            "limit": { "type": "integer", "minimum": 1, "description": "Cap on the evaluated list." }
        },
        "required": ["rules"]
    });

    vec![
        // ---- read ----------------------------------------------------------
        ToolDef {
            name: "search",
            description: "Full-text search of the local library mirror (title/artist/album names, prefix matching, \
diacritic-insensitive). Returns compact tracks, albums, artists and playlists with ids you can pass to other tools.",
            control: false,
            schema: obj(
                with_page(json!({
                    "query": { "type": "string", "minLength": 1, "maxLength": 200 },
                    "type": { "enum": ["all", "track", "album", "artist", "playlist"], "default": "all" }
                }), 20),
                &["query"],
            ),
        },
        ToolDef {
            name: "get_tracks",
            description: "Page through every track in the library with the same sort/filter as SONIX's Tracks view. \
`filter` is a text match on title/artist/album. For richer filtering (genre, duration, last played, rating, year) \
use get_smart_playlist_tracks with ad-hoc rules.",
            control: false,
            schema: obj(
                with_page(json!({
                    "sort": { "enum": ["title", "artist", "album", "year", "dateAdded", "duration", "format"], "default": "title" },
                    "dir": { "enum": ["asc", "desc"], "default": "asc" },
                    "filter": { "type": "string", "maxLength": 200 }
                }), 50),
                &[],
            ),
        },
        ToolDef {
            name: "get_album",
            description: "One album with a page of its tracks in disc/track order.",
            control: false,
            schema: obj(with_page(json!({ "id": { "type": "string" } }), 100), &["id"]),
        },
        ToolDef {
            name: "get_artist",
            description: "One artist with a page of their albums (use get_tracks/search for individual tracks).",
            control: false,
            schema: obj(with_page(json!({ "id": { "type": "string" } }), 100), &["id"]),
        },
        ToolDef {
            name: "list_playlists",
            description: "The user's server playlists (id, name, track count, duration, owner).",
            control: false,
            schema: obj(page_props(100), &[]),
        },
        ToolDef {
            name: "get_playlist",
            description: "One playlist with a page of its tracks in playlist order.",
            control: false,
            schema: obj(with_page(json!({ "id": { "type": "string" } }), 100), &["id"]),
        },
        ToolDef {
            name: "list_genres",
            description: "Genres with album and track counts. There is no instrumental/vocal flag in the library: \
genre names are the only signal for 'no vocals'.",
            control: false,
            schema: obj(with_page(json!({ "sort": { "enum": ["tracks", "alphabetical"], "default": "tracks" } }), 100), &[]),
        },
        ToolDef {
            name: "list_smart_playlists",
            description: "The user's local smart playlists (rule definitions, not tracks).",
            control: false,
            schema: obj(page_props(100), &[]),
        },
        ToolDef {
            name: "get_smart_playlist_tracks",
            description: "Evaluate a smart playlist (pass `id`) or an ad-hoc rule set (pass `rules`; nothing is saved) \
against the local library and return a page of matching tracks. Use this for questions like 'genre X, shorter than 6 \
minutes, not played in 7 days'. Rule values: duration in seconds; lastPlayed ops olderThanDays/withinDays/never.",
            control: false,
            schema: obj(
                with_page(json!({
                    "id": { "type": "string", "description": "A saved smart playlist id (see list_smart_playlists)." },
                    "rules": rules_schema,
                    "seed": { "type": "integer", "minimum": 0, "description": "Makes `random` sort stable across pages." }
                }), 50),
                &[],
            ),
        },
        ToolDef {
            name: "listening_history",
            description: "Plays recorded by SONIX (a play counts after the user's play threshold), newest first. \
Only plays since SONIX started recording are present; plays from other clients are not. Call library_stats first if \
you need the current local date/time.",
            control: false,
            schema: obj(
                with_page(json!({
                    "since": { "type": "string", "description": format!("Start (inclusive). {WHEN}") },
                    "until": { "type": "string", "description": format!("End (exclusive). {WHEN}") }
                }), 50),
                &[],
            ),
        },
        ToolDef {
            name: "listen_events",
            description: "Every listening session SONIX recorded, newest first: start and end time, seconds actually \
heard, track length, how it ended (finished, next, previous, jump, stopped, quit, error), the queue source it played \
from, whether the user queued it or it came from the source/autoplay, and whether it counted as a play. A skip made \
through a playback tool looks the same as one the user made. Only sessions since SONIX started recording them exist.",
            control: false,
            schema: obj(
                with_page(json!({
                    "since": { "type": "string", "description": format!("Start (inclusive, on startedAt). {WHEN}") },
                    "until": { "type": "string", "description": format!("End (exclusive). {WHEN}") }
                }), 50),
                &[],
            ),
        },
        ToolDef {
            name: "skip_stats",
            description: "Per-track aggregate of listen_events over a time range, most-listened first: sessions, \
skips (ended by next/previous/jump with under 80% heard), plays counted, average fraction heard, last listened. \
Useful for 'what do I keep skipping'.",
            control: false,
            schema: obj(
                with_page(json!({
                    "since": { "type": "string", "description": format!("Start (inclusive). {WHEN}") },
                    "until": { "type": "string", "description": format!("End (exclusive). {WHEN}") },
                    "minSessions": { "type": "integer", "minimum": 1, "default": 1,
                                     "description": "Only tracks with at least this many sessions." }
                }), 50),
                &[],
            ),
        },
        ToolDef {
            name: "top_tracks",
            description: "Most-played tracks over a time range (from SONIX's own play history), with play counts, \
last-played time and whether the track is liked (`starred`). Set unlikedOnly to answer 'what did I play a lot but \
never liked'.",
            control: false,
            schema: obj(
                json!({
                    "since": { "type": "string", "description": format!("Start (inclusive). {WHEN}") },
                    "until": { "type": "string", "description": format!("End (exclusive). {WHEN}") },
                    "unlikedOnly": { "type": "boolean", "default": false },
                    "limit": { "type": "integer", "minimum": 1, "maximum": MAX_PAGE, "default": 25 }
                }),
                &[],
            ),
        },
        ToolDef {
            name: "top_artists",
            description: "Most-played artists over a time range (from SONIX's own play history) with play counts \
and the number of distinct tracks played.",
            control: false,
            schema: obj(
                json!({
                    "since": { "type": "string", "description": format!("Start (inclusive). {WHEN}") },
                    "until": { "type": "string", "description": format!("End (exclusive). {WHEN}") },
                    "limit": { "type": "integer", "minimum": 1, "maximum": MAX_PAGE, "default": 25 }
                }),
                &[],
            ),
        },
        ToolDef {
            name: "now_playing",
            description: "The current track, status, position and the real output format (device mode, bit depth, \
sample rate, bit-perfect).",
            control: false,
            schema: obj(json!({}), &[]),
        },
        ToolDef {
            name: "get_queue",
            description: "A page of the play queue (history, current track, upcoming). `currentIndex` locates the \
current track; `source` says which playlist/artist/etc. the queue rolls from. Items with origin 'user' were queued \
explicitly.",
            control: false,
            schema: obj(page_props(50), &[]),
        },
        ToolDef {
            name: "library_stats",
            description: "Library counts (tracks, albums, artists, playlists, liked, recorded plays), the last sync \
time and the current local date/time and UTC offset.",
            control: false,
            schema: obj(json!({}), &[]),
        },
        // ---- control (also need "Allow playback control") -------------------
        ToolDef {
            name: "play_pause",
            description: "Toggle, start or pause playback. Requires 'Allow playback control' in SONIX settings.",
            control: true,
            schema: obj(json!({ "action": { "enum": ["toggle", "play", "pause"], "default": "toggle" } }), &[]),
        },
        ToolDef {
            name: "next",
            description: "Skip to the next track. Requires 'Allow playback control'.",
            control: true,
            schema: obj(json!({}), &[]),
        },
        ToolDef {
            name: "previous",
            description: "Previous track (restarts the current one after a few seconds, like the app). Requires \
'Allow playback control'.",
            control: true,
            schema: obj(json!({}), &[]),
        },
        ToolDef {
            name: "seek",
            description: "Seek within the current track. Requires 'Allow playback control'.",
            control: true,
            schema: obj(json!({ "positionSec": { "type": "number", "minimum": 0 } }), &["positionSec"]),
        },
        ToolDef {
            name: "set_volume_db",
            description: "Set the volume in dB: 0 is full scale (bit-perfect), -60 is the bottom of SONIX's range; \
anything below -60 mutes. Requires 'Allow playback control'.",
            control: true,
            schema: obj(json!({ "db": { "type": "number", "maximum": 0 } }), &["db"]),
        },
        ToolDef {
            name: "play_tracks",
            description: "Replace the queue with these track ids (max 500) and start playing at `startIndex`. \
Unknown ids are skipped and reported. If SONIX's shuffle mode is on (see now_playing.shuffle) the \
queue plays shuffled, start track first, not in the order given; the result reports shuffleOn. Requires 'Allow \
playback control'.",
            control: true,
            schema: obj(
                json!({
                    "trackIds": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": 500 },
                    "startIndex": { "type": "integer", "minimum": 0, "default": 0 }
                }),
                &["trackIds"],
            ),
        },
        ToolDef {
            name: "enqueue",
            description: "Add track ids (max 500) to the queue, in the given order: 'next' right after the current \
track, 'end' after anything already queued explicitly. Unknown ids are skipped and reported. Requires 'Allow \
playback control'.",
            control: true,
            schema: obj(
                json!({
                    "trackIds": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": 500 },
                    "position": { "enum": ["next", "end"], "default": "end" }
                }),
                &["trackIds"],
            ),
        },
        ToolDef {
            name: "play_source",
            description: "Start playing a whole source: playlist, artist, album, genre (id = genre name), liked, \
smart (saved smart playlist id) or allTracks. For these rolling sources `shuffle` selects shuffled or in-order \
play (and becomes SONIX's global shuffle mode), and `flow` instead selects SONIX's DJ-style Flow sequencing with an \
energy arc: steady, build (rising) or peak (rise, then cool down). Pass shuffle or flow, not both. Album is a plain \
queue: with shuffle false it plays in album order unless SONIX's shuffle mode is already on (the result reports \
shuffleOn); with shuffle true the order is randomised once; flow is not available for albums. Requires 'Allow \
playback control'.",
            control: true,
            schema: obj(
                json!({
                    "kind": { "enum": ["playlist", "artist", "album", "genre", "liked", "smart", "allTracks"] },
                    "id": { "type": "string", "description": "Required for everything except liked and allTracks." },
                    "shuffle": { "type": "boolean", "default": false },
                    "flow": { "enum": ["steady", "build", "peak"],
                              "description": "Flow sequencing arc (not with shuffle, not for album)." },
                    "startTrackId": { "type": "string" }
                }),
                &["kind"],
            ),
        },
    ]
}

/// Tools that throw away state the user may care about: the current queue
/// is replaced (MCP `destructiveHint`). Everything else only reads, or adds
/// to / moves within what is already there.
pub const DESTRUCTIVE: &[&str] = &["play_tracks", "play_source"];

pub fn find(name: &str) -> Option<ToolDef> {
    tools().into_iter().find(|t| t.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn names_are_unique_and_schemas_are_object_schemas() {
        let all = tools();
        let names: HashSet<_> = all.iter().map(|t| t.name).collect();
        assert_eq!(names.len(), all.len());
        for t in &all {
            assert!(!t.description.is_empty());
            assert_eq!(t.schema["type"], "object", "{}", t.name);
            assert!(t.schema["properties"].is_object(), "{}", t.name);
            assert!(t.schema["required"].is_array(), "{}", t.name);
            // Every required property is declared.
            for r in t.schema["required"].as_array().unwrap() {
                assert!(t.schema["properties"].get(r.as_str().unwrap()).is_some(), "{}: {r}", t.name);
            }
        }
        assert!(all.iter().any(|t| t.control) && all.iter().any(|t| !t.control));
    }

    #[test]
    fn destructive_tools_exist_and_are_control_tools() {
        for name in DESTRUCTIVE {
            assert!(find(name).is_some_and(|t| t.control), "{name}");
        }
    }

    #[test]
    fn control_tools_say_so() {
        for t in tools().iter().filter(|t| t.control) {
            assert!(t.description.contains("Allow playback control"), "{}", t.name);
        }
    }
}
