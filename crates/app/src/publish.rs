//! Upload client for the tour platform, and the API token store.
//!
//! An upload is: create (the manifest), one PUT per step image, the logo if the
//! manifest names one, then commit. Progress is reported and cancellation checked
//! between requests. After the upload exists, any failure or cancel sends exactly one
//! abort (DELETE) and returns a message naming the step that failed.
//!
//! The token is a password. It lives only in the OS credential store (Windows
//! Credential Manager), is sent only in the `Authorization` header and only to the
//! configured server, and is never part of an error, a log line or a `Debug` print.
//! Redirects are never followed, so it can never reach a second host.
//!
//! Nothing is retried blindly. Only two requests are ever sent a second time, and only
//! after a transport failure (no answer at all): a step image, where the server answers
//! 409 `already_sent` if the first copy did arrive, and commit, which the server treats
//! as idempotent (a repeated commit answers with the first one's result).

#![allow(dead_code)] // The "Publish tour" UI that calls this comes in a later change.

use std::fmt;
use std::path::{Component, Path};
use std::time::Duration;

use serde_json::{Value, json};
use ureq::tls::{RootCerts, TlsConfig};

/// Keyring entry that holds the token.
const KEYRING_SERVICE: &str = "mythicshot";
const KEYRING_USER: &str = "demo-platform-token";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Whole-request timeout, sending and receiving included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Cap on any response body read from the server.
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
/// The server's own caps, checked here so a bad bundle fails before anything is created.
const MAX_IMAGE_BYTES: u64 = 2_000_000;
const MAX_LOGO_BYTES: u64 = 200_000;
const MAX_STEPS: usize = 200;
/// Longest piece of server text put into a message.
const MAX_DETAIL_CHARS: usize = 200;

pub const REJECTED_TOKEN: &str = "The server rejected the API token.";
pub const CANCELLED: &str = "cancelled";

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteTour {
    pub id: String,
    pub title: String,
    pub status: String,
    pub chapters: u32,
    pub steps: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    New,
    Chapter(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Published {
    pub tour_id: String,
    pub steps: usize,
}

/// Accepts https URLs, and http only for localhost and 127.0.0.1. Returns the URL without a trailing slash.
pub fn check_server(url: &str) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("Enter the server address.".into());
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("The server address must not contain spaces.".into());
    }
    let (scheme, rest) = url.split_once("://").ok_or("The server address must start with https://.")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "https" && scheme != "http" {
        return Err("The server address must start with https://.".into());
    }
    if rest.contains(['?', '#', '@', '\\']) {
        return Err("The server address must be just the address, without a query, a user name or a #.".into());
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let authority = authority.to_ascii_lowercase();
    let (host, port) = split_host_port(&authority).ok_or("The server address has no valid host name.")?;
    if let Some(p) = port
        && (p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) || !(1..=65535).contains(&p.parse::<u32>().unwrap_or(0)))
    {
        return Err("The server address has an invalid port.".into());
    }
    if scheme == "http" && host != "localhost" && host != "127.0.0.1" {
        return Err("The server address must start with https:// (http:// is only for localhost).".into());
    }
    let path = path.trim_end_matches('/');
    if path.split('/').any(|seg| seg == "." || seg == "..") || path.contains("//") {
        return Err("The server address has an invalid path.".into());
    }
    Ok(format!("{scheme}://{authority}{path}"))
}

/// Splits `host[:port]` or `[v6]:port`; None when the host is empty or has characters a host name cannot have.
fn split_host_port(authority: &str) -> Option<(&str, Option<&str>)> {
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let end = rest.find(']')?;
        let host = &authority[..end + 2];
        let after = &rest[end + 1..];
        if !rest[..end].bytes().all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.') || rest[..end].is_empty() {
            return None;
        }
        match after {
            "" => (host, None),
            p => (host, Some(p.strip_prefix(':')?)),
        }
    } else {
        let (host, port) = match authority.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        };
        if host.is_empty() || host.starts_with(['.', '-']) || host.contains("..") || !host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.') {
            return None;
        }
        (host, port)
    };
    Some((host, port))
}

pub fn workspace_name(server: &str, token: &str) -> Result<String, String> {
    Client::new(server, token, Timeouts::default())?.workspace_name()
}

pub fn list_tours(server: &str, token: &str) -> Result<Vec<RemoteTour>, String> {
    Client::new(server, token, Timeouts::default())?.list_tours()
}

/// Uploads the bundle in `dir`. `progress` gets 0.0–1.0 and returns false to cancel.
pub fn upload_bundle(server: &str, token: &str, dir: &Path, target: &Target, progress: impl FnMut(f32) -> bool) -> Result<Published, String> {
    Client::new(server, token, Timeouts::default())?.upload_bundle(dir, target, progress)
}

pub fn load_token() -> Option<String> {
    load_token_from(&OsStore)
}

pub fn store_token(token: &str) -> Result<(), String> {
    store_token_in(&OsStore, token)
}

pub fn forget_token() -> Result<(), String> {
    forget_token_in(&OsStore)
}

// ---- The token ----

/// An API token: `dp_` and 40 lowercase hex characters. Its `Debug` hides the value.
struct Token(String);

