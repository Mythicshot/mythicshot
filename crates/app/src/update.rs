//! Asking mythicshot.com whether a newer version exists, downloading its installer, and starting
//! that installer. No drawing here: the line in the main window, the Preferences section and the
//! tray entry are `update_ui`'s.
//!
//! The rules, all of them on purpose:
//! - The site is a constant, [`HOST`]. Nothing (no setting, no environment variable, no file)
//!   changes it: an [`Origin`] other than [`Origin::official`] exists only in a test build, and a
//!   test build refuses every origin but its stand-in sites on 127.0.0.1.
//! - HTTPS only (in every build but a test build), no redirect followed, the proxy rule of the
//!   app's other requests, short timeouts, and a size cap on every answer.
//! - A request carries nothing about the person or the computer: no token, no cookie, no query
//!   string, only `Host` and `User-Agent: Mythicshot/<version>`.
//! - The release file must meet [`parse_release`] exactly; the installer's name is built here from
//!   its version, never taken from the site, so nothing from the network becomes a path or an
//!   address beyond that checked version.
//! - The installer is written as a partial file in the app's own `updates` folder and takes its
//!   name only once its size and SHA-256 match the release file; any mismatch deletes it.
//! - It is started only from that file, held open so nothing can change, move or delete it from
//!   its check until the app has gone ([`open_verified`]), by `msiexec.exe` from the Windows
//!   system folder, with real arguments and no shell ([`start_installer`]).

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde::Deserialize;
use ureq::tls::{RootCerts, TlsConfig};

/// The one site asked. Never changed by anything at run time.
pub const HOST: &str = "https://mythicshot.com";
/// Where the release file is on [`HOST`].
pub const RELEASE_PATH: &str = "/download/release.json";
/// The folder of installers on [`HOST`].
const DOWNLOAD_PATH: &str = "/download/";
/// The download page, for when a download did not match.
pub const DOWNLOAD_PAGE: &str = crate::menu_bar::DOWNLOAD_PAGE;
/// The largest release file read; more is a failed check.
pub const MAX_RELEASE_BYTES: u64 = 4 * 1024;
/// The sizes an installer may have, both included.
pub const MIN_INSTALLER_BYTES: u64 = 1024 * 1024;
pub const MAX_INSTALLER_BYTES: u64 = 64 * 1024 * 1024;

/// How long a request may take: to connect, in all, and without a byte arriving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub connect: Duration,
    pub whole: Duration,
    pub stall: Duration,
}

/// The check: about 5 seconds, everything included.
pub const CHECK: Timeouts = Timeouts { connect: Duration::from_secs(5), whole: Duration::from_secs(5), stall: Duration::from_secs(5) };
/// The download: a slow line has half an hour, but 30 seconds without a byte stops it.
pub const DOWNLOAD: Timeouts = Timeouts { connect: Duration::from_secs(10), whole: Duration::from_secs(30 * 60), stall: Duration::from_secs(30) };

// ---- Versions ----

/// A version: three decimal numbers, compared as numbers part by part (0.2.10 is newer than 0.2.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version([u32; 3]);

impl Version {
    /// `a.b.c`, each part plain ASCII digits without a sign, a space or a leading zero (other
    /// than `0` itself), at most 9 digits. Anything else is None.
    pub fn parse(text: &str) -> Option<Version> {
        let mut parts = [0u32; 3];
        let mut pieces = text.split('.');
        for part in &mut parts {
            let p = pieces.next()?;
            let plain = !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()) && (p == "0" || !p.starts_with('0'));
            if !plain {
                return None;
            }
            *part = p.parse().ok()?;
        }
        pieces.next().is_none().then_some(Version(parts))
    }

    /// The version that is running.
    pub fn running() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).expect("the package version is three numbers")
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [a, b, c] = self.0;
        write!(f, "{a}.{b}.{c}")
    }
}

