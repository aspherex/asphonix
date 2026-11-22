# Contributing to ASPHONIX

Thanks for helping. Bug reports, server compatibility reports, docs fixes and code are all welcome.
By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).

## Before you open an issue

- Search existing issues first, and try the latest release.
- Use the issue forms: they ask for the details that are needed to reproduce playback and sync problems
  (OS, ASPHONIX version, server and version, output device and mode).
- Security problems go through [SECURITY.md](SECURITY.md), never public issues.

## Before you open a pull request

- For anything larger than a small fix, open an issue first so we can agree on the approach.
- Keep pull requests focused: one change per PR, no drive-by reformatting.

## Development setup

See [Build from source](README.md#build-from-source) for the toolchain and per-OS system packages, and
[CLAUDE.md](CLAUDE.md) for the architecture and project rules.

```sh
cargo build --profile fast -p asphonix-ui     # quick iteration build (release without LTO)
cargo test
cargo clippy --all-targets -- -D warnings
```

CI runs clippy, tests and a release build on Windows, macOS and Linux for every pull request.

## Ground rules

- **The `asphonix_api::Backend` trait is the only seam** between UI and backend. The UI never talks to the server
  or the audio engine directly.
- **Real data only.** Never show a number the app does not measure or the server does not provide.
- **Performance is a feature.** Changes to render paths, caching or the audio engine must keep the budgets in
  [docs/PERF.md](docs/PERF.md). Include before/after numbers from `scripts/perf.ps1` (or a clear argument why
  the change cannot affect them).
- **GPUI stays unpatched** (crates.io version, no fork). Work around gaps in our own code and document them in
  [docs/GPUI-GAPS.md](docs/GPUI-GAPS.md).
- **Tests never contact a real server.** Use the local mock HTTP servers in the existing tests.
- UI values (sizes, gaps, timings) live in `crates/asphonix-ui/src/metrics.rs`, colors in `theme.rs`.
- Comments explain *why*; commit messages follow `type(scope): summary` (e.g. `fix(audio): ...`).

## Logs for bug reports

- Crash log: `logs/crash.log` in the data directory (`%LOCALAPPDATA%\dev.asphonix.app` on Windows,
  `~/Library/Application Support/dev.asphonix.app` on macOS, `~/.local/share/dev.asphonix.app` on Linux).
- Detailed trace: start ASPHONIX with the environment variable `ASPHONIX_LOG=<path to a file>`.

Check logs for your server URL or user name before posting them publicly.