impl Token {
    fn parse(text: &str) -> Result<Token, String> {
        let t = text.trim();
        let ok = t.len() == 43 && t.starts_with("dp_") && t[3..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if ok { Ok(Token(t.to_string())) } else { Err("That is not an API token. A token starts with dp_ and comes from the server's setup page.".into()) }
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(hidden)")
    }
}

/// Where the token is kept. The real one is the OS credential store; tests use a fake.
trait SecretStore {
    fn get(&self) -> Result<Option<String>, String>;
    fn set(&self, secret: &str) -> Result<(), String>;
    fn delete(&self) -> Result<(), String>;
}

struct OsStore;

impl OsStore {
    fn entry() -> Result<keyring::Entry, String> {
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(|e| keyring_message(&e))
    }
}

/// A fixed description of a credential store error. The error itself is never printed,
/// because some variants carry the stored bytes.
fn keyring_message(e: &keyring::Error) -> String {
    use keyring::Error as E;
    let what = match e {
        E::NoEntry => "no token is stored",
        E::NoStorageAccess(_) => "the credential store is locked or not reachable",
        E::NoDefaultStore => "no credential store is available on this system",
        E::BadEncoding(_) | E::BadDataFormat(..) => "the stored token could not be read",
        E::TooLong(..) => "the token is too long for the credential store",
        _ => "the credential store reported an error",
    };
    format!("Could not use the credential store: {what}.")
}

impl SecretStore for OsStore {
    fn get(&self) -> Result<Option<String>, String> {
        match Self::entry()?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(keyring_message(&e)),
        }
    }
    fn set(&self, secret: &str) -> Result<(), String> {
        Self::entry()?.set_password(secret).map_err(|e| keyring_message(&e))
    }
    fn delete(&self) -> Result<(), String> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(keyring_message(&e)),
        }
    }
}

fn load_token_from(store: &dyn SecretStore) -> Option<String> {
    let stored = store.get().ok().flatten()?;
    Token::parse(&stored).ok().map(|t| t.0)
}

fn store_token_in(store: &dyn SecretStore, token: &str) -> Result<(), String> {
    let token = Token::parse(token)?;
    store.set(&token.0)
}

fn forget_token_in(store: &dyn SecretStore) -> Result<(), String> {
    store.delete()
}

// ---- HTTP ----

#[derive(Debug, Clone, Copy)]
struct Timeouts {
    connect: Duration,
    request: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self { connect: CONNECT_TIMEOUT, request: REQUEST_TIMEOUT }
    }
}

#[derive(Debug)]
struct Client {
    base: String,
    token: Token,
    agent: AgentDebug,
}

/// The agent, with a `Debug` that prints nothing of its state.
struct AgentDebug(ureq::Agent);

impl fmt::Debug for AgentDebug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Agent")
    }
}

/// A server answer: status and a capped body.
struct Reply {
    status: u16,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Option<Value> {
        serde_json::from_slice(&self.body).ok()
    }
    fn error_code(&self) -> Option<String> {
        self.json()?.get("error")?.as_str().map(str::to_string)
    }
}