/// The installer's name for `version`, built here and never taken from the site.
pub fn installer_name(version: Version) -> String {
    format!("Mythicshot-{version}-x64.msi")
}

/// The changes page.
pub const CHANGES_PAGE: &str = "https://mythicshot.com/changes/";

/// `version`'s entry on the changes page: `v`, and the version with its dots as dashes.
pub fn changes_anchor(version: Version) -> String {
    format!("v{}", version.to_string().replace('.', "-"))
}

/// The changes page at `version`'s entry.
#[cfg(test)]
pub fn changes_url(version: Version) -> String {
    format!("{CHANGES_PAGE}#{}", changes_anchor(version))
}

// ---- The release file ----

/// What the release file says, checked and signature-verified. Only [`parse_release`] makes one
/// (the private field sees to that), so every size and SHA-256 the download is held to comes
/// from a file whose signature verified under a release key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    version: Version,
    size: u64,
    /// 64 lowercase hex digits.
    sha256: String,
    verified: (),
}

impl Release {
    pub fn version(&self) -> Version {
        self.version
    }

    /// The installer's size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The installer's SHA-256, 64 lowercase hex digits.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub fn file_name(&self) -> String {
        installer_name(self.version)
    }

    /// A release for tests that never touch the signature.
    #[cfg(test)]
    pub fn for_test(version: Version, size: u64, sha256: String) -> Release {
        Release { version, size, sha256, verified: () }
    }
}

/// The release file as it is on the site. Fields it does not list are ignored; a field named
/// twice is refused.
#[derive(Deserialize)]
struct RawRelease {
    version: String,
    file: String,
    size: u64,
    sha256: String,
    /// Read as optional so that a missing one is said to be a signature that did not verify.
    signature: Option<String>,
}

// ---- The signature ----

/// The public keys a release file must be signed with, at least one of them: Ed25519, each the
/// raw 32-byte public key in standard base64 with padding (44 characters), with a short id.
///
/// These are the owner's keys: a main one and a backup beside it, one line per key, for example
/// `("2026-main", "<44 base64 characters of the raw public key>"),`. Rotating means adding the
/// new key in one version and removing the old one in a later version.
/// Nothing (no setting, no environment variable, no debug switch) adds to or skips this list.
pub const RELEASE_KEYS: &[(&str, &str)] = &[
    ("primary", "v8SQJVA9CaiFcBQkcsZ6I+Kh7caBv74L+SMF8BhN1hA="),
    ("backup", "7ZYdjH8mnVUOJeqR78A2XAHONIAwQ8rgdv5YfaRIrZ0="),
];

/// What stood in the list before the real keys: not base64 of 32 bytes, so it is never a key.
/// Kept for the tests that check it is refused and is no longer in the list.
#[cfg(test)]
pub const RELEASE_KEY_PLACEHOLDER: (&str, &str) = ("placeholder: no release key yet", "NOT-A-KEY-REPLACE-WITH-THE-OWNERS-PUBLIC-KEY");

/// What the log says for a release file whose signature is missing, malformed or does not
/// verify. Nothing of the keys or the signature is in it.
pub const SIGNATURE_FAILED: &str = "the release file's signature did not verify";

/// The keys a release file is checked against. A release build can only make
/// [`Keys::official`]; a test build can also make its own ([`Keys::test`]), the same way it can
/// point at a stand-in site, so release behaviour never depends on a test key.
#[derive(Debug, Clone)]
pub struct Keys(Vec<[u8; 32]>);

impl Keys {
    /// The release keys from [`RELEASE_KEYS`].
    pub fn official() -> Keys {
        Keys::from_list(RELEASE_KEYS)
    }

    /// The entries of `list` that are base64 of exactly 32 bytes; anything else is no key.
    fn from_list(list: &[(&str, &str)]) -> Keys {
        Keys(list.iter().filter_map(|(_, b)| base64_decode(b)).filter_map(|k| <[u8; 32]>::try_from(k).ok()).collect())
    }

