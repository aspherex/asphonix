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

## Track context menu (counts are appended as " (3 tracks)")

menu-clear-rating = Clear rating
menu-play-now = Play now
menu-play-next = Play next
menu-add-to-queue = Add to queue
menu-start-radio = Start radio
menu-add-to-playlist = Add to playlist
menu-like = Like
menu-unlike = Unlike
menu-rate = Rate
menu-go-to-album = Go to album
menu-go-to-artist = Go to artist
menu-remove-from-playlist = Remove from this playlist
menu-remove-from-queue = Remove from queue
menu-track-info = Track info
menu-copy-artist-title = Copy "Artist – Title"

## Flow

flow-menu = Flow
flow-steady = Steady
flow-steady-hint = Smooth transitions, even energy
flow-build = Build
flow-build-hint = Energy rises over each ~40 tracks
flow-peak = Peak
flow-peak-hint = Rise, then cool down
# $kind is an id (playlist, artist, genre, view, list), $pct a number or "<1"
flow-hint-no-bpm-loudness = No BPM tags in this { $kind ->
    [playlist] playlist
    [artist] artist
    [genre] genre
    [view] view
   *[list] list
} yet: Flow orders by genre, artist spread and loudness (ReplayGain on { $pct }%)
flow-hint-no-bpm = No BPM tags in this { $kind ->
    [playlist] playlist
    [artist] artist
    [genre] genre
    [view] view
   *[list] list
} yet: Flow orders by genre and artist spread
flow-hint-bpm = BPM known for { $pct }% of this { $kind ->
    [playlist] playlist
    [artist] artist
    [genre] genre
    [view] view
   *[list] list
}
flow-hint-key = key { $pct }%

## Album / artist / playlist menus

menu-play = Play
menu-play-all = Play all
menu-shuffle = Shuffle
menu-star = Star
menu-unstar = Unstar
menu-album-radio = Album radio
menu-artist-radio = Artist radio
menu-rename = Rename
menu-duplicate = Duplicate
menu-delete = Delete
menu-edit-rules = Edit rules…
playlist-readonly-hint = Read-only playlist.

## Dialogs

dialog-cancel = Cancel
dialog-close = Close
dialog-create = Create
dialog-new-playlist = New playlist
dialog-new-playlist-ellipsis = New playlist…
dialog-playlist-name = Playlist name
dialog-rename-playlist = Rename playlist
dialog-rename-smart = Rename smart playlist
dialog-delete-playlist = Delete playlist
dialog-delete-playlist-body = Delete "{ $name }"? This cannot be undone.
dialog-delete-smart = Delete smart playlist
dialog-delete-smart-body = Delete "{ $name }"? Only the rules are removed, never any tracks.

## Update dialog

update-available-title = Update available
update-later = Later
update-install = Install & restart
update-playback-will-stop = Playback will stop while SONIX updates and restarts.
update-downloading = Downloading…
update-downloading-pct = Downloading { $pct }% · { $size }
update-installing = Verifying & installing…

## Track info dialog

info-format = Format
info-bit-depth = Bit depth
info-bit-depth-value = { $bits }-bit
info-sample-rate = Sample rate
info-bitrate = Bitrate
info-channels = Channels
info-mono = Mono
info-stereo = Stereo
info-duration = Duration
info-file-size = File size
info-track-gain = Track gain
info-album-gain = Album gain
info-album = Album
info-year = Year
info-genre = Genre

## Text field menu

kbd-ctrl = Ctrl
edit-undo = Undo
edit-redo = Redo
edit-cut = Cut
edit-copy = Copy
edit-paste = Paste
edit-select-all = Select all

## Backend errors (by error code)

error-not-connected = Not connected to a server.
error-network = The server could not be reached.
error-auth-failed = Wrong username or password.
error-not-found = Not found.
error-server = The server reported an error.
error-io = A file could not be read or written.
error-decode = The audio file could not be decoded.
error-output-unavailable = The audio output is not available.
error-unimplemented = Not supported by this server.
error-invalid-request = The request was invalid.
error-unknown = Something went wrong.

## Durations, dates, relative times (shown upper-cased)

