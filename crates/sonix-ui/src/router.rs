//! Hash-router equivalent (`src/lib/router.svelte.ts`): a route enum, a history
//! stack with back/forward, and the `#/…` fragment form used by the Svelte app.

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Route {
    Home,
    Albums,
    Artists,
    Tracks,
    Genres,
    History,
    Playlists,
    Liked,
    Settings,
    Search,
    NowPlaying,
    Playlist(String),
    Album(String),
    Artist(String),
    Genre(String),
}

impl Route {
    /// Fragment without the leading `#/`, same names as the Svelte router.
    pub fn path(&self) -> String {
        match self {
            Route::Home => "home".into(),
            Route::Albums => "albums".into(),
            Route::Artists => "artists".into(),
            Route::Tracks => "tracks".into(),
            Route::Genres => "genres".into(),
            Route::History => "history".into(),
            Route::Playlists => "playlists".into(),
            Route::Liked => "liked".into(),
            Route::Settings => "settings".into(),
            Route::Search => "search".into(),
            Route::NowPlaying => "nowplaying".into(),
            Route::Playlist(id) => format!("playlist/{id}"),
            Route::Album(id) => format!("album/{id}"),
            Route::Artist(id) => format!("artist/{id}"),
            Route::Genre(n) => format!("genre/{}", encode_component(n)),
        }
    }

    pub fn parse(s: &str) -> Option<Route> {
        let s = s.trim_start_matches('#').trim_start_matches('/');
        let (head, rest) = match s.split_once('/') {
            Some((h, r)) => (h, Some(r)),
            None => (s, None),
        };
        Some(match (head, rest) {
            ("home", None) | ("", None) => Route::Home,
            ("albums", None) => Route::Albums,
            ("artists", None) => Route::Artists,
            ("tracks", None) => Route::Tracks,
            ("genres", None) => Route::Genres,
            ("history", None) => Route::History,
            ("playlists", None) => Route::Playlists,
            ("liked", None) => Route::Liked,
            ("settings", None) => Route::Settings,
            ("search", None) => Route::Search,
            ("nowplaying", None) => Route::NowPlaying,
            ("playlist", Some(id)) if !id.is_empty() => Route::Playlist(id.into()),
            ("album", Some(id)) if !id.is_empty() => Route::Album(id.into()),
            ("artist", Some(id)) if !id.is_empty() => Route::Artist(id.into()),
            ("genre", Some(n)) if !n.is_empty() => Route::Genre(decode_component(n)),
            _ => return None,
        })
    }
}

/// `encodeURIComponent`: everything except `A-Z a-z 0-9 - _ . ! ~ * ' ( )`.
pub fn encode_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `decodeURIComponent` (invalid escapes are kept literally).
pub fn decode_component(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Browser-like history: navigating drops the forward entries; navigating to the
/// route already shown is a no-op.
#[derive(Debug, Clone)]
pub struct Router {
    stack: Vec<Route>,
    pos: usize,
}

impl Router {
    pub fn new(start: Route) -> Self {
        Self { stack: vec![start], pos: 0 }
    }
    pub fn current(&self) -> &Route {
        &self.stack[self.pos]
    }
    /// Returns true when the current route changed.
    pub fn navigate(&mut self, r: Route) -> bool {
        if *self.current() == r {
            return false;
        }
        self.stack.truncate(self.pos + 1);
        self.stack.push(r);
        self.pos += 1;
        true
    }
    pub fn can_back(&self) -> bool {
        self.pos > 0
    }
    pub fn can_forward(&self) -> bool {
        self.pos + 1 < self.stack.len()
    }
    pub fn back(&mut self) -> bool {
        if self.can_back() {
            self.pos -= 1;
            true
        } else {
            false
        }
    }
    pub fn forward(&mut self) -> bool {
        if self.can_forward() {
            self.pos += 1;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_path_round_trip() {
        for r in [
            Route::Home,
            Route::Tracks,
            Route::NowPlaying,
            Route::Playlist("pl_3".into()),
            Route::Album("al_9".into()),
            Route::Genre("Drum & Bass".into()),
        ] {
            assert_eq!(Route::parse(&format!("#/{}", r.path())), Some(r.clone()), "{r:?}");
        }
        assert_eq!(Route::Genre("Drum & Bass".into()).path(), "genre/Drum%20%26%20Bass");
        assert_eq!(Route::parse("#/genre/Drum%20%26%20Bass"), Some(Route::Genre("Drum & Bass".into())));
        assert_eq!(Route::parse("#/"), Some(Route::Home));
        assert_eq!(Route::parse("#/nope"), None);
        assert_eq!(Route::parse("#/playlist/"), None);
        assert_eq!(Route::parse("#/tracks/extra"), None);
    }

    #[test]
    fn history_back_forward_and_truncation() {
        let mut r = Router::new(Route::Home);
        assert!(!r.can_back() && !r.can_forward());
        assert!(r.navigate(Route::Tracks));
        assert!(!r.navigate(Route::Tracks), "same route is a no-op");
        assert!(r.navigate(Route::Albums));
        assert_eq!(r.current(), &Route::Albums);
        assert!(r.back());
        assert_eq!(r.current(), &Route::Tracks);
        assert!(r.can_forward());
        assert!(r.forward());
        assert_eq!(r.current(), &Route::Albums);
        assert!(!r.forward());
        // navigating after going back discards the forward entries
        r.back();
        r.back();
        assert_eq!(r.current(), &Route::Home);
        assert!(r.navigate(Route::Genres));
        assert!(!r.can_forward());
        assert!(r.back());
        assert_eq!(r.current(), &Route::Home);
        assert!(!r.back());
    }
}