    #[cfg(test)]
    pub fn test(keys: &[&[u8]]) -> Keys {
        Keys(keys.iter().map(|k| <[u8; 32]>::try_from(*k).expect("an Ed25519 public key is 32 bytes")).collect())
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[cfg(test)]
    fn contains(&self, key: &[u8]) -> bool {
        self.0.iter().any(|k| k.as_slice() == key)
    }

    /// Whether `signature` is a valid Ed25519 signature of `message` under at least one key.
    fn verify(&self, message: &[u8], signature: &[u8]) -> bool {
        use ring::signature::{ED25519, UnparsedPublicKey};
        self.0.iter().any(|k| UnparsedPublicKey::new(&ED25519, k).verify(message, signature).is_ok())
    }
}

/// The message a release file's signature covers, built here from the checked fields: exactly
/// `mythicshot-release-v1`, `version=`, `file=`, `size=` (decimal) and `sha256=` lines, each
/// ending in `\n`. Nothing of the file's own layout (spacing, order, other fields) is in it.
fn signed_message(release: &Release) -> String {
    format!("mythicshot-release-v1\nversion={}\nfile={}\nsize={}\nsha256={}\n", release.version(), release.file_name(), release.size(), release.sha256())
}

/// Standard base64 with padding, read strictly: the 64-letter alphabet, a length that is a
/// multiple of 4, `=` only as the last one or two characters, and no stray bits. None otherwise.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let quads = bytes.len() / 4;
    for (i, quad) in bytes.chunks(4).enumerate() {
        let pad = quad.iter().rev().take_while(|c| **c == b'=').count();
        if pad > 2 || (pad > 0 && i + 1 != quads) {
            return None;
        }
        let mut n = 0u32;
        for c in &quad[..4 - pad] {
            n = n << 6 | value(*c)?;
        }
        n <<= 6 * pad as u32;
        let got = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        let keep = 3 - pad;
        // The bits the padding leaves unused must be zero, so each value has one spelling.
        if got[keep..].iter().any(|b| *b != 0) {
            return None;
        }
        out.extend_from_slice(&got[..keep]);
    }
    Some(out)
}

/// Read the release file: `version` three plain numbers, `file` exactly the name that version
/// gives, `sha256` 64 lowercase hex digits, `size` from 1 MB to 64 MB; then `signature`, base64
/// of 64 bytes, must verify over [`signed_message`] under one of `keys`. Anything else is an
/// error, in a fixed sentence for the log ([`SIGNATURE_FAILED`] for the signature).
pub fn parse_release(bytes: &[u8], keys: &Keys) -> Result<Release, &'static str> {
    let raw: RawRelease = serde_json::from_slice(bytes).map_err(|_| "the release file is not what was expected")?;
    let version = Version::parse(&raw.version).ok_or("the release file's version is not three plain numbers")?;
    if raw.file != installer_name(version) {
        return Err("the release file names another file than its version gives");
    }
    if raw.sha256.len() != 64 || !raw.sha256.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err("the release file's hash is not 64 lowercase hex digits");
    }
    if !(MIN_INSTALLER_BYTES..=MAX_INSTALLER_BYTES).contains(&raw.size) {
        return Err("the release file's size is outside 1 to 64 MB");
    }
    let release = Release { version, size: raw.size, sha256: raw.sha256, verified: () };
    let signature = raw.signature.as_deref().and_then(base64_decode).filter(|s| s.len() == 64).ok_or(SIGNATURE_FAILED)?;
    if !keys.verify(signed_message(&release).as_bytes(), &signature) {
        return Err(SIGNATURE_FAILED);
    }
    Ok(release)
}

// ---- Where requests go ----

/// The site requests go to. A release build can only make [`Origin::official`]; a test build can
/// also point at a stand-in site on 127.0.0.1, and nowhere else ([`Origin::reachable`]).
#[derive(Debug, Clone)]
pub struct Origin(String);

impl Origin {
    pub fn official() -> Origin {
        Origin(HOST.to_string())
    }

