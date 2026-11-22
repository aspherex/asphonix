//! `getPlayQueue` wire type (continuity: cross-device resume).

use serde::Deserialize;

use super::models::RawSong;

/// `getPlayQueue` payload: the queue another client (or this one) saved with
/// `savePlayQueue`. `current` is a song id (Navidrome sends a string, some
/// servers a number), `position` is milliseconds into it, `changed` an ISO
/// 8601 timestamp (server clock). The whole `playQueue` object is absent when
/// the user never saved one.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct RawPlayQueue {
    #[serde(default)]
    pub entry: Vec<RawSong>,
    #[serde(default, deserialize_with = "lenient_string")]
    pub current: Option<String>,
    #[serde(default)]
    pub position: Option<u64>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub changed: Option<String>,
    #[serde(default)]
    pub changed_by: Option<String>,
}

fn lenient_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum S {
        Str(String),
        Int(i64),
    }
    Ok(Option::<S>::deserialize(d)?.map(|v| match v {
        S::Str(s) => s,
        S::Int(i) => i.to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::super::models::Envelope;

    #[test]
    fn parses_a_get_play_queue_response() {
        let json = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","playQueue":{
            "entry":[{"id":"a1","title":"One","suffix":"flac","artist":"X"},{"id":"a2","title":"Two","suffix":"mp3"}],
            "current":"a2","position":93000,"username":"andi","changed":"2026-10-11T09:30:00.123Z","changedBy":"DSub"}}}"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        let pq = env.subsonic_response.play_queue.unwrap();
        assert_eq!(pq.entry.len(), 2);
        assert_eq!(pq.current.as_deref(), Some("a2"));
        assert_eq!(pq.position, Some(93_000));
        assert_eq!(pq.changed_by.as_deref(), Some("DSub"));
        assert_eq!(pq.changed.as_deref(), Some("2026-10-11T09:30:00.123Z"));
    }

    #[test]
    fn numeric_current_and_missing_queue_are_tolerated() {
        let json = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","playQueue":{"entry":[],"current":42}}}"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        assert_eq!(env.subsonic_response.play_queue.unwrap().current.as_deref(), Some("42"));
        let none = r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#;
        let env: Envelope = serde_json::from_str(none).unwrap();
        assert!(env.subsonic_response.play_queue.is_none());
    }
}
