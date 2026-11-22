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

## Durations, dates, relative times (shown upper-cased)

dur-hours-minutes = { $h } Std. { $m } Min.
dur-minutes = { $m } Min.
formats-mixed-rates = { $format } · gemischte Raten
volume-mute = Stumm
date-invalid = Ungültiges Datum
time-just-now = Gerade eben
time-seconds-ago = Vor { $n } s
time-minutes-ago = Vor { $n } Min.
time-hours-ago = Vor { $n } Std.
time-days-ago = Vor { $n } Tg.
day-today = Heute
day-yesterday = Gestern
day-header = { $weekday }, { $day }. { $month } { $year }
weekday-sun = So
weekday-mon = Mo
weekday-tue = Di
weekday-wed = Mi
weekday-thu = Do
weekday-fri = Fr
weekday-sat = Sa
month-jan = Jan.
month-feb = Feb.
month-mar = März
month-apr = Apr.
month-may = Mai
month-jun = Juni
month-jul = Juli
month-aug = Aug.
month-sep = Sept.
month-oct = Okt.
month-nov = Nov.
month-dec = Dez.

## Library: album and artist pages

album-play = Album abspielen
album-track-vectors = Titelvektoren
album-telemetry = Albumtelemetrie
album-meta-tracks = Titel
album-meta-plays = Wiedergaben
album-meta-added = Hinzugefügt
album-more-by-artist = { $artist } im Archiv
album-unavailable = Album nicht verfügbar.
artist-play-discography = Diskografie abspielen
flow-button-tip = Nach Tempo, Tonart und Genre ordnen, bitgenaue Wiedergabe bleibt erhalten
artist-most-played = Meistgespielt
artist-popular = Beliebt
artist-from-history = Aus dem Wiedergabeverlauf
artist-no-tracks = Keine Titel.
artist-about = Info
artist-show-less = Weniger anzeigen
artist-show-more = Mehr anzeigen
artist-no-bio = Keine Biografie.
artist-core-releases = Wichtigste Veröffentlichungen
artist-fans-also-like = Fans mögen auch
artist-unavailable = Künstler nicht verfügbar.

## Library: grids, genres, columns

sort-artist = Künstler
sort-newest = Neueste
sort-year = Jahr
sort-random = Zufällig
albums-starred-only = Nur Favoriten
genres-sort-most-tracks = Meiste Titel
genres-unavailable = Genres nicht verfügbar.
genres-empty = Noch keine Genre-Tags in der Bibliothek.
genre-no-tracks = Keine Titel mit dem Tag „{ $genre }“.
genre-no-albums = Keine Alben mit diesem Genre getaggt (Titel eventuell schon).
col-title = Titel
col-artist = Künstler
col-album = Album
col-year = Jahr
col-added = Hinzugefügt
col-format = Format
col-time = Zeit

## Home

home-welcome-back = Willkommen zurück
home-welcome-back-name = Willkommen zurück, { $name }
home-tracks-indexed = { $count } Titel indiziert
home-synced = Synchronisiert { $when }
home-not-connected = Nicht verbunden — in den Einstellungen einen Server hinzufügen
home-server-unreachable = Server nicht erreichbar — lokale Bibliothek wird angezeigt
kbd-space = Leertaste
kbd-enter = Eingabe
home-key-play-pause = Wiedergabe/Pause
home-key-scroll = Scrollen
home-key-or = oder
home-key-search = Suche
home-last-played-on = Zuletzt gespielt auf { $device }
home-queue-on-server = Warteschlange auf dem Server
home-resume-from-server = Vom Server fortsetzen
home-dismiss = Verwerfen
home-recently-ingested = Neu eingelesen // Server-Pipeline
home-unavailable = Nicht verfügbar, bis ein Server verbunden ist.
home-jump-back-in = Weitermachen // Sessions
home-view-history = Verlauf anzeigen
home-recently-played = Zuletzt gespielte Titel
home-heavy-rotation = Heavy Rotation
home-playlists-vectors = Wiedergabelisten // Vektoren