    /// A stand-in site on 127.0.0.1, for tests only.
    #[cfg(test)]
    pub fn stub(base: &str) -> Origin {
        assert!(base.starts_with("http://127.0.0.1:"), "a test origin is on 127.0.0.1");
        Origin(base.to_string())
    }

    /// Whether requests may go there. Always in a release build, whose only origin is [`HOST`];
    /// a test build only ever talks to its stand-in sites.
    fn reachable(&self) -> bool {
        #[cfg(test)]
        if !self.0.starts_with("http://127.0.0.1:") {
            return false;
        }
        true
    }
}

/// HTTPS only, except for the stand-in sites of a test build.
#[cfg(not(test))]
const HTTPS_ONLY: bool = true;
#[cfg(test)]
const HTTPS_ONLY: bool = false;

/// The agent for one request: no redirects, the system's trust store, the proxy rule of the
/// app's other requests (none for a plain local address), no automatic header but `Host` and the
/// user agent.
fn agent(origin: &Origin, t: Timeouts) -> ureq::Agent {
    let plain_local = origin.0.starts_with("http://");
    ureq::Agent::config_builder()
        .timeout_connect(Some(t.connect))
        .timeout_global(Some(t.whole))
        .timeout_per_read(Some(t.stall))
        .max_redirects(0)
        .http_status_as_error(false)
        .https_only(HTTPS_ONLY)
        .proxy(if plain_local { None } else { ureq::Proxy::try_from_env() })
        .user_agent(format!("Mythicshot/{}", env!("CARGO_PKG_VERSION")))
        .accept("")
        .accept_encoding("")
        .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
        .build()
        .new_agent()
}

/// A fixed sentence for a request that got no usable answer: never the library's message.
fn failed(e: &ureq::Error) -> String {
    use ureq::Error as E;
    match e {
        E::Timeout(_) => "no answer in time",
        E::HostNotFound => "the name could not be found",
        E::ConnectionFailed => "could not connect",
        E::BodyExceedsLimit(_) => "the answer was too large",
        E::Tls(_) | E::Rustls(_) => "the secure connection failed",
        E::RequireHttpsOnly(_) => "the address is not https",
        _ => "the connection failed",
    }
    .to_string()
}

// ---- The check ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checked {
    /// The site lists this version or an older one.
    UpToDate,
    /// The site lists a newer version than `running`.
    Newer(Release),
}

/// Ask [`HOST`] once which version is current. Blocks for up to about 5 seconds: never on the UI
/// thread. The error is a fixed sentence for the log.
pub fn check(running: Version) -> Result<Checked, String> {
    check_with(&Origin::official(), &Keys::official(), running, CHECK)
}

fn check_with(origin: &Origin, keys: &Keys, running: Version, t: Timeouts) -> Result<Checked, String> {
    if !origin.reachable() {
        return Err("a test build sends nothing to a real site".into());
    }
    let url = format!("{}{RELEASE_PATH}", origin.0);
    let mut resp = agent(origin, t).get(&url).call().map_err(|e| failed(&e))?;
    match resp.status().as_u16() {
        200 => {}
        s @ 300..=399 => return Err(format!("the site answered with a redirect (HTTP {s}), which is not followed")),
        s => return Err(format!("the site answered HTTP {s}")),
    }
    let body = resp.body_mut().with_config().limit(MAX_RELEASE_BYTES).read_to_vec().map_err(|e| failed(&e))?;
    // Size cap above, then the fields, then the signature; only then the versions are compared.
    let release = parse_release(&body, keys)?;
    Ok(if release.version() > running { Checked::Newer(release) } else { Checked::UpToDate })
}

// ---- The download ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Downloaded {
    /// The installer, its size and SHA-256 matching the release file.
    Ready(PathBuf),
    /// What arrived is not what the release file lists (size or hash): deleted.
    Mismatch,
    /// No usable answer, or the connection broke or stalled; a fixed sentence for the log.
    Failed(String),
    /// Cancel was pressed; what had arrived is deleted.
    Cancelled,
}

