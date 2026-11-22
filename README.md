<div align="center">

# SONIX//

**A native desktop player for your Navidrome and Subsonic library.**<br>
Rust + GPUI, no WebView. Bit-perfect on Windows. Built to stay out of your RAM.

[![Build](https://github.com/aspherex/sonix/actions/workflows/build.yml/badge.svg)](https://github.com/aspherex/sonix/actions/workflows/build.yml)
[![Release](https://img.shields.io/github/v/release/aspherex/sonix?include_prereleases&color=d4ff00&labelColor=0a0a0c)](https://github.com/aspherex/sonix/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-d4ff00?labelColor=0a0a0c)](LICENSE)
![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20macOS%20%7C%20Linux-d4ff00?labelColor=0a0a0c)

<picture>
  <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/now-playing-light.png">
  <img alt="SONIX Now Playing: cover, waveform seek bar, 28-band spectrum, peak meters and the active output mode" src="docs/screenshots/now-playing-dark.png" width="100%">
</picture>

</div>

SONIX streams your library from a [Navidrome](https://www.navidrome.org/) (or other Subsonic / OpenSubsonic)
server and plays it through a native UI: a dark "audio terminal" with real readouts instead of decoration.
Every number on screen is either measured by the app or reported by your server.

> SONIX is not related to Sonixd, the earlier Electron client by the Feishin authors.

## Why SONIX

- **An honest signal path.** On Windows, files play bit-perfect through WASAPI exclusive mode at their native
  sample rate, gapless. The header always tells you what is really happening (format, sample rate, exclusive or
  shared mode), and a fallback to shared mode is shown as such, never hidden.
- **Native and lean.** No browser engine: a Rust + [GPUI](https://www.gpui.rs/) UI with hard budgets
  (<= 40 MB private working set in normal use, a few MB when minimized, ~300 ms to a visible window,
  refresh-rate scrolling). See [docs/PERF.md](docs/PERF.md) for the budgets and how they are measured.
- **Instant library.** A local SQLite (FTS5) mirror of your server makes browsing, sorting and search instant,
  even for libraries with tens of thousands of tracks.
- **Your music, your machine.** No telemetry, no accounts, no network traffic beyond your own server
  (and nothing else unless you turn it on: scrobbling, update checks).

## Features

- **Playback:** gapless, WASAPI exclusive / shared output, ReplayGain (optional), A-B loop, +-10 s skips,
  session resume, optional play-queue sync with the server.
- **Library:** Home, Albums, Artists, Tracks, Genres, History, playlists, Liked Songs, ratings and stars,
  local smart playlists with a rule editor, full-text search across everything.
- **Now Playing:** waveform seek bar, 28-band FFT spectrum, peak meters, synced lyrics.
- **Queue:** rolling queue window, drag-and-drop reordering, shuffle with artist spread, **Flow** ordering
  (by tempo, key and genre where your tags provide them), autoplay / radio when the queue ends.
- **Desktop:** Windows media controls and media keys, scrobbling to your server, light and dark theme,
  multiple server profiles, keyboard-first (`Space`, `J`/`K`, `Ctrl+K`, `N`, `L`, ...).
- **MCP bridge (opt-in):** lets Claude Code or any MCP client browse your library and, only if you allow it,
  control playback. See [MCP bridge](#mcp-bridge).

<table>
  <tr>
    <td><a href="docs/screenshots/home.png"><img src="docs/screenshots/home.png" alt="Home: recent albums and sessions"></a></td>
    <td><a href="docs/screenshots/albums.png"><img src="docs/screenshots/albums.png" alt="Albums grid"></a></td>
  </tr>
  <tr>
    <td><a href="docs/screenshots/tracks.png"><img src="docs/screenshots/tracks.png" alt="Tracks table with format column"></a></td>
    <td><a href="docs/screenshots/search.png"><img src="docs/screenshots/search.png" alt="Instant search"></a></td>
  </tr>
</table>

<sub>Screenshots use the public <a href="https://demo.navidrome.org">Navidrome demo</a> library; its playlists
are created by other demo visitors.</sub>

## Servers

- **Tested:** Navidrome 0.6x.
- **Should work** (Subsonic API 1.16.1 / OpenSubsonic): Gonic, LMS, Ampache, Airsonic-Advanced and others.
  Features such as synced lyrics depend on what the server supports. Reports from other servers are welcome.

## Platforms

| | Windows 10/11 | macOS (Apple Silicon) | Linux (x64) |
|---|---|---|---|
| Status | primary | newer, less tested | newer, less tested |
| Output | WASAPI exclusive (bit-perfect) or shared | shared (CoreAudio via cpal) | shared (ALSA via cpal) |
| Media keys / OS now-playing | yes (SMTC) | – | – |
| Password storage | Credential Manager | Keychain | Secret Service (GNOME Keyring, KWallet) |

GPUI has no accessibility support yet, so screen readers see an unlabeled window
(see [docs/GPUI-GAPS.md](docs/GPUI-GAPS.md)).

## Install

Download the archive for your platform from [Releases](https://github.com/aspherex/sonix/releases).
The builds are not code-signed yet:

- **Windows:** unzip and run `sonix.exe`. SmartScreen may warn on first start ("More info" > "Run anyway").
- **macOS:** open the `.dmg` and drag `SONIX.app` to Applications. Gatekeeper blocks the first start:
  right-click > Open, or `xattr -dr com.apple.quarantine /Applications/SONIX.app`.
- **Linux:** unpack the `.tar.gz` and run `./sonix`. Needs a Secret Service provider (GNOME Keyring or KWallet)
  to remember your password.

On first start, enter your server URL, user name and password. The password goes into the OS keychain, never
into a file; settings, the session and the library database live in the platform config/data directories
(`%APPDATA%\dev.sonix.app` and `%LOCALAPPDATA%\dev.sonix.app` on Windows).

## Build from source

Requirements: a recent stable Rust toolchain (edition 2024), plus

- **Windows:** the MSVC build tools.
- **macOS:** Xcode (the full install: GPUI compiles its Metal shaders at build time).
- **Linux** (Debian/Ubuntu names): `pkg-config libasound2-dev libdbus-1-dev libfontconfig1-dev libfreetype6-dev
  libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev libx11-xcb-dev libxcb1-dev libvulkan-dev libzstd-dev`.

```sh
cargo run -p sonix-ui --release      # or: cargo build --release  ->  target/release/sonix(.exe)
cargo test
cargo clippy --all-targets -- -D warnings
```

`cargo build --release` also builds `sonix-mcp`; keep it next to `sonix` for the MCP bridge.

## MCP bridge

Off by default. Enable it in **Settings > Claude (MCP)**; the page shows the exact command to register it:

```sh
claude mcp add sonix -- "C:\path\to\sonix-mcp.exe"
```

The bridge only listens locally (a named pipe on Windows, a Unix socket elsewhere), proves its identity to the
client and requires a per-session token. Read tools (library, queue, history) work as soon as it is enabled;
playback control is a separate switch.

## FAQ

**The header says SHARED MODE. Why not exclusive?** Exclusive mode is Windows-only (check Settings > Output)
and needs a device that accepts the file's sample rate and bit depth, and no other app holding it exclusively.
If the device refuses, SONIX falls back to shared mode and says so.

**Does it work with Jellyfin or Plex?** No. SONIX speaks the Subsonic / OpenSubsonic API only.

**Where are the logs?** Crashes are written to `logs/crash.log` in the data directory
(`%LOCALAPPDATA%\dev.sonix.app` on Windows). For a detailed trace, start SONIX with `SONIX_LOG=<file>`.

**Does SONIX phone home?** No. Update checks are off unless a release channel is configured and you enable
them (see [docs/UPDATES.md](docs/UPDATES.md)).

## Contributing

Bug reports, server compatibility reports and pull requests are welcome. Please read
[CONTRIBUTING.md](CONTRIBUTING.md) and the [Code of Conduct](CODE_OF_CONDUCT.md); security issues go through
[SECURITY.md](SECURITY.md), not public issues.

```
crates/sonix-api    the Backend trait the UI talks to (types, commands, events)
crates/sonix-core   Subsonic client, library DB + sync, audio engine, scrobbling, session, MCP bridge, updater
crates/sonix-real   Backend implementation on top of sonix-core (+ cover-art cache)
crates/sonix-ui     the GPUI app, binary `sonix`
crates/sonix-mcp    MCP stdio server `sonix-mcp` that connects to a running SONIX
```

## License

MIT, see [LICENSE](LICENSE). The bundled fonts (Inter, JetBrains Mono, Space Grotesk) are under the SIL Open
Font License 1.1, see `assets/fonts/OFL-*.txt`.

Built on [GPUI](https://www.gpui.rs/) (Zed Industries), [Symphonia](https://github.com/pdeljanov/Symphonia),
[rusqlite](https://github.com/rusqlite/rusqlite) and the [OpenSubsonic](https://opensubsonic.netlify.app/) API.
