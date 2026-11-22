//! In-app updates: "notify, the user clicks" - nothing here ever installs on its own.
//!
//! The mechanism is a minisign-signed update feed:
//!
//! - The feed is a static `latest.json`
//!   (`{"version", "notes", "pub_date", "platforms": {"windows-x86_64": {"url", "signature"}}}`) at an
//!   HTTPS URL. Plain HTTP is refused (loopback HTTP only in this module's own tests).
//! - The installer is verified against the minisign public key (`signature` = base64 of the `.sig` file,
//!   public key = base64 of the minisign `.pub` file) BEFORE anything is
//!   executed, and the version in the signed trusted comment (`version:X`) must equal the version the
//!   feed announced (so a tampered feed can't pair a new version with an old,
//!   validly signed installer).
//! - Only a strictly newer version is ever offered ([`is_newer`]).
//! - The UI reaches the updater only through `updates_check` / `updates_install`, which take no
//!   URL/path arguments; the endpoint and key are compile-time constants below.
//!
//! DISABLED BY DEFAULT: [`UPDATE_ENDPOINT`] is empty until a release channel
//! exists. With an empty endpoint or key, `check` answers "no update" without touching the network and
//! `install` refuses. To enable: see `docs/UPDATES.md`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::ipc::{IpcError, IpcErrorCode};

/// Feed URL (HTTPS `latest.json`). EMPTY = updates disabled, no request is ever made.
pub const UPDATE_ENDPOINT: &str = "";
/// Minisign public key, base64 of the whole minisign `.pub` file.
/// Empty = updates disabled.
pub const UPDATE_PUBKEY: &str = "";

/// First background check after startup, then every `CHECK_INTERVAL`.
pub const FIRST_CHECK_DELAY: Duration = Duration::from_secs(10);
pub const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Release notes are shown as plain text in a small dialog; cap what we pass to the UI.
const MAX_NOTES_BYTES: usize = 8 * 1024;
/// A hostile endpoint must not make us buffer without bound.
const MAX_MANIFEST_BYTES: usize = 1 << 20;
const MAX_INSTALLER_BYTES: usize = 512 << 20;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub available: bool,
    pub current_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Release notes, PLAIN TEXT (never rendered as markup).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Release date, `YYYY-MM-DD`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

/// Strictly-newer rule (no downgrade, no reinstall of the same version).
/// Pre-release ordering follows semver (`0.3.0-beta.1 < 0.3.0`).
pub fn is_newer(current: &Version, remote: &Version) -> bool {
    remote > current
}

fn truncate_notes(notes: &str) -> String {
    if notes.len() <= MAX_NOTES_BYTES {
        return notes.to_string();
    }
    let mut end = MAX_NOTES_BYTES;
    while !notes.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[...]", &notes[..end])
}

fn updater_err(message: &str) -> IpcError {
    IpcError { code: IpcErrorCode::Network, message: message.to_string() }
}

// --- manifest ---------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    pub_date: Option<String>,
    #[serde(default)]
    platforms: std::collections::HashMap<String, PlatformEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct PlatformEntry {
    url: String,
    signature: String,
}

/// What the last check found; `install` installs exactly this.
#[derive(Debug, Clone)]
struct Pending {
    version: String,
    url: String,
    signature: String,
}

// --- signature ----------------------------------------------------------------------------------

fn b64_to_string(s: &str) -> Result<String, String> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(s.trim()).map_err(|e| format!("bad base64: {e}"))?;
    String::from_utf8(bytes).map_err(|_| "not UTF-8".to_string())
}

/// The `version:` field of the signature's trusted comment (`timestamp:..\tfile:..\tversion:1.2.3`).
fn signed_version(trusted_comment: &str) -> Option<&str> {
    trusted_comment.split('\t').find_map(|f| f.strip_prefix("version:"))
}

/// The signature must carry a version and it must equal the announced one
/// (semver-equal, so `v1.2.3` == `1.2.3`).
fn verify_signed_version(trusted_comment: &str, announced: &str) -> Result<(), String> {
    let Some(signed) = signed_version(trusted_comment) else {
        return Err("signature carries no version".into());
    };
    let same = match (Version::parse(signed.trim_start_matches('v')), Version::parse(announced.trim_start_matches('v'))) {
        (Ok(a), Ok(b)) => a == b,
        _ => signed == announced,
    };
    if same {
        Ok(())
    } else {
        Err(format!("signed version {signed} != announced {announced}"))
    }
}