/// Download `release`'s installer from [`HOST`] into `dir`, counting the bytes in `done` and
/// stopping when `cancel` is set. `attempt` keeps the partial files of two attempts apart. Blocks:
/// never on the UI thread.
pub fn download(release: &Release, dir: &Path, attempt: u64, done: &AtomicU64, cancel: &AtomicBool) -> Downloaded {
    download_with(&Origin::official(), release, dir, attempt, done, cancel, DOWNLOAD)
}

/// The partial file of an attempt, a name built here.
fn partial_name(release: &Release, attempt: u64) -> String {
    format!("{}.{attempt}.partial", release.file_name())
}

fn download_with(origin: &Origin, release: &Release, dir: &Path, attempt: u64, done: &AtomicU64, cancel: &AtomicBool, t: Timeouts) -> Downloaded {
    if !origin.reachable() {
        return Downloaded::Failed("a test build sends nothing to a real site".into());
    }
    if let Err(e) = prepare_dir(dir) {
        return Downloaded::Failed(e.into());
    }
    let partial = dir.join(partial_name(release, attempt));
    let got = fetch_into(origin, release, &partial, done, cancel, t);
    match got {
        Ok(()) => {
            let path = dir.join(release.file_name());
            match std::fs::rename(&partial, &path) {
                Ok(()) => Downloaded::Ready(path),
                Err(_) => {
                    let _ = std::fs::remove_file(&partial);
                    Downloaded::Failed("the download could not be put in place".into())
                }
            }
        }
        Err(end) => {
            let _ = std::fs::remove_file(&partial);
            end
        }
    }
}

/// Write the installer to `partial` and check it: Ok only when it is whole and matches.
fn fetch_into(origin: &Origin, release: &Release, partial: &Path, done: &AtomicU64, cancel: &AtomicBool, t: Timeouts) -> Result<(), Downloaded> {
    use sha2::Digest;
    let url = format!("{}{DOWNLOAD_PATH}{}", origin.0, release.file_name());
    let resp = agent(origin, t).get(&url).call().map_err(|e| Downloaded::Failed(failed(&e)))?;
    match resp.status().as_u16() {
        200 => {}
        s @ 300..=399 => return Err(Downloaded::Failed(format!("the site answered with a redirect (HTTP {s}), which is not followed"))),
        s => return Err(Downloaded::Failed(format!("the site answered HTTP {s}"))),
    }
    // A length that is not the listed size: not the file the release file lists.
    let announced = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
    if announced.is_some_and(|n| n != release.size()) {
        return Err(Downloaded::Mismatch);
    }
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(partial).map_err(|_| Downloaded::Failed("the download could not be written".into()))?;
    // One byte past the listed size is enough to know it is not the file.
    let mut body = resp.into_body().into_with_config().limit(release.size()).reader();
    let mut hasher = sha2::Sha256::new();
    let mut piece = vec![0u8; 64 * 1024];
    let mut len = 0u64;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(Downloaded::Cancelled);
        }
        let n = match body.read(&mut piece) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                let more = e.get_ref().and_then(|inner| inner.downcast_ref::<ureq::Error>()).is_some_and(|u| matches!(u, ureq::Error::BodyExceedsLimit(_)));
                return Err(if more { Downloaded::Mismatch } else if cancel.load(Ordering::SeqCst) { Downloaded::Cancelled } else { Downloaded::Failed("the connection broke or stalled".into()) });
            }
        };
        len += n as u64;
        if len > release.size() {
            return Err(Downloaded::Mismatch);
        }
        hasher.update(&piece[..n]);
        file.write_all(&piece[..n]).map_err(|_| Downloaded::Failed("the download could not be written".into()))?;
        done.store(len, Ordering::SeqCst);
    }
    if cancel.load(Ordering::SeqCst) {
        return Err(Downloaded::Cancelled);
    }
    if len != release.size() || hex(hasher.finalize()) != release.sha256() {
        return Err(Downloaded::Mismatch);
    }
    file.sync_all().map_err(|_| Downloaded::Failed("the download could not be written".into()))?;
    Ok(())
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

