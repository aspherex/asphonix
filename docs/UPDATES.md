# In-app updates (off until a release channel is configured)

Code: `crates/asphonix-core/src/updates.rs`, backend glue in `crates/asphonix-real/src/services.rs`.
The model is "notify, the user clicks": nothing installs by itself.

`UPDATE_ENDPOINT` and `UPDATE_PUBKEY` in `updates.rs` are empty in this repository. While either is empty,
`updates_check` answers `available: false` (with the app version) **without any network request**,
`updates_install` refuses ("No update available"), and the background checker never starts. Builds from source
therefore never phone home.

## Setting up a channel (for people who ship builds)

1. Generate a [minisign](https://jedisct1.github.io/minisign/) key pair, e.g. `minisign -G -p asphonix.pub -s asphonix.key`
   (or `rsign generate`). Keep the secret key and its password outside the repository and back them up: installed
   apps trust only this public key, so losing it means every user has to reinstall manually.
2. Set `UPDATE_PUBKEY` to the base64 of the whole `.pub` file (`base64 -w0 asphonix.pub`) and `UPDATE_ENDPOINT` to an
   HTTPS URL of a static `latest.json`:

   ```json
   {"version":"0.2.0","notes":"...","pub_date":"2026-11-02T10:00:00Z",
    "platforms":{"windows-x86_64":{"url":"https://.../asphonix-setup.exe","signature":"<base64 of the .minisig file>"}}}
   ```

   (`windows-x86_64-nsis` is tried first). Plain HTTP is refused for the feed, the installer URL and redirects.
3. Sign every installer with a trusted comment that carries its version, e.g.
   `minisign -S -s asphonix.key -m asphonix-setup.exe -t "version:0.2.0"`. The signed version must equal the feed's
   `version` (anti-rollback); only a strictly newer semver is offered.
4. Rebuild. Settings > "Check for updates automatically" starts the checker (10 s after start, then every 6 h).

Install = download (512 MB cap), verify signature + signed version, persist the session, stop playback, then run
the setup with `/P /UPDATE /R` (NSIS: passive, update mode, relaunch) and exit. The repository does not ship an
installer definition yet, so a channel also needs an NSIS installer that understands those switches.

Tests (`cargo test -p asphonix-core updates`) use a local `127.0.0.1` server, keys generated at test time and a
recording installer; nothing is downloaded from the network or executed.
