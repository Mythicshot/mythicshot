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
//! Publishing is a second, separate step on a tour that is already uploaded: the publish
//! route is called again and again while it answers 202 with its progress, until it
//! answers 200 with the public link. Cancelling just stops calling; the server clears a
//! publish nobody drives any more, and the public version changes only on the last call.
//!
//! Nothing is retried blindly. Only two requests are ever sent a second time, and only
//! after a transport failure (no answer at all): a step image, where the server answers
//! 409 `already_sent` if the first copy did arrive, and commit, which the server treats
//! as idempotent (a repeated commit answers with the first one's result).


use std::fmt;
use std::path::{Component, Path};
use std::time::Duration;

use serde_json::{Value, json};
use ureq::tls::{RootCerts, TlsConfig};

/// Keyring entry that holds the token.
const KEYRING_SERVICE: &str = "mythicshot";
const KEYRING_USER: &str = "demo-platform-token";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Whole-request timeout for a request without a body; a body adds time, see [`request_timeout`].
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Bytes of request body per extra second of timeout: one second per 20 KB, about 160 kbit/s.
const BODY_BYTES_PER_SECOND: usize = 20_000;
/// Longest any one request may take.
const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
/// Cap on any response body read from the server.
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
/// The server's own caps, checked here so a bad bundle fails before anything is created.
const MAX_IMAGE_BYTES: u64 = 2_000_000;
const MAX_LOGO_BYTES: u64 = 200_000;
const MAX_STEPS: usize = 200;
/// Longest piece of server text put into a message.
const MAX_DETAIL_CHARS: usize = 200;

/// Pause between two calls of one publish, as the server's own helper does.
const PUBLISH_PAUSE: Duration = Duration::from_millis(250);
/// Calls in a row that may show no progress before the publish is given up.
const MAX_STALLED_CALLS: u32 = 3;
/// A hard stop, far above the 34 calls the largest tour takes.
const MAX_PUBLISH_CALLS: u32 = 200;
/// Longest public link shown or opened.
const MAX_URL_CHARS: usize = 300;

pub const REJECTED_TOKEN: &str = "The server rejected the API token.";
pub const CANCELLED: &str = "cancelled";

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteTour {
    pub id: String,
    pub title: String,
    pub status: String,
    pub chapters: u32,
    pub steps: u32,
    /// The tour's public link, when the tour list gives one (a `url` or a `slug`) and it passes the link rule.
    pub link: Option<String>,
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

/// A tour that went public: its step count and its link, when the server gave a usable one.
#[derive(Debug, Clone, PartialEq)]
pub struct Live {
    pub steps: usize,
    pub url: Option<String>,
}

/// Where an upload ended up.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Uploaded as a draft, as asked.
    Draft,
    /// Uploaded and published.
    Public { url: Option<String> },
    /// Uploaded, but the publish did not finish: cancelled, or failed with `error`.
    NotPublic { cancelled: bool, error: String },
}

/// An upload that worked, and what became of its publish.
#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
    pub tour_id: String,
    pub steps: usize,
    pub outcome: Outcome,
}

/// Images copied so far out of how many, from the server's last answer.
pub type Images = (u64, u64);

/// What a publish's result means for a tour that is already uploaded with `steps` steps.
/// A failed or cancelled publish is not an error of the whole job: the draft is there.
pub fn after_publish(tour_id: &str, steps: usize, result: Result<Live, String>) -> Finished {
    let (steps, outcome) = match result {
        Ok(live) => (live.steps, Outcome::Public { url: live.url }),
        Err(e) if e == CANCELLED => (steps, Outcome::NotPublic { cancelled: true, error: String::new() }),
        Err(error) => (steps, Outcome::NotPublic { cancelled: false, error }),
    };
    Finished { tour_id: tour_id.to_string(), steps, outcome }
}

/// The public link for a published tour. `url` is what the server answered: a path is
/// put after the configured server address, and a full address is used only when it is
/// on the configured server (same scheme, host and port); otherwise, or without a url,
/// the link is built from the slug. The result is https, or http for localhost and
/// 127.0.0.1 only, and holds nothing but plain address characters; None when no such
/// link can be made. A link never leads to another host than the one the user set up.
pub fn public_url(server: &str, url: Option<&str>, slug: Option<&str>) -> Option<String> {
    let base = check_server(server).ok()?;
    // `scheme://host[:port]` of the configured server.
    let origin = match base.find("://").and_then(|i| base[i + 3..].find('/').map(|j| i + 3 + j)) {
        Some(end) => &base[..end],
        None => base.as_str(),
    };
    let from_server = url.and_then(|u| {
        if u.contains("://") {
            let full = check_server(u).ok()?;
            let on_server = full.strip_prefix(origin).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
            on_server.then_some(full)
        } else if u.starts_with('/') && !u.starts_with("//") {
            Some(format!("{base}{u}"))
        } else {
            None
        }
    });
    let from_slug = slug.filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')).map(|s| format!("{base}/t/{s}"));
    from_server.and_then(|u| safe_link(&u)).or_else(|| from_slug.and_then(|u| safe_link(&u)))
}

