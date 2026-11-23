# Releasing

Pipeline: `.github/workflows/release.yml`, started by a pushed tag `vX.Y.Z`. It ends with a **draft** release; nothing
is public until you press "Publish".

## Every release

1. Set the version in the root `Cargo.toml` (`[workspace.package] version = "X.Y.Z"`), run `cargo build` once so
   `Cargo.lock` follows, and commit.
2. Write the notes to `release-notes/vX.Y.Z.md` (Markdown; the first 8 KB also go into the update feed, so lead with
   what users notice). Without the file the draft gets GitHub's generated notes.
3. Tag and push: `git tag vX.Y.Z && git push origin vX.Y.Z`.
4. Watch Actions > Release. The pipeline refuses a tag that does not match `Cargo.toml`.
5. Check the draft (notes, assets, `SHA256SUMS.txt`), then publish it. With a releases repository configured, publish
   the mirrored draft there too: the in-app updater reads `releases/latest/download/latest.json`, so an update
   reaches users the moment that release is published.

Pre-releases (`X.Y.Z-rc.1`) are marked as such. GitHub's "latest" never points at a pre-release, so the updater does
not offer them.

Dry run: Actions > Release > Run workflow (any branch), or a pull request that changes the pipeline or `packaging/`,
builds and packages everything (unsigned: the release key is used for tag builds only) and uploads the assets as
a workflow artifact, without creating a release.

## Assets

| File | What |
|---|---|
| `ASPHONIX_X.Y.Z_x64-setup.exe` | Windows installer (NSIS, per user, no admin rights), `packaging/windows/asphonix.nsi` |
| `ASPHONIX_X.Y.Z_x64-setup.exe.sig` | minisign signature, trusted comment `version:X.Y.Z` (signed builds only) |
| `latest.json` | update feed for the in-app updater (signed builds only) |
| `asphonix-X.Y.Z-windows-x64.zip` | portable Windows build |
| `asphonix-X.Y.Z-macos-arm64.dmg` | macOS app (ad-hoc signed, not notarized). Optional: when GitHub has no macOS arm64 runner free, the release goes out without it (the run shows a warning); build it later with a dry run (Actions > Release > Run workflow on the tag) and attach it to the release |
| `asphonix-X.Y.Z-linux-x64.tar.gz` | Linux binaries |
| `SHA256SUMS.txt` | checksums of all of the above |

## One-time setup: signing and the update channel

Without this, releases are built unsigned and the in-app updater stays off in those builds (the pipeline warns).

1. **Key pair** (keep the secret key and a backup outside the repository; installed apps trust only this public key,
   losing it means every user has to reinstall by hand). With `rsign2` (`cargo install rsign2 --locked`, the
   minisign-compatible tool the pipeline signs with):

   ```
   rsign generate -W -p asphonix.pub -s asphonix.key
   ```

   `-W` creates the key without a password: CI cannot answer a prompt, the repository secret store is the protection.

   **Current key** (set up 2026-11-23): key id `1FA0ED03DFB7014A`, public key
   `RWRKAbffA+2gH9c3M74HQ+jRTUR8lP8oc1CvH+Hq6nNpPQet2WueXPkV`. The secret key is stored in the repository secret
   `MINISIGN_SECRET_KEY` and on the maintainer's machine; GitHub never shows a secret again, so that local copy and
   its backup are the only way to sign outside CI or to recover.
2. **Releases repository** (recommended while the source repository is private, because assets of a private repository
   cannot be downloaded without a login): a public repository holding only installers, signatures and feeds, like
   `sonix-releases` for the Tauri app. Create a fine-grained token with *Contents: read and write* on that repository
   only.
3. **Settings > Secrets and variables > Actions** of the source repository:

   | Kind | Name | Value |
   |---|---|---|
   | Secret | `MINISIGN_SECRET_KEY` | content of `asphonix.key` |
   | Variable | `ASPHONIX_UPDATE_PUBKEY` | `base64 -w0 asphonix.pub` |
   | Variable | `ASPHONIX_UPDATE_ENDPOINT` | `https://github.com/<owner>/<releases-repo>/releases/latest/download/latest.json` |
   | Variable | `ASPHONIX_RELEASES_REPO` | `<owner>/<releases-repo>` (omit to publish only in the source repository) |
   | Secret | `RELEASES_REPO_TOKEN` | the token from step 2 |

   The endpoint and public key are compiled into the Windows release build (`option_env!` in
   `crates/asphonix-core/src/updates.rs`); builds from source leave them empty and never contact anything.
4. Make a release. The `sign` job verifies the signature with the public key and the signed version, the same checks
   the updater runs before it installs anything.

## Not covered (yet)

- Authenticode signing of the Windows binaries (SmartScreen warns on first launch) and Apple notarization.
- A macOS/Linux update path: the in-app updater is Windows only.
