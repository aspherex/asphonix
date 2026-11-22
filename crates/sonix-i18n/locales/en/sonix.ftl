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
