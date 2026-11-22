# SONIX

A fast, bit-perfect desktop music player for [Navidrome](https://www.navidrome.org/) and other
Subsonic/OpenSubsonic servers, built in Rust with [GPUI](https://www.gpui.rs/).

- **Bit-perfect playback**: original files are streamed untouched, decoded in Rust (symphonia) and sent to the
  device in WASAPI exclusive mode at their native sample rate, gapless. A shared-mode fallback is shown as such.
- **Lean by design**: a native UI with hard budgets (<= 40 MB private working set in normal use, a few MB when
  minimized, ~300 ms to a visible window, refresh-rate scrolling and animation; see [docs/PERF.md](docs/PERF.md)).
- **Library**: local SQLite (FTS5) mirror of the server library for instant browsing and search across Home,
  Albums, Artists, Tracks, Genres, History, playlists, Liked Songs and local smart playlists.
- **Now Playing**: waveform seek, spectrum and peak meters, synced lyrics, A-B loop, ratings.
- **Queue**: rolling queue window, shuffle with artist spread, Flow ordering (tempo/key/genre), autoplay/radio,
  drag-and-drop reordering, session resume and optional server play-queue sync.
- **Desktop integration**: Windows media controls (SMTC) and media keys, scrobbling, ReplayGain (optional),
  light and dark theme, multiple server profiles, a track-change card while the app is in the background.
- **Claude / MCP bridge** (off by default): `sonix-mcp` lets an MCP client such as Claude Code browse the library
  and, if you allow it, control playback.

## Status

CI builds SONIX for Windows (x64), macOS (Apple Silicon) and Linux (x64); tagged versions are published as
GitHub releases (unsigned: SmartScreen / Gatekeeper warn on first launch).

| | Windows 10/11 | macOS | Linux |
|---|---|---|---|
| Output | WASAPI exclusive (bit-perfect), shared fallback | shared mode (cpal) | shared mode (cpal / ALSA) |
| Media keys / OS now-playing | SMTC | – | – |
| Password storage | Credential Manager | Keychain | Secret Service (GNOME Keyring, KWallet) |
| Minimize handling (cache drop, working-set trim) | yes | – | – |

Windows is the primary platform; macOS and Linux run the same UI and backend but have seen far less testing.

GPUI currently has no accessibility support, so screen readers see an unlabeled window
(see [docs/GPUI-GAPS.md](docs/GPUI-GAPS.md)).

## Build

Requirements: a recent stable Rust toolchain (edition 2024), plus
- Windows: the MSVC build tools;
- macOS: Xcode (the full install: GPUI compiles its Metal shaders at build time);
- Linux (Debian/Ubuntu package names): `pkg-config libasound2-dev libdbus-1-dev libfontconfig1-dev
  libfreetype6-dev libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev libx11-xcb-dev libxcb1-dev libvulkan-dev
  libzstd-dev`.

```
cargo run -p sonix-ui --release        # or: cargo build --release -> target/release/sonix(.exe)
cargo test
cargo clippy --all-targets -- -D warnings
```

`cargo build --release` also produces `sonix-mcp`; keep it next to `sonix` for the MCP bridge.

On first start SONIX asks for a server URL, user name and password. The password is stored in the OS keychain,
never in a file. On Windows, settings, the session and the library database live under `%APPDATA%\dev.sonix.app`
and `%LOCALAPPDATA%\dev.sonix.app` (the platform config/data dirs elsewhere).

### MCP bridge

Enable it in Settings > Claude (MCP); the page shows the exact command to register it, e.g.

```
claude mcp add sonix -- "C:\path\to\sonix-mcp.exe"
```

The bridge talks to the running app over a local named pipe with a per-session token; playback control is a
separate opt-in.

## Layout

```
crates/sonix-api    the Backend trait the UI talks to (types, commands, events)
crates/sonix-core   Subsonic client, library DB + sync, audio engine, scrobbling, session, MCP bridge, updater
crates/sonix-real   Backend implementation on top of sonix-core (+ cover-art cache)
crates/sonix-ui     the GPUI app, binary `sonix`
crates/sonix-mcp    MCP stdio server `sonix-mcp` that connects to a running SONIX
crates/sonix-logging  structured JSON-lines logging (subscriber, daily files, runtime verbosity) for both binaries
assets/             fonts (TTF, SIL OFL), icons (SVG), pre-rendered backdrops
scripts/            perf harness and memory tools (docs/PERF.md), font fix-up script
docs/               PERF, GPUI-GAPS, UPDATES
```

The UI never talks to the server or the audio engine directly; everything goes through `sonix_api::Backend`.
Contributor notes are in [CLAUDE.md](CLAUDE.md).

## License

MIT, see [LICENSE](LICENSE). The bundled fonts (Inter, JetBrains Mono, Space Grotesk) are under the SIL Open Font
License 1.1, see `assets/fonts/OFL-*.txt`.