## Lists: login

login-username-placeholder = benutzername
login-server-url = Server-URL
login-username = Benutzername
login-password = Passwort
login-status-error = Status: Fehler
login-status-ok = Status: OK
login-latency = Latenz
login-host = Host
login-extensions = Erweiterungen
login-connecting = Verbinde…
login-connect = Initialisieren & Terminal öffnen
login-cancel = Abbrechen · zurück zum aktuellen Profil
login-engine = Engine
login-tagline = Hochleistungs-Audio-Workstation
login-idle = Bereit
login-status = Status

## Lists: profiles

profile-switched = Gewechselt zu { $name }
profile-switch-failed = Profilwechsel fehlgeschlagen: { $reason }
profile-removed = Profil entfernt
profile-add = Profil hinzufügen…
profile-sign-out = Abmelden

## Lists: playlists, liked, favorites, history

playlist-badge-smart = Intelligent
playlist-badge-readonly = Schreibgeschützt
playlists-new-smart = Neue intelligente Wiedergabeliste
playlists-smart-section = Intelligente Wiedergabelisten · lokal, auf diesem Gerät ausgewertet
playlists-server-section = Server-Wiedergabelisten
playlists-empty = Noch keine Wiedergabelisten.
playlist-empty = Noch keine Titel. Mit „Zu Wiedergabeliste hinzufügen“ bei einem beliebigen Titel füllen.
playlist-empty-readonly = Gerade keine Titel. Diese Wiedergabeliste ist schreibgeschützt und füllt sich selbst.
playlist-reorder-failed = Wiedergabeliste konnte nicht umsortiert werden
playlist-unavailable = Wiedergabeliste nicht verfügbar.
playlist-server-smart-note = Regelbasiert auf dem Server; der Inhalt aktualisiert sich automatisch.
playlist-owned-by = Gehört { $owner }.
playlist-owned-by-other = Gehört einem anderen Benutzer.
liked-empty = Noch nichts mit „Gefällt mir“ markiert. Während der Wiedergabe L drücken oder bei einem Titel auf das Herz tippen.
liked-pinned = Wiedergabeliste // angeheftet
liked-newest-first = Neueste zuerst
liked-starred-albums = Favorisierte Alben →
favorites-title = Favoriten-Kisten
favorites-to-liked = Favorisierte Titel → Lieblingssongs
favorites-empty = Noch keine favorisierten Alben — bei einem Album oder Titel auf den Stern tippen.
history-empty = Noch nichts hier. Titel, die über die Scrobble-Schwelle hinaus gehört werden, erscheinen hier.
history-clear = Verlauf löschen
history-clear-body = Alle aufgezeichneten Wiedergaben von diesem Gerät entfernen? Das kann nicht rückgängig gemacht werden.
history-clear-confirm = Löschen
history-recorded-locally = Lokal aufgezeichnet · letzte { $count } Wiedergaben
count-plays = { $count ->
    [one] { $count } Wiedergabe
   *[other] { $count } Wiedergaben
}
dialog-save = Speichern

## Lists: search

search-title = Indexsuche
search-tab-all = Alle
search-start = Tippen, um Titel, Alben, Künstler und Wiedergabelisten zu suchen.
search-searching = Suche läuft…
search-no-matches = Keine Treffer für „{ $query }“.
search-best-match = Bester Treffer
search-kind-track = Titel
search-kind-album = Album
search-kind-artist = Künstler
search-kind-playlist = Wiedergabeliste

## Smart playlists

