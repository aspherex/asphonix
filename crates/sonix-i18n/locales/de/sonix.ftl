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

## Track context menu (counts are appended as " (3 Titel)")

menu-clear-rating = Bewertung entfernen
menu-play-now = Jetzt abspielen
menu-play-next = Als Nächstes abspielen
menu-add-to-queue = Zur Warteschlange hinzufügen
menu-start-radio = Radio starten
menu-add-to-playlist = Zu Wiedergabeliste hinzufügen
menu-like = Gefällt mir
menu-unlike = Gefällt mir nicht mehr
menu-rate = Bewerten
menu-go-to-album = Zum Album
menu-go-to-artist = Zum Künstler
menu-remove-from-playlist = Aus dieser Wiedergabeliste entfernen
menu-remove-from-queue = Aus der Warteschlange entfernen
menu-track-info = Titelinfo
menu-copy-artist-title = „Künstler – Titel“ kopieren

## Flow

flow-menu = Flow
flow-steady = Gleichmäßig
flow-steady-hint = Sanfte Übergänge, gleichbleibende Energie
flow-build = Aufbauend
flow-build-hint = Energie steigt über je ~40 Titel an
flow-peak = Höhepunkt
flow-peak-hint = Ansteigen, dann abklingen
flow-hint-no-bpm-loudness = Noch keine BPM-Tags { $kind ->
    [playlist] in dieser Wiedergabeliste
    [artist] bei diesem Künstler
    [genre] in diesem Genre
    [view] in dieser Ansicht
   *[list] in dieser Liste
}: Flow sortiert nach Genre, Künstlerverteilung und Lautheit (ReplayGain bei { $pct } %)
flow-hint-no-bpm = Noch keine BPM-Tags { $kind ->
    [playlist] in dieser Wiedergabeliste
    [artist] bei diesem Künstler
    [genre] in diesem Genre
    [view] in dieser Ansicht
   *[list] in dieser Liste
}: Flow sortiert nach Genre und Künstlerverteilung
flow-hint-bpm = BPM bekannt für { $pct } % { $kind ->
    [playlist] dieser Wiedergabeliste
    [artist] dieses Künstlers
    [genre] dieses Genres
    [view] dieser Ansicht
   *[list] dieser Liste
}
flow-hint-key = Tonart { $pct } %

## Album / artist / playlist menus

menu-play = Abspielen
menu-play-all = Alle abspielen
menu-shuffle = Zufallswiedergabe
menu-star = Zu Favoriten hinzufügen
menu-unstar = Aus Favoriten entfernen
menu-album-radio = Album-Radio
menu-artist-radio = Künstler-Radio
menu-rename = Umbenennen
menu-duplicate = Duplizieren
menu-delete = Löschen
menu-edit-rules = Regeln bearbeiten…
playlist-readonly-hint = Schreibgeschützte Wiedergabeliste.

## Dialogs

dialog-cancel = Abbrechen
dialog-close = Schließen
dialog-create = Erstellen
dialog-new-playlist = Neue Wiedergabeliste
dialog-new-playlist-ellipsis = Neue Wiedergabeliste…
dialog-playlist-name = Name der Wiedergabeliste
dialog-rename-playlist = Wiedergabeliste umbenennen
dialog-rename-smart = Intelligente Wiedergabeliste umbenennen
dialog-delete-playlist = Wiedergabeliste löschen
dialog-delete-playlist-body = „{ $name }“ löschen? Das kann nicht rückgängig gemacht werden.
dialog-delete-smart = Intelligente Wiedergabeliste löschen
dialog-delete-smart-body = „{ $name }“ löschen? Nur die Regeln werden entfernt, niemals Titel.

## Update dialog

update-available-title = Update verfügbar
update-later = Später
update-install = Installieren & neu starten
update-playback-will-stop = Die Wiedergabe stoppt, während SONIX aktualisiert und neu startet.
update-downloading = Wird heruntergeladen…
update-downloading-pct = Wird heruntergeladen: { $pct } % · { $size }
update-installing = Wird geprüft und installiert…

## Track info dialog

info-format = Format
info-bit-depth = Bittiefe
info-bit-depth-value = { $bits } Bit
info-sample-rate = Abtastrate
info-bitrate = Bitrate
info-channels = Kanäle
info-mono = Mono
info-stereo = Stereo
info-duration = Dauer
info-file-size = Dateigröße
info-track-gain = Titel-Gain
info-album-gain = Album-Gain
info-album = Album
info-year = Jahr
info-genre = Genre

## Text field menu

kbd-ctrl = Strg
edit-undo = Rückgängig
edit-redo = Wiederholen
edit-cut = Ausschneiden
edit-copy = Kopieren
edit-paste = Einfügen
edit-select-all = Alles auswählen

## Backend errors (by error code)

error-not-connected = Nicht mit einem Server verbunden.
error-network = Der Server ist nicht erreichbar.
error-auth-failed = Benutzername oder Passwort falsch.
error-not-found = Nicht gefunden.
error-server = Der Server hat einen Fehler gemeldet.
error-io = Eine Datei konnte nicht gelesen oder geschrieben werden.
error-decode = Die Audiodatei konnte nicht dekodiert werden.
error-output-unavailable = Die Audioausgabe ist nicht verfügbar.
error-unimplemented = Von diesem Server nicht unterstützt.
error-invalid-request = Die Anfrage war ungültig.
error-unknown = Etwas ist schiefgelaufen.