// ---- The updates folder ----

/// `updates` in the app's local data folder for this user (which does not roam), only as a full
/// path; a debug build with `MYTHICSHOT_CONFIG_DIR` uses that folder, as the settings file does.
/// Never the system temp folder, never Downloads. None when the user's folder cannot be found.
pub fn updates_dir() -> Option<PathBuf> {
    // A test build never touches the user's own folders.
    #[cfg(test)]
    if cfg!(test) {
        return Some(crate::settings::test_data_dir().join("updates"));
    }
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::var_os("MYTHICSHOT_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("updates"));
    }
    directories::ProjectDirs::from("", "", "Mythicshot").map(|d| d.data_local_dir().to_path_buf()).filter(|d| d.is_absolute()).map(|d| d.join("updates"))
}

/// Make the folder (with the user's own permissions, as every folder the app makes) and check it
/// is a real folder, not a link to somewhere else.
fn prepare_dir(dir: &Path) -> Result<(), &'static str> {
    std::fs::create_dir_all(dir).map_err(|_| "the updates folder could not be made")?;
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => Ok(()),
        _ => Err("the updates folder is not a plain folder"),
    }
}

/// Whether `name` is one this module gives a file and [`clean`] removes: a partial download, or
/// an installer of another version than `running`. The installer of the running version stays:
/// msiexec may still be finishing with it, and a Repair from Settings › Apps may ask for it.
fn is_removable(name: &str, running: Version) -> bool {
    let Some(rest) = name.strip_prefix("Mythicshot-") else { return false };
    let Some((version, tail)) = rest.split_once("-x64.msi") else { return false };
    let Some(version) = Version::parse(version) else { return false };
    let partial = tail.strip_prefix('.').and_then(|t| t.strip_suffix(".partial")).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    partial || (tail.is_empty() && version != running)
}

/// Remove the partial downloads and the installers of other versions than `running` in `dir`:
/// only plain files with a name this module gives. How many were removed.
pub fn clean(dir: &Path, running: Version) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(|n| is_removable(n, running)))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| std::fs::remove_file(e.path()).is_ok())
        .count()
}

// ---- Starting the installer ----

/// What the signature check says about an installer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureCheck {
    /// No code-signing certificate exists yet, so installers are not signed: allowed.
    NotSignedAllowed,
}

impl SignatureCheck {
    pub fn allows(self) -> bool {
        matches!(self, SignatureCheck::NotSignedAllowed)
    }
}

/// The installer's Authenticode signature and publisher.
///
/// There is no code-signing certificate yet, so no installer is signed and this allows every
/// one. Once a certificate exists, this is the one function to change: check the signature with
/// `WinVerifyTrust` and that the signer is Mythicshot, LLC, and add a refusing variant. The size
/// and SHA-256 checks stay as they are either way.
pub fn check_signature(_installer: &Path) -> SignatureCheck {
    SignatureCheck::NotSignedAllowed
}

/// An installer whose size and SHA-256 matched the release file, held open: while this lives
/// nobody can write to it, move it or delete it (Windows), so what is started is what was checked.
#[derive(Debug)]
pub struct Verified {
    _file: std::fs::File,
    path: PathBuf,
}