/// A request that got no usable answer: the text is fixed, never the library's message.
struct Transport(&'static str);

#[derive(Clone, Copy)]
enum Method {
    Get,
    Post,
    Put,
    Delete,
}

impl Client {
    fn new(server: &str, token: &str, timeouts: Timeouts) -> Result<Client, String> {
        let base = check_server(server)?;
        let token = Token::parse(token)?;
        let plain_local = base.starts_with("http://");
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(timeouts.connect))
            .timeout_global(Some(timeouts.request))
            // A redirect is answered as it is and treated as an error: the token never goes to a second URL.
            .max_redirects(0)
            .http_status_as_error(false)
            .https_only(!plain_local)
            // Plain http is only for localhost: never hand that request, header and all, to a proxy.
            .proxy(if plain_local { None } else { ureq::Proxy::try_from_env() })
            .user_agent("Mythicshot")
            .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
            .build();
        Ok(Client { base, token, agent: AgentDebug(config.new_agent()) })
    }

    /// One request to `path` on the configured server, with the token. Never retried here.
    fn send(&self, method: Method, path: &str, body: Option<(&str, &[u8])>) -> Result<Reply, Transport> {
        debug_assert!(path.starts_with('/'));
        let url = format!("{}{}", self.base, path);
        let auth = format!("Bearer {}", self.token.0);
        let a = &self.agent.0;
        let result = match (method, body) {
            (Method::Get, _) => a.get(&url).header("Authorization", &auth).call(),
            (Method::Delete, _) => a.delete(&url).header("Authorization", &auth).call(),
            (Method::Post, None) => a.post(&url).header("Authorization", &auth).send_empty(),
            (Method::Put, None) => a.put(&url).header("Authorization", &auth).send_empty(),
            (Method::Post, Some((ct, data))) => a.post(&url).header("Authorization", &auth).header("Content-Type", ct).send(data),
            (Method::Put, Some((ct, data))) => a.put(&url).header("Authorization", &auth).header("Content-Type", ct).send(data),
        };
        let mut resp = result.map_err(|e| transport(&e))?;
        let status = resp.status().as_u16();
        let body = resp.body_mut().with_config().limit(MAX_RESPONSE_BYTES).read_to_vec().map_err(|e| transport(&e))?;
        Ok(Reply { status, body })
    }

    /// Sends a request and turns anything but a 2xx answer into a message starting with `what`.
    fn exchange(&self, what: &str, method: Method, path: &str, body: Option<(&str, &[u8])>) -> Result<Reply, String> {
        let reply = self.send(method, path, body).map_err(|t| format!("{what}: {}.", t.0))?;
        self.check(what, reply)
    }

    fn check(&self, what: &str, reply: Reply) -> Result<Reply, String> {
        match reply.status {
            200..=299 => Ok(reply),
            401 => Err(REJECTED_TOKEN.into()),
            300..=399 => Err(format!("{what}: the server answered with a redirect (HTTP {}), which is not followed. Check the server address.", reply.status)),
            s => Err(match self.server_detail(&reply) {
                Some(d) => format!("{what}: {d} (HTTP {s})."),
                None => format!("{what}: the server answered HTTP {s}."),
            }),
        }
    }

    /// The server's `detail` (or `error`) as one short line of plain text, never holding the token.
    fn server_detail(&self, reply: &Reply) -> Option<String> {
        let v = reply.json()?;
        let text = v.get("detail").and_then(Value::as_str).or_else(|| v.get("error").and_then(Value::as_str))?;
        let clean: String = text.chars().filter(|c| !c.is_control()).take(MAX_DETAIL_CHARS).collect();
        let clean = clean.replace(&self.token.0, "[token]").replace("dp_", "dp-");
        let clean = clean.trim().to_string();
        (!clean.is_empty()).then_some(clean)
    }

    fn workspace_name(&self) -> Result<String, String> {
        let what = "Could not check the server";
        let reply = self.exchange(what, Method::Get, "/api/v1/me", None)?;
        reply.json().and_then(|v| v.get("workspace")?.as_str().map(str::to_string)).ok_or_else(|| not_understood(what))
    }

    fn list_tours(&self) -> Result<Vec<RemoteTour>, String> {
        let what = "Could not list the tours";
        let reply = self.exchange(what, Method::Get, "/api/v1/tours", None)?;
        let v = reply.json().ok_or_else(|| not_understood(what))?;
        let rows = v.get("tours").and_then(Value::as_array).ok_or_else(|| not_understood(what))?;
        rows.iter()
            .map(|r| {
                let s = |k: &str| r.get(k).and_then(Value::as_str).map(str::to_string);
                let n = |k: &str| r.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
                Some(RemoteTour { id: s("id")?, title: s("title")?, status: s("status")?, chapters: n("chapters")?, steps: n("steps")? })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| not_understood(what))
    }

    fn upload_bundle(&self, dir: &Path, target: &Target, mut progress: impl FnMut(f32) -> bool) -> Result<Published, String> {
        let bundle = Bundle::read(dir)?;
        let total = (bundle.steps.len() + usize::from(bundle.logo.is_some()) + 2) as f32;
        if !progress(0.0) {
            return Err(CANCELLED.into());
        }

        let target_json = match target {
            Target::New => json!({ "kind": "new" }),
            Target::Chapter(id) => json!({ "kind": "chapter", "tour_id": id }),
        };
        let body = serde_json::to_vec(&json!({ "manifest": bundle.manifest, "target": target_json })).map_err(|_| "Could not read tour.json.".to_string())?;
        let what = "Could not start the upload";
        let reply = self.exchange(what, Method::Post, "/api/v1/uploads", Some(("application/json", &body)))?;
        let id = reply.json().and_then(|v| v.get("upload_id")?.as_str().map(str::to_string)).ok_or_else(|| not_understood(what))?;
        if !is_upload_id(&id) {
            return Err(not_understood(what));
        }

        let mut done = 1.0;
        let sent = self.send_parts(&id, dir, &bundle, &mut || {
            let go = progress(done / total);
            done += 1.0;
            go
        });
        match sent {
            Ok(published) => {
                progress(1.0);
                Ok(published)
            }
            Err(e) => {
                // Exactly one abort, whatever it answers. After a commit that did land, the server ignores it.
                let _ = self.send(Method::Delete, &format!("/api/v1/uploads/{id}"), None);
                Err(e)
            }
        }
    }

    /// Every image, the logo, then commit. `next` reports progress and says whether to go on.
    fn send_parts(&self, id: &str, dir: &Path, bundle: &Bundle, next: &mut dyn FnMut() -> bool) -> Result<Published, String> {
        for (i, step) in bundle.steps.iter().enumerate() {
            if !next() {
                return Err(CANCELLED.into());
            }
            let n = i + 1;
            let what = format!("Could not send step {n}");
            let data = std::fs::read(dir.join(&step.file)).map_err(|_| format!("{what}: could not read {}.", step.file))?;
            self.send_image(&what, &format!("/api/v1/uploads/{id}/steps/{n}"), step.content_type, &data)?;
        }
        if let Some(logo) = &bundle.logo {
            if !next() {
                return Err(CANCELLED.into());
            }
            let what = "Could not send the logo";
            let data = std::fs::read(dir.join(&logo.file)).map_err(|_| format!("{what}: could not read {}.", logo.file))?;
            self.send_image(what, &format!("/api/v1/uploads/{id}/logo"), logo.content_type, &data)?;
        }
        if !next() {
            return Err(CANCELLED.into());
        }
        self.commit(id)
    }

    /// PUTs an image. Sent a second time only when the first got no answer at all; a 409
    /// `already_sent` then means the first copy arrived.
    fn send_image(&self, what: &str, path: &str, content_type: &str, data: &[u8]) -> Result<(), String> {
        match self.send(Method::Put, path, Some((content_type, data))) {
            Ok(reply) => self.check(what, reply).map(drop),
            Err(_) => match self.send(Method::Put, path, Some((content_type, data))) {
                Ok(reply) if reply.status == 409 && reply.error_code().as_deref() == Some("already_sent") => Ok(()),
                Ok(reply) => self.check(what, reply).map(drop),
                Err(t) => Err(format!("{what}: {}.", t.0)),
            },
        }
    }

    /// Commit, sent a second time only when the first got no answer: the server answers a repeat with the first result.
    fn commit(&self, id: &str) -> Result<Published, String> {
        let what = "Could not finish the upload";
        let path = format!("/api/v1/uploads/{id}/commit");
        let reply = match self.send(Method::Post, &path, None) {
            Ok(r) => r,
            Err(_) => self.send(Method::Post, &path, None).map_err(|t| format!("{what}: {}. Check the tour list before uploading again.", t.0))?,
        };
        let reply = self.check(what, reply)?;
        let v = reply.json().ok_or_else(|| not_understood(what))?;
        let tour_id = v.get("tour_id").and_then(Value::as_str).ok_or_else(|| not_understood(what))?.to_string();
        let steps = v.get("steps").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).ok_or_else(|| not_understood(what))?;
        Ok(Published { tour_id, steps })
    }
}