dur-hours-minutes = { $h } hr { $m } min
dur-minutes = { $m } min
formats-mixed-rates = { $format } · mixed rates
volume-mute = Mute
date-invalid = Invalid date
time-just-now = Just now
time-seconds-ago = { $n }s ago
time-minutes-ago = { $n }min ago
time-hours-ago = { $n }h ago
time-days-ago = { $n }d ago
day-today = Today
day-yesterday = Yesterday
# history day header; $day and $year are plain digits
day-header = { $weekday } { $day } { $month } { $year }
weekday-sun = Sun
weekday-mon = Mon
weekday-tue = Tue
weekday-wed = Wed
weekday-thu = Thu
weekday-fri = Fri
weekday-sat = Sat
month-jan = Jan
month-feb = Feb
month-mar = Mar
month-apr = Apr
month-may = May
month-jun = Jun
month-jul = Jul
month-aug = Aug
month-sep = Sept
month-oct = Oct
month-nov = Nov
month-dec = Dec

## Library: album and artist pages

album-play = Play album
album-track-vectors = Track Vectors
album-telemetry = Album Telemetry
album-meta-tracks = Tracks
album-meta-plays = Plays
album-meta-added = Added
album-more-by-artist = { $artist } in Vault
album-unavailable = Album unavailable.
artist-play-discography = Play discography
flow-button-tip = Order by tempo, key and genre, keeping bit-perfect playback
artist-most-played = Most played
artist-popular = Popular
artist-from-history = From your listening history
artist-no-tracks = No tracks.
artist-about = About
artist-show-less = Show less
artist-show-more = Show more
artist-no-bio = No biography.
artist-core-releases = Core Releases
artist-fans-also-like = Fans also like
artist-unavailable = Artist unavailable.

## Library: grids, genres, columns

sort-artist = Artist
sort-newest = Newest
sort-year = Year
sort-random = Random
albums-starred-only = Starred only
genres-sort-most-tracks = Most tracks
genres-unavailable = Genres unavailable.
genres-empty = No genre tags in your library yet.
genre-no-tracks = No tracks tagged “{ $genre }”.
genre-no-albums = No albums are tagged with this genre (tracks may still be).
col-title = Title
col-artist = Artist
col-album = Album
col-year = Year
col-added = Added
col-format = Format
col-time = Time

## Home

home-welcome-back = Welcome back
home-welcome-back-name = Welcome back, { $name }
home-tracks-indexed = { $count ->
    [one] { $count } track indexed
   *[other] { $count } tracks indexed
}
home-synced = Synced { $when }
home-not-connected = Not connected — head to Settings to add a server
home-server-unreachable = Server unreachable — browsing local library
kbd-space = Space
kbd-enter = Enter
home-key-play-pause = Play/Pause
home-key-scroll = Scroll
home-key-or = or
home-key-search = Search
home-last-played-on = Last played on { $device }
home-queue-on-server = Queue on server
home-resume-from-server = Resume from server
home-dismiss = Dismiss
home-recently-ingested = Recently Ingested // Server Pipeline
home-unavailable = Unavailable until a server is connected.
home-jump-back-in = Jump Back In // Sessions
home-view-history = View history
home-recently-played = Recently Played Tracks
home-heavy-rotation = Heavy Rotation
home-playlists-vectors = Playlists // Vectors

## Lists: login

login-username-placeholder = username
login-server-url = Server URL
login-username = Username
login-password = Password
login-status-error = Status: error
login-status-ok = Status: OK
login-latency = Latency
login-host = Host
login-extensions = Extensions
login-connecting = Connecting…
login-connect = Initialize & enter terminal
login-cancel = Cancel · back to current profile
login-engine = Engine
login-tagline = High-performance audio workstation
login-idle = Idle
login-status = Status

## Lists: profiles

profile-switched = Switched to { $name }
profile-switch-failed = Could not switch profile: { $reason }
profile-removed = Profile removed
profile-add = Add profile…
profile-sign-out = Sign out

## Lists: playlists, liked, favorites, history

playlist-badge-smart = Smart
playlist-badge-readonly = Read-only
playlists-new-smart = New smart playlist
playlists-smart-section = Smart playlists · local, evaluated on this machine
playlists-server-section = Server playlists
playlists-empty = No playlists yet.
playlist-empty = No tracks yet. Use "Add to playlist" from any track to fill this up.
playlist-empty-readonly = No tracks right now. This playlist is read-only and fills itself.
playlist-reorder-failed = Could not reorder playlist
playlist-unavailable = Playlist unavailable.
playlist-server-smart-note = Rule-based on the server; contents update automatically.
playlist-owned-by = Owned by { $owner }.
playlist-owned-by-other = Owned by another user.
liked-empty = Nothing liked yet. Press L while a track plays, or tap the heart on any track.
liked-pinned = Playlist // pinned
liked-newest-first = Newest first
liked-starred-albums = Starred albums →
favorites-title = Starred Crates
favorites-to-liked = Starred tracks → Liked Songs
favorites-empty = No starred albums yet — tap the star on any album or track.
history-empty = Nothing here yet. Tracks you listen to (past the scrobble threshold) appear here.
history-clear = Clear history
history-clear-body = Remove all recorded plays from this device? This cannot be undone.
history-clear-confirm = Clear
history-recorded-locally = Recorded locally · last { $count } plays
count-plays = { $count ->
    [one] { $count } play
   *[other] { $count } plays
}
dialog-save = Save