/// `url` when it is an address the server-address rule accepts and one the opener
/// itself accepts ([`mythicshot_destinations::is_web_link`]: short, https or local http,
/// plain address characters only).
fn safe_link(url: &str) -> Option<String> {
    (url.len() <= MAX_URL_CHARS && mythicshot_destinations::is_web_link(url) && check_server(url).is_ok()).then(|| url.to_string())
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
///
/// For the caller: this blocks for as long as the upload takes, up to minutes on a slow
/// line (each request may take up to five minutes). Cancel is only checked between
/// requests, so a cancel takes effect when the request in flight finishes. Run it off
/// the UI thread, and show "Cancelling…" from the moment `progress` returns false until
/// this returns. A chapter upload never sends the bundle's logo: the tour keeps its own.
#[cfg(test)]
pub fn upload_bundle(server: &str, token: &str, dir: &Path, target: &Target, progress: impl FnMut(f32) -> bool) -> Result<Published, String> {
    Client::new(server, token, Timeouts::default())?.upload_bundle(dir, target, progress)
}

/// Publishes the uploaded tour `tour_id`: calls the publish route until the new version
/// is live. `progress` gets the images copied so far (None before the first answer) and
/// returns false to cancel, which ends in `Err(CANCELLED)`. Blocks like [`upload_bundle`].
pub fn publish_tour(server: &str, token: &str, tour_id: &str, progress: impl FnMut(Option<Images>) -> bool) -> Result<Live, String> {
    Client::new(server, token, Timeouts::default())?.publish_tour(tour_id, PUBLISH_PAUSE, progress)
}

/// Uploads the bundle and, with `publish_now`, publishes the tour it went into. An
/// error means nothing was uploaded; once the upload is in, the answer is `Ok` and says
/// whether the tour is public.
pub fn upload_and_publish(
    server: &str,
    token: &str,
    dir: &Path,
    target: &Target,
    publish_now: bool,
    upload_progress: impl FnMut(f32) -> bool,
    publish_progress: impl FnMut(Option<Images>) -> bool,
) -> Result<Finished, String> {
    Client::new(server, token, Timeouts::default())?.upload_and_publish(dir, target, publish_now, PUBLISH_PAUSE, upload_progress, publish_progress)
}

/// The stored token: `Ok(None)` when none is stored, `Err` when the credential store
/// cannot be used (locked, unavailable), so the UI can say so instead of asking for a new token.
pub fn load_token() -> Result<Option<String>, String> {
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

fn load_token_from(store: &dyn SecretStore) -> Result<Option<String>, String> {
    match store.get()? {
        None => Ok(None),
        Some(stored) => match Token::parse(&stored) {
            Ok(t) => Ok(Some(t.0)),
            Err(_) => Err("Could not use the credential store: the stored token could not be read. Forget it and save the token again.".into()),
        },
    }
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
    timeouts: Timeouts,
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
    /// `Retry-After` in seconds, when the server sent a plain number.
    retry_after: Option<u32>,
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
        Ok(Client { base, token, agent: AgentDebug(config.new_agent()), timeouts })
    }

    /// One request to `path` on the configured server, with the token. Never retried here.
    fn send(&self, method: Method, path: &str, body: Option<(&str, &[u8])>) -> Result<Reply, Transport> {
        debug_assert!(path.starts_with('/'));
        let url = format!("{}{}", self.base, path);
        let auth = format!("Bearer {}", self.token.0);
        let a = &self.agent.0;
        // Sending the body counts against the timeout, so a bigger body gets more time.
        let limit = Some(request_timeout(self.timeouts.request, body.map_or(0, |(_, d)| d.len())));
        let result = match (method, body) {
            (Method::Get, _) => a.get(&url).config().timeout_global(limit).build().header("Authorization", &auth).call(),
            (Method::Delete, _) => a.delete(&url).config().timeout_global(limit).build().header("Authorization", &auth).call(),
            (Method::Post, None) => a.post(&url).config().timeout_global(limit).build().header("Authorization", &auth).send_empty(),
            (Method::Put, None) => a.put(&url).config().timeout_global(limit).build().header("Authorization", &auth).send_empty(),
            (Method::Post, Some((ct, data))) => a.post(&url).config().timeout_global(limit).build().header("Authorization", &auth).header("Content-Type", ct).send(data),
            (Method::Put, Some((ct, data))) => a.put(&url).config().timeout_global(limit).build().header("Authorization", &auth).header("Content-Type", ct).send(data),
        };
        let mut resp = result.map_err(|e| transport(&e))?;
        let status = resp.status().as_u16();
        let retry_after = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u32>().ok());
        let body = resp.body_mut().with_config().limit(MAX_RESPONSE_BYTES).read_to_vec().map_err(|e| transport(&e))?;
        Ok(Reply { status, body, retry_after })
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
            429 if reply.error_code().as_deref() == Some("too_many_uploads") => Err(format!(
                "{what}: the server already has too many unfinished uploads for this workspace. Wait for another upload to finish{}, then try again.",
                reply.retry_after.map(|s| format!(" or about {} seconds", s.min(3600))).unwrap_or_default()
            )),
            429 => Err(format!("{what}: the server is busy (HTTP 429). Try again later.")),
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
        let clean: String = text.chars().filter(|&c| !c.is_control() && !is_invisible(c)).take(MAX_DETAIL_CHARS).collect();
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
                let link = public_url(&self.base, r.get("url").and_then(Value::as_str), r.get("slug").and_then(Value::as_str));
                Some(RemoteTour { id: s("id")?, title: s("title")?, status: s("status")?, chapters: n("chapters")?, steps: n("steps")?, link })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| not_understood(what))
    }

    fn upload_bundle(&self, dir: &Path, target: &Target, mut progress: impl FnMut(f32) -> bool) -> Result<Published, String> {
        // An upload into an existing tour keeps that tour's logo; the server refuses another (409 logo_not_used).
        let bundle = Bundle::read(dir, matches!(target, Target::New))?;
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

        // From here every exit but a successful commit, a panic included, sends exactly one abort.
        let guard = AbortGuard { client: self, id: &id, armed: true };
        let mut done = 1.0;
        let published = self.send_parts(&id, dir, &bundle, &mut || {
            let go = progress(done / total);
            done += 1.0;
            go
        })?;
        guard.disarm();
        progress(1.0);
        Ok(published)
    }

    /// Every image, the logo, then commit. `next` reports progress and says whether to go on.
    fn send_parts(&self, id: &str, dir: &Path, bundle: &Bundle, next: &mut dyn FnMut() -> bool) -> Result<Published, String> {
        for (i, step) in bundle.steps.iter().enumerate() {
            if !next() {
                return Err(CANCELLED.into());
            }
            let n = i + 1;
            let what = format!("Could not send step {n}");
            let data = read_bundle_file(dir, step).map_err(|e| format!("{what}: {e}"))?;
            self.send_image(&what, &format!("/api/v1/uploads/{id}/steps/{n}"), step.content_type, &data)?;
        }
        if let Some(logo) = &bundle.logo {
            if !next() {
                return Err(CANCELLED.into());
            }
            let what = "Could not send the logo";
            let data = read_bundle_file(dir, logo).map_err(|e| format!("{what}: {e}"))?;
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

impl Client {
    fn upload_and_publish(
        &self,
        dir: &Path,
        target: &Target,
        publish_now: bool,
        pause: Duration,
        upload_progress: impl FnMut(f32) -> bool,
        publish_progress: impl FnMut(Option<Images>) -> bool,
    ) -> Result<Finished, String> {
        let up = self.upload_bundle(dir, target, upload_progress)?;
        if !publish_now {
            return Ok(Finished { tour_id: up.tour_id, steps: up.steps, outcome: Outcome::Draft });
        }
        let result = self.publish_tour(&up.tour_id, pause, publish_progress);
        Ok(after_publish(&up.tour_id, up.steps, result))
    }

    /// The publish loop: one call with no body, then calls naming the publish it started,
    /// `pause` apart, until the server answers 200. Gives up after [`MAX_STALLED_CALLS`]
    /// answers in a row without progress: only a `done` count higher than any before is
    /// progress, so an answer without one counts as none. A call that fails is not sent
    /// again: publishing again later joins the same publish on the server, or starts a new one.
    fn publish_tour(&self, tour_id: &str, pause: Duration, mut progress: impl FnMut(Option<Images>) -> bool) -> Result<Live, String> {
        let what = "Could not publish the tour";
        if !is_id(tour_id, "tour_") {
            return Err(not_understood(what));
        }
        let path = format!("/api/v1/tours/{tour_id}/publish");
        let mut publish_id: Option<String> = None;
        let mut images = None;
        let (mut last_done, mut stalled) = (-1i128, 0u32);
        for call in 1u32.. {
            if !progress(images) {
                return Err(CANCELLED.into());
            }
            let body = publish_id.as_ref().map(|id| json!({ "publish_id": id }).to_string().into_bytes());
            let reply = self.send(Method::Post, &path, body.as_deref().map(|b| ("application/json", b))).map_err(|t| {
                if call == 1 { format!("{what}: {}.", t.0) } else { format!("{what}: {} while the images were being copied.", t.0) }
            })?;
            if reply.status == 409 {
                match reply.error_code().as_deref() {
                    Some("publish_busy") => return Err(format!("{what}: another publish of this tour is starting at this moment.")),
                    Some("publish_cancelled") => return Err(format!("{what}: the publish was stopped on the server before it finished.")),
                    _ => {}
                }
            }
            let reply = self.check(what, reply)?;
            let v = reply.json().ok_or_else(|| not_understood(what))?;
            if reply.status == 202 {
                let id = v.get("publish_id").and_then(Value::as_str).filter(|id| is_id(id, "pub_")).ok_or_else(|| not_understood(what))?;
                publish_id = Some(id.to_string());
                let done = v.get("done").and_then(Value::as_u64);
                // A count is shown only when it makes sense.
                images = done.zip(v.get("total").and_then(Value::as_u64)).filter(|(d, t)| *t > 0 && d <= t);
                // Each call should move the work on. If several in a row do not, say so and stop.
                let moved = done.is_some_and(|d| i128::from(d) > last_done);
                stalled = if moved { 0 } else { stalled + 1 };
                last_done = done.map_or(last_done, |d| last_done.max(i128::from(d)));
                if stalled >= MAX_STALLED_CALLS {
                    return Err(match images {
                        Some((d, t)) => format!("{what}: the server stopped making progress at {d} of {t} images."),
                        None => not_understood(what),
                    });
                }
                if call >= MAX_PUBLISH_CALLS {
                    return Err(format!("{what}: the server did not finish within {MAX_PUBLISH_CALLS} calls."));
                }
                std::thread::sleep(pause);
                continue;
            }
            if reply.status != 200 {
                return Err(not_understood(what));
            }
            let steps = v.get("steps").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).ok_or_else(|| not_understood(what))?;
            let url = public_url(&self.base, v.get("url").and_then(Value::as_str), v.get("slug").and_then(Value::as_str));
            return Ok(Live { steps, url });
        }
        Err(not_understood(what))
    }
}

fn not_understood(what: &str) -> String {
    format!("{what}: the server's answer was not understood.")
}

fn is_upload_id(id: &str) -> bool {
    is_id(id, "up_")
}

/// A server id: `prefix` and 24 lowercase hex characters.
fn is_id(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix).is_some_and(|hex| hex.len() == 24 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// The timeout for one request: `base`, plus a second per 20 KB of body, at most five minutes.
fn request_timeout(base: Duration, body_bytes: usize) -> Duration {
    let extra = u64::try_from(body_bytes / BODY_BYTES_PER_SECOND).unwrap_or(u64::MAX);
    base.saturating_add(Duration::from_secs(extra)).min(MAX_REQUEST_TIMEOUT)
}

/// Characters that show nothing but can reorder or hide text: bidi controls, zero-width marks, the BOM.
fn is_invisible(c: char) -> bool {
    matches!(c, '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
}

/// Sends the upload's abort when dropped, unless disarmed after a successful commit.
struct AbortGuard<'a> {
    client: &'a Client,
    id: &'a str,
    armed: bool,
}

impl AbortGuard<'_> {
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for AbortGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.armed = false;
            // Whatever it answers. After a commit that did land, the server ignores it. A panic
            // here while already unwinding would abort the process, so it is caught.
            let path = format!("/api/v1/uploads/{}", self.id);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = self.client.send(Method::Delete, &path, None);
            }));
        }
    }
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
    /// The size `tour.json` gives, or for the logo its cap.
    bytes: u64,
    /// Whether `bytes` must match exactly.
    exact: bool,
}