fn not_understood(what: &str) -> String {
    format!("{what}: the server's answer was not understood.")
}

fn is_upload_id(id: &str) -> bool {
    id.len() == 27 && id.starts_with("up_") && id[3..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn transport(e: &ureq::Error) -> Transport {
    use ureq::Error as E;
    Transport(match e {
        E::Timeout(_) => "the server did not answer in time",
        E::HostNotFound => "the server's name could not be found",
        E::ConnectionFailed => "could not connect to the server",
        E::BodyExceedsLimit(_) => "the server's answer was too large",
        E::Io(_) => "the connection failed",
        E::Tls(_) | E::Rustls(_) => "the secure connection failed (certificate or TLS problem)",
        E::RequireHttpsOnly(_) => "the address is not https",
        _ => "the request failed",
    })
}

// ---- The bundle ----

struct BundleFile {
    file: String,
    content_type: &'static str,
}

struct Bundle {
    /// `tour.json` as it is, so any manifest version goes through unchanged.
    manifest: Value,
    steps: Vec<BundleFile>,
    logo: Option<BundleFile>,
}

impl Bundle {
    /// Reads `tour.json` and checks every file it names, so a bad bundle fails before anything is created.
    fn read(dir: &Path) -> Result<Bundle, String> {
        let text = std::fs::read_to_string(dir.join("tour.json")).map_err(|_| "Could not read tour.json in the export folder.".to_string())?;
        let manifest: Value = serde_json::from_str(&text).map_err(|_| "tour.json is not valid JSON.".to_string())?;
        let steps = manifest.get("steps").and_then(Value::as_array).ok_or("tour.json has no steps.")?;
        if steps.is_empty() || steps.len() > MAX_STEPS {
            return Err(format!("A tour needs 1 to {MAX_STEPS} steps."));
        }
        let steps = steps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let name = s.get("image").and_then(Value::as_str).ok_or_else(|| format!("Step {} in tour.json has no image.", i + 1))?;
                bundle_file(dir, name, MAX_IMAGE_BYTES).map_err(|e| format!("Step {}: {e}", i + 1))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let logo = match manifest.get("cover").and_then(|c| c.get("logo")).and_then(Value::as_str) {
            Some(name) => Some(bundle_file(dir, name, MAX_LOGO_BYTES).map_err(|e| format!("Logo: {e}"))?),
            None => None,
        };
        Ok(Bundle { manifest, steps, logo })
    }
}

/// A file named in `tour.json`: a plain name inside the folder, PNG or JPEG by name, within `max` bytes.
fn bundle_file(dir: &Path, name: &str, max: u64) -> Result<BundleFile, String> {
    let mut parts = Path::new(name).components();
    let plain = matches!((parts.next(), parts.next()), (Some(Component::Normal(_)), None)) && !name.contains([':', '/', '\\']) && !name.chars().any(char::is_control);
    if !plain {
        return Err("the file name in tour.json is not a plain file name.".into());
    }
    let lower = name.to_ascii_lowercase();
    let content_type = if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else {
        return Err(format!("{name} is not a PNG or JPEG file."));
    };
    let meta = std::fs::metadata(dir.join(name)).map_err(|_| format!("{name} is missing from the export folder."))?;
    if !meta.is_file() || meta.len() > max {
        return Err(format!("{name} is larger than {max} bytes."));
    }
    Ok(BundleFile { file: name.to_string(), content_type })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    const TOKEN: &str = "dp_0123456789abcdef0123456789abcdef01234567";
    const UP: &str = "up_0123456789abcdef01234567";

    #[derive(Debug, Clone)]
    struct Req {
        line: String,
        auth: Option<String>,
        content_type: Option<String>,
        body: Vec<u8>,
    }

    /// What the stub does with a request: answer, or drop the connection without a word.
    enum Answer {
        Reply(u16, String, Vec<(&'static str, String)>),
        Drop,
        Hang,
    }

    fn reply(status: u16, body: &str) -> Answer {
        Answer::Reply(status, body.to_string(), vec![])
    }

    struct Stub {
        base: String,
        log: Arc<Mutex<Vec<Req>>>,
    }

    impl Stub {
        fn lines(&self) -> Vec<String> {
            self.log.lock().unwrap().iter().map(|r| r.line.clone()).collect()
        }
        fn requests(&self) -> Vec<Req> {
            self.log.lock().unwrap().clone()
        }
    }

    /// A one-thread HTTP server on 127.0.0.1 that records each request and answers with `handler`.
    /// `handler` gets the request and how many earlier requests had the same line.
    fn stub(handler: impl Fn(&Req, usize) -> Answer + Send + 'static) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let log = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let Some(req) = read_request(&conn) else { continue };
                let seen = log2.lock().unwrap().iter().filter(|r: &&Req| r.line == req.line).count();
                log2.lock().unwrap().push(req.clone());
                match handler(&req, seen) {
                    Answer::Reply(status, body, headers) => {
                        let mut out = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
                        for (k, v) in headers {
                            out.push_str(&format!("{k}: {v}\r\n"));
                        }
                        out.push_str("\r\n");
                        out.push_str(&body);
                        let mut c = conn;
                        let _ = c.write_all(out.as_bytes());
                        let _ = c.flush();
                    }
                    Answer::Drop => drop(conn),
                    Answer::Hang => std::thread::sleep(Duration::from_secs(5)),
                }
            }
        });
        Stub { base, log }
    }

    fn read_request(conn: &TcpStream) -> Option<Req> {
        let mut r = BufReader::new(conn.try_clone().ok()?);
        let mut line = String::new();
        r.read_line(&mut line).ok()?;
        let line = line.trim_end().trim_end_matches(" HTTP/1.1").to_string();
        let (mut len, mut chunked, mut auth, mut content_type) = (0usize, false, None, None);
        loop {
            let mut h = String::new();
            r.read_line(&mut h).ok()?;
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            let (k, v) = h.split_once(':')?;
            let (k, v) = (k.to_ascii_lowercase(), v.trim().to_string());
            match k.as_str() {
                "content-length" => len = v.parse().ok()?,
                "transfer-encoding" => chunked = v.eq_ignore_ascii_case("chunked"),
                "authorization" => auth = Some(v),
                "content-type" => content_type = Some(v),
                _ => {}
            }
        }
        let mut body = Vec::new();
        if chunked {
            loop {
                let mut size = String::new();
                r.read_line(&mut size).ok()?;
                let n = usize::from_str_radix(size.trim(), 16).ok()?;
                let mut chunk = vec![0; n + 2];
                r.read_exact(&mut chunk).ok()?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n]);
            }
        } else {
            body.resize(len, 0);
            r.read_exact(&mut body).ok()?;
        }
        Some(Req { line, auth, content_type, body })
    }

    /// The happy-path server: every route answers as the platform does.
    fn platform(req: &Req, seen: usize) -> Answer {
        let l = req.line.as_str();
        if l == "POST /api/v1/uploads" {
            reply(201, &format!(r#"{{"upload_id":"{UP}","steps":2}}"#))
        } else if l.starts_with("PUT ") {
            if seen > 0 { reply(409, r#"{"error":"already_sent","detail":"step was already sent"}"#) } else { reply(204, "") }
        } else if l.ends_with("/commit") {
            reply(201, r#"{"tour_id":"tour_aaaaaaaaaaaaaaaaaaaaaaaa","chapter_id":"ch_x","steps":2}"#)
        } else if l.starts_with("DELETE ") {
            reply(204, "")
        } else if l == "GET /api/v1/me" {
            reply(200, r#"{"workspace":"Acme"}"#)
        } else {
            reply(404, r#"{"error":"not_found"}"#)
        }
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfakepng";

    /// A bundle folder with `steps` PNG steps, and a logo when `logo` is set.
    fn bundle(steps: usize, logo: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let mut list = Vec::new();
        for i in 1..=steps {
            let name = format!("step-{i:03}.png");
            std::fs::write(dir.path().join(&name), PNG).unwrap();
            list.push(json!({ "image": name, "width": 10, "height": 10, "bytes": PNG.len() }));
        }
        let cover = if logo {
            std::fs::write(dir.path().join("logo.png"), PNG).unwrap();
            json!({ "title": "Hi", "body": "", "button": "Start", "logo": "logo.png" })
        } else {
            json!({ "title": "Hi", "body": "", "button": "Start", "logo": null })
        };
        let manifest = json!({ "version": 3, "title": "Demo", "steps": list, "cover": cover });
        std::fs::write(dir.path().join("tour.json"), serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
        dir
    }

    fn upload(stub: &Stub, steps: usize, logo: bool, target: &Target) -> Result<Published, String> {
        let dir = bundle(steps, logo);
        upload_bundle(&stub.base, TOKEN, dir.path(), target, |_| true)
    }

    #[test]
    fn check_server_wants_https_except_on_localhost() {
        assert!(check_server("http://example.com").is_err());
        assert_eq!(check_server("https://x.dev/").unwrap(), "https://x.dev");
        assert_eq!(check_server("  HTTPS://X.Dev:8443/demo//  ").unwrap(), "https://x.dev:8443/demo");
        assert!(check_server("https://x.dev/a//b").is_err());
        assert_eq!(check_server("HTTPS://X.Dev:8443/demo/").unwrap(), "https://x.dev:8443/demo");
        assert_eq!(check_server("http://localhost:8787").unwrap(), "http://localhost:8787");
        assert_eq!(check_server("http://127.0.0.1:8787/").unwrap(), "http://127.0.0.1:8787");
        for bad in [
            "",
            "x.dev",
            "ftp://x.dev",
            "http://localhost.evil.com",
            "http://127.0.0.1.evil.com",
            "http://evil.com#@localhost",
            "https://user@x.dev",
            "https://x.dev?a=1",
            "https://x.dev/a b",
            "https://",
            "https://x.dev:0",
            "https://x.dev:99999",
            "https://x.dev:abc",
            "https://x.dev/../y",
            "https://x.dev\\y",
            "https://-x.dev",
        ] {
            assert!(check_server(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn upload_sends_create_then_each_image_then_commit() {
        let s = stub(platform);
        let out = upload(&s, 2, false, &Target::New).unwrap();
        assert_eq!(out, Published { tour_id: "tour_aaaaaaaaaaaaaaaaaaaaaaaa".into(), steps: 2 });
        assert_eq!(
            s.lines(),
            [
                "POST /api/v1/uploads".to_string(),
                format!("PUT /api/v1/uploads/{UP}/steps/1"),
                format!("PUT /api/v1/uploads/{UP}/steps/2"),
                format!("POST /api/v1/uploads/{UP}/commit"),
            ]
        );
        let reqs = s.requests();
        assert!(reqs.iter().all(|r| r.auth.as_deref() == Some(&format!("Bearer {TOKEN}")[..])));
        let create: Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(create["target"], json!({ "kind": "new" }));
        assert_eq!(create["manifest"]["title"], "Demo");
        assert_eq!(create["manifest"]["steps"][1]["image"], "step-002.png");
        assert_eq!(reqs[1].body, PNG);
        assert_eq!(reqs[1].content_type.as_deref(), Some("image/png"));
    }

    #[test]
    fn a_chapter_target_names_the_tour() {
        let s = stub(platform);
        upload(&s, 1, false, &Target::Chapter("tour_bbbbbbbbbbbbbbbbbbbbbbbb".into())).unwrap();
        let create: Value = serde_json::from_slice(&s.requests()[0].body).unwrap();
        assert_eq!(create["target"], json!({ "kind": "chapter", "tour_id": "tour_bbbbbbbbbbbbbbbbbbbbbbbb" }));
    }

    #[test]
    fn a_bundle_with_a_cover_logo_sends_it_before_commit() {
        let s = stub(platform);
        upload(&s, 2, true, &Target::New).unwrap();
        let lines = s.lines();
        assert_eq!(lines.iter().filter(|l| l.ends_with("/logo")).count(), 1);
        assert_eq!(lines[3], format!("PUT /api/v1/uploads/{UP}/logo"));
        assert_eq!(lines[4], format!("POST /api/v1/uploads/{UP}/commit"));

        let s = stub(platform);
        upload(&s, 2, false, &Target::New).unwrap();
        assert!(!s.lines().iter().any(|l| l.ends_with("/logo")));
    }

    #[test]
    fn a_failed_image_aborts_the_upload_once() {
        let s = stub(|req, seen| {
            if req.line.ends_with("/steps/2") { reply(400, r#"{"error":"bad_image","detail":"step 2: image length differs from the manifest's bytes"}"#) } else { platform(req, seen) }
        });
        let err = upload(&s, 3, false, &Target::New).unwrap_err();
        assert!(err.contains("step 2"), "{err}");
        assert!(err.contains("image length differs"), "{err}");
        let lines = s.lines();
        assert_eq!(lines.iter().filter(|l| l.starts_with("DELETE ")).count(), 1);
        assert_eq!(lines.last().unwrap(), &format!("DELETE /api/v1/uploads/{UP}"));
        assert!(!lines.iter().any(|l| l.ends_with("/commit") || l.ends_with("/steps/3")));
    }

    #[test]
    fn a_failed_commit_aborts_once_and_names_the_step() {
        let s = stub(|req, seen| if req.line.ends_with("/commit") { reply(409, r#"{"error":"incomplete","detail":"step 1 has not been sent"}"#) } else { platform(req, seen) });
        let err = upload(&s, 1, false, &Target::New).unwrap_err();
        assert!(err.starts_with("Could not finish the upload: step 1 has not been sent"), "{err}");
        assert_eq!(s.lines().iter().filter(|l| l.starts_with("DELETE ")).count(), 1);
        assert_eq!(s.lines().iter().filter(|l| l.ends_with("/commit")).count(), 1);
    }

    #[test]
    fn cancel_aborts_the_upload() {
        let s = stub(platform);
        let dir = bundle(3, false);
        let mut calls = 0;
        let err = upload_bundle(&s.base, TOKEN, dir.path(), &Target::New, |_| {
            calls += 1;
            calls <= 2 // 0.0, then after create; false after step 1
        })
        .unwrap_err();
        assert_eq!(err, "cancelled");
        let lines = s.lines();
        assert_eq!(lines, ["POST /api/v1/uploads".to_string(), format!("PUT /api/v1/uploads/{UP}/steps/1"), format!("DELETE /api/v1/uploads/{UP}")]);
    }

    #[test]
    fn cancel_before_create_sends_nothing() {
        let s = stub(platform);
        let dir = bundle(1, false);
        assert_eq!(upload_bundle(&s.base, TOKEN, dir.path(), &Target::New, |_| false).unwrap_err(), "cancelled");
        assert!(s.lines().is_empty());
    }

    #[test]
    fn progress_climbs_to_one() {
        let s = stub(platform);
        let dir = bundle(2, true);
        let mut seen = Vec::new();
        upload_bundle(&s.base, TOKEN, dir.path(), &Target::New, |p| {
            seen.push(p);
            true
        })
        .unwrap();
        assert_eq!(seen.first(), Some(&0.0));
        assert_eq!(seen.last(), Some(&1.0));
        assert!(seen.windows(2).all(|w| w[0] < w[1]), "{seen:?}");
    }

    #[test]
    fn a_401_reads_as_a_token_problem_without_the_token() {
        let s = stub(|_, _| reply(401, r#"{"error":"unauthorized"}"#));
        assert_eq!(workspace_name(&s.base, TOKEN).unwrap_err(), "The server rejected the API token.");
        assert_eq!(list_tours(&s.base, TOKEN).unwrap_err(), "The server rejected the API token.");
        let err = upload(&s, 1, false, &Target::New).unwrap_err();
        assert_eq!(err, "The server rejected the API token.");
        assert!(!err.contains("dp_"));

        // A 401 part-way still aborts once.
        let s = stub(|req, seen| if req.line.ends_with("/steps/1") { reply(401, r#"{"error":"unauthorized"}"#) } else { platform(req, seen) });
        assert_eq!(upload(&s, 2, false, &Target::New).unwrap_err(), "The server rejected the API token.");
        assert_eq!(s.lines().iter().filter(|l| l.starts_with("DELETE ")).count(), 1);
    }

    #[test]
    fn list_tours_parses_the_picker_rows() {
        let s = stub(|_, _| {
            reply(200, r#"{"tours":[{"id":"tour_1","title":"Onboarding","status":"draft","chapters":2,"steps":9,"updated_at":1700000000000},{"id":"tour_2","title":"Billing","status":"published","chapters":1,"steps":3,"updated_at":1}]}"#)
        });
        let tours = list_tours(&s.base, TOKEN).unwrap();
        assert_eq!(
            tours,
            [
                RemoteTour { id: "tour_1".into(), title: "Onboarding".into(), status: "draft".into(), chapters: 2, steps: 9 },
                RemoteTour { id: "tour_2".into(), title: "Billing".into(), status: "published".into(), chapters: 1, steps: 3 },
            ]
        );
        assert_eq!(s.lines(), ["GET /api/v1/tours"]);
        assert_eq!(workspace_name(&stub(platform).base, TOKEN).unwrap(), "Acme");

        let s = stub(|_, _| reply(200, r#"{"tours":[{"id":"t"}]}"#));
        assert_eq!(list_tours(&s.base, TOKEN).unwrap_err(), "Could not list the tours: the server's answer was not understood.");
    }

    #[test]
    fn the_token_is_never_in_an_error_or_a_debug_print() {
        let echo = format!(r#"{{"error":"x","detail":"you sent {TOKEN}"}}"#);
        let mut errors = vec![
            check_server("http://example.com").unwrap_err(),
            workspace_name("http://example.com", TOKEN).unwrap_err(),
            workspace_name("https://x.dev", "dp_short").unwrap_err(),
            store_token_in(&FakeStore::default(), &format!("{TOKEN}x")).unwrap_err(),
        ];
        let e2 = echo.clone();
        let s = stub(move |req, seen| if req.line.ends_with("/steps/1") { Answer::Reply(500, e2.clone(), vec![]) } else { platform(req, seen) });
        errors.push(upload(&s, 1, false, &Target::New).unwrap_err());
        let s = stub(move |_, _| Answer::Reply(400, echo.clone(), vec![]));
        errors.push(workspace_name(&s.base, TOKEN).unwrap_err());
        errors.push(list_tours(&s.base, TOKEN).unwrap_err());
        errors.push(upload(&s, 1, false, &Target::New).unwrap_err());
        errors.push(upload(&stub(|_, _| reply(302, "")), 1, false, &Target::New).unwrap_err());
        errors.push(workspace_name(&stub(|_, _| Answer::Drop).base, TOKEN).unwrap_err());
        for e in &errors {
            assert!(!e.contains(TOKEN) && !format!("{e:?}").contains(TOKEN), "{e}");
            assert!(!e.contains("0123456789abcdef0123"), "{e}");
        }
        assert!(errors[5].contains("you sent [token]"), "{}", errors[5]);

        let client = Client::new("https://x.dev", TOKEN, Timeouts::default()).unwrap();
        assert!(!format!("{client:?}").contains("0123456789abcdef"));
        assert_eq!(format!("{:?}", Token::parse(TOKEN).unwrap()), "Token(hidden)");
    }

    #[test]
    fn redirects_are_not_followed_and_the_token_stays_home() {
        let other = stub(platform);
        let to = format!("{}/api/v1/me", other.base);
        let s = stub(move |_, _| Answer::Reply(307, String::new(), vec![("Location", to.clone())]));
        let err = workspace_name(&s.base, TOKEN).unwrap_err();
        assert!(err.contains("redirect"), "{err}");
        assert!(other.lines().is_empty());
        let err = upload(&s, 1, false, &Target::New).unwrap_err();
        assert!(err.starts_with("Could not start the upload"), "{err}");
        assert!(other.lines().is_empty());
    }

    #[test]
    fn a_step_resent_after_a_lost_answer_accepts_already_sent() {
        // The first PUT of step 1 gets no answer, the resend is told it already arrived.
        let s = stub(|req, seen| if req.line.ends_with("/steps/1") && seen == 0 { Answer::Drop } else { platform(req, seen) });
        upload(&s, 2, false, &Target::New).unwrap();
        let lines = s.lines();
        assert_eq!(lines.iter().filter(|l| l.ends_with("/steps/1")).count(), 2);
        assert!(!lines.iter().any(|l| l.starts_with("DELETE ")));

        // A 409 to a first send is a real error.
        let s = stub(|req, seen| if req.line.ends_with("/steps/1") { reply(409, r#"{"error":"already_sent","detail":"step 1 was already sent"}"#) } else { platform(req, seen) });
        let err = upload(&s, 1, false, &Target::New).unwrap_err();
        assert!(err.contains("step 1 was already sent"), "{err}");
        assert_eq!(s.lines().iter().filter(|l| l.ends_with("/steps/1")).count(), 1);
    }

    #[test]
    fn a_commit_resent_after_a_lost_answer_is_not_doubled() {
        let s = stub(|req, seen| if req.line.ends_with("/commit") && seen == 0 { Answer::Drop } else { platform(req, seen) });
        let out = upload(&s, 1, false, &Target::New).unwrap();
        assert_eq!(out.tour_id, "tour_aaaaaaaaaaaaaaaaaaaaaaaa");
        let lines = s.lines();
        assert_eq!(lines.iter().filter(|l| l.ends_with("/commit")).count(), 2);
        assert!(!lines.iter().any(|l| l.starts_with("DELETE ")));

        // Create is never resent: a lost answer fails, and with no upload id there is nothing to abort.
        let s = stub(|_, _| Answer::Drop);
        let err = upload(&s, 1, false, &Target::New).unwrap_err();
        assert_eq!(err, "Could not start the upload: the connection failed.");
        assert_eq!(s.lines().len(), 1);
    }

    #[test]
    fn a_silent_server_times_out() {
        let s = stub(|_, _| Answer::Hang);
        let client = Client::new(&s.base, TOKEN, Timeouts { connect: Duration::from_millis(500), request: Duration::from_millis(300) }).unwrap();
        let start = std::time::Instant::now();
        assert_eq!(client.workspace_name().unwrap_err(), "Could not check the server: the server did not answer in time.");
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn a_huge_answer_is_cut_off() {
        let big = format!(r#"{{"workspace":"{}"}}"#, "x".repeat(MAX_RESPONSE_BYTES as usize + 10));
        let s = stub(move |_, _| Answer::Reply(200, big.clone(), vec![]));
        assert_eq!(workspace_name(&s.base, TOKEN).unwrap_err(), "Could not check the server: the server's answer was too large.");
    }

    #[test]
    fn an_upload_id_that_is_not_one_is_refused() {
        let s = stub(|req, seen| if req.line == "POST /api/v1/uploads" { reply(201, r#"{"upload_id":"../../evil","steps":1}"#) } else { platform(req, seen) });
        let err = upload(&s, 1, false, &Target::New).unwrap_err();
        assert_eq!(err, "Could not start the upload: the server's answer was not understood.");
        assert_eq!(s.lines().len(), 1);
    }

    #[test]
    fn a_bad_bundle_fails_before_anything_is_sent() {
        let s = stub(platform);
        let dir = bundle(2, false);
        std::fs::remove_file(dir.path().join("step-002.png")).unwrap();
        let err = upload_bundle(&s.base, TOKEN, dir.path(), &Target::New, |_| true).unwrap_err();
        assert!(err.contains("step-002.png is missing"), "{err}");

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tour.json"), r#"{"steps":[{"image":"../secret.png"}]}"#).unwrap();
        let err = upload_bundle(&s.base, TOKEN, dir.path(), &Target::New, |_| true).unwrap_err();
        assert!(err.contains("not a plain file name"), "{err}");

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tour.json"), r#"{"steps":[{"image":"step-001.png"}],"cover":{"logo":"C:logo.png"}}"#).unwrap();
        std::fs::write(dir.path().join("step-001.png"), PNG).unwrap();
        assert!(upload_bundle(&s.base, TOKEN, dir.path(), &Target::New, |_| true).unwrap_err().starts_with("Logo:"));
        assert!(s.lines().is_empty());
    }

    #[test]
    fn the_client_uses_the_system_trust_store_and_follows_no_redirects() {
        let c = Client::new("https://x.dev", TOKEN, Timeouts::default()).unwrap();
        let config = c.agent.0.config();
        assert!(matches!(config.tls_config().root_certs(), RootCerts::PlatformVerifier));
        assert_eq!(config.max_redirects(), 0);
        assert!(config.https_only());
        let local = Client::new("http://localhost:8787", TOKEN, Timeouts::default()).unwrap();
        assert!(local.agent.0.config().proxy().is_none());
    }

    #[derive(Default)]
    struct FakeStore {
        value: RefCell<Option<String>>,
    }

    impl SecretStore for FakeStore {
        fn get(&self) -> Result<Option<String>, String> {
            Ok(self.value.borrow().clone())
        }
        fn set(&self, secret: &str) -> Result<(), String> {
            *self.value.borrow_mut() = Some(secret.to_string());
            Ok(())
        }
        fn delete(&self) -> Result<(), String> {
            *self.value.borrow_mut() = None;
            Ok(())
        }
    }

    #[test]
    fn the_token_store_keeps_only_a_valid_token() {
        let store = FakeStore::default();
        assert_eq!(load_token_from(&store), None);
        store_token_in(&store, &format!("  {TOKEN}\n")).unwrap();
        assert_eq!(store.value.borrow().as_deref(), Some(TOKEN));
        assert_eq!(load_token_from(&store).as_deref(), Some(TOKEN));
        for bad in ["", "dp_", "dp_0123", &TOKEN.to_uppercase(), &format!("{TOKEN}\r\nX-Evil: 1")] {
            assert!(store_token_in(&store, bad).is_err(), "{bad:?}");
        }
        assert_eq!(load_token_from(&store).as_deref(), Some(TOKEN));
        forget_token_in(&store).unwrap();
        assert_eq!(load_token_from(&store), None);
        *store.value.borrow_mut() = Some("garbage".into());
        assert_eq!(load_token_from(&store), None);
    }

    #[test]
    fn the_settings_file_has_the_server_but_no_token() {
        let text = toml::to_string(&crate::settings::Settings::default()).unwrap();
        assert!(text.contains("publish_server = \"\""), "{text}");
        assert!(!text.to_ascii_lowercase().contains("token"));
    }
}