/// Verifies `data` against the base64 minisign signature + base64 public key, then the signed version.
pub fn verify_signature(data: &[u8], signature_b64: &str, pubkey_b64: &str, announced_version: &str) -> Result<(), String> {
    let pk = minisign_verify::PublicKey::decode(&b64_to_string(pubkey_b64)?).map_err(|e| format!("public key: {e}"))?;
    let sig = minisign_verify::Signature::decode(&b64_to_string(signature_b64)?).map_err(|e| format!("signature: {e}"))?;
    pk.verify(data, &sig, true).map_err(|e| format!("verify: {e}"))?;
    // Only after `verify` is the trusted comment trustworthy (the global signature covers it).
    verify_signed_version(sig.trusted_comment(), announced_version)
}

// --- installer seam -----------------------------------------------------------------------------

/// Runs a verified installer. Production: [`NsisInstaller`] (starts it and exits the process). Tests: a recorder.
pub trait Installer: Send + Sync + 'static {
    fn install(&self, bytes: &[u8], version: &str) -> Result<(), String>;
}

/// Windows NSIS setup (`/P /UPDATE /R`: passive UI, update mode, relaunch afterwards). Writes the exe to a fresh temp dir, starts it with `ShellExecuteW`
/// (so a UAC prompt works) and then exits this process so the installer can replace the files.
pub struct NsisInstaller;

impl Installer for NsisInstaller {
    #[cfg(windows)]
    fn install(&self, bytes: &[u8], version: &str) -> Result<(), String> {
        use windows::core::{w, HSTRING};
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOW;
        let dir = tempfile::Builder::new().prefix(&format!("sonix-{version}-updater-")).tempdir().map_err(|e| e.to_string())?.keep();
        let path = dir.join("sonix-setup.exe");
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
        // SAFETY: all string arguments are valid NUL-terminated wide strings that outlive the call; no window handle is passed.
        let r = unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(path.as_os_str()), w!("/P /UPDATE /R"), None, SW_SHOW) };
        if r.0 as isize <= 32 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        std::process::exit(0);
    }
    #[cfg(not(windows))]
    fn install(&self, _bytes: &[u8], _version: &str) -> Result<(), String> {
        Err("in-app install is only implemented for Windows".into())
    }
}

// --- updater ------------------------------------------------------------------------------------

#[derive(Clone)]
pub struct UpdateConfig {
    endpoint: String,
    pubkey: String,
    current_version: String,
    /// Test only: allow `http://127.0.0.1` endpoints/downloads.
    #[cfg(test)]
    loopback_http: bool,
}

impl UpdateConfig {
    /// The compile-time channel ([`UPDATE_ENDPOINT`] / [`UPDATE_PUBKEY`]) for this crate's version.
    pub fn built_in() -> Self {
        Self::new(UPDATE_ENDPOINT, UPDATE_PUBKEY, env!("CARGO_PKG_VERSION"))
    }

    pub(crate) fn new(endpoint: &str, pubkey: &str, current_version: &str) -> Self {
        Self {
            endpoint: endpoint.trim().to_string(),
            pubkey: pubkey.trim().to_string(),
            current_version: current_version.to_string(),
            #[cfg(test)]
            loopback_http: false,
        }
    }

    pub fn is_enabled(&self) -> bool {
        !self.endpoint.is_empty() && !self.pubkey.is_empty()
    }

    fn url_allowed(&self, u: &url::Url) -> bool {
        if u.scheme() == "https" {
            return true;
        }
        #[cfg(test)]
        if self.loopback_http && u.scheme() == "http" && u.host_str() == Some("127.0.0.1") {
            return true;
        }
        false
    }
}

fn platform_keys() -> [String; 2] {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "x86" => "i686",
        other => other,
    };
    [format!("windows-{arch}-nsis"), format!("windows-{arch}")]
}

pub struct Updater {
    cfg: UpdateConfig,
    installer: Arc<dyn Installer>,
    pending: Mutex<Option<Pending>>,
    installing: AtomicBool,
}

impl Updater {
    pub fn new(cfg: UpdateConfig, installer: Arc<dyn Installer>) -> Self {
        Self { cfg, installer, pending: Mutex::new(None), installing: AtomicBool::new(false) }
    }

