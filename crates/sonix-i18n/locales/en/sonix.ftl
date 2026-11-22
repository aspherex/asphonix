# SONIX UI strings: English (source locale).
#
# Rules:
# - ids are hierarchical: <area>-<thing>[-<detail>]; build.rs turns them into `Key` variants (nav-home -> NavHome).
# - normal sentence/title case only: upper-case styling is applied at render time, never written here.
# - symbols and format tags ("·", "--:--", "FLAC 24/96", "BPM", "dB") stay out of the files.
# - every message here must exist in every other locale with the same variables (tests/locales.rs).
# - numbers passed as variables are grouped per locale ("1,234" / "1.234") and select the plural form.

## Counts (shared)

count-tracks = { $count ->
    [one] { $count } track
   *[other] { $count } tracks
}
count-albums = { $count ->
    [one] { $count } album
   *[other] { $count } albums
}
count-artists = { $count ->
    [one] { $count } artist
   *[other] { $count } artists
}
count-genres = { $count ->
    [one] { $count } genre
   *[other] { $count } genres
}
count-selected = { $count } selected
count-total = { $count } total

## Connection state

status-connected = Connected
status-connecting = Connecting
status-disconnected = Disconnected
status-error = Error

## Title bar / top bar

update-badge = Update { $version }
update-badge-tip = A new version is available
topbar-back = Back · ALT+←
topbar-forward = Forward · ALT+→
search-placeholder = Search archive
topbar-search-kbd = CTRL K
# abbreviated collection counts next to the numbers (keep about three letters)
topbar-stat-tracks = TRK
topbar-stat-albums = ALB
topbar-stat-artists = ART
theme-dark = Dark mode
theme-light = Light mode

## Sidebar

nav-library = Library
nav-home = Home
nav-albums = Albums
nav-artists = Artists
nav-tracks = Tracks
nav-genres = Genres
nav-history = History
nav-playlists = Playlists
nav-settings = Settings
sidebar-new-smart = New smart playlist (local)
sidebar-new-playlist = New playlist
liked-songs = Liked Songs
sidebar-smart-local = Local smart playlist
playlist-smart-readonly = Smart playlist (read-only)
playlist-readonly = Read-only playlist
profile-switch = Switch profile
profile-not-signed-in = Not signed in

## Settings

settings-on = On
settings-off = Off
settings-panel-language = Language
settings-language = Display language
settings-language-system = System
settings-panel-output = Output
settings-device = Device
settings-device-default = { $name } (default)
settings-mode = Mode
settings-mode-exclusive = Exclusive
settings-mode-shared = Shared
settings-exclusive-unavailable = Exclusive unavailable
settings-current-stream = Current stream
settings-panel-scrobbling = Scrobbling
settings-scrobble-submit = Submit plays to Navidrome
settings-play-threshold = Count a play after (seconds, history and scrobbles)
settings-panel-shuffle = Shuffle
settings-spread-artists = Spread artists in shuffle
settings-queue-window = Queue window
settings-panel-playback = Playback
settings-autoplay = Autoplay similar music when the queue ends
settings-sync-queue = Sync play queue with server
settings-track-preview = Show track preview when SONIX is in the background
settings-track-preview-position = Track preview position
settings-corner-top-left = Top left
settings-corner-top-right = Top right
settings-corner-bottom-left = Bottom left
settings-corner-bottom-right = Bottom right
settings-free-memory = Free memory when minimized (UI reloads on restore)
settings-replaygain = Volume normalization (ReplayGain)
settings-replaygain-track = Track
settings-replaygain-album = Album
settings-replaygain-hint = Applies the file's ReplayGain tag as digital gain, so output is no longer bit-perfect while a gain is applied. Tracks without a tag play unchanged.
settings-panel-server = Server
settings-server-url = URL
settings-server-user = User
settings-server-version = Server version
settings-api-version = API version
settings-ping = Ping
settings-tracks-indexed = Tracks indexed
settings-artwork-cache = Artwork cache
settings-panel-updates = Updates
settings-current-version = Current version
settings-auto-updates = Check for updates automatically
settings-checking = Checking…
settings-check-now = Check now
settings-update-available = Update { $version } available
settings-up-to-date = You are up to date
settings-update-check-failed = Could not check for updates
settings-panel-mcp = Claude (MCP)
settings-mcp-hint = Lets Claude Code or Claude Desktop read your library, listening history and play queue through a local-only connection (no network port). Everything Claude reads is sent to Claude (Anthropic) as part of your conversation: track, album and artist names, playlists, play history, what is playing. It never sees your server address, password or file paths, and this version cannot change anything on the server (no playlist edits, likes or ratings).
settings-mcp-enable = Let Claude read my library
settings-mcp-control = Allow playback control (play, skip, queue, volume)
settings-mcp-connection = Connection
settings-mcp-listening = Listening
settings-mcp-not-listening = Not listening
settings-mcp-register = Register it once in Claude Code:
settings-copied = Copied
settings-copy-command = Copy command
settings-mcp-missing = sonix-mcp was not found next to SONIX. The Windows installer puts it there, so reinstalling or updating SONIX fixes this. Development builds and macOS: build it with npm run build:mcp (or cargo build --release in tools/sonix-mcp) and put it at that path.
settings-logout = Log out

## Drag and drop, toasts, track-change card

drag-named-tracks = { $name } ({ $count ->
    [one] { $count } track
   *[other] { $count } tracks
})
drag-album = album: { $name }
drag-artist = artist: { $name }
toast-added-to-playlist = Added { $what } to "{ $playlist }"
toast-add-failed = Could not add to "{ $playlist }"
overlay-now-playing = Now playing
