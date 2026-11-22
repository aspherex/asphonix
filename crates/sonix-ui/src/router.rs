//! Routing: a route enum, a history stack with back/forward, and a path form of each
//! route for logs.

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
    /// `smart/:id` (local smart playlist)
    Smart(String),
    Favorites,
    Login,
}

impl Route {
    /// Path form of the route (`album/<id>`, `genre/<encoded name>`, ...).
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
            Route::Smart(id) => format!("smart/{id}"),
            Route::Favorites => "favorites".into(),
            Route::Login => "login".into(),
        }
    }
}

/// Percent-encodes everything except `A-Z a-z 0-9 - _ . ! ~ * ' ( )`.
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

/// The route a cold start opens: Home, or Tracks when the perf harness (`SONIX_SCRIPT`) drives the list.
pub fn initial_route(scripted: bool) -> Route {
    if scripted { Route::Tracks } else { Route::Home }
}

/// Back/forward history: navigating drops the forward entries; navigating to the
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
    fn cold_start_opens_home() {
        assert_eq!(initial_route(false), Route::Home);
        assert_eq!(initial_route(true), Route::Tracks, "the perf script drives the Tracks list");
    }

    #[test]
    fn path_encodes_genre_names() {
        assert_eq!(Route::Album("al_9".into()).path(), "album/al_9");
        assert_eq!(Route::Genre("Drum & Bass".into()).path(), "genre/Drum%20%26%20Bass");
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