smart-empty = Keine Titel entsprechen diesen Regeln.
smart-not-found = Intelligente Wiedergabeliste nicht gefunden.
smart-badge = Intelligente Wiedergabeliste
smart-badge-local = Lokal · nicht auf dem Server
smart-reroll = Zufallsauswahl neu würfeln
smart-edit-rules = Regeln bearbeiten
smart-edit-title = Intelligente Wiedergabeliste bearbeiten
smart-local-note = Nur lokal — auf diesem Gerät aus der synchronisierten Bibliothek ausgewertet; wird nie an den Server gesendet.
smart-preset-title = Mit einer Vorlage beginnen
smart-match = Treffer
smart-match-all = Alle Regeln
smart-match-any = Eine Regel
smart-add-rule = Regel hinzufügen
smart-no-rules = Keine Regeln: jeder Titel der Bibliothek passt.
smart-no-limit = kein Limit
smart-sort = Sortierung
smart-limit = Limit (optional)
smart-evaluating = Wird ausgewertet…
smart-matching = { $count ->
    [one] passender Titel
   *[other] passende Titel
}
smart-err-name = Bitte einen Namen eingeben.
smart-err-value = { $field }: Wert eingeben
smart-err-number = { $field }: Zahl eingeben
smart-err-limit = Das Limit muss eine ganze Zahl von 1 bis { $max } sein.
smart-err-save = Speichern fehlgeschlagen: { $reason }
smart-field-rating = Bewertung
smart-field-starred = Gefällt mir (markiert)
smart-field-play-count = Wiedergaben
smart-field-last-played = Zuletzt gespielt
smart-field-date-added = Zur Bibliothek hinzugefügt
smart-unit-plays = Wiedergaben
smart-unit-days = Tage
smart-unit-seconds = Sekunden
smart-placeholder-genre = z. B. Jazz
smart-placeholder-artist = z. B. Aphex Twin
smart-op-eq = ist genau
smart-op-after = nach
smart-op-gte = mindestens (≥)
smart-op-lte = höchstens (≤)
smart-op-unrated = ist unbewertet
smart-op-yes = ja
smart-op-no = nein
smart-op-never = nie
smart-op-older-than-days = vor mehr als … Tagen
smart-op-within-days = in den letzten … Tagen
smart-op-is = ist
smart-op-is-not = ist nicht
smart-op-contains = enthält
smart-sort-recently-added = Zuletzt hinzugefügt
smart-sort-recently-played = Zuletzt gespielt
smart-undefined = undefiniert
smart-desc-unrated = Unbewertet
smart-desc-liked = Gefällt mir
smart-desc-yes = Ja
smart-desc-not-liked = Gefällt mir nicht
smart-desc-never-played = Nie gespielt
smart-desc-older-than = { $field }: vor mehr als { $value } Tagen
smart-desc-within = { $field }: in den letzten { $value } Tagen
smart-desc-is = { $field } ist { $value }
smart-desc-is-not = { $field } ist nicht { $value }
smart-desc-contains = { $field } enthält „{ $value }“
smart-desc-all-tracks = Alle Titel
smart-desc-and = UND
smart-desc-or = ODER
smart-desc-max = max. { $count }
smart-desc-random = zufällig
smart-desc-by-recently-added = nach zuletzt hinzugefügt
smart-desc-by-most-played = nach meistgespielt
smart-desc-by-recently-played = nach zuletzt gespielt
smart-desc-by-title = nach Titel
smart-desc-by-artist = nach Künstler
smart-desc-by-rating = nach Bewertung
preset-hi-res-name = Nur Hi-Res
preset-hi-res-desc = Abtastrate von 88,2 kHz oder höher.
preset-24-bit-name = Nur 24 Bit
preset-24-bit-desc = Titel mit 24 Bit oder mehr Bittiefe.
preset-top-rated-name = Bestbewertet (4★+)
preset-top-rated-desc = Mit vier oder fünf Sternen bewertet, beste zuerst.
preset-stale-name = Seit 6 Monaten nicht gespielt
preset-stale-desc = Schon einmal gespielt, aber nicht in den letzten 180 Tagen. 200 zufällige Titel.
preset-recent-name = Zuletzt hinzugefügt
preset-recent-desc = In den letzten 30 Tagen zur Bibliothek hinzugefügt, neueste zuerst.
preset-never-played-name = Nie gespielt
preset-never-played-desc = Keine aufgezeichneten Wiedergaben. 500 zufällige Titel.
