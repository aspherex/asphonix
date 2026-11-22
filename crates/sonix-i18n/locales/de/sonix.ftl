# SONIX UI strings: German.
#
# Style: neutral imperative without "du"/"Sie" ("Wiedergabeliste erstellen"); established music-app terms
# (Titel, Alben, Künstler, Wiedergabeliste, Warteschlange, Bibliothek, Lieblingssongs, Zufallswiedergabe).
# Quotes: „…“. Upper case only at render time.

## Counts (shared)

count-tracks = { $count ->
    [one] { $count } Titel
   *[other] { $count } Titel
}
count-albums = { $count ->
    [one] { $count } Album
   *[other] { $count } Alben
}
count-artists = { $count ->
    [one] { $count } Künstler
   *[other] { $count } Künstler
}
count-genres = { $count ->
    [one] { $count } Genre
   *[other] { $count } Genres
}
count-selected = { $count } ausgewählt
count-total = { $count } gesamt

## Connection state

status-connected = Verbunden
status-connecting = Verbinde
status-disconnected = Getrennt
status-error = Fehler

## Title bar / top bar

update-badge = Update { $version }
update-badge-tip = Eine neue Version ist verfügbar
topbar-back = Zurück · ALT+←
topbar-forward = Vorwärts · ALT+→
search-placeholder = Archiv durchsuchen
topbar-search-kbd = STRG K
topbar-stat-tracks = TIT
topbar-stat-albums = ALB
topbar-stat-artists = KÜN
theme-dark = Dunkles Design
theme-light = Helles Design

## Sidebar

nav-library = Bibliothek
nav-home = Start
nav-albums = Alben
nav-artists = Künstler
nav-tracks = Titel
nav-genres = Genres
nav-history = Verlauf
nav-playlists = Wiedergabelisten
nav-settings = Einstellungen
sidebar-new-smart = Neue intelligente Wiedergabeliste (lokal)
sidebar-new-playlist = Neue Wiedergabeliste
liked-songs = Lieblingssongs
sidebar-smart-local = Lokale intelligente Wiedergabeliste
playlist-smart-readonly = Intelligente Wiedergabeliste (schreibgeschützt)
playlist-readonly = Schreibgeschützte Wiedergabeliste
profile-switch = Profil wechseln
profile-not-signed-in = Nicht angemeldet