impl Verified {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Open the installer at `path` so that nobody can change, move or delete it while it is held,
/// and check it again: a plain file (no link), the name `release`'s version gives, its size and
/// its SHA-256. Read through the same handle that holds it.
pub fn open_verified(path: &Path, release: &Release) -> Result<Verified, &'static str> {
    use sha2::Digest;
    if path.file_name().and_then(|n| n.to_str()) != Some(release.file_name().as_str()) || !path.is_absolute() {
        return Err("the installer is not where the app put it");
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        // Others may read it (msiexec does), nobody may write, rename or delete it.
        options.share_mode(FILE_SHARE_READ);
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut file = options.open(path).map_err(|_| "the installer could not be opened")?;
    let meta = file.metadata().map_err(|_| "the installer could not be read")?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() != release.size() {
        return Err("the installer is not the file that was checked");
    }
    let mut hasher = sha2::Sha256::new();
    let mut piece = vec![0u8; 64 * 1024];
    let mut len = 0u64;
    loop {
        let n = file.read(&mut piece).map_err(|_| "the installer could not be read")?;
        if n == 0 {
            break;
        }
        len += n as u64;
        hasher.update(&piece[..n]);
    }
    if len != release.size() || hex(hasher.finalize()) != release.sha256() {
        return Err("the installer is not the file that was checked");
    }
    Ok(Verified { _file: file, path: path.to_path_buf() })
}

/// What starting the installer needs of the system: a test hands in its own, which runs nothing.
pub trait Launch {
    /// The Windows system folder, as a full path.
    fn system_dir(&self) -> Option<PathBuf>;
    /// Start `program` with `args` as a real argument vector (no shell), without waiting for it.
    fn spawn(&self, program: &Path, args: &[OsString]) -> std::io::Result<()>;
}

/// The real system.
pub struct WindowsLaunch;

impl Launch for WindowsLaunch {
    #[cfg(windows)]
    fn system_dir(&self) -> Option<PathBuf> {
        use std::os::windows::ffi::OsStringExt;
        let mut buf = vec![0u16; 520];
        // SAFETY: the buffer is valid for its whole length, which is what the call is told.
        let n = unsafe { windows::Win32::System::SystemInformation::GetSystemDirectoryW(Some(&mut buf)) } as usize;
        if n == 0 || n >= buf.len() {
            return None;
        }
        Some(PathBuf::from(OsString::from_wide(&buf[..n]))).filter(|p| p.is_absolute())
    }

    #[cfg(not(windows))]
    fn system_dir(&self) -> Option<PathBuf> {
        None
    }

    fn spawn(&self, program: &Path, args: &[OsString]) -> std::io::Result<()> {
        let mut cmd = std::process::Command::new(program);
        cmd.args(args);
        // Its own folder as the working folder: nothing is looked up in the app's.
        if let Some(dir) = program.parent() {
            cmd.current_dir(dir);
        }
        cmd.spawn().map(drop)
    }
}

/// The arguments msiexec gets: install the file, with the small progress window and no restart,
/// and tell the installer it was started from the app, so it opens Mythicshot again once the
/// install has succeeded (see `installer/mythicshot.wxs`).
pub fn msiexec_args(installer: &Path) -> Vec<OsString> {
    vec!["/i".into(), installer.as_os_str().to_os_string(), "/passive".into(), "/norestart".into(), "MYTHICSHOT_RELAUNCH=1".into()]
}

/// Start `msiexec.exe` from the system folder on the held, checked installer. The caller keeps
/// `held` until the app has gone. The error is a fixed sentence for the log.
pub fn start_installer(held: &Verified, launch: &dyn Launch) -> Result<(), &'static str> {
    if !check_signature(held.path()).allows() {
        return Err("the installer's signature was refused");
    }
    let system = launch.system_dir().filter(|d| d.is_absolute()).ok_or("the Windows system folder is not known")?;
    launch.spawn(&system.join("msiexec.exe"), &msiexec_args(held.path())).map_err(|_| "msiexec could not be started")
}

// ---- The log ----

/// One line of `capture.log` about an update step: the time and the step, never a path or a name.
pub fn log_line(time: &str, step: &str) -> String {
    format!("== {time} update · {step}")
}

/// Write one update step to `capture.log`.
pub fn log(step: &str) {
    crate::capture_log::append(&[log_line(&crate::capture_log::now(), step)]);
}

#[cfg(test)]
mod tests;
