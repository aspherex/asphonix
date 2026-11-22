//! Wire types for the artist-page endpoints (`getTopSongs`, `getArtistInfo2`).
#![allow(dead_code)]

use serde::Deserialize;

/// `getArtistInfo2` payload. Navidrome fills `biography`/`lastFmUrl`/image
/// urls only when a Last.fm (or other) agent is configured; every field may
/// be absent or empty.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RawArtistInfo2 {
    #[serde(default)]
    pub biography: Option<String>,
    #[serde(default)]
    pub last_fm_url: Option<String>,
    #[serde(default)]
    pub large_image_url: Option<String>,
    #[serde(default)]
    pub similar_artist: Vec<RawSimilarArtist>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RawSimilarArtist {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
}

#[cfg(test)]
mod tests {
    use crate::subsonic::models::Envelope;

    #[test]
    fn parses_get_top_songs_response() {
        let json = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","topSongs":{"song":[
            {"id":"s1","title":"Hit","artist":"A","suffix":"flac"},
            {"id":"s2","title":"Other","artist":"A","suffix":"mp3","duration":200}]}}}"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        let songs = env.subsonic_response.top_songs.unwrap().song;
        assert_eq!(songs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), vec!["s1", "s2"]);
    }

    #[test]
    fn empty_top_songs_object_parses_to_no_songs() {
        let json = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","topSongs":{}}}"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        assert!(env.subsonic_response.top_songs.unwrap().song.is_empty());
    }

    #[test]
    fn parses_get_artist_info2_response() {
        let json = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","artistInfo2":{
            "biography":"Hello <a target='_blank' href=\"https://www.last.fm/music/A\">Read more on Last.fm</a>",
            "lastFmUrl":"https://www.last.fm/music/A","largeImageUrl":"https://example.com/x.png",
            "similarArtist":[{"id":"ar-9","name":"Other","albumCount":3},{"name":"No Id"}]}}}"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        let info = env.subsonic_response.artist_info2.unwrap();
        assert!(info.biography.unwrap().starts_with("Hello"));
        assert_eq!(info.similar_artist.len(), 2);
        assert_eq!(info.similar_artist[0].id.as_deref(), Some("ar-9"));
        assert!(info.similar_artist[1].id.is_none());
    }

    #[test]
    fn artist_info2_without_any_agent_data_is_all_empty() {
        let json = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","artistInfo2":{}}}"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        let info = env.subsonic_response.artist_info2.unwrap();
        assert!(info.biography.is_none() && info.similar_artist.is_empty());
    }
}
