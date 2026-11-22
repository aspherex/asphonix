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

## Settings

settings-on = An
settings-off = Aus
settings-panel-language = Sprache
settings-language = Anzeigesprache
settings-language-system = System
settings-panel-output = Ausgabe
settings-device = Gerät
settings-device-default = { $name } (Standard)
settings-mode = Modus
settings-mode-exclusive = Exklusiv
settings-mode-shared = Geteilt
settings-exclusive-unavailable = Exklusivmodus nicht verfügbar
settings-current-stream = Aktueller Stream
settings-panel-scrobbling = Scrobbling
settings-scrobble-submit = Wiedergaben an Navidrome übermitteln
settings-play-threshold = Wiedergabe zählen nach (Sekunden, für Verlauf und Scrobbles)
settings-panel-shuffle = Zufallswiedergabe
settings-spread-artists = Künstler bei Zufallswiedergabe verteilen
settings-queue-window = Warteschlangenfenster
settings-panel-playback = Wiedergabe
settings-autoplay = Ähnliche Musik abspielen, wenn die Warteschlange endet
settings-sync-queue = Warteschlange mit dem Server synchronisieren
settings-track-preview = Titelvorschau zeigen, wenn SONIX im Hintergrund ist
settings-track-preview-position = Position der Titelvorschau
settings-corner-top-left = Oben links
settings-corner-top-right = Oben rechts
settings-corner-bottom-left = Unten links
settings-corner-bottom-right = Unten rechts
settings-free-memory = Speicher beim Minimieren freigeben (lädt beim Wiederherstellen neu)
settings-replaygain = Lautstärkeangleichung (ReplayGain)
settings-replaygain-track = Titel
settings-replaygain-album = Album
settings-replaygain-hint = Wendet das ReplayGain-Tag der Datei als digitale Verstärkung an; solange eine Verstärkung aktiv ist, ist die Ausgabe nicht mehr bitgenau. Titel ohne Tag werden unverändert wiedergegeben.
settings-panel-server = Server
settings-server-url = URL
settings-server-user = Benutzer
settings-server-version = Serverversion
settings-api-version = API-Version
settings-ping = Ping
settings-tracks-indexed = Indizierte Titel
settings-artwork-cache = Cover-Cache
settings-panel-updates = Updates
settings-current-version = Aktuelle Version
settings-auto-updates = Automatisch nach Updates suchen
settings-checking = Wird geprüft…
settings-check-now = Jetzt prüfen
settings-update-available = Update { $version } verfügbar
settings-up-to-date = SONIX ist auf dem neuesten Stand
settings-update-check-failed = Suche nach Updates fehlgeschlagen
settings-panel-mcp = Claude (MCP)
settings-mcp-hint = Erlaubt Claude Code oder Claude Desktop, über eine rein lokale Verbindung (kein Netzwerkport) die Bibliothek, den Wiedergabeverlauf und die Warteschlange zu lesen. Alles, was Claude liest, wird als Teil der Unterhaltung an Claude (Anthropic) gesendet: Titel-, Album- und Künstlernamen, Wiedergabelisten, Verlauf und die aktuelle Wiedergabe. Serveradresse, Passwort und Dateipfade sieht Claude nie, und diese Version kann auf dem Server nichts ändern (keine Änderungen an Wiedergabelisten, Likes oder Bewertungen).
settings-mcp-enable = Claude meine Bibliothek lesen lassen
settings-mcp-control = Wiedergabesteuerung erlauben (Abspielen, Überspringen, Warteschlange, Lautstärke)
settings-mcp-connection = Verbindung
settings-mcp-listening = Empfangsbereit
settings-mcp-not-listening = Nicht empfangsbereit
settings-mcp-register = Einmalig in Claude Code registrieren:
settings-copied = Kopiert
settings-copy-command = Befehl kopieren
settings-mcp-missing = sonix-mcp wurde neben SONIX nicht gefunden. Der Windows-Installer legt es dort ab; eine Neuinstallation oder ein Update von SONIX behebt das. Entwicklungs-Builds und macOS: mit npm run build:mcp bauen (oder cargo build --release in tools/sonix-mcp) und unter diesem Pfad ablegen.
settings-logout = Abmelden

## Drag and drop, toasts, track-change card

drag-named-tracks = { $name } ({ $count } Titel)
drag-album = Album: { $name }
drag-artist = Künstler: { $name }
toast-added-to-playlist = { $what } zu „{ $playlist }“ hinzugefügt
toast-add-failed = Hinzufügen zu „{ $playlist }“ fehlgeschlagen
overlay-now-playing = Läuft gerade