struct Bundle {
    /// `tour.json` as it is, so any manifest version goes through unchanged.
    manifest: Value,
    steps: Vec<BundleFile>,
    logo: Option<BundleFile>,
}

impl Bundle {
    /// Reads `tour.json` and checks every file it names (the logo only when `with_logo`),
    /// so a bad bundle fails before anything is created.
    fn read(dir: &Path, with_logo: bool) -> Result<Bundle, String> {
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
                let bytes = s.get("bytes").and_then(Value::as_u64).filter(|&b| b <= MAX_IMAGE_BYTES).ok_or_else(|| format!("Step {} in tour.json has no valid size.", i + 1))?;
                bundle_file(dir, name, bytes, true).map_err(|e| format!("Step {}: {e}", i + 1))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let logo = match manifest.get("cover").and_then(|c| c.get("logo")).and_then(Value::as_str) {
            Some(name) if with_logo => Some(bundle_file(dir, name, MAX_LOGO_BYTES, false).map_err(|e| format!("Logo: {e}"))?),
            _ => None,
        };
        Ok(Bundle { manifest, steps, logo })
    }
}

/// A file named in `tour.json`: a plain name directly inside the folder, a regular file
/// (not a link or a folder), PNG or JPEG by name, of exactly `bytes` bytes (`exact`) or at most.
fn bundle_file(dir: &Path, name: &str, bytes: u64, exact: bool) -> Result<BundleFile, String> {
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
    let meta = std::fs::symlink_metadata(dir.join(name)).map_err(|_| format!("{name} is missing from the export folder."))?;
    check_regular(name, &meta)?;
    let size_ok = if exact { meta.len() == bytes } else { meta.len() <= bytes };
    if !size_ok {
        return Err(if exact { format!("{name} is not the size tour.json gives.") } else { format!("{name} is larger than {bytes} bytes.") });
    }
    Ok(BundleFile { file: name.to_string(), content_type, bytes, exact })
}

