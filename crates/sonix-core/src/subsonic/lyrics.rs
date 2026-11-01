//! Wire types + parsing for lyrics: OpenSubsonic `getLyricsBySongId`
//! (structured, optionally time-synced) and classic `getLyrics` (plain text
//! by artist/title). Normalized into [`ParsedLyrics`] here so the IPC layer
//! only deals with one shape.
#![allow(dead_code)]

use serde::Deserialize;

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RawLyricsList {
    #[serde(default)]
    pub structured_lyrics: Vec<RawStructuredLyrics>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RawStructuredLyrics {
    #[serde(default)]
    pub synced: bool,
    /// OpenSubsonic: milliseconds to apply to every line; positive means
    /// the lyrics appear sooner.
    #[serde(default)]
    pub offset: Option<i64>,
    #[serde(default)]
    pub line: Vec<RawLyricLine>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RawLyricLine {
    /// Milliseconds from the start of the track; absent on unsynced lyrics.
    #[serde(default)]
    pub start: Option<i64>,
    #[serde(default)]
    pub value: String,
}

/// Classic `getLyrics`: one text blob (Navidrome returns `{}` when none).
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RawClassicLyrics {
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedLine {
    pub start_ms: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedLyrics {
    pub synced: bool,
    pub lines: Vec<ParsedLine>,
}

/// Picks the best structured set and normalizes it: the first synced set
/// with text, else the first set with any text (servers list the embedded
/// and sidecar sets in their own order; language is not used to choose,
/// Navidrome mostly reports `xxx`/`und`). The set's `offset` is applied
/// (`start - offset`, clamped at 0). `None` when there is no line with text.
pub fn from_structured(list: &RawLyricsList) -> Option<ParsedLyrics> {
    let pick = list
        .structured_lyrics
        .iter()
        .find(|l| l.synced && l.line.iter().any(|x| x.start.is_some() && !x.value.trim().is_empty()))
        .or_else(|| list.structured_lyrics.iter().find(|l| l.line.iter().any(|x| !x.value.trim().is_empty())))?;
    // A set only counts as synced when its lines really carry timestamps.
    let synced = pick.synced && pick.line.iter().all(|l| l.start.is_some());
    let mut lines: Vec<ParsedLine> = pick
        .line
        .iter()
        .map(|l| ParsedLine {
            start_ms: if synced {
                l.start.map(|s| s.saturating_sub(pick.offset.unwrap_or(0)).clamp(0, u32::MAX as i64) as u32)
            } else {
                None
            },
            text: l.value.trim().to_string(),
        })
        .collect();
    if synced {
        lines.sort_by_key(|l| l.start_ms);
    }
    trim_blank_edges(&mut lines);
    (!lines.is_empty()).then_some(ParsedLyrics { synced, lines })
}

/// Classic plain text: one entry per line; blank lines kept as stanza
/// breaks (but collapsed and trimmed at the edges).
pub fn from_plain(text: &str) -> Option<ParsedLyrics> {
    let mut lines: Vec<ParsedLine> = Vec::new();
    for raw in text.replace("\r\n", "\n").split('\n') {
        let t = raw.trim();
        if t.is_empty() && lines.last().is_some_and(|l| l.text.is_empty()) {
            continue;
        }
        lines.push(ParsedLine { start_ms: None, text: t.to_string() });
    }
    trim_blank_edges(&mut lines);
    (!lines.is_empty()).then_some(ParsedLyrics { synced: false, lines })
}

fn trim_blank_edges(lines: &mut Vec<ParsedLine>) {
    while lines.last().is_some_and(|l| l.text.is_empty()) {
        lines.pop();
    }
    let lead = lines.iter().take_while(|l| l.text.is_empty()).count();
    lines.drain(..lead);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subsonic::models::Envelope;

    #[test]
    fn parses_synced_structured_lyrics_and_prefers_them() {
        let json = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","lyricsList":{"structuredLyrics":[
            {"lang":"eng","synced":false,"line":[{"value":"plain one"},{"value":"plain two"}]},
            {"lang":"eng","synced":true,"offset":0,"line":[
              {"start":15000,"value":" Second "},{"start":1200,"value":"First"},{"start":30000,"value":""}]}]}}}"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        let l = from_structured(&env.subsonic_response.lyrics_list.unwrap()).unwrap();
        assert!(l.synced);
        // Sorted by time, trimmed, trailing empty line dropped.
        assert_eq!(
            l.lines,
            vec![
                ParsedLine { start_ms: Some(1200), text: "First".into() },
                ParsedLine { start_ms: Some(15000), text: "Second".into() },
            ]
        );
    }

    #[test]
    fn offset_shifts_every_line_and_never_goes_negative() {
        let json = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","lyricsList":{"structuredLyrics":[
            {"synced":true,"offset":500,"line":[{"start":300,"value":"a"},{"start":2000,"value":"b"}]}]}}}"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        let l = from_structured(&env.subsonic_response.lyrics_list.unwrap()).unwrap();
        assert_eq!(l.lines.iter().map(|x| x.start_ms).collect::<Vec<_>>(), vec![Some(0), Some(1500)]);
        let json = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","lyricsList":{"structuredLyrics":[
            {"synced":true,"offset":-250,"line":[{"start":1000,"value":"a"}]}]}}}"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        let l = from_structured(&env.subsonic_response.lyrics_list.unwrap()).unwrap();
        assert_eq!(l.lines[0].start_ms, Some(1250), "negative offset: later");
    }

    #[test]
    fn unsynced_structured_lyrics_have_no_timestamps() {
        let json = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","lyricsList":{"structuredLyrics":[
            {"synced":false,"line":[{"value":"a"},{"value":""},{"value":"b"}]}]}}}"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        let l = from_structured(&env.subsonic_response.lyrics_list.unwrap()).unwrap();
        assert!(!l.synced);
        assert_eq!(l.lines.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), vec!["a", "", "b"]);
        assert!(l.lines.iter().all(|x| x.start_ms.is_none()));
    }

    #[test]
    fn a_synced_flag_without_timestamps_is_downgraded() {
        let json = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","lyricsList":{"structuredLyrics":[
            {"synced":true,"line":[{"value":"a"},{"start":500,"value":"b"}]}]}}}"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        let l = from_structured(&env.subsonic_response.lyrics_list.unwrap()).unwrap();
        assert!(!l.synced);
    }

    #[test]
    fn missing_or_empty_lyrics_are_none() {
        let json = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","lyricsList":{"structuredLyrics":[]}}}"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        assert!(from_structured(&env.subsonic_response.lyrics_list.unwrap()).is_none());
        let json = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","lyricsList":{}}}"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        assert!(from_structured(&env.subsonic_response.lyrics_list.unwrap()).is_none());
        let json = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","lyrics":{}}}"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        assert!(env.subsonic_response.lyrics.unwrap().value.is_none());
        assert!(from_plain("  \n\r\n ").is_none());
    }

    #[test]
    fn classic_plain_text_keeps_stanza_breaks() {
        let json = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","lyrics":{"artist":"A","title":"T","value":"line 1\r\nline 2\n\n\n\nline 3\n"}}}"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        let text = env.subsonic_response.lyrics.unwrap().value.unwrap();
        let l = from_plain(&text).unwrap();
        assert!(!l.synced);
        assert_eq!(l.lines.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), vec!["line 1", "line 2", "", "line 3"]);
    }
}
