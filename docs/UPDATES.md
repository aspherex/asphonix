# In-app updates (disabled by default)

Code: `crates/sonix-core/src/updates.rs` (port of the reference `updates.rs`, which used `tauri-plugin-updater`);
backend glue in `crates/sonix-real/src/services.rs`. "Notify, the user clicks": nothing installs by itself.

The native app has no release channel yet, so `UPDATE_ENDPOINT` and `UPDATE_PUBKEY` in `updates.rs` are empty.
While either is empty `updates_check` answers `available: false` (with the app version) WITHOUT any network
request, `updates_install` refuses ("No update available"), and the background checker is never started.

## Enable
1. Generate a minisign key pair, e.g. `npx tauri signer generate -w <key>` (keep the private key and password
   outside the repo, back them up: installed apps trust only this public key).
2. Set `UPDATE_PUBKEY` to the base64 text of the `.pub` file (what `tauri signer generate` prints) and
   `UPDATE_ENDPOINT` to an HTTPS URL of a static `latest.json`:
   `{"version":"0.2.0","notes":"...","pub_date":"2026-11-02T10:00:00Z","platforms":{"windows-x86_64":{"url":"https://.../sonix-setup.exe","signature":"<base64 of the .sig file>"}}}`
   (`windows-x86_64-nsis` is tried first). Plain HTTP is refused for the feed, the installer URL and redirects.
3. Sign each release installer with `tauri signer sign`; its trusted comment carries `version:X`, which must equal the
   feed's `version` (anti-rollback, same as the reference `requireSignedVersion`).
4. Rebuild. The Settings layer calls `RealBackend::set_auto_check_updates(true)` to start the 10 s / 6 h checker.

Install = download (512 MB cap), verify signature + signed version, persist the session, stop playback, then run the
NSIS setup with `/P /UPDATE /R` (passive, update mode, relaunch) and exit. Only a strictly newer semver is offered.
There is no NSIS installer for the native app yet; until there is one, leave the channel empty.

Tests (`cargo test -p sonix-core updates`) use a local `127.0.0.1` mock server, keys generated at test time and a
recording installer; nothing is downloaded from the network or executed.