fn check_regular(name: &str, meta: &std::fs::Metadata) -> Result<(), String> {
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(format!("{name} is not a regular file."));
    }
    Ok(())
}

/// Opens a checked bundle file once, without following a link on Windows, and reads at
/// most its size plus one byte, refusing it if it is no longer what was checked.
fn read_bundle_file(dir: &Path, f: &BundleFile) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(dir.join(&f.file)).map_err(|_| format!("could not read {}.", f.file))?;
    check_regular(&f.file, &file.metadata().map_err(|_| format!("could not read {}.", f.file))?)?;
    let mut data = Vec::new();
    file.take(f.bytes + 1).read_to_end(&mut data).map_err(|_| format!("could not read {}.", f.file))?;
    let len = data.len() as u64;
    if len > f.bytes || (f.exact && len != f.bytes) {
        return Err(format!("{} changed since it was checked.", f.file));
    }
    Ok(data)
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

    const TOUR: &str = "tour_aaaaaaaaaaaaaaaaaaaaaaaa";
    const PUB: &str = "pub_0123456789abcdef01234567";
    const PUBLISH: &str = "POST /api/v1/tours/tour_aaaaaaaaaaaaaaaaaaaaaaaa/publish";

    fn client(stub: &Stub) -> Client {
        Client::new(&stub.base, TOKEN, Timeouts::default()).unwrap()
    }

    fn in_progress(done: u32, total: u32) -> Answer {
        reply(202, &format!(r#"{{"status":"in_progress","publish_id":"{PUB}","done":{done},"total":{total}}}"#))
    }

    const LIVE: &str = r#"{"slug":"abc123","url":"/t/abc123","steps":5}"#;

    /// A stub whose publish route answers as `publish` says (given how many publish calls came
    /// before) and that holds the client to the protocol: a call after a 202 must name that
    /// publish, any other must have no body; a call that does not is answered 400. Every other
    /// route answers as the platform does.
    fn publish_stub(publish: impl Fn(usize) -> Answer + Send + 'static) -> Stub {
        let named = std::sync::atomic::AtomicBool::new(false);
        stub(move |req, seen| {
            if req.line != PUBLISH {
                return platform(req, seen);
            }
            use std::sync::atomic::Ordering::SeqCst;
            let ok = if named.load(SeqCst) { serde_json::from_slice::<Value>(&req.body).ok() == Some(json!({ "publish_id": PUB })) } else { req.body.is_empty() };
            if !ok {
                named.store(false, SeqCst);
                return reply(400, r#"{"error":"bad_request","detail":"the stub got a publish call that breaks the protocol"}"#);
            }
            let answer = publish(seen);
            named.store(matches!(answer, Answer::Reply(202, ..)), SeqCst);
            answer
        })
    }

    /// Publishes TOUR against the stub with no pause, recording every progress report.
    fn publish(stub: &Stub) -> (Result<Live, String>, Vec<Option<Images>>) {
        let mut seen = Vec::new();
        let r = client(stub).publish_tour(TOUR, Duration::ZERO, |p| {
            seen.push(p);
            true
        });
        (r, seen)
    }

    #[test]
    fn publish_calls_until_the_server_says_it_is_live_and_returns_the_link() {
        let s = publish_stub(|seen| match seen {
            0 => in_progress(12, 22),
            1 => in_progress(22, 22),
            _ => reply(200, LIVE),
        });
        let (r, progress) = publish(&s);
        assert_eq!(r.unwrap(), Live { steps: 5, url: Some(format!("{}/t/abc123", s.base)) });
        assert_eq!(progress, [None, Some((12, 22)), Some((22, 22))]);
        let reqs = s.requests();
        assert_eq!(reqs.len(), 3);
        // The first call has no body; the later ones name the publish. Every call carries the token.
        assert!(reqs[0].body.is_empty());
        for r in &reqs[1..] {
            assert_eq!(serde_json::from_slice::<Value>(&r.body).unwrap(), json!({ "publish_id": PUB }));
            assert_eq!(r.content_type.as_deref(), Some("application/json"));
        }
        assert!(reqs.iter().all(|r| r.auth.as_deref() == Some(&format!("Bearer {TOKEN}"))));
    }

    #[test]
    fn a_small_tour_is_live_after_one_call() {
        let s = publish_stub(|_| reply(200, LIVE));
        let (r, progress) = publish(&s);
        assert_eq!(r.unwrap().steps, 5);
        assert_eq!(progress, [None]);
        assert_eq!(s.lines(), [PUBLISH]);
    }

    #[test]
    fn a_publish_that_stops_making_progress_is_given_up() {
        let s = publish_stub(|_| in_progress(12, 22));
        let (r, _) = publish(&s);
        assert_eq!(r.unwrap_err(), "Could not publish the tour: the server stopped making progress at 12 of 22 images.");
        // The first answer counts as progress; three more without any end it.
        assert_eq!(s.lines().len(), 4);
    }

    #[test]
    fn a_stall_counter_starts_again_when_the_server_moves_on() {
        let s = publish_stub(|seen| match seen {
            0..=2 => in_progress(12, 22),
            3..=5 => in_progress(20, 22),
            _ => reply(200, LIVE),
        });
        assert!(publish(&s).0.is_ok());
        assert_eq!(s.lines().len(), 7);
    }

    #[test]
    fn the_stub_refuses_a_follow_up_call_that_does_not_name_the_publish() {
        // The check the other tests rely on: after a 202, a call with no body is a protocol error.
        let s = publish_stub(|_| in_progress(1, 2));
        let c = client(&s);
        assert_eq!(c.send(Method::Post, "/api/v1/tours/tour_aaaaaaaaaaaaaaaaaaaaaaaa/publish", None).ok().map(|r| r.status), Some(202));
        assert_eq!(c.send(Method::Post, "/api/v1/tours/tour_aaaaaaaaaaaaaaaaaaaaaaaa/publish", None).ok().map(|r| r.status), Some(400));
        // And a first call that names one is refused as well.
        let body = json!({ "publish_id": PUB }).to_string();
        assert_eq!(c.send(Method::Post, "/api/v1/tours/tour_aaaaaaaaaaaaaaaaaaaaaaaa/publish", Some(("application/json", body.as_bytes()))).ok().map(|r| r.status), Some(400));
    }

    #[test]
    fn an_answer_without_a_count_is_not_progress() {
        // No `done` at all, or one that is not a number: three such answers end it as not understood.
        for body in [format!(r#"{{"status":"in_progress","publish_id":"{PUB}"}}"#), format!(r#"{{"status":"in_progress","publish_id":"{PUB}","done":"12","total":22}}"#)] {
            let s = publish_stub(move |_| reply(202, &body));
            let (r, progress) = publish(&s);
            assert_eq!(r.unwrap_err(), "Could not publish the tour: the server's answer was not understood.");
            assert_eq!(s.lines().len(), 3);
            assert!(progress.iter().all(Option::is_none));
        }
        // A count that goes missing in the middle does not reset the stall counter.
        let s = publish_stub(|seen| if seen == 0 { in_progress(12, 22) } else { reply(202, &format!(r#"{{"status":"in_progress","publish_id":"{PUB}"}}"#)) });
        assert!(publish(&s).0.is_err());
        assert_eq!(s.lines().len(), 4);
    }

    #[test]
    fn counts_that_make_no_sense_are_not_shown() {
        // More done than there is: the work still moves, so the publish goes on, but no count is shown.
        let s = publish_stub(|seen| match seen {
            0 => in_progress(30, 22),
            1 => in_progress(40, 22),
            _ => reply(200, LIVE),
        });
        let (r, progress) = publish(&s);
        assert!(r.is_ok());
        assert_eq!(progress, [None, None, None]);
        // Nothing of nothing, again and again: no progress, and no "0 of 0 images" in the sentence.
        let s = publish_stub(|_| in_progress(0, 0));
        let (r, progress) = publish(&s);
        assert_eq!(r.unwrap_err(), "Could not publish the tour: the server's answer was not understood.");
        assert_eq!(s.lines().len(), 4);
        assert!(progress.iter().all(Option::is_none));
    }

    #[test]
    fn a_publish_that_never_finishes_stops_at_the_hard_limit() {
        // Always a little further, never done.
        let s = publish_stub(|seen| in_progress(seen as u32 + 1, 100_000));
        let (r, progress) = publish(&s);
        assert_eq!(r.unwrap_err(), "Could not publish the tour: the server did not finish within 200 calls.");
        assert_eq!(s.lines().len(), 200);
        assert_eq!(progress.last(), Some(&Some((199, 100_000))));
    }

    #[test]
    fn a_web_page_or_a_redirect_instead_of_an_answer_is_refused() {
        // A 200 that is a web page (a login wall, a proxy's page), not the platform's answer.
        let s = publish_stub(|_| reply(200, "<!doctype html><html><body>Sign in</body></html>"));
        assert_eq!(publish(&s).0.unwrap_err(), "Could not publish the tour: the server's answer was not understood.");
        assert_eq!(s.lines().len(), 1);
        // A redirect is never followed: the token stays with the configured server.
        for status in [301, 302, 307, 308] {
            let s = publish_stub(move |_| Answer::Reply(status, String::new(), vec![("Location", "https://elsewhere.example/api/v1/tours/x/publish".to_string())]));
            let e = publish(&s).0.unwrap_err();
            assert_eq!(e, format!("Could not publish the tour: the server answered with a redirect (HTTP {status}), which is not followed. Check the server address."));
            assert_eq!(s.lines(), [PUBLISH]);
        }
        // The same in the middle of a publish.
        let s = publish_stub(|seen| if seen == 0 { in_progress(12, 22) } else { Answer::Reply(302, String::new(), vec![("Location", "/login".to_string())]) });
        assert!(publish(&s).0.unwrap_err().contains("redirect"));
        assert_eq!(s.lines().len(), 2);
    }

    #[test]
    fn publish_refusals_are_sentences_and_are_not_retried() {
        let cases: [(u16, &str, &str); 6] = [
            (409, r#"{"error":"publish_busy","detail":"another publish of this tour is starting at this moment; try again"}"#, "Could not publish the tour: another publish of this tour is starting at this moment."),
            (409, r#"{"error":"publish_cancelled","detail":"x"}"#, "Could not publish the tour: the publish was stopped on the server before it finished."),
            (429, r#"{"error":"rate_limited"}"#, "Could not publish the tour: the server is busy (HTTP 429). Try again later."),
            (500, r#"{"error":"publish_failed","detail":"the tour could not be published; what was published before is unchanged"}"#, "Could not publish the tour: the tour could not be published; what was published before is unchanged (HTTP 500)."),
            (503, "", "Could not publish the tour: the server answered HTTP 503."),
            (400, r#"{"error":"empty_tour","detail":"a tour with no steps cannot be published"}"#, "Could not publish the tour: a tour with no steps cannot be published (HTTP 400)."),
        ];
        for (status, body, message) in cases {
            let s = publish_stub(move |_| reply(status, body));
            let (r, _) = publish(&s);
            assert_eq!(r.unwrap_err(), message);
            assert_eq!(s.lines().len(), 1, "{status} must not be sent again");
        }
        let s = publish_stub(|_| reply(401, r#"{"error":"unauthorized"}"#));
        assert_eq!(publish(&s).0.unwrap_err(), REJECTED_TOKEN);
    }

    #[test]
    fn a_refusal_in_the_middle_of_a_publish_ends_it() {
        let s = publish_stub(|seen| if seen == 0 { in_progress(12, 22) } else { reply(409, r#"{"error":"publish_cancelled"}"#) });
        assert!(publish(&s).0.unwrap_err().contains("stopped on the server"));
        assert_eq!(s.lines().len(), 2);
    }

    #[test]
    fn a_lost_connection_while_publishing_says_so_without_the_token() {
        let s = publish_stub(|_| Answer::Drop);
        let e = publish(&s).0.unwrap_err();
        assert!(e.starts_with("Could not publish the tour: ") && e.ends_with('.') && !e.contains("dp_"), "{e}");
        assert_eq!(s.lines().len(), 1);
        let s = publish_stub(|seen| if seen == 0 { in_progress(12, 22) } else { Answer::Drop });
        let e = publish(&s).0.unwrap_err();
        assert!(e.ends_with("while the images were being copied.") && !e.contains("dp_"), "{e}");
    }

    #[test]
    fn publish_answers_that_make_no_sense_are_refused() {
        for body in [r#"{"status":"in_progress","done":1,"total":2}"#, r#"{"status":"in_progress","publish_id":"pub_../../x","done":1,"total":2}"#] {
            let s = publish_stub(move |_| reply(202, body));
            assert_eq!(publish(&s).0.unwrap_err(), "Could not publish the tour: the server's answer was not understood.");
            assert_eq!(s.lines().len(), 1);
        }
        let s = publish_stub(|_| reply(200, r#"{"slug":"abc"}"#));
        assert!(publish(&s).0.unwrap_err().contains("not understood"));
        // A tour id that is not one never reaches the network.
        let s = publish_stub(|_| reply(200, LIVE));
        assert!(client(&s).publish_tour("tour_x/../y", Duration::ZERO, |_| true).is_err());
        assert!(s.lines().is_empty());
    }

    #[test]
    fn cancel_in_the_middle_of_a_publish_stops_calling() {
        let s = publish_stub(|_| in_progress(12, 22));
        let mut calls = 0;
        let r = client(&s).publish_tour(TOUR, Duration::ZERO, |_| {
            calls += 1;
            calls < 3
        });
        assert_eq!(r.unwrap_err(), CANCELLED);
        // Two calls went out; nothing else is sent, the server clears a publish nobody drives.
        assert_eq!(s.lines(), [PUBLISH, PUBLISH]);
        // Cancelled before the first call: nothing is sent at all.
        let s = publish_stub(|_| reply(200, LIVE));
        assert_eq!(client(&s).publish_tour(TOUR, Duration::ZERO, |_| false).unwrap_err(), CANCELLED);
        assert!(s.lines().is_empty());
    }

    /// The platform, with a publish route that answers as `publish` says and checks the protocol.
    fn platform_with(publish: impl Fn(usize) -> Answer + Send + 'static) -> Stub {
        publish_stub(publish)
    }

    fn upload_and_publish_to(stub: &Stub, publish_now: bool, go: impl FnMut(Option<Images>) -> bool) -> Result<Finished, String> {
        let dir = bundle(2, false);
        client(stub).upload_and_publish(dir.path(), &Target::New, publish_now, Duration::ZERO, |_| true, go)
    }

    #[test]
    fn publish_now_uploads_then_publishes_and_a_draft_only_uploads() {
        let s = platform_with(|seen| if seen == 0 { in_progress(2, 2) } else { reply(200, LIVE) });
        let done = upload_and_publish_to(&s, true, |_| true).unwrap();
        assert_eq!(done, Finished { tour_id: TOUR.into(), steps: 5, outcome: Outcome::Public { url: Some(format!("{}/t/abc123", s.base)) } });
        let lines = s.lines();
        let commit = lines.iter().position(|l| l.ends_with("/commit")).unwrap();
        assert_eq!(&lines[commit + 1..], [PUBLISH, PUBLISH], "publishing starts after the commit");

        let s = platform_with(|_| reply(200, LIVE));
        let done = upload_and_publish_to(&s, false, |_| true).unwrap();
        assert_eq!(done, Finished { tour_id: TOUR.into(), steps: 2, outcome: Outcome::Draft });
        assert!(!s.lines().iter().any(|l| l == PUBLISH), "a draft upload never calls publish");
    }

    #[test]
    fn a_failed_publish_keeps_the_draft_and_trying_again_does_not_upload_again() {
        let s = platform_with(|seen| if seen == 0 { reply(500, r#"{"error":"publish_failed","detail":"the tour could not be published"}"#) } else { reply(200, LIVE) });
        let done = upload_and_publish_to(&s, true, |_| true).unwrap();
        assert_eq!(done.tour_id, TOUR);
        assert_eq!(done.steps, 2);
        assert!(matches!(&done.outcome, Outcome::NotPublic { cancelled: false, error } if error == "Could not publish the tour: the tour could not be published (HTTP 500)."), "{done:?}");
        // The failed publish did not abort the upload: the draft stays.
        assert!(!s.lines().iter().any(|l| l.starts_with("DELETE ")));
        let before = s.lines().len();

        // "Try again" is the publish step alone.
        let again = after_publish(&done.tour_id, done.steps, client(&s).publish_tour(&done.tour_id, Duration::ZERO, |_| true));
        assert_eq!(again.outcome, Outcome::Public { url: Some(format!("{}/t/abc123", s.base)) });
        assert_eq!(again.steps, 5);
        let lines = s.lines();
        assert_eq!(&lines[before..], [PUBLISH]);
        assert_eq!(lines.iter().filter(|l| *l == "POST /api/v1/uploads").count(), 1, "no second upload");
        assert_eq!(lines.iter().filter(|l| l.ends_with("/commit")).count(), 1);
        assert_eq!(lines.iter().filter(|l| l.starts_with("PUT ")).count(), 2);
    }

    #[test]
    fn cancel_during_the_publish_step_leaves_an_uploaded_draft() {
        let s = platform_with(|_| in_progress(1, 2));
        let mut calls = 0;
        let done = upload_and_publish_to(&s, true, |_| {
            calls += 1;
            calls < 2
        })
        .unwrap();
        assert_eq!(done, Finished { tour_id: TOUR.into(), steps: 2, outcome: Outcome::NotPublic { cancelled: true, error: String::new() } });
        let lines = s.lines();
        assert_eq!(lines.iter().filter(|l| *l == PUBLISH).count(), 1);
        assert!(!lines.iter().any(|l| l.starts_with("DELETE ")), "the upload is not aborted: {lines:?}");
    }

    #[test]
    fn a_failed_upload_never_reaches_publish() {
        let s = stub(|req, seen| if req.line.ends_with("/commit") { reply(500, "{}") } else if req.line == PUBLISH { reply(200, LIVE) } else { platform(req, seen) });
        assert!(upload_and_publish_to(&s, true, |_| true).is_err());
        assert!(!s.lines().iter().any(|l| l == PUBLISH));
    }

    #[test]
    fn the_public_link_is_the_servers_made_whole_and_never_plain_http() {
        let base = "https://tours.example.com";
        // A path goes after the configured address; a server under a path keeps it.
        assert_eq!(public_url(base, Some("/t/abc123"), Some("abc123")).as_deref(), Some("https://tours.example.com/t/abc123"));
        assert_eq!(public_url("https://x.dev/demo/", Some("/t/abc"), None).as_deref(), Some("https://x.dev/demo/t/abc"));
        // A full address is used only when it is on the configured server.
        assert_eq!(public_url(base, Some("https://tours.example.com/t/abc"), None).as_deref(), Some("https://tours.example.com/t/abc"));
        assert_eq!(public_url(base, Some("HTTPS://Tours.Example.com/t/abc"), None).as_deref(), Some("https://tours.example.com/t/abc"));
        assert_eq!(public_url("https://x.dev:8443/demo", Some("https://x.dev:8443/t/abc"), None).as_deref(), Some("https://x.dev:8443/t/abc"));
        // Another host, another port, a look-alike host: the link is built from the slug instead, or there is none.
        for elsewhere in ["https://view.example.com/t/abc", "https://tours.example.com:8443/t/abc", "https://tours.example.com.evil.example/t/abc", "https://tours.example.comx/t/abc", "https://evil.example/tours.example.com/t/abc"] {
            assert_eq!(public_url(base, Some(elsewhere), Some("abc")).as_deref(), Some("https://tours.example.com/t/abc"), "{elsewhere}");
            assert_eq!(public_url(base, Some(elsewhere), None), None, "{elsewhere}");
        }
        assert_eq!(public_url("http://localhost:8787", Some("http://localhost:9999/t/abc"), None), None);
        assert_eq!(public_url("http://localhost:8787", Some("https://localhost:8787/t/abc"), None), None);
        // No url: built from the slug.
        assert_eq!(public_url(base, None, Some("abc-1_2")).as_deref(), Some("https://tours.example.com/t/abc-1_2"));
        // http is for localhost and 127.0.0.1 only.
        assert_eq!(public_url("http://localhost:8787", Some("/t/abc"), None).as_deref(), Some("http://localhost:8787/t/abc"));
        assert_eq!(public_url("http://127.0.0.1:8787", None, Some("abc")).as_deref(), Some("http://127.0.0.1:8787/t/abc"));
        assert_eq!(public_url("http://localhost:8787", Some("http://localhost:8787/t/abc"), None).as_deref(), Some("http://localhost:8787/t/abc"));
        // A link that is not https falls back to the slug, and without one there is no link.
        assert_eq!(public_url(base, Some("http://tours.example.com/t/abc"), Some("abc")).as_deref(), Some("https://tours.example.com/t/abc"));
        assert_eq!(public_url(base, Some("http://tours.example.com/t/abc"), None), None);
        for bad in ["javascript://x/%0aalert(1)", "file:///c:/x", "//evil.example/t/abc", "t/abc", "", "/t/a b", "/t/abc?x=1", "/t/abc#x", "/t/\"x", "/t/a&calc", "/../x", "https://user@x.dev/t/a"] {
            assert_eq!(public_url(base, Some(bad), None), None, "{bad:?}");
            assert_eq!(public_url(base, Some(bad), Some("abc")).as_deref(), Some("https://tours.example.com/t/abc"), "{bad:?}");
        }
        for bad_slug in ["", "a/b", "a b", "..", "a?b"] {
            assert_eq!(public_url(base, None, Some(bad_slug)), None, "{bad_slug:?}");
        }
        assert_eq!(public_url(base, Some(&format!("/t/{}", "a".repeat(400))), None), None);
        assert_eq!(public_url("http://example.com", Some("/t/abc"), Some("abc")), None);
        // Every link this makes is one the opener accepts.
        for (server, url, slug) in [(base, Some("/t/abc123"), None), ("http://localhost:8787", None, Some("abc")), ("https://x.dev:8443/demo/", Some("/t/a-b_c"), None)] {
            assert!(mythicshot_destinations::is_web_link(&public_url(server, url, slug).unwrap()));
        }
    }

    #[test]
    fn a_publish_result_says_whether_the_tour_is_public() {
        let live = Live { steps: 7, url: Some("https://x.dev/t/a".into()) };
        assert_eq!(after_publish(TOUR, 2, Ok(live)), Finished { tour_id: TOUR.into(), steps: 7, outcome: Outcome::Public { url: Some("https://x.dev/t/a".into()) } });
        assert_eq!(after_publish(TOUR, 2, Err(CANCELLED.into())).outcome, Outcome::NotPublic { cancelled: true, error: String::new() });
        assert_eq!(after_publish(TOUR, 2, Err("Could not publish the tour: x.".into())), Finished { tour_id: TOUR.into(), steps: 2, outcome: Outcome::NotPublic { cancelled: false, error: "Could not publish the tour: x.".into() } });
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
                RemoteTour { id: "tour_1".into(), title: "Onboarding".into(), status: "draft".into(), chapters: 2, steps: 9, link: None },
                RemoteTour { id: "tour_2".into(), title: "Billing".into(), status: "published".into(), chapters: 1, steps: 3, link: None },
            ]
        );
        // A list that names a tour's slug or url gives its link, by the same rule as a publish answer.
        let s2 = stub(|_, _| reply(200, r#"{"tours":[{"id":"a","title":"A","status":"published","chapters":1,"steps":3,"slug":"abc"},{"id":"b","title":"B","status":"published","chapters":1,"steps":3,"url":"https://evil.example/t/x"}]}"#));
        let links: Vec<Option<String>> = list_tours(&s2.base, TOKEN).unwrap().into_iter().map(|t| t.link).collect();
        assert_eq!(links, [Some(format!("{}/t/abc", s2.base)), None]);
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
        std::fs::write(dir.path().join("tour.json"), r#"{"steps":[{"image":"../secret.png","bytes":15}]}"#).unwrap();
        let err = upload_bundle(&s.base, TOKEN, dir.path(), &Target::New, |_| true).unwrap_err();
        assert!(err.contains("not a plain file name"), "{err}");

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tour.json"), r#"{"steps":[{"image":"step-001.png","bytes":15}],"cover":{"logo":"C:logo.png"}}"#).unwrap();
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

    #[test]
    fn a_chapter_upload_of_a_bundle_with_a_logo_leaves_the_logo_out() {
        let s = stub(|req, seen| if req.line.ends_with("/logo") { reply(409, r#"{"error":"logo_not_used","detail":"do not send one"}"#) } else { platform(req, seen) });
        let dir = bundle(2, true);
        let mut seen = Vec::new();
        upload_bundle(&s.base, TOKEN, dir.path(), &Target::Chapter("tour_bbbbbbbbbbbbbbbbbbbbbbbb".into()), |p| {
            seen.push(p);
            true
        })
        .unwrap();
        assert!(!s.lines().iter().any(|l| l.ends_with("/logo")), "{:?}", s.lines());
        assert_eq!(seen, [0.0, 0.25, 0.5, 0.75, 1.0]);

        // A chapter upload does not even need the logo file.
        let dir = bundle(1, true);
        std::fs::remove_file(dir.path().join("logo.png")).unwrap();
        let s = stub(platform);
        upload_bundle(&s.base, TOKEN, dir.path(), &Target::Chapter("tour_bbbbbbbbbbbbbbbbbbbbbbbb".into()), |_| true).unwrap();
    }

    #[test]
    fn a_request_gets_more_time_for_a_bigger_body() {
        let base = Duration::from_secs(30);
        assert_eq!(request_timeout(base, 0), base);
        assert_eq!(request_timeout(base, 2_000_000), Duration::from_secs(130));
        assert_eq!(request_timeout(base, 200_000), Duration::from_secs(40));
        assert_eq!(request_timeout(base, usize::MAX), MAX_REQUEST_TIMEOUT);
    }

    #[test]
    fn a_panic_mid_upload_still_aborts_once() {
        let s = stub(platform);
        let dir = bundle(3, false);
        let mut calls = 0;
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            upload_bundle(&s.base, TOKEN, dir.path(), &Target::New, |_| {
                calls += 1;
                assert!(calls < 3, "progress callback fails");
                true
            })
        }));
        assert!(caught.is_err());
        let lines = s.lines();
        assert_eq!(lines.iter().filter(|l| l.starts_with("DELETE ")).count(), 1, "{lines:?}");
        assert!(!lines.iter().any(|l| l.ends_with("/commit")));
    }

    #[test]
    fn a_bundle_file_must_be_a_regular_file_of_the_declared_size() {
        let s = stub(platform);
        let dir = bundle(1, false);
        std::fs::remove_file(dir.path().join("step-001.png")).unwrap();
        std::fs::create_dir(dir.path().join("step-001.png")).unwrap();
        let err = upload_bundle(&s.base, TOKEN, dir.path(), &Target::New, |_| true).unwrap_err();
        assert!(err.contains("step-001.png is not a regular file"), "{err}");

        let dir = bundle(1, false);
        std::fs::write(dir.path().join("step-001.png"), b"\x89PNG\r\n\x1a\nfakepng and more").unwrap();
        let err = upload_bundle(&s.base, TOKEN, dir.path(), &Target::New, |_| true).unwrap_err();
        assert!(err.contains("step-001.png is not the size tour.json gives"), "{err}");
        assert!(s.lines().is_empty());

        // A file that grows after the check is read only up to its declared size plus one byte, and refused.
        let dir = bundle(1, false);
        let b = Bundle::read(dir.path(), true).unwrap();
        std::fs::write(dir.path().join("step-001.png"), vec![7u8; 5_000_000]).unwrap();
        let err = read_bundle_file(dir.path(), &b.steps[0]).unwrap_err();
        assert!(err.contains("changed"), "{err}");
    }

    #[test]
    fn a_symlinked_bundle_file_is_refused() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.png"), PNG).unwrap();
        let dir = bundle(1, false);
        std::fs::remove_file(dir.path().join("step-001.png")).unwrap();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(outside.path().join("secret.png"), dir.path().join("step-001.png"));
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(outside.path().join("secret.png"), dir.path().join("step-001.png"));
        if made.is_err() {
            eprintln!("cannot make symlinks here; skipped");
            return;
        }
        let s = stub(platform);
        let err = upload_bundle(&s.base, TOKEN, dir.path(), &Target::New, |_| true).unwrap_err();
        assert!(err.contains("not a regular file"), "{err}");
        assert!(s.lines().is_empty());
    }

    #[test]
    fn server_text_loses_invisible_characters_and_is_capped() {
        let detail = format!("a\u{202E}b\u{2066}c\u{200B}d\u{FEFF}e\u{00AD}f\u{061C}g\u{2060}h{}", "x".repeat(1000));
        let body = json!({ "error": "bad", "detail": detail }).to_string();
        let s = stub(move |_, _| Answer::Reply(400, body.clone(), vec![]));
        let err = workspace_name(&s.base, TOKEN).unwrap_err();
        assert!(err.contains("abcdefgh"), "{err}");
        assert!(!err.chars().any(|c| is_invisible(c) || c.is_control()), "{err:?}");
        assert!(err.chars().count() < MAX_DETAIL_CHARS + 60, "{}", err.len());
    }

    #[test]
    fn too_many_open_uploads_is_explained_and_not_retried() {
        let s = stub(|req, seen| {
            if req.line == "POST /api/v1/uploads" {
                Answer::Reply(429, r#"{"error":"too_many_uploads","detail":"this workspace already has 5 open uploads; finish or cancel one first"}"#.into(), vec![("Retry-After", "60".into())])
            } else {
                platform(req, seen)
            }
        });
        let err = upload(&s, 1, false, &Target::New).unwrap_err();
        assert!(err.contains("too many unfinished uploads"), "{err}");
        assert!(err.contains("60 seconds"), "{err}");
        assert_eq!(s.lines(), ["POST /api/v1/uploads"]);
    }

    #[derive(Default)]
    struct FakeStore {
        value: RefCell<Option<String>>,
        locked: bool,
    }

    impl SecretStore for FakeStore {
        fn get(&self) -> Result<Option<String>, String> {
            if self.locked {
                return Err("Could not use the credential store: the credential store is locked or not reachable.".into());
            }
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
        assert_eq!(load_token_from(&store), Ok(None));
        store_token_in(&store, &format!("  {TOKEN}\n")).unwrap();
        assert_eq!(store.value.borrow().as_deref(), Some(TOKEN));
        assert_eq!(load_token_from(&store).unwrap().as_deref(), Some(TOKEN));
        for bad in ["", "dp_", "dp_0123", &TOKEN.to_uppercase(), &format!("{TOKEN}\r\nX-Evil: 1")] {
            assert!(store_token_in(&store, bad).is_err(), "{bad:?}");
        }
        assert_eq!(load_token_from(&store).unwrap().as_deref(), Some(TOKEN));
        forget_token_in(&store).unwrap();
        assert_eq!(load_token_from(&store), Ok(None));
        // Something stored that is not a token is reported, not taken for "no token", so it can be forgotten.
        *store.value.borrow_mut() = Some("garbage".into());
        let err = load_token_from(&store).unwrap_err();
        assert!(err.contains("could not be read") && !err.contains("garbage"), "{err}");
    }

    #[test]
    fn a_locked_credential_store_is_not_the_same_as_no_token() {
        let store = FakeStore { value: RefCell::new(Some(TOKEN.into())), locked: true };
        let err = load_token_from(&store).unwrap_err();
        assert!(err.contains("locked or not reachable"), "{err}");
        assert!(!err.contains(TOKEN));
    }

    #[test]
    fn the_settings_file_has_the_server_but_no_token() {
        let text = toml::to_string(&crate::settings::Settings::default()).unwrap();
        assert!(text.contains("publish_server = \"\""), "{text}");
        assert!(!text.to_ascii_lowercase().contains("token"));
    }
}