    /// The production updater: compile-time channel + NSIS installer.
    pub fn built_in() -> Self {
        Self::new(UpdateConfig::built_in(), Arc::new(NsisInstaller))
    }

    pub fn is_enabled(&self) -> bool {
        self.cfg.is_enabled()
    }

    fn none_info(&self) -> UpdateInfo {
        UpdateInfo { available: false, current_version: self.cfg.current_version.clone(), version: None, notes: None, date: None }
    }

    /// The update the last check found (`available: false` when none).
    pub fn pending_info(&self) -> UpdateInfo {
        match &*self.pending.lock().unwrap_or_else(|p| p.into_inner()) {
            Some(p) => UpdateInfo { available: true, version: Some(p.version.clone()), ..self.none_info() },
            None => self.none_info(),
        }
    }

    fn client(&self, timeout: Duration) -> Result<reqwest::Client, reqwest::Error> {
        let cfg = self.cfg.clone();
        reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::custom(move |a| {
                if a.previous().len() >= 5 || !cfg.url_allowed(a.url()) {
                    a.stop()
                } else {
                    a.follow()
                }
            }))
            .build()
    }

    async fn get_capped(&self, url: &str, max: usize, timeout: Duration, mut progress: impl FnMut(u64, Option<u64>)) -> Result<Vec<u8>, String> {
        use futures_util::StreamExt;
        let parsed = url::Url::parse(url).map_err(|e| e.to_string())?;
        if !self.cfg.url_allowed(&parsed) {
            return Err("refusing a non-HTTPS URL".into());
        }
        let resp = self.client(timeout).map_err(|e| e.to_string())?.get(parsed).send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        let total = resp.content_length();
        if total.is_some_and(|t| t > max as u64) {
            return Err("response too large".into());
        }
        let mut out = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            out.extend_from_slice(&chunk);
            if out.len() > max {
                return Err("response too large".into());
            }
            progress(out.len() as u64, total);
        }
        Ok(out)
    }

    /// One check. Disabled (empty endpoint/key) = "no update", NO network. Stores the result for `install`.
    pub async fn check(&self) -> Result<UpdateInfo, IpcError> {
        if !self.cfg.is_enabled() {
            return Ok(self.none_info());
        }
        match self.check_inner().await {
            Ok(info) => Ok(info),
            Err(e) => {
                log::debug!("update check failed: {e}");
                Err(updater_err("Could not check for updates"))
            }
        }
    }

    async fn check_inner(&self) -> Result<UpdateInfo, String> {
        let current = Version::parse(&self.cfg.current_version).map_err(|e| format!("own version: {e}"))?;
        let body = self.get_capped(&self.cfg.endpoint, MAX_MANIFEST_BYTES, REQUEST_TIMEOUT, |_, _| {}).await?;
        let m: Manifest = serde_json::from_slice(&body).map_err(|e| format!("manifest: {e}"))?;
        let remote = Version::parse(m.version.trim_start_matches('v')).map_err(|e| format!("manifest version: {e}"))?;
        if !is_newer(&current, &remote) {
            *self.pending.lock().unwrap_or_else(|p| p.into_inner()) = None;
            return Ok(self.none_info());
        }
        let entry = platform_keys().iter().find_map(|k| m.platforms.get(k).cloned()).ok_or("no build for this platform")?;
        let date = m.pub_date.as_deref().and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok()).map(|d| d.date_naive().to_string());
        let info = UpdateInfo {
            available: true,
            current_version: self.cfg.current_version.clone(),
            version: Some(m.version.clone()),
            notes: m.notes.as_deref().map(truncate_notes),
            date,
        };
        *self.pending.lock().unwrap_or_else(|p| p.into_inner()) = Some(Pending { version: m.version, url: entry.url, signature: entry.signature });
        Ok(info)
    }

    /// Downloads the update the last check found (re-checking when there is none), verifies its signature
    /// and signed version, runs `before_install` (persist the session, stop playback, release the audio
    /// device) and hands the verified bytes to the [`Installer`]. Only ever from an explicit user click.
    pub async fn install(&self, progress: impl FnMut(u64, Option<u64>), before_install: impl FnOnce()) -> Result<(), IpcError> {
        if !self.cfg.is_enabled() {
            return Err(updater_err("No update available"));
        }
        if self.installing.swap(true, Ordering::SeqCst) {
            return Err(updater_err("An update is already being installed"));
        }
        let r = self.install_inner(progress, before_install).await;
        if r.is_err() {
            self.installing.store(false, Ordering::SeqCst);
        }
        r
    }

    async fn install_inner(&self, progress: impl FnMut(u64, Option<u64>), before_install: impl FnOnce()) -> Result<(), IpcError> {
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if pending.is_none() {
            self.check_inner().await.map_err(|e| {
                log::debug!("update re-check failed: {e}");
                updater_err("Could not check for updates")
            })?;
            pending = self.pending.lock().unwrap_or_else(|p| p.into_inner()).clone();
        }
        let p = pending.ok_or_else(|| updater_err("No update available"))?;
        let bytes = self.get_capped(&p.url, MAX_INSTALLER_BYTES, DOWNLOAD_TIMEOUT, progress).await.map_err(|e| {
            log::debug!("update download failed: {e}");
            updater_err("Update download failed or could not be verified")
        })?;
        // Nothing is executed (or even written) before this passes.
        verify_signature(&bytes, &p.signature, &self.cfg.pubkey, &p.version).map_err(|e| {
            log::debug!("update verification failed: {e}");
            updater_err("Update download failed or could not be verified")
        })?;
        before_install();
        self.installer.install(&bytes, &p.version).map_err(|e| {
            log::debug!("update install failed: {e}");
            updater_err("Update could not be installed")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn only_strictly_newer_versions_are_offered() {
        assert!(is_newer(&v("0.2.1"), &v("0.2.2")));
        assert!(is_newer(&v("0.9.0"), &v("1.0.0")));
        assert!(is_newer(&v("0.3.0-beta.1"), &v("0.3.0")));
        assert!(!is_newer(&v("0.2.1"), &v("0.2.1")), "same version is not an update");
        assert!(!is_newer(&v("0.2.1"), &v("0.2.0")), "downgrade refused");
        assert!(!is_newer(&v("0.3.0"), &v("0.3.0-rc.1")), "pre-release of the same version is older");
    }

    #[test]
    fn notes_are_capped_on_a_char_boundary() {
        let long = "\u{e9}".repeat(MAX_NOTES_BYTES);
        let t = truncate_notes(&long);
        assert!(t.len() <= MAX_NOTES_BYTES + 8);
        assert!(t.ends_with("[...]"));
        assert_eq!(truncate_notes("short"), "short");
    }

    #[test]
    fn info_without_update_serialises_without_optional_fields() {
        let u = Updater::new(UpdateConfig::new("", "", "0.2.1"), Arc::new(Recorder::default()));
        let json = serde_json::to_value(u.pending_info()).unwrap();
        assert_eq!(json["currentVersion"], "0.2.1");
        assert_eq!(json["available"], false);
        assert!(json.get("version").is_none());
    }

    #[test]
    fn signed_version_is_read_and_compared_as_semver() {
        let c = "timestamp:1700000000\tfile:app.exe\tversion:1.2.3";
        assert_eq!(signed_version(c), Some("1.2.3"));
        assert_eq!(signed_version("timestamp:1\tfile:app-version:2.zip"), None);
        assert!(verify_signed_version(c, "1.2.3").is_ok());
        assert!(verify_signed_version(c, "v1.2.3").is_ok());
        assert!(verify_signed_version(c, "9.9.9").is_err());
        assert!(verify_signed_version("timestamp:1\tfile:x", "1.2.3").is_err(), "an unversioned signature is refused");
    }

    // --- mock server + signing -------------------------------------------------------------------

    #[derive(Default)]
    struct Recorder {
        installed: Mutex<Vec<(Vec<u8>, String)>>,
    }
    impl Installer for Recorder {
        fn install(&self, bytes: &[u8], version: &str) -> Result<(), String> {
            self.installed.lock().unwrap().push((bytes.to_vec(), version.to_string()));
            Ok(())
        }
    }
    impl Installer for Arc<Recorder> {
        fn install(&self, bytes: &[u8], version: &str) -> Result<(), String> {
            (**self).install(bytes, version)
        }
    }

    /// Tiny HTTP server; routes are built from the bound port (the manifest links back to it). Counts requests.
    struct Mock {
        port: u16,
        hits: Arc<std::sync::atomic::AtomicUsize>,
    }
    fn serve(routes: impl FnOnce(u16) -> Vec<(&'static str, Vec<u8>)>) -> Mock {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let routes = routes(port);
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h2 = hits.clone();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { break };
                h2.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                match routes.iter().find(|(p, _)| *p == path) {
                    Some((_, body)) => {
                        let _ = write!(s, "HTTP/1.1 200 OK
Content-Length: {}
Connection: close

", body.len());
                        let _ = s.write_all(body);
                    }
                    None => {
                        let _ = s.write_all(b"HTTP/1.1 404 Not Found
Content-Length: 0
Connection: close

");
                    }
                }
            }
        });
        Mock { port, hits }
    }

    struct Keys {
        pubkey_b64: String,
        sk: minisign::SecretKey,
        pk: minisign::PublicKey,
    }
    fn keys() -> Keys {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let pubkey_b64 = base64::engine::general_purpose::STANDARD.encode(kp.pk.to_box().unwrap().to_string());
        Keys { pubkey_b64, sk: kp.sk, pk: kp.pk }
    }
    /// Signs a release: trusted comment `timestamp:..\tfile:..\tversion:X`, base64 of the .sig text.
    fn sign(k: &Keys, data: &[u8], version: &str) -> String {
        let tc = format!("timestamp:1700000000\tfile:setup.exe\tversion:{version}");
        let sb = minisign::sign(Some(&k.pk), &k.sk, std::io::Cursor::new(data), Some(&tc), None).unwrap();
        base64::engine::general_purpose::STANDARD.encode(sb.to_string())
    }
    fn manifest(version: &str, port: u16, sig: &str) -> Vec<u8> {
        let key = platform_keys()[1].clone();
        serde_json::json!({
            "version": version, "notes": "fixes", "pub_date": "2026-11-02T10:00:00Z",
            "platforms": { key: { "url": format!("http://127.0.0.1:{port}/setup.exe"), "signature": sig } }
        })
        .to_string()
        .into_bytes()
    }
    fn updater(port: u16, k: &Keys, current: &str) -> (Updater, Arc<Recorder>) {
        let mut cfg = UpdateConfig::new(&format!("http://127.0.0.1:{port}/latest.json"), &k.pubkey_b64, current);
        cfg.loopback_http = true;
        let rec = Arc::new(Recorder::default());
        (Updater::new(cfg, Arc::new(rec.clone())), rec)
    }
    fn mock_with(version: &str, installer: &[u8], sig: impl FnOnce(&Keys) -> String, k: &Keys) -> Mock {
        let (sig, exe, version) = (sig(k), installer.to_vec(), version.to_string());
        serve(move |port| vec![("/latest.json", manifest(&version, port, &sig)), ("/setup.exe", exe)])
    }

    const EXE: &[u8] = b"MZ-fake-installer-bytes";

    #[tokio::test]
    async fn disabled_by_default_makes_no_request() {
        // The shipped constants are empty: updates are off.
        assert!(UPDATE_ENDPOINT.is_empty());
        let u = Updater::built_in();
        assert!(!u.is_enabled());
        let info = u.check().await.unwrap();
        assert!(!info.available);
        assert_eq!(info.current_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(u.install(|_, _| {}, || {}).await.unwrap_err().message, "No update available");
        // A configured endpoint with an empty key is disabled too, and a listener proves nothing connects.
        let m = serve(|_| vec![]);
        let u = Updater::new(UpdateConfig::new(&format!("http://127.0.0.1:{}/latest.json", m.port), "", "0.1.0"), Arc::new(Recorder::default()));
        assert!(!u.check().await.unwrap().available);
        assert_eq!(m.hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn newer_version_is_offered_and_installed_after_verification() {
        let k = keys();
        let m = mock_with("0.2.0", EXE, |k| sign(k, EXE, "0.2.0"), &k);
        let (u, rec) = updater(m.port, &k, "0.1.0");
        let info = u.check().await.unwrap();
        assert!(info.available);
        assert_eq!(info.version.as_deref(), Some("0.2.0"));
        assert_eq!(info.notes.as_deref(), Some("fixes"));
        assert_eq!(info.date.as_deref(), Some("2026-11-02"));
        assert_eq!(u.pending_info().version.as_deref(), Some("0.2.0"));
        let (mut last, mut before) = ((0, None), false);
        u.install(|d, t| last = (d, t), || before = true).await.unwrap();
        assert!(before, "before_install ran");
        assert_eq!(last.0, EXE.len() as u64);
        let got = rec.installed.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], (EXE.to_vec(), "0.2.0".to_string()));
    }

    #[tokio::test]
    async fn same_or_older_version_is_no_update() {
        let k = keys();
        let m = mock_with("0.1.0", EXE, |k| sign(k, EXE, "0.1.0"), &k);
        let (u, rec) = updater(m.port, &k, "0.1.0");
        assert!(!u.check().await.unwrap().available);
        let (u2, _) = updater(m.port, &k, "0.3.0");
        assert!(!u2.check().await.unwrap().available);
        assert!(rec.installed.lock().unwrap().is_empty());
        // Nothing to install: the re-check inside install finds none.
        assert_eq!(u.install(|_, _| {}, || {}).await.unwrap_err().message, "No update available");
    }

    #[tokio::test]
    async fn tampered_installer_is_rejected_and_never_run() {
        let k = keys();
        let m = mock_with("0.2.0", b"MZ-evil-bytes", |k| sign(k, EXE, "0.2.0"), &k);
        let (u, rec) = updater(m.port, &k, "0.1.0");
        assert!(u.check().await.unwrap().available);
        let mut before = false;
        let e = u.install(|_, _| {}, || before = true).await.unwrap_err();
        assert_eq!(e.code, IpcErrorCode::Network);
        assert_eq!(e.message, "Update download failed or could not be verified");
        assert!(!before && rec.installed.lock().unwrap().is_empty(), "nothing ran");
        // The failed install can be retried (the in-progress flag was released).
        assert_eq!(u.install(|_, _| {}, || {}).await.unwrap_err().message, "Update download failed or could not be verified");
    }

    #[tokio::test]
    async fn wrong_key_and_signed_version_mismatch_are_rejected() {
        let k = keys();
        let other = keys();
        // Signed by a different key.
        let m = mock_with("0.2.0", EXE, |_| sign(&other, EXE, "0.2.0"), &k);
        let (u, rec) = updater(m.port, &k, "0.1.0");
        u.check().await.unwrap();
        assert!(u.install(|_, _| {}, || {}).await.is_err());
        assert!(rec.installed.lock().unwrap().is_empty(), "wrong key: nothing ran");
        // Validly signed old installer (version 0.0.5) announced as 0.2.0 (rollback attempt).
        let m = mock_with("0.2.0", EXE, |k| sign(k, EXE, "0.0.5"), &k);
        let (u, rec) = updater(m.port, &k, "0.1.0");
        u.check().await.unwrap();
        assert!(u.install(|_, _| {}, || {}).await.is_err());
        assert!(rec.installed.lock().unwrap().is_empty(), "signed version mismatch: nothing ran");
        // Signature without a version in the trusted comment.
        let nover = |k: &Keys| {
            let sb = minisign::sign(Some(&k.pk), &k.sk, std::io::Cursor::new(EXE), Some("timestamp:1"), None).unwrap();
            base64::engine::general_purpose::STANDARD.encode(sb.to_string())
        };
        let m = mock_with("0.2.0", EXE, nover, &k);
        let (u, rec) = updater(m.port, &k, "0.1.0");
        u.check().await.unwrap();
        assert!(u.install(|_, _| {}, || {}).await.is_err());
        assert!(rec.installed.lock().unwrap().is_empty(), "unversioned signature: nothing ran");
    }

    #[tokio::test]
    async fn plain_http_to_a_non_loopback_host_is_refused() {
        // A real local listener, but `loopback_http` stays off: plain http is refused before any connection.
        let k = keys();
        let m = mock_with("0.2.0", EXE, |k| sign(k, EXE, "0.2.0"), &k);
        let u = Updater::new(UpdateConfig::new(&format!("http://127.0.0.1:{}/latest.json", m.port), &k.pubkey_b64, "0.1.0"), Arc::new(Recorder::default()));
        let e = u.check().await.unwrap_err();
        assert_eq!(e.message, "Could not check for updates");
        assert_eq!(m.hits.load(Ordering::SeqCst), 0, "no request went out");
    }
}