## Lists: search

search-title = Index Search
search-tab-all = All
search-start = Start typing to search tracks, albums, artists and playlists.
search-searching = Searching…
search-no-matches = No matches for "{ $query }".
search-best-match = Best match
search-kind-track = track
search-kind-album = album
search-kind-artist = artist
search-kind-playlist = playlist

## Smart playlists

smart-empty = No tracks match these rules.
smart-not-found = Smart playlist not found.
smart-badge = Smart playlist
smart-badge-local = Local · not on server
smart-reroll = Re-roll the random pick
smart-edit-rules = Edit rules
smart-edit-title = Edit smart playlist
smart-local-note = Local only — evaluated on this machine from your synced library; never sent to the server.
smart-preset-title = Start from a preset
smart-match = Match
smart-match-all = All rules
smart-match-any = Any rule
smart-add-rule = Add rule
smart-no-rules = No rules: every track in your library matches.
smart-no-limit = no limit
smart-sort = Sort
smart-limit = Limit (optional)
smart-evaluating = Evaluating…
smart-matching = { $count ->
    [one] matching track
   *[other] matching tracks
}
smart-err-name = Give it a name.
smart-err-value = { $field }: enter a value
smart-err-number = { $field }: enter a number
smart-err-limit = Limit must be a whole number from 1 to { $max }.
smart-err-save = Could not save: { $reason }
smart-field-rating = Rating
smart-field-starred = Liked (starred)
smart-field-play-count = Play count
smart-field-last-played = Last played
smart-field-date-added = Added to library
smart-unit-plays = plays
smart-unit-days = days
smart-unit-seconds = seconds
smart-placeholder-genre = e.g. Jazz
smart-placeholder-artist = e.g. Aphex Twin
smart-op-eq = is exactly
smart-op-after = after
smart-op-gte = at least (≥)
smart-op-lte = at most (≤)
smart-op-unrated = is unrated
smart-op-yes = yes
smart-op-no = no
smart-op-never = never
smart-op-older-than-days = more than … days ago
smart-op-within-days = within the last … days
smart-op-is = is
smart-op-is-not = is not
smart-op-contains = contains
smart-sort-recently-added = Recently added
smart-sort-recently-played = Recently played
smart-undefined = undefined
smart-desc-unrated = Unrated
smart-desc-liked = Liked
smart-desc-yes = Yes
smart-desc-not-liked = Not liked
smart-desc-never-played = Never played
smart-desc-older-than = { $field }: more than { $value } days ago
smart-desc-within = { $field }: within { $value } days
smart-desc-is = { $field } is { $value }
smart-desc-is-not = { $field } is not { $value }
smart-desc-contains = { $field } contains "{ $value }"
smart-desc-all-tracks = All tracks
smart-desc-and = AND
smart-desc-or = OR
smart-desc-max = max { $count }
smart-desc-random = random
smart-desc-by-recently-added = by recently added
smart-desc-by-most-played = by most played
smart-desc-by-recently-played = by recently played
smart-desc-by-title = by title
smart-desc-by-artist = by artist
smart-desc-by-rating = by rating
preset-hi-res-name = Hi-Res only
preset-hi-res-desc = Sampling rate of 88.2 kHz or higher.
preset-24-bit-name = 24-bit only
preset-24-bit-desc = Tracks with a 24-bit or deeper sample format.
preset-top-rated-name = Top rated (4★+)
preset-top-rated-desc = Rated four or five stars, best first.
preset-stale-name = Not played in 6 months
preset-stale-desc = Played before, but not within the last 180 days. 200 random picks.
preset-recent-name = Recently added
preset-recent-desc = Added to the library in the last 30 days, newest first.
preset-never-played-name = Never played
preset-never-played-desc = No recorded plays. 500 random picks.
