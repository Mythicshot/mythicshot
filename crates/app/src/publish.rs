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
//! Managing a tour that is already on the site (the Tours page) is a call or a short loop
//! each: rename is one PATCH; unpublish and delete are called again while the server
//! answers 202, like publish. A tour that is gone answers 404 `no such tour`: for delete
//! that is what was asked for, for the others the error says the tour no longer exists
//! (see [`is_gone`]). A site older than 0.1.1 has no rename or delete route, and says so.
//!
//! An update replaces one chapter of a tour that is already on the site (`Target::Replace`).
//! The create also carries a SHA-256 of every step picture, and the site answers which steps
//! it already has (`have`): those are not sent again. The logo goes only when the site says
//! it takes the tour's settings from this chapter (`settings`). The commit's answer says
//! whether publishing is safe; an update is published only when it says `ready`.
//!
//! Nothing is retried blindly. Only two requests are ever sent a second time, and only
//! after a transport failure (no answer at all): a step image, where the server answers
//! 409 `already_sent` if the first copy did arrive, and commit, which the server treats
//! as idempotent (a repeated commit answers with the first one's result).
//!
//! Sharing a single image or clip with a link is its own protocol on the same client: see [`share`].
//!
//! Signing in (this computer gets its own token after a code is confirmed in the browser) is
//! another, on a client that sends no credential: see [`connect`].

pub mod connect;
pub mod share;

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

/// Longest site version shown.
const MAX_VERSION_CHARS: usize = 20;
/// The 404 detail the server gives for a tour it does not have (unknown, deleted, another
/// workspace's). A 404 without it is a route the server does not have: an older site.
const NO_SUCH_TOUR: &str = "no such tour";
/// How an error ends when the tour it was about is no longer on the site; see [`is_gone`].
const GONE: &str = "that tour no longer exists.";
/// The sentence the Tours page shows for such an error.
pub const TOUR_GONE: &str = "That tour no longer exists.";
/// What a delete's error ends with when the tour is gone but files are left. The tour is no
/// longer in the list then, so the way back is the Tours page's own block for it.
const UNFINISHED_TRAILER: &str = " The tour itself is gone from the site, but some of its files remain. Use Finish deleting on the Tours page to remove them.";
/// What rename and delete say to a site that does not have those routes yet.
const NEEDS_UPDATE: &str = "the site needs an update to version 0.1.1 or newer.";
/// The 404 detail the server gives for a chapter that is not (or no longer) the tour's.
const NO_SUCH_CHAPTER: &str = "no such chapter";
/// What an update says when the chapter it would replace is not there.
const CHAPTER_GONE: &str = "that chapter is no longer part of the tour.";
/// What an update says to a site that cannot replace a chapter yet.
const UPDATE_NEEDS_UPDATE: &str = "the site needs an update to version 0.1.4 or newer.";
/// The first site version that can replace a chapter.
const UPDATE_SINCE: (u32, u32, u32) = (0, 1, 4);

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
    /// When it was last uploaded to, in milliseconds since 1970, when the list says.
    pub updated_at: Option<i64>,
    /// When it went public, in milliseconds since 1970: only a published tour on a site of 0.1.1 or newer has it.
    pub published_at: Option<i64>,
    /// The tour is public and its live page is behind its draft: only a site of 0.1.6 or newer
    /// says. False when the list does not say (an older site), says anything but yes or no, or
    /// says it of a draft.
    pub unpublished_changes: bool,
}

impl RemoteTour {
    pub fn is_public(&self) -> bool {
        self.status == "published"
    }

    /// Whether changes wait to be published: a public tour the site says is behind its draft.
    pub fn changes_waiting(&self) -> bool {
        self.is_public() && self.unpublished_changes
    }
}

/// What `GET /api/v1/me` says: the workspace's name, and the site's version when the site is new enough to tell.
#[derive(Debug, Clone, PartialEq)]
pub struct Workspace {
    pub name: String,
    pub version: Option<String>,
}

/// How a delete that ended well was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deleted {
    /// The site answered this delete for this tour: it removed it, or had begun to before it
    /// said nothing is left.
    BySite,
    /// The site's first answer was that it has no such tour. That is a tour deleted earlier or
    /// elsewhere, and just as well a tour of another workspace than the token's: the site
    /// answers both the same way. Nothing of the token's workspace is left to delete either way.
    NotThere,
}

/// A delete that did not finish.
#[derive(Debug, Clone, PartialEq)]
pub struct DeleteFailed {
    pub message: String,
    /// The site has already removed the tour itself; only some of its files are left.
    /// Deleting the same tour again carries on where this stopped.
    pub unfinished: bool,
    /// The site refused before it began (or was never asked): the tour is as it was, whole.
    /// Never set together with `unfinished`. Neither set: the answer was lost or unclear, so
    /// the site may or may not have begun.
    pub not_started: bool,
}

/// Whether `error` (from rename, publish or unpublish) says the tour is no longer on the site.
/// Text from the server can never produce this ending: it is always followed by "(HTTP …)".
pub fn is_gone(error: &str) -> bool {
    error.ends_with(&format!(": {GONE}"))
}

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    New,
    Chapter(String),
    /// Replace the steps of a chapter that is already on the site; the tour's link stays.
    Replace { tour_id: String, chapter_id: String },
}

/// What the site says about publishing after a chapter was replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishAdvice {
    /// The tour is not public: nothing to publish.
    Draft,
    /// The tour is public and this update is its only unpublished change.
    Ready,
    /// The tour is public but holds other changes that were never published.
    OtherChanges,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Published {
    pub tour_id: String,
    /// The chapter the upload made, or the one it replaced.
    pub chapter_id: String,
    pub steps: usize,
    /// Only a replace gets one; None for a new tour or a new chapter.
    pub publish: Option<PublishAdvice>,
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
    /// An update that was uploaded and not published, because the tour has other changes that were never published.
    Held,
}

/// An upload that worked, and what became of its publish.
#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
    pub tour_id: String,
    /// The chapter the upload made or replaced. Empty when only a publish ran and the caller did not know it.
    pub chapter_id: String,
    pub steps: usize,
    pub outcome: Outcome,
}

/// Images copied so far out of how many, from the server's last answer.
pub type Images = (u64, u64);

/// What a publish's result means for a tour that is already uploaded with `steps` steps.
/// A failed or cancelled publish is not an error of the whole job: the draft is there.
pub fn after_publish(tour_id: &str, chapter_id: &str, steps: usize, result: Result<Live, String>) -> Finished {
    let (steps, outcome) = match result {
        Ok(live) => (live.steps, Outcome::Public { url: live.url }),
        Err(e) if e == CANCELLED => (steps, Outcome::NotPublic { cancelled: true, error: String::new() }),
        Err(error) => (steps, Outcome::NotPublic { cancelled: false, error }),
    };
    Finished { tour_id: tour_id.to_string(), chapter_id: chapter_id.to_string(), steps, outcome }
}

/// Whether a site of this version can replace a chapter ([`update_tour`]): 0.1.4 and anything
/// newer. No version (an older site tells none) and text that is not `major.minor.patch` are a no.
pub fn supports_update(version: Option<&str>) -> bool {
    let Some(version) = version else { return false };
    let mut parts = version.split('.').map(|p| if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) { p.parse::<u32>().ok() } else { None });
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(Some(major)), Some(Some(minor)), Some(Some(patch)), None) => (major, minor, patch) >= UPDATE_SINCE,
        _ => false,
    }
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

/// The host name of a site address the address rule accepts, in lower case; an IPv6 address keeps its brackets.
pub fn host_of(address: &str) -> Option<String> {
    let url = check_server(address).ok()?;
    let authority = url.split_once("://")?.1.split('/').next()?;
    let host = match authority.strip_prefix('[') {
        Some(rest) => &authority[..rest.find(']')? + 2],
        None => authority.rsplit_once(':').map_or(authority, |(host, _port)| host),
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Whether `id` has the form of a tour id.
pub fn is_tour_id(id: &str) -> bool {
    is_id(id, "tour_")
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

/// The workspace's name and the site's version.
pub fn workspace(server: &str, token: &str) -> Result<Workspace, String> {
    Client::new(server, token, Timeouts::default())?.workspace()
}

pub fn list_tours(server: &str, token: &str) -> Result<Vec<RemoteTour>, String> {
    Client::new(server, token, Timeouts::default())?.list_tours()
}

/// Renames the tour; returns the title as the site saved it. The link stays the same.
pub fn rename_tour(server: &str, token: &str, tour_id: &str, title: &str) -> Result<String, String> {
    Client::new(server, token, Timeouts::default())?.rename_tour(tour_id, title)
}

/// Deletes the tour: calls the delete route until the site says nothing of it is left.
/// `progress` is called once before every request, with the files removed so far when the
/// site says. A second call of it therefore means the site has begun: the tour itself is
/// gone and only files remain, which is the moment to note the delete as unfinished. A tour that is already
/// gone is a success, told apart as [`Deleted::NotThere`]. There is no cancel: the first call removes the tour, the rest only
/// its files, and stopping half-way would leave files nobody can find again. Blocks like
/// [`publish_tour`].
pub fn delete_tour(server: &str, token: &str, tour_id: &str, progress: impl FnMut(Option<Images>)) -> Result<Deleted, DeleteFailed> {
    Client::new(server, token, Timeouts::default()).map_err(|message| DeleteFailed { message, unfinished: false, not_started: true })?.delete_tour(tour_id, PUBLISH_PAUSE, progress)
}

/// Takes the tour off the public web: calls the unpublish route until its public images
/// are gone too. The tour stays on the site as a draft.
pub fn unpublish_tour(server: &str, token: &str, tour_id: &str) -> Result<(), String> {
    Client::new(server, token, Timeouts::default())?.unpublish_tour(tour_id, PUBLISH_PAUSE)
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
/// whether the tour is public. `uploaded` is called once, when the commit has answered and
/// before the publish step: with what the site made of the upload, and never for an upload
/// that failed or was cancelled.
#[allow(clippy::too_many_arguments)]
pub fn upload_and_publish(
    server: &str,
    token: &str,
    dir: &Path,
    target: &Target,
    publish_now: bool,
    upload_progress: impl FnMut(f32) -> bool,
    uploaded: impl FnOnce(&Published),
    publish_progress: impl FnMut(Option<Images>) -> bool,
) -> Result<Finished, String> {
    Client::new(server, token, Timeouts::default())?.upload_and_publish(dir, target, publish_now, PUBLISH_PAUSE, upload_progress, uploaded, publish_progress)
}

/// Updates a tour over the same link: replaces the chapter `chapter_id` of `tour_id` with the
/// bundle in `dir`, sending only the pictures the site does not have yet, and then publishes
/// the tour when the site says that is safe. A tour that is a draft stays one
/// (`Outcome::Draft`); a public tour with other changes that were never published is uploaded
/// and not published (`Outcome::Held`); a publish that fails or is cancelled leaves the
/// update uploaded (`Outcome::NotPublic`). An error means the site's chapter is as it was,
/// with one exception the error names itself: when the commit's answer was lost ("Check the
/// tour list before uploading again."), the site may or may not hold the update.
/// `uploaded` is called exactly once, when the commit has answered and before anything is
/// published, whatever the site says about publishing; never for an update that failed or was
/// cancelled. A panic in it is caught: the update's outcome is reported all the same.
/// Blocks like [`upload_bundle`].
#[allow(clippy::too_many_arguments)]
pub fn update_tour(
    server: &str,
    token: &str,
    dir: &Path,
    tour_id: &str,
    chapter_id: &str,
    upload_progress: impl FnMut(f32) -> bool,
    uploaded: impl FnOnce(&Published),
    publish_progress: impl FnMut(Option<Images>) -> bool,
) -> Result<Finished, String> {
    Client::new(server, token, Timeouts::default())?.update_tour(dir, tour_id, chapter_id, PUBLISH_PAUSE, upload_progress, uploaded, publish_progress)
}

/// Whether `error` (from [`update_tour`]) says the chapter is no longer part of its tour.
pub fn is_chapter_gone(error: &str) -> bool {
    error.ends_with(&format!(": {CHAPTER_GONE}"))
}

/// Whether `error` (from [`update_tour`]) says the site is too old to replace a chapter.
pub fn needs_newer_site(error: &str) -> bool {
    error.ends_with(&format!(": {UPDATE_NEEDS_UPDATE}"))
}

/// The heading over the message for an upload the site refused because the workspace has no room for it.
pub const FULL_HEADING: &str = "The tour was not uploaded";
/// The small line under that message.
pub const FULL_NOTE: &str = "Tours that are already online are not affected.";

/// The error for an upload refused for want of room: the heading, the sentence with the three
/// sizes, and the small line, as one text. Waiting does not help; making room does. What the
/// tour needs is rounded up and what is left down, so the two never read as if it would fit.
fn full_workspace_error(needed: u64, left: u64, allowance: u64) -> String {
    format!(
        "{FULL_HEADING}. This tour needs {}, and your workspace has {} left of its {}. Delete tours you no longer need, or ask for more room, and publish again. {FULL_NOTE}",
        size_rounded(needed, true),
        size_rounded(left, false),
        crate::publish_ui::size_text(allowance)
    )
}

/// A size written the way the app writes sizes (whole KB, MB with one decimal below 100 and
/// whole above, GB with one decimal), but rounded up or down instead of to the nearest. Rounded
/// up, anything above nothing is at least "1 KB", and a size never reads as 1024 of a unit.
fn size_rounded(bytes: u64, up: bool) -> String {
    const MB: u128 = 1 << 20;
    const GB: u128 = 1 << 30;
    let bytes = u128::from(bytes);
    let over = |unit: u128| if up { bytes.div_ceil(unit) } else { bytes / unit };
    let tenths = |unit: u128| if up { (bytes * 10).div_ceil(unit) } else { bytes * 10 / unit };
    if bytes < MB && over(1024) < 1024 {
        format!("{} KB", over(1024))
    } else if tenths(MB) < 1000 {
        format!("{}.{} MB", tenths(MB) / 10, tenths(MB) % 10)
    } else if bytes < GB && over(MB) < 1024 {
        format!("{} MB", over(MB))
    } else {
        format!("{}.{} GB", tenths(GB) / 10, tenths(GB) % 10)
    }
}

/// When `error` (from an upload or an update) says the workspace has no room for the tour: the
/// sentence with the sizes, to show between [`FULL_HEADING`] and [`FULL_NOTE`]. Text from the
/// server can never produce this: it always follows "Could not …: ".
pub fn full_workspace(error: &str) -> Option<&str> {
    let body = error.strip_prefix(FULL_HEADING)?.strip_prefix(". ")?.strip_suffix(FULL_NOTE)?.strip_suffix(' ')?;
    (!body.is_empty()).then_some(body)
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
        if is_token(t) { Ok(Token(t.to_string())) } else { Err("That is not an API token. A token starts with dp_ and comes from the server's setup page.".into()) }
    }
}

/// Whether `text` is an API token as it is: `dp_` and 40 lowercase hex characters, nothing around it.
fn is_token(text: &str) -> bool {
    text.len() == 43 && text.starts_with("dp_") && text[3..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Drop for Token {
    /// The token's bytes are overwritten when it goes, like a typed one.
    fn drop(&mut self) {
        crate::publish_ui::wipe(&mut self.0);
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
        Some(mut stored) => {
            let parsed = Token::parse(&stored);
            crate::publish_ui::wipe(&mut stored);
            match parsed {
                Ok(mut t) => Ok(Some(std::mem::take(&mut t.0))),
                Err(_) => Err("Could not use the credential store: the stored token could not be read. Forget it and save the token again.".into()),
            }
        }
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
    /// None for the requests that carry no credential (signing in); see [`Client::anonymous`].
    token: Option<Token>,
    agent: AgentDebug,
    timeouts: Timeouts,
}

/// Adds the `Authorization` header when the client has a token, and nothing otherwise.
trait WithAuth {
    fn with_auth(self, client: &Client) -> Self;
}

impl<B> WithAuth for ureq::RequestBuilder<B> {
    fn with_auth(self, client: &Client) -> Self {
        match &client.token {
            Some(token) => self.header("Authorization", format!("Bearer {}", token.0)),
            None => self,
        }
    }
}

/// The tokens a site refused (401), so that the windows stop saying "Connected" for a token
/// that was revoked on the site. Kept only as SHA-256 fingerprints, never the token, and only a
/// few; a later answer that accepts the same token takes it off. See [`is_refused`].
mod refused {
    use std::sync::Mutex;

    /// More than enough: there is one saved token at a time.
    const KEEP: usize = 8;

    static REFUSED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    fn list() -> std::sync::MutexGuard<'static, Vec<String>> {
        REFUSED.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn rejected(token: &super::Token) {
        let fingerprint = super::token_fingerprint(&token.0);
        let mut list = list();
        list.retain(|f| *f != fingerprint);
        list.push(fingerprint);
        let over = list.len().saturating_sub(KEEP);
        list.drain(..over);
    }

    pub(super) fn accepted(token: &super::Token) {
        let mut list = list();
        if !list.is_empty() {
            let fingerprint = super::token_fingerprint(&token.0);
            list.retain(|f| *f != fingerprint);
        }
    }

    pub(super) fn is(fingerprint: &str) -> bool {
        list().iter().any(|f| f == fingerprint)
    }
}

/// A fingerprint of `token` (its SHA-256), for telling whether it is the one a site refused
/// without keeping the token itself.
pub fn token_fingerprint(token: &str) -> String {
    sha256_hex(token.trim().as_bytes())
}

/// Whether the token with this fingerprint is the one a site last refused with 401.
pub fn is_refused(fingerprint: &str) -> bool {
    refused::is(fingerprint)
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
    Patch,
    Delete,
}

impl Client {
    fn new(server: &str, token: &str, timeouts: Timeouts) -> Result<Client, String> {
        let token = Token::parse(token)?;
        Self::build(server, Some(token), timeouts)
    }

    /// A client that sends no credential at all: for the sign-in requests, which come before
    /// there is a token. Same rules otherwise: the configured server only, no redirects.
    fn anonymous(server: &str, timeouts: Timeouts) -> Result<Client, String> {
        Self::build(server, None, timeouts)
    }

    fn build(server: &str, token: Option<Token>, timeouts: Timeouts) -> Result<Client, String> {
        let base = check_server(server)?;
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
        let a = &self.agent.0;
        // Sending the body counts against the timeout, so a bigger body gets more time.
        let limit = Some(request_timeout(self.timeouts.request, body.map_or(0, |(_, d)| d.len())));
        let result = match (method, body) {
            (Method::Get, _) => a.get(&url).config().timeout_global(limit).build().with_auth(self).call(),
            (Method::Delete, _) => a.delete(&url).config().timeout_global(limit).build().with_auth(self).call(),
            (Method::Post, None) => a.post(&url).config().timeout_global(limit).build().with_auth(self).send_empty(),
            (Method::Put, None) => a.put(&url).config().timeout_global(limit).build().with_auth(self).send_empty(),
            (Method::Post, Some((ct, data))) => a.post(&url).config().timeout_global(limit).build().with_auth(self).header("Content-Type", ct).send(data),
            (Method::Put, Some((ct, data))) => a.put(&url).config().timeout_global(limit).build().with_auth(self).header("Content-Type", ct).send(data),
            (Method::Patch, None) => a.patch(&url).config().timeout_global(limit).build().with_auth(self).send_empty(),
            (Method::Patch, Some((ct, data))) => a.patch(&url).config().timeout_global(limit).build().with_auth(self).header("Content-Type", ct).send(data),
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
            200..=299 => {
                if let Some(token) = &self.token {
                    refused::accepted(token);
                }
                Ok(reply)
            }
            401 => {
                if let Some(token) = &self.token {
                    refused::rejected(token);
                }
                Err(REJECTED_TOKEN.into())
            }
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

    /// What a `storage_full` answer says in bytes: what the upload needs, what is left of the
    /// allowance (never below nothing), and the allowance. None unless all three are whole numbers.
    fn room(reply: &Reply) -> Option<(u64, u64, u64)> {
        let v = reply.json()?;
        let n = |key: &str| v.get(key).and_then(Value::as_u64);
        let (used, allowance, needed) = (n("used_bytes")?, n("allowance_bytes")?, n("needed_bytes")?);
        Some((needed, allowance.saturating_sub(used), allowance))
    }

    /// The server's `detail` (or `error`) as one short line of plain text, never holding the token.
    fn server_detail(&self, reply: &Reply) -> Option<String> {
        let v = reply.json()?;
        let text = v.get("detail").and_then(Value::as_str).or_else(|| v.get("error").and_then(Value::as_str))?;
        let clean: String = text.chars().filter(|&c| !c.is_control() && !is_invisible(c)).take(MAX_DETAIL_CHARS).collect();
        let clean = match &self.token {
            Some(token) => clean.replace(&token.0, "[token]"),
            None => clean,
        };
        let clean = clean.replace("dp_", "dp-");
        let clean = clean.trim().to_string();
        (!clean.is_empty()).then_some(clean)
    }

    fn workspace(&self) -> Result<Workspace, String> {
        let what = "Could not check the server";
        let reply = self.exchange(what, Method::Get, "/api/v1/me", None)?;
        let v = reply.json().ok_or_else(|| not_understood(what))?;
        let name = v.get("workspace").and_then(Value::as_str).ok_or_else(|| not_understood(what))?.to_string();
        // A version is shown only when it looks like one; an older site sends none.
        let version = v.get("version").and_then(Value::as_str).filter(|s| is_version(s)).map(str::to_string);
        Ok(Workspace { name, version })
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
                let ms = |k: &str| r.get(k).and_then(Value::as_i64).filter(|ms| *ms > 0);
                let status = s("status")?;
                // Only a published tour has a link, whatever else the row says.
                let link = if status == "published" { public_url(&self.base, r.get("url").and_then(Value::as_str), r.get("slug").and_then(Value::as_str)) } else { None };
                let published_at = if status == "published" { ms("published_at") } else { None };
                // Only a plain yes counts, and only of a published tour: there the live page can be behind.
                let unpublished_changes = status == "published" && r.get("unpublished_changes").and_then(Value::as_bool).unwrap_or(false);
                Some(RemoteTour { id: s("id")?, title: s("title")?, status, chapters: n("chapters")?, steps: n("steps")?, link, updated_at: ms("updated_at"), published_at, unpublished_changes })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| not_understood(what))
    }

    fn upload_bundle(&self, dir: &Path, target: &Target, mut progress: impl FnMut(f32) -> bool) -> Result<Published, String> {
        let what = "Could not start the upload";
        let replace = matches!(target, Target::Replace { .. });
        let target_json = match target {
            Target::New => json!({ "kind": "new" }),
            Target::Chapter(id) => json!({ "kind": "chapter", "tour_id": id }),
            Target::Replace { tour_id, chapter_id } => {
                // Ids that are not ids never reach the network; the site would answer the same.
                if !is_id(tour_id, "tour_") {
                    return Err(format!("{what}: {GONE}"));
                }
                if !is_id(chapter_id, "ch_") {
                    return Err(format!("{what}: {CHAPTER_GONE}"));
                }
                json!({ "kind": "replace", "tour_id": tour_id, "chapter_id": chapter_id })
            }
        };
        // A new chapter keeps its tour's logo; the server refuses another (409 logo_not_used).
        // A replace sends it only when the site asks for the tour's settings, which its answer says.
        let bundle = Bundle::read(dir, !matches!(target, Target::Chapter(_)))?;
        if !progress(0.0) {
            return Err(CANCELLED.into());
        }

        let mut create = json!({ "manifest": bundle.manifest, "target": target_json });
        if replace {
            // One hash per step, in step order: the site answers which pictures it already has.
            create["sha256"] = json!(bundle.steps.iter().map(|s| s.sha256.as_str()).collect::<Vec<_>>());
        }
        // The logo's exact size, so a site with a storage allowance keeps room for that much and
        // not for the largest logo there can be. Only with a logo; an older site ignores it.
        if let Some(logo) = bundle.logo.as_ref().filter(|l| l.bytes > 0) {
            create["logo_bytes"] = json!(logo.bytes);
        }
        let body = serde_json::to_vec(&create).map_err(|_| "Could not read tour.json.".to_string())?;
        let reply = self.send(Method::Post, "/api/v1/uploads", Some(("application/json", &body))).map_err(|t| format!("{what}: {}.", t.0))?;
        if !matches!(target, Target::New) && Self::says_no_such_tour(&reply) {
            return Err(format!("{what}: {GONE}"));
        }
        if replace && Self::says_not_found(&reply, NO_SUCH_CHAPTER) {
            return Err(format!("{what}: {CHAPTER_GONE}"));
        }
        // A site that does not know the `replace` target yet refuses it by that name.
        if replace && reply.status == 400 && reply.error_code().as_deref() == Some("bad_target") {
            return Err(format!("{what}: {UPDATE_NEEDS_UPDATE}"));
        }
        // No room in the workspace: said with the site's numbers, never with its text. Only the
        // create is refused this way; an answer without all three numbers (another site) keeps
        // the site's own sentence, like any other refusal.
        if reply.status == 413
            && reply.error_code().as_deref() == Some("storage_full")
            && let Some((needed, left, allowance)) = Self::room(&reply)
        {
            return Err(full_workspace_error(needed, left, allowance));
        }
        let reply = self.check(what, reply)?;
        let answer = reply.json().ok_or_else(|| not_understood(what))?;
        let id = answer.get("upload_id").and_then(Value::as_str).filter(|id| is_upload_id(id)).ok_or_else(|| not_understood(what))?.to_string();

        // From here every exit but a successful commit, a panic included, sends exactly one abort.
        let guard = AbortGuard::new(self, format!("/api/v1/uploads/{id}"));
        let parts = if replace { Parts::of_replace(&answer, &bundle).ok_or_else(|| not_understood(what))? } else { Parts { have: Vec::new(), logo: bundle.logo.is_some() } };
        let total = (bundle.steps.len() - parts.have.len() + usize::from(parts.logo) + 2) as f32;
        let mut done = 1.0;
        let published = self.send_parts(&id, dir, &bundle, &parts, target, &mut || {
            let go = progress(done / total);
            done += 1.0;
            go
        })?;
        guard.disarm();
        progress(1.0);
        Ok(published)
    }

    /// Every image the site does not have yet, the logo when it is wanted, then commit.
    /// `next` reports progress and says whether to go on.
    fn send_parts(&self, id: &str, dir: &Path, bundle: &Bundle, parts: &Parts, target: &Target, next: &mut dyn FnMut() -> bool) -> Result<Published, String> {
        for (i, step) in bundle.steps.iter().enumerate() {
            let n = i + 1;
            if parts.have.contains(&n) {
                continue;
            }
            if !next() {
                return Err(CANCELLED.into());
            }
            let what = format!("Could not send step {n}");
            let data = read_bundle_file(dir, step).map_err(|e| format!("{what}: {e}"))?;
            // The site matched its pictures against the hash taken at the check, so what goes out must be those bytes.
            if sha256_hex(&data) != step.sha256 {
                return Err(format!("{what}: {} changed since it was checked.", step.file));
            }
            self.send_image(&what, &format!("/api/v1/uploads/{id}/steps/{n}"), step.content_type, &data)?;
        }
        if let Some(logo) = bundle.logo.as_ref().filter(|_| parts.logo) {
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
        self.commit(id, target)
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
    /// The answer names the tour and the chapter; for a replace they must be the ones replaced,
    /// and it must say what to do about publishing.
    fn commit(&self, id: &str, target: &Target) -> Result<Published, String> {
        let what = "Could not finish the upload";
        let path = format!("/api/v1/uploads/{id}/commit");
        let reply = match self.send(Method::Post, &path, None) {
            Ok(r) => r,
            Err(_) => self.send(Method::Post, &path, None).map_err(|t| format!("{what}: {}. Check the tour list before uploading again.", t.0))?,
        };
        if matches!(target, Target::Replace { .. }) && reply.status == 409 && reply.error_code().as_deref() == Some("chapter_changed") {
            return Err(format!("{what}: the chapter was changed by another upload meanwhile. Update again."));
        }
        let reply = self.check(what, reply)?;
        let v = reply.json().ok_or_else(|| not_understood(what))?;
        // Both ids are checked for their form: they are kept, and name folders of a saved recording.
        let id_of = |key: &str, prefix: &str| v.get(key).and_then(Value::as_str).filter(|id| is_id(id, prefix)).map(str::to_string).ok_or_else(|| not_understood(what));
        let (tour_id, chapter_id) = (id_of("tour_id", "tour_")?, id_of("chapter_id", "ch_")?);
        let steps = v.get("steps").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).ok_or_else(|| not_understood(what))?;
        let publish = match target {
            Target::Replace { tour_id: tour, chapter_id: chapter } => {
                if *tour != tour_id || *chapter != chapter_id {
                    return Err(not_understood(what));
                }
                Some(match v.get("publish").and_then(Value::as_str) {
                    Some("draft") => PublishAdvice::Draft,
                    Some("ready") => PublishAdvice::Ready,
                    Some("other_changes") => PublishAdvice::OtherChanges,
                    _ => return Err(not_understood(what)),
                })
            }
            Target::New | Target::Chapter(_) => None,
        };
        Ok(Published { tour_id, chapter_id, steps, publish })
    }
}

impl Client {
    #[allow(clippy::too_many_arguments)]
    fn upload_and_publish(
        &self,
        dir: &Path,
        target: &Target,
        publish_now: bool,
        pause: Duration,
        upload_progress: impl FnMut(f32) -> bool,
        uploaded: impl FnOnce(&Published),
        publish_progress: impl FnMut(Option<Images>) -> bool,
    ) -> Result<Finished, String> {
        let up = self.upload_bundle(dir, target, upload_progress)?;
        uploaded(&up);
        if !publish_now {
            return Ok(Finished { tour_id: up.tour_id, chapter_id: up.chapter_id, steps: up.steps, outcome: Outcome::Draft });
        }
        let result = self.publish_tour(&up.tour_id, pause, publish_progress);
        Ok(after_publish(&up.tour_id, &up.chapter_id, up.steps, result))
    }

    /// Replace the chapter, hand the commit's answer to `uploaded`, then publish only when the
    /// commit said that is safe.
    #[allow(clippy::too_many_arguments)]
    fn update_tour(
        &self,
        dir: &Path,
        tour_id: &str,
        chapter_id: &str,
        pause: Duration,
        upload_progress: impl FnMut(f32) -> bool,
        uploaded: impl FnOnce(&Published),
        publish_progress: impl FnMut(Option<Images>) -> bool,
    ) -> Result<Finished, String> {
        let target = Target::Replace { tour_id: tour_id.to_string(), chapter_id: chapter_id.to_string() };
        let up = self.upload_bundle(dir, &target, upload_progress)?;
        // The site holds the update from here on. Whatever the caller's hook does, a panic
        // included, the publish decision below is made and the real outcome is reported.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| uploaded(&up)));
        let outcome = match up.publish {
            Some(PublishAdvice::Ready) => {
                let result = self.publish_tour(&up.tour_id, pause, publish_progress);
                return Ok(after_publish(&up.tour_id, &up.chapter_id, up.steps, result));
            }
            Some(PublishAdvice::OtherChanges) => Outcome::Held,
            Some(PublishAdvice::Draft) | None => Outcome::Draft,
        };
        Ok(Finished { tour_id: up.tour_id, chapter_id: up.chapter_id, steps: up.steps, outcome })
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
            if Self::says_no_such_tour(&reply) {
                return Err(format!("{what}: {GONE}"));
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

impl Client {
    /// Whether a 404 says the tour is not there, as opposed to the route not being there.
    fn says_no_such_tour(reply: &Reply) -> bool {
        Self::says_not_found(reply, NO_SUCH_TOUR)
    }

    /// Whether the answer is the site's own 404 with exactly this detail.
    fn says_not_found(reply: &Reply, detail: &str) -> bool {
        reply.status == 404 && reply.json().is_some_and(|v| v.get("error").and_then(Value::as_str) == Some("not_found") && v.get("detail").and_then(Value::as_str) == Some(detail))
    }

    /// Whether the answer means the site has no such route: any other 404, or a 405.
    fn says_no_such_route(reply: &Reply) -> bool {
        reply.status == 405 || (reply.status == 404 && !Self::says_no_such_tour(reply))
    }

    fn rename_tour(&self, tour_id: &str, title: &str) -> Result<String, String> {
        let what = "Could not rename the tour";
        if !is_id(tour_id, "tour_") {
            return Err(not_understood(what));
        }
        let body = json!({ "title": title }).to_string().into_bytes();
        let reply = self.send(Method::Patch, &format!("/api/v1/tours/{tour_id}"), Some(("application/json", &body))).map_err(|t| format!("{what}: {}.", t.0))?;
        if Self::says_no_such_tour(&reply) {
            return Err(format!("{what}: {GONE}"));
        }
        if Self::says_no_such_route(&reply) {
            return Err(format!("{what}: {NEEDS_UPDATE}"));
        }
        let reply = self.check(what, reply)?;
        let v = reply.json().ok_or_else(|| not_understood(what))?;
        // The answer must be about this tour.
        if reply.status != 200 || v.get("id").and_then(Value::as_str) != Some(tour_id) {
            return Err(not_understood(what));
        }
        let saved = v.get("title").and_then(Value::as_str).ok_or_else(|| not_understood(what))?;
        Ok(saved.to_string())
    }

    /// The delete loop: the same call, `pause` apart, until the server answers 200, or 404
    /// for a tour that is gone. Unlike publish, a call may be sent again: the server
    /// carries on where it stopped, so a 500 `cleanup_pending` is simply called again.
    /// Gives up after [`MAX_STALLED_CALLS`] calls in a row that removed nothing more.
    fn delete_tour(&self, tour_id: &str, pause: Duration, mut progress: impl FnMut(Option<Images>)) -> Result<Deleted, DeleteFailed> {
        let what = "Could not delete the tour";
        let failed = |message: String, unfinished: bool| {
            let message = if unfinished { format!("{message}{UNFINISHED_TRAILER}") } else { message };
            DeleteFailed { message, unfinished, not_started: false }
        };
        // A clear refusal: unless an earlier answer said the delete is under way, nothing was begun.
        let refused = |message: String, unfinished: bool| DeleteFailed { not_started: !unfinished, ..failed(message, unfinished) };
        if !is_id(tour_id, "tour_") {
            return Err(refused(not_understood(what), false));
        }
        let path = format!("/api/v1/tours/{tour_id}");
        // Once the server has answered that the delete is under way, the tour itself is gone.
        let mut unfinished = false;
        let mut files = None;
        let (mut last_done, mut stalled) = (-1i128, 0u32);
        for call in 1u32.. {
            progress(files);
            let reply = match self.send(Method::Delete, &path, None) {
                Ok(r) => r,
                Err(t) => return Err(failed(format!("{what}: {}.", t.0), unfinished)),
            };
            // Gone: deleted by this loop's earlier call, or elsewhere. Either way it is not there.
            // Only after an earlier answer about this tour is it known to have been the token's.
            if Self::says_no_such_tour(&reply) {
                return Ok(if unfinished { Deleted::BySite } else { Deleted::NotThere });
            }
            // Never with the "files remain" ending: half-way through, a 404 that is not the
            // site's own (a proxy's page) says nothing about the tour either way.
            if Self::says_no_such_route(&reply) {
                return Err(DeleteFailed { not_started: !unfinished, ..failed(format!("{what}: {NEEDS_UPDATE}"), false) });
            }
            if reply.status == 409 && reply.error_code().as_deref() == Some("publish_busy") {
                return Err(refused(format!("{what}: a publish of this tour is in progress. Wait for it to finish, or unpublish the tour, then delete it again."), unfinished));
            }
            if reply.status == 500 && reply.error_code().as_deref() == Some("cleanup_pending") {
                // The tour is deleted; some files could not be removed yet. Calling again retries.
                unfinished = true;
                stalled += 1;
                if stalled >= MAX_STALLED_CALLS {
                    return Err(failed(format!("{what} completely: the site could not remove some of its files yet."), true));
                }
            } else {
                // A rejected token is a refusal too; any other answer that is not a yes says nothing either way.
                let rejected = reply.status == 401;
                let reply = self.check(what, reply).map_err(|m| if rejected { refused(m, unfinished) } else { failed(m, unfinished) })?;
                let v = reply.json().ok_or_else(|| failed(not_understood(what), unfinished))?;
                match reply.status {
                    200 if v.get("deleted").and_then(Value::as_bool) == Some(true) => return Ok(Deleted::BySite),
                    202 if v.get("status").and_then(Value::as_str) == Some("in_progress") => {}
                    _ => return Err(failed(not_understood(what), unfinished)),
                }
                unfinished = true;
                let done = v.get("done").and_then(Value::as_u64);
                files = done.zip(v.get("total").and_then(Value::as_u64)).filter(|(d, t)| *t > 0 && d <= t);
                let moved = done.is_some_and(|d| i128::from(d) > last_done);
                stalled = if moved { 0 } else { stalled + 1 };
                last_done = done.map_or(last_done, |d| last_done.max(i128::from(d)));
                if stalled >= MAX_STALLED_CALLS {
                    let message = match files {
                        Some((d, t)) => format!("{what} completely: the site stopped making progress at {d} of {t} files."),
                        None => not_understood(what),
                    };
                    return Err(failed(message, true));
                }
            }
            if call >= MAX_PUBLISH_CALLS {
                return Err(failed(format!("{what} completely: the site did not finish within {MAX_PUBLISH_CALLS} calls."), true));
            }
            std::thread::sleep(pause);
        }
        Err(failed(not_understood(what), unfinished))
    }

    /// The unpublish loop: the first call takes the tour off the web; while the server
    /// answers 202 (public images still to remove) or 500 `cleanup_pending`, it is called
    /// again, `pause` apart. The server gives no counts, so the stop is a number of calls.
    fn unpublish_tour(&self, tour_id: &str, pause: Duration) -> Result<(), String> {
        let what = "Could not unpublish the tour";
        if !is_id(tour_id, "tour_") {
            return Err(not_understood(what));
        }
        let path = format!("/api/v1/tours/{tour_id}/unpublish");
        // After a 202 or a `cleanup_pending` the tour is already off the web.
        let mut off = false;
        let mut pending = 0u32;
        for call in 1u32.. {
            let reply = self.send(Method::Post, &path, None).map_err(|t| if off { UNPUBLISHED_NOT_CLEARED.to_string() } else { format!("{what}: {}.", t.0) })?;
            if Self::says_no_such_tour(&reply) {
                return Err(format!("{what}: {GONE}"));
            }
            if reply.status == 500 && reply.error_code().as_deref() == Some("cleanup_pending") {
                off = true;
                pending += 1;
                if pending >= MAX_STALLED_CALLS {
                    return Err(UNPUBLISHED_NOT_CLEARED.to_string());
                }
            } else {
                let reply = self.check(what, reply).map_err(|m| if off { UNPUBLISHED_NOT_CLEARED.to_string() } else { m })?;
                match reply.status {
                    204 => return Ok(()),
                    202 if reply.json().is_some_and(|v| v.get("status").and_then(Value::as_str) == Some("in_progress")) => {
                        off = true;
                        pending = 0;
                    }
                    _ => return Err(not_understood(what)),
                }
            }
            if call >= MAX_PUBLISH_CALLS {
                return Err(UNPUBLISHED_NOT_CLEARED.to_string());
            }
            std::thread::sleep(pause);
        }
        Err(not_understood(what))
    }
}

/// What unpublish says when the tour is off the web but the loop could not see the clean-up through.
pub const UNPUBLISHED_NOT_CLEARED: &str = "The tour is no longer public, but the site has not removed all of its public images yet. It removes the rest later.";

/// A version as a site reports it: short, digits, letters, dots and hyphens, starting with a digit.
fn is_version(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_VERSION_CHARS && s.as_bytes()[0].is_ascii_digit() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
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

/// Sends one DELETE to `path` (an upload's, or a shared item's) when dropped, unless disarmed
/// after a successful commit.
struct AbortGuard<'a> {
    client: &'a Client,
    path: String,
    armed: bool,
}

impl<'a> AbortGuard<'a> {
    fn new(client: &'a Client, path: String) -> AbortGuard<'a> {
        AbortGuard { client, path, armed: true }
    }

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
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = self.client.send(Method::Delete, &self.path, None);
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
    /// The size the file had when the bundle was checked, which it must still have when it is
    /// read: for a step the size `tour.json` gives, for the logo the size the create names.
    bytes: u64,
    /// The SHA-256 of a step picture as it was when the bundle was checked, in lowercase hex. Empty for the logo.
    sha256: String,
}

/// Which parts of a bundle an upload sends.
struct Parts {
    /// The step numbers (from 1) the site already has: not sent.
    have: Vec<usize>,
    /// Whether the logo is sent (only ever when the bundle has one).
    logo: bool,
}

impl Parts {
    /// What the site's answer to a replace asks for. `have` must be a list of step numbers of
    /// this bundle (it may be left out: then every step is sent), and `settings` yes or no.
    fn of_replace(answer: &Value, bundle: &Bundle) -> Option<Parts> {
        let mut have = Vec::new();
        match answer.get("have") {
            None => {}
            Some(list) => {
                for n in list.as_array()? {
                    let n = n.as_u64().and_then(|n| usize::try_from(n).ok()).filter(|n| (1..=bundle.steps.len()).contains(n))?;
                    if !have.contains(&n) {
                        have.push(n);
                    }
                }
            }
        }
        let settings = match answer.get("settings") {
            None => false,
            Some(v) => v.as_bool()?,
        };
        Some(Parts { have, logo: settings && bundle.logo.is_some() })
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex(sha2::Sha256::digest(bytes))
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

struct Bundle {
    /// `tour.json` as it is, so any manifest version goes through unchanged.
    manifest: Value,
    steps: Vec<BundleFile>,
    logo: Option<BundleFile>,
}

impl Bundle {
    /// Reads `tour.json` and checks every file it names (the logo only when `with_logo`),
    /// so a bad bundle fails before anything is created. Each step picture is read once
    /// here, in pieces, for its SHA-256, and once more when it is sent. The logo's size is
    /// taken here and must still be that when it is sent: the create names it.
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
                let mut file = bundle_file(dir, name, bytes, true).map_err(|e| format!("Step {}: {e}", i + 1))?;
                file.sha256 = hash_bundle_file(dir, &file).map_err(|e| format!("Step {}: {e}", i + 1))?;
                Ok::<_, String>(file)
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
/// (not a link or a folder), PNG or JPEG by name, and now of exactly `bytes` bytes (`exact`) or
/// of at most that many. What comes back holds the size the file really has, not `bytes`: it
/// must still have that size when it is hashed and when it is sent.
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
    Ok(BundleFile { file: name.to_string(), content_type, bytes: meta.len(), sha256: String::new() })
}

fn check_regular(name: &str, meta: &std::fs::Metadata) -> Result<(), String> {
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(format!("{name} is not a regular file."));
    }
    Ok(())
}

/// Opens a checked bundle file once, without following a link on Windows, and reads at
/// most its size plus one byte, refusing it if it is no longer the size that was checked.
fn read_bundle_file(dir: &Path, f: &BundleFile) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = open_bundle_file(dir, f)?;
    let mut data = Vec::new();
    file.take(f.bytes + 1).read_to_end(&mut data).map_err(|_| format!("could not read {}.", f.file))?;
    let len = data.len() as u64;
    if len != f.bytes {
        return Err(format!("{} changed since it was checked.", f.file));
    }
    Ok(data)
}

/// Opens a checked bundle file, without following a link on Windows, and checks again that it is a regular file.
fn open_bundle_file(dir: &Path, f: &BundleFile) -> Result<std::fs::File, String> {
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
    Ok(file)
}

/// The SHA-256 of a checked bundle file in lowercase hex, read in pieces and never past its size.
fn hash_bundle_file(dir: &Path, f: &BundleFile) -> Result<String, String> {
    use sha2::Digest;
    use std::io::Read;
    let mut file = open_bundle_file(dir, f)?.take(f.bytes + 1);
    let mut hasher = sha2::Sha256::new();
    let mut piece = [0u8; 64 * 1024];
    let mut len = 0u64;
    loop {
        let n = file.read(&mut piece).map_err(|_| format!("could not read {}.", f.file))?;
        if n == 0 {
            break;
        }
        hasher.update(&piece[..n]);
        len += n as u64;
    }
    if len != f.bytes {
        return Err(format!("{} changed since it was checked.", f.file));
    }
    Ok(hex(hasher.finalize()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    pub(crate) const TOKEN: &str = "dp_0123456789abcdef0123456789abcdef01234567";
    const UP: &str = "up_0123456789abcdef01234567";
    pub(crate) const CH: &str = "ch_0123456789abcdef01234567";

    #[derive(Debug, Clone)]
    pub(crate) struct Req {
        pub(crate) line: String,
        pub(crate) auth: Option<String>,
        pub(crate) content_type: Option<String>,
        pub(crate) body: Vec<u8>,
    }

    /// What the stub does with a request: answer, or drop the connection without a word.
    pub(crate) enum Answer {
        Reply(u16, String, Vec<(&'static str, String)>),
        Drop,
        Hang,
    }

    pub(crate) fn reply(status: u16, body: &str) -> Answer {
        Answer::Reply(status, body.to_string(), vec![])
    }

    pub(crate) struct Stub {
        pub(crate) base: String,
        log: Arc<Mutex<Vec<Req>>>,
    }

    impl Stub {
        pub(crate) fn lines(&self) -> Vec<String> {
            self.log.lock().unwrap().iter().map(|r| r.line.clone()).collect()
        }
        pub(crate) fn requests(&self) -> Vec<Req> {
            self.log.lock().unwrap().clone()
        }
        /// The live list of requests, for a test that must see them from another closure.
        pub(crate) fn requests_handle(&self) -> Arc<Mutex<Vec<Req>>> {
            self.log.clone()
        }
    }

    /// A one-thread HTTP server on 127.0.0.1 that records each request and answers with `handler`.
    /// `handler` gets the request and how many earlier requests had the same line.
    pub(crate) fn stub(handler: impl Fn(&Req, usize) -> Answer + Send + 'static) -> Stub {
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
    pub(crate) fn platform(req: &Req, seen: usize) -> Answer {
        let l = req.line.as_str();
        if l == "POST /api/v1/uploads" {
            reply(201, &format!(r#"{{"upload_id":"{UP}","steps":2}}"#))
        } else if l.starts_with("PUT ") {
            if seen > 0 { reply(409, r#"{"error":"already_sent","detail":"step was already sent"}"#) } else { reply(204, "") }
        } else if l.ends_with("/commit") {
            reply(201, &format!(r#"{{"tour_id":"tour_aaaaaaaaaaaaaaaaaaaaaaaa","chapter_id":"{CH}","steps":2}}"#))
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

    pub(crate) const TOUR: &str = "tour_aaaaaaaaaaaaaaaaaaaaaaaa";
    const PUB: &str = "pub_0123456789abcdef01234567";
    pub(crate) const PUBLISH: &str = "POST /api/v1/tours/tour_aaaaaaaaaaaaaaaaaaaaaaaa/publish";

    fn client(stub: &Stub) -> Client {
        Client::new(&stub.base, TOKEN, Timeouts::default()).unwrap()
    }

    fn in_progress(done: u32, total: u32) -> Answer {
        reply(202, &format!(r#"{{"status":"in_progress","publish_id":"{PUB}","done":{done},"total":{total}}}"#))
    }

    pub(crate) const LIVE: &str = r#"{"slug":"abc123","url":"/t/abc123","steps":5}"#;

    /// A stub whose publish route answers as `publish` says (given how many publish calls came
    /// before) and that holds the client to the protocol: a call after a 202 must name that
    /// publish, any other must have no body; a call that does not is answered 400. Every other
    /// route answers as the platform does.
    pub(crate) fn publish_stub(publish: impl Fn(usize) -> Answer + Send + 'static) -> Stub {
        publish_stub_over(platform, publish)
    }

    /// [`publish_stub`], with `rest` answering every route but publish.
    pub(crate) fn publish_stub_over(rest: impl Fn(&Req, usize) -> Answer + Send + 'static, publish: impl Fn(usize) -> Answer + Send + 'static) -> Stub {
        let named = std::sync::atomic::AtomicBool::new(false);
        stub(move |req, seen| {
            if req.line != PUBLISH {
                return rest(req, seen);
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
        client(stub).upload_and_publish(dir.path(), &Target::New, publish_now, Duration::ZERO, |_| true, |_| {}, go)
    }

    #[test]
    fn the_upload_is_handed_over_once_after_the_commit_and_before_the_publish_step() {
        for publish_now in [true, false] {
            let s = platform_with(|_| reply(200, LIVE));
            let dir = bundle(2, false);
            let seen: RefCell<Vec<(Published, Vec<String>)>> = RefCell::new(Vec::new());
            client(&s).upload_and_publish(dir.path(), &Target::New, publish_now, Duration::ZERO, |_| true, |up| seen.borrow_mut().push((up.clone(), s.lines())), |_| true).unwrap();
            let seen = seen.into_inner();
            assert_eq!(seen.len(), 1);
            let (up, lines) = &seen[0];
            assert_eq!(up, &Published { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 2, publish: None });
            assert!(lines.last().is_some_and(|l| l.ends_with("/commit")), "the commit had answered: {lines:?}");
            assert!(!lines.iter().any(|l| l == PUBLISH), "and the publish step had not begun");
        }
        // An upload that fails or is cancelled hands nothing over.
        let s = stub(|req, seen| if req.line.ends_with("/commit") { reply(500, r#"{"error":"commit_failed","detail":"no"}"#) } else { platform(req, seen) });
        let dir = bundle(2, false);
        let mut called = 0;
        assert!(client(&s).upload_and_publish(dir.path(), &Target::New, true, Duration::ZERO, |_| true, |_| called += 1, |_| true).is_err());
        let s = platform_with(|_| reply(200, LIVE));
        assert_eq!(client(&s).upload_and_publish(dir.path(), &Target::New, true, Duration::ZERO, |_| false, |_| called += 1, |_| true), Err(CANCELLED.to_string()));
        assert_eq!(called, 0);
    }

    #[test]
    fn publish_now_uploads_then_publishes_and_a_draft_only_uploads() {
        let s = platform_with(|seen| if seen == 0 { in_progress(2, 2) } else { reply(200, LIVE) });
        let done = upload_and_publish_to(&s, true, |_| true).unwrap();
        assert_eq!(done, Finished { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 5, outcome: Outcome::Public { url: Some(format!("{}/t/abc123", s.base)) } });
        let lines = s.lines();
        let commit = lines.iter().position(|l| l.ends_with("/commit")).unwrap();
        assert_eq!(&lines[commit + 1..], [PUBLISH, PUBLISH], "publishing starts after the commit");

        let s = platform_with(|_| reply(200, LIVE));
        let done = upload_and_publish_to(&s, false, |_| true).unwrap();
        assert_eq!(done, Finished { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 2, outcome: Outcome::Draft });
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
        let again = after_publish(&done.tour_id, &done.chapter_id, done.steps, client(&s).publish_tour(&done.tour_id, Duration::ZERO, |_| true));
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
        assert_eq!(done, Finished { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 2, outcome: Outcome::NotPublic { cancelled: true, error: String::new() } });
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
        assert_eq!(after_publish(TOUR, CH, 2, Ok(live)), Finished { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 7, outcome: Outcome::Public { url: Some("https://x.dev/t/a".into()) } });
        assert_eq!(after_publish(TOUR, CH, 2, Err(CANCELLED.into())).outcome, Outcome::NotPublic { cancelled: true, error: String::new() });
        assert_eq!(after_publish(TOUR, CH, 2, Err("Could not publish the tour: x.".into())), Finished { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 2, outcome: Outcome::NotPublic { cancelled: false, error: "Could not publish the tour: x.".into() } });
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
        assert_eq!(out, Published { tour_id: "tour_aaaaaaaaaaaaaaaaaaaaaaaa".into(), chapter_id: CH.into(), steps: 2, publish: None });
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

    // ---- Update: replace a chapter ----

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::Digest;
        sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The bytes of step `n` in [`distinct_bundle`]: every step its own.
    fn picture(n: usize) -> Vec<u8> {
        [PNG, format!("-{n}").as_bytes()].concat()
    }

    /// A bundle like [`bundle`], but every step has its own bytes.
    fn distinct_bundle(steps: usize, logo: bool) -> tempfile::TempDir {
        let dir = bundle(steps, logo);
        let mut manifest: Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join("tour.json")).unwrap()).unwrap();
        for n in 1..=steps {
            std::fs::write(dir.path().join(format!("step-{n:03}.png")), picture(n)).unwrap();
            manifest["steps"][n - 1]["bytes"] = json!(picture(n).len());
        }
        std::fs::write(dir.path().join("tour.json"), manifest.to_string()).unwrap();
        dir
    }

    fn replace_target() -> Target {
        Target::Replace { tour_id: TOUR.into(), chapter_id: CH.into() }
    }

    pub(crate) fn created(have: &str, settings: bool) -> String {
        format!(r#"{{"upload_id":"{UP}","steps":3,"have":{have},"settings":{settings}}}"#)
    }

    pub(crate) fn committed(advice: &str) -> String {
        format!(r#"{{"tour_id":"{TOUR}","chapter_id":"{CH}","steps":3,"publish":"{advice}"}}"#)
    }

    /// The platform for a replace: create answers `have` and `settings`, commit gives `advice`,
    /// and publish answers as `publish` says. Every other route is the platform's.
    pub(crate) fn replace_stub(have: &'static str, settings: bool, advice: &'static str, publish: impl Fn(usize) -> Answer + Send + 'static) -> Stub {
        publish_stub_over(
            move |req, seen| {
                if req.line == "POST /api/v1/uploads" {
                    reply(201, &created(have, settings))
                } else if req.line.ends_with("/commit") {
                    reply(201, &committed(advice))
                } else {
                    platform(req, seen)
                }
            },
            publish,
        )
    }

    fn update(stub: &Stub, dir: &Path) -> Result<Finished, String> {
        client(stub).update_tour(dir, TOUR, CH, Duration::ZERO, |_| true, |_| {}, |_| true)
    }

    fn count(lines: &[String], test: impl Fn(&str) -> bool) -> usize {
        lines.iter().filter(|l| test(l)).count()
    }

    const CREATE: &str = "POST /api/v1/uploads";

    #[test]
    fn an_update_sends_one_hash_per_step_and_only_the_pictures_the_site_lacks() {
        let s = replace_stub("[1,3]", false, "draft", |_| reply(200, LIVE));
        let dir = distinct_bundle(3, false);
        update(&s, dir.path()).unwrap();
        assert_eq!(s.lines(), [CREATE.to_string(), format!("PUT /api/v1/uploads/{UP}/steps/2"), format!("POST /api/v1/uploads/{UP}/commit")]);
        let reqs = s.requests();
        let create: Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(create["target"], json!({ "kind": "replace", "tour_id": TOUR, "chapter_id": CH }));
        assert_eq!(create["sha256"], json!([sha256_hex(&picture(1)), sha256_hex(&picture(2)), sha256_hex(&picture(3))]));
        assert_eq!(create["manifest"]["steps"].as_array().unwrap().len(), 3);
        assert_eq!(reqs[1].body, picture(2));
        assert!(reqs.iter().all(|r| r.auth.as_deref() == Some(&format!("Bearer {TOKEN}")[..])));

        // Two steps with the same bytes get the same hash, one entry each.
        let s = replace_stub("[]", false, "draft", |_| reply(200, LIVE));
        let dir = bundle(2, false);
        update(&s, dir.path()).unwrap();
        let create: Value = serde_json::from_slice(&s.requests()[0].body).unwrap();
        assert_eq!(create["sha256"], json!([sha256_hex(PNG), sha256_hex(PNG)]));
        assert_eq!(count(&s.lines(), |l| l.starts_with("PUT ")), 2);

        // A new tour and a new chapter send no hashes: the site refuses them there.
        for target in [Target::New, Target::Chapter(TOUR.into())] {
            let s = stub(platform);
            upload(&s, 2, false, &target).unwrap();
            let create: Value = serde_json::from_slice(&s.requests()[0].body).unwrap();
            assert!(create.get("sha256").is_none(), "{target:?}");
        }
    }

    #[test]
    fn an_update_with_nothing_changed_sends_no_picture_and_still_commits() {
        let s = replace_stub("[1,2,3]", false, "draft", |_| reply(200, LIVE));
        let dir = distinct_bundle(3, false);
        let mut seen = Vec::new();
        let done = client(&s)
            .update_tour(
                dir.path(),
                TOUR,
                CH,
                Duration::ZERO,
                |p| {
                    seen.push(p);
                    true
                },
                |_| {},
                |_| true,
            )
            .unwrap();
        assert_eq!(done, Finished { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 3, outcome: Outcome::Draft });
        assert_eq!(s.lines(), [CREATE.to_string(), format!("POST /api/v1/uploads/{UP}/commit")]);
        // Progress counts only what is really sent, and still ends at one.
        assert_eq!(seen.first(), Some(&0.0));
        assert_eq!(seen.last(), Some(&1.0));
        assert!(seen.windows(2).all(|w| w[0] < w[1]), "{seen:?}");
    }

    #[test]
    fn the_logo_goes_only_when_the_site_asks_for_the_settings() {
        // The first chapter: the site takes the tour's settings, so the logo is sent, once, before the commit.
        let s = replace_stub("[1]", true, "draft", |_| reply(200, LIVE));
        let dir = distinct_bundle(2, true);
        update(&s, dir.path()).unwrap();
        assert_eq!(s.lines(), [CREATE.to_string(), format!("PUT /api/v1/uploads/{UP}/steps/2"), format!("PUT /api/v1/uploads/{UP}/logo"), format!("POST /api/v1/uploads/{UP}/commit")]);

        // Any other chapter: the tour keeps its logo, and the site would refuse one.
        let s = replace_stub("[1]", false, "draft", |_| reply(200, LIVE));
        update(&s, dir.path()).unwrap();
        assert_eq!(count(&s.lines(), |l| l.ends_with("/logo")), 0);

        // Every picture is there already, the logo still goes.
        let s = replace_stub("[1,2]", true, "draft", |_| reply(200, LIVE));
        update(&s, dir.path()).unwrap();
        assert_eq!(s.lines(), [CREATE.to_string(), format!("PUT /api/v1/uploads/{UP}/logo"), format!("POST /api/v1/uploads/{UP}/commit")]);

        // The site asks for the settings, but the tour has no logo: nothing to send.
        let s = replace_stub("[]", true, "draft", |_| reply(200, LIVE));
        let plain = distinct_bundle(2, false);
        update(&s, plain.path()).unwrap();
        assert_eq!(count(&s.lines(), |l| l.ends_with("/logo")), 0);

        // An answer whose `settings` is not yes or no is not guessed at.
        let s = stub(|req, seen| if req.line == CREATE { reply(201, &format!(r#"{{"upload_id":"{UP}","steps":2,"have":[],"settings":"yes"}}"#)) } else { platform(req, seen) });
        assert_eq!(update(&s, dir.path()).unwrap_err(), "Could not start the upload: the server's answer was not understood.");
        assert_eq!(s.lines(), [CREATE.to_string(), format!("DELETE /api/v1/uploads/{UP}")]);
    }

    #[test]
    fn a_public_tour_is_published_after_the_update_and_a_draft_is_not() {
        let s = replace_stub("[]", false, "ready", |seen| if seen == 0 { in_progress(2, 2) } else { reply(200, LIVE) });
        let dir = distinct_bundle(3, false);
        let done = update(&s, dir.path()).unwrap();
        assert_eq!(done, Finished { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 5, outcome: Outcome::Public { url: Some(format!("{}/t/abc123", s.base)) } });
        let lines = s.lines();
        let commit = lines.iter().position(|l| l.ends_with("/commit")).unwrap();
        assert_eq!(&lines[commit + 1..], [PUBLISH, PUBLISH], "publishing starts after the commit");

        let s = replace_stub("[]", false, "draft", |_| reply(200, LIVE));
        let done = update(&s, dir.path()).unwrap();
        assert_eq!(done, Finished { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 3, outcome: Outcome::Draft });
        assert_eq!(count(&s.lines(), |l| l == PUBLISH), 0, "a draft stays a draft");
    }

    #[test]
    fn a_tour_with_other_unpublished_changes_is_uploaded_and_not_published() {
        let s = replace_stub("[2]", false, "other_changes", |_| reply(200, LIVE));
        let dir = distinct_bundle(3, false);
        let done = update(&s, dir.path()).unwrap();
        assert_eq!(done, Finished { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 3, outcome: Outcome::Held });
        let lines = s.lines();
        assert_eq!(count(&lines, |l| l == PUBLISH), 0, "{lines:?}");
        assert_eq!(count(&lines, |l| l.starts_with("DELETE ")), 0, "the upload is kept: {lines:?}");
        assert_eq!(count(&lines, |l| l.ends_with("/commit")), 1);
    }

    #[test]
    fn cancel_before_the_commit_aborts_once_and_never_publishes() {
        let s = replace_stub("[1]", false, "ready", |_| reply(200, LIVE));
        let dir = distinct_bundle(3, false);
        let mut calls = 0;
        let r = client(&s).update_tour(
            dir.path(),
            TOUR,
            CH,
            Duration::ZERO,
            |_| {
                calls += 1;
                calls <= 2 // 0.0, then before step 2; false before step 3
            },
            |_| {},
            |_| true,
        );
        assert_eq!(r.unwrap_err(), CANCELLED);
        assert_eq!(s.lines(), [CREATE.to_string(), format!("PUT /api/v1/uploads/{UP}/steps/2"), format!("DELETE /api/v1/uploads/{UP}")]);

        // Cancelled when nothing is left to send but the commit itself.
        let s = replace_stub("[1,2,3]", false, "ready", |_| reply(200, LIVE));
        let mut calls = 0;
        let r = client(&s).update_tour(
            dir.path(),
            TOUR,
            CH,
            Duration::ZERO,
            |_| {
                calls += 1;
                calls <= 1
            },
            |_| {},
            |_| true,
        );
        assert_eq!(r.unwrap_err(), CANCELLED);
        assert_eq!(s.lines(), [CREATE.to_string(), format!("DELETE /api/v1/uploads/{UP}")]);

        // Cancelled before anything was asked: nothing is sent at all.
        let s = replace_stub("[]", false, "ready", |_| reply(200, LIVE));
        assert_eq!(client(&s).update_tour(dir.path(), TOUR, CH, Duration::ZERO, |_| false, |_| {}, |_| true).unwrap_err(), CANCELLED);
        assert!(s.lines().is_empty());
    }

    #[test]
    fn a_lost_connection_mid_upload_aborts_once_and_says_which_step() {
        let s = publish_stub_over(
            |req, seen| {
                if req.line == CREATE {
                    reply(201, &created("[1]", false))
                } else if req.line.ends_with("/steps/3") {
                    Answer::Drop
                } else {
                    platform(req, seen)
                }
            },
            |_| reply(200, LIVE),
        );
        let dir = distinct_bundle(3, false);
        let e = update(&s, dir.path()).unwrap_err();
        assert!(e.starts_with("Could not send step 3: ") && e.ends_with('.') && !e.contains("dp_"), "{e}");
        let lines = s.lines();
        assert_eq!(count(&lines, |l| l.starts_with("DELETE ")), 1, "{lines:?}");
        assert_eq!(lines.last().unwrap(), &format!("DELETE /api/v1/uploads/{UP}"));
        assert_eq!(count(&lines, |l| l.ends_with("/commit") || l == PUBLISH), 0, "{lines:?}");
        // The step the site already has was never sent; the lost one was tried twice, as for any upload.
        assert_eq!(count(&lines, |l| l.ends_with("/steps/1")), 0);
        assert_eq!(count(&lines, |l| l.ends_with("/steps/3")), 2);
    }

    #[test]
    fn a_failed_publish_after_the_update_reports_the_upload_as_done() {
        let s = replace_stub("[]", false, "ready", |_| reply(500, r#"{"error":"publish_failed","detail":"the tour could not be published"}"#));
        let dir = distinct_bundle(3, false);
        let done = update(&s, dir.path()).unwrap();
        assert_eq!(
            done,
            Finished {
                tour_id: TOUR.into(),
                chapter_id: CH.into(),
                steps: 3,
                outcome: Outcome::NotPublic { cancelled: false, error: "Could not publish the tour: the tour could not be published (HTTP 500).".into() }
            }
        );
        let lines = s.lines();
        assert_eq!(count(&lines, |l| l.starts_with("DELETE ")), 0, "the committed update is not aborted: {lines:?}");
        assert_eq!(count(&lines, |l| l == PUBLISH), 1, "a failed publish is not sent again");

        // The same when the publish is cancelled.
        let s = replace_stub("[]", false, "ready", |_| in_progress(1, 2));
        let done = client(&s).update_tour(dir.path(), TOUR, CH, Duration::ZERO, |_| true, |_| {}, |images| images.is_none()).unwrap();
        assert_eq!(done.outcome, Outcome::NotPublic { cancelled: true, error: String::new() });
        assert_eq!(done.chapter_id, CH);
        assert_eq!(count(&s.lines(), |l| l.starts_with("DELETE ")), 0);
    }

    #[test]
    fn an_update_is_handed_over_once_after_the_commit_and_before_anything_is_published() {
        // Whatever the site says about publishing: published, a draft, or held back.
        for (advice, outcome_is) in [("ready", "public"), ("draft", "draft"), ("other_changes", "held")] {
            let s = replace_stub("[1]", false, advice, |_| reply(200, LIVE));
            let dir = distinct_bundle(3, false);
            let seen: RefCell<Vec<(Published, Vec<String>)>> = RefCell::new(Vec::new());
            let done = client(&s).update_tour(dir.path(), TOUR, CH, Duration::ZERO, |_| true, |up| seen.borrow_mut().push((up.clone(), s.lines())), |_| true).unwrap();
            let seen = seen.into_inner();
            assert_eq!(seen.len(), 1, "{advice}");
            let (up, lines) = &seen[0];
            assert_eq!((up.tour_id.as_str(), up.chapter_id.as_str(), up.steps), (TOUR, CH, 3), "{advice}");
            assert!(lines.last().is_some_and(|l| l.ends_with("/commit")), "the commit had answered: {lines:?}");
            assert!(!lines.iter().any(|l| l == PUBLISH), "and nothing was published yet");
            let is = match done.outcome {
                Outcome::Public { .. } => "public",
                Outcome::Draft => "draft",
                Outcome::Held => "held",
                Outcome::NotPublic { .. } => "not public",
            };
            assert_eq!(is, outcome_is);
        }
        // A publish that fails afterwards does not hand it over a second time.
        let s = replace_stub("[]", false, "ready", |_| reply(500, r#"{"error":"publish_failed","detail":"no"}"#));
        let dir = distinct_bundle(3, false);
        let mut called = 0;
        let done = client(&s).update_tour(dir.path(), TOUR, CH, Duration::ZERO, |_| true, |_| called += 1, |_| true).unwrap();
        assert!(matches!(done.outcome, Outcome::NotPublic { cancelled: false, .. }));
        assert_eq!(called, 1);
    }

    #[test]
    fn an_update_that_fails_or_is_cancelled_hands_nothing_over() {
        let dir = distinct_bundle(3, false);
        let mut called = 0;
        // Cancelled before the commit.
        let s = replace_stub("[1]", false, "ready", |_| reply(200, LIVE));
        assert_eq!(client(&s).update_tour(dir.path(), TOUR, CH, Duration::ZERO, |_| false, |_| called += 1, |_| true), Err(CANCELLED.to_string()));
        // Refused at create, and refused at the commit.
        let s = refusing_create(404, r#"{"error":"not_found","detail":"no such tour"}"#);
        assert!(client(&s).update_tour(dir.path(), TOUR, CH, Duration::ZERO, |_| true, |_| called += 1, |_| true).is_err());
        let s = stub(|req, seen| {
            if req.line == CREATE {
                reply(201, &created("[]", false))
            } else if req.line.ends_with("/commit") {
                reply(409, r#"{"error":"chapter_changed","detail":"the chapter was changed"}"#)
            } else {
                platform(req, seen)
            }
        });
        assert!(client(&s).update_tour(dir.path(), TOUR, CH, Duration::ZERO, |_| true, |_| called += 1, |_| true).is_err());
        assert_eq!(called, 0);
    }

    #[test]
    fn an_update_whose_commit_answer_was_lost_hands_nothing_over_and_says_to_check() {
        // Both tries of the commit get no answer: the site may or may not hold the update.
        let s = publish_stub_over(
            |req, seen| {
                if req.line == CREATE {
                    reply(201, &created("[1,2,3]", false))
                } else if req.line.ends_with("/commit") {
                    Answer::Drop
                } else {
                    platform(req, seen)
                }
            },
            |_| reply(200, LIVE),
        );
        let dir = distinct_bundle(3, false);
        let mut called = 0;
        let e = client(&s).update_tour(dir.path(), TOUR, CH, Duration::ZERO, |_| true, |_| called += 1, |_| true).unwrap_err();
        assert!(e.starts_with("Could not finish the upload: ") && e.ends_with(" Check the tour list before uploading again."), "{e}");
        assert_eq!(called, 0, "nothing is known about the site, so nothing is handed over");
        let lines = s.lines();
        assert_eq!(count(&lines, |l| l.ends_with("/commit")), 2);
        assert_eq!(count(&lines, |l| l == PUBLISH), 0);
    }

    #[test]
    fn a_hand_over_that_panics_does_not_change_what_the_update_reports() {
        for (advice, published) in [("ready", 1), ("draft", 0), ("other_changes", 0)] {
            let s = replace_stub("[]", false, advice, |_| reply(200, LIVE));
            let dir = distinct_bundle(3, false);
            let done = client(&s).update_tour(dir.path(), TOUR, CH, Duration::ZERO, |_| true, |_| panic!("the hand-over broke"), |_| true).unwrap();
            assert_eq!((done.tour_id.as_str(), done.chapter_id.as_str()), (TOUR, CH));
            match advice {
                "ready" => assert_eq!(done.outcome, Outcome::Public { url: Some(format!("{}/t/abc123", s.base)) }),
                "draft" => assert_eq!(done.outcome, Outcome::Draft),
                _ => assert_eq!(done.outcome, Outcome::Held),
            }
            let lines = s.lines();
            assert_eq!(count(&lines, |l| l == PUBLISH), published, "{advice}");
            assert_eq!(count(&lines, |l| l.starts_with("DELETE ")), 0, "the committed update is never aborted");
        }
    }

    #[test]
    fn an_update_error_says_whether_updating_again_can_work() {
        let dir = distinct_bundle(3, false);
        let gone = update(&refusing_create(404, r#"{"error":"not_found","detail":"no such tour"}"#), dir.path()).unwrap_err();
        assert!(is_gone(&gone) && !is_chapter_gone(&gone) && !needs_newer_site(&gone), "{gone}");
        let chapter = update(&refusing_create(404, r#"{"error":"not_found","detail":"no such chapter"}"#), dir.path()).unwrap_err();
        assert_eq!(chapter, "Could not start the upload: that chapter is no longer part of the tour.");
        assert!(is_chapter_gone(&chapter) && !is_gone(&chapter) && !needs_newer_site(&chapter));
        let old = update(&refusing_create(400, r#"{"error":"bad_target","detail":"unknown target"}"#), dir.path()).unwrap_err();
        assert_eq!(old, "Could not start the upload: the site needs an update to version 0.1.4 or newer.");
        assert!(needs_newer_site(&old) && !is_gone(&old) && !is_chapter_gone(&old));
        // Text from the site can never look like one of them: it is followed by the status.
        let said = update(&refusing_create(500, r#"{"error":"x","detail":"Could not start the upload: that chapter is no longer part of the tour."}"#), dir.path()).unwrap_err();
        assert!(!is_chapter_gone(&said) && !needs_newer_site(&said) && !is_gone(&said), "{said}");
    }

    /// A stub whose create answers `status` and `body`; nothing else should ever be asked.
    fn refusing_create(status: u16, body: &'static str) -> Stub {
        stub(move |req, seen| if req.line == CREATE { reply(status, body) } else { platform(req, seen) })
    }

    #[test]
    fn a_tour_that_is_gone_says_so() {
        let s = refusing_create(404, r#"{"error":"not_found","detail":"no such tour"}"#);
        let dir = distinct_bundle(2, false);
        let e = update(&s, dir.path()).unwrap_err();
        assert_eq!(e, "Could not start the upload: that tour no longer exists.");
        assert!(is_gone(&e));
        assert_eq!(s.lines(), [CREATE], "nothing was created, so nothing is aborted");
        // An id that is not a tour id never reaches the network, and reads the same.
        let e = client(&s).update_tour(dir.path(), "tour_x/../y", CH, Duration::ZERO, |_| true, |_| {}, |_| true).unwrap_err();
        assert!(is_gone(&e), "{e}");
        assert_eq!(s.lines().len(), 1);
    }

    #[test]
    fn a_chapter_that_is_gone_says_so() {
        let s = refusing_create(404, r#"{"error":"not_found","detail":"no such chapter"}"#);
        let dir = distinct_bundle(2, false);
        let e = update(&s, dir.path()).unwrap_err();
        assert_eq!(e, "Could not start the upload: that chapter is no longer part of the tour.");
        assert!(!is_gone(&e), "the tour is still there");
        assert_eq!(s.lines(), [CREATE]);
        let e = client(&s).update_tour(dir.path(), TOUR, "ch_../x", Duration::ZERO, |_| true, |_| {}, |_| true).unwrap_err();
        assert_eq!(e, "Could not start the upload: that chapter is no longer part of the tour.");
        assert_eq!(s.lines().len(), 1);
    }

    #[test]
    fn an_older_site_is_told_to_update_and_nothing_is_retried() {
        let s = refusing_create(400, r#"{"error":"bad_target","detail":"target.kind must be \"new\" or \"chapter\""}"#);
        let dir = distinct_bundle(2, false);
        let e = update(&s, dir.path()).unwrap_err();
        assert_eq!(e, "Could not start the upload: the site needs an update to version 0.1.4 or newer.");
        assert_eq!(s.lines(), [CREATE], "not sent again, and not sent as a new tour instead");
        // Another 400 keeps the site's own words.
        let s = refusing_create(400, r#"{"error":"bad_manifest","detail":"steps: too many"}"#);
        assert_eq!(update(&s, dir.path()).unwrap_err(), "Could not start the upload: steps: too many (HTTP 400).");
    }

    // ---- A workspace without room ----

    const MB: u64 = 1 << 20;
    const FULL_SENTENCE: &str = "The tour was not uploaded. This tour needs 310 MB, and your workspace has 120 MB left of its 5.0 GB. Delete tours you no longer need, or ask for more room, and publish again. Tours that are already online are not affected.";

    /// The site's 413 for a create that would go past the allowance, with `detail` as its sentence.
    fn storage_full(used: &str, allowance: &str, needed: &str, detail: &str) -> String {
        format!(r#"{{"error":"storage_full","detail":"{detail}","used_bytes":{used},"allowance_bytes":{allowance},"needed_bytes":{needed}}}"#)
    }

    fn full_create(body: String) -> Stub {
        stub(move |req, seen| if req.line == CREATE { reply(413, &body) } else { platform(req, seen) })
    }

    #[test]
    fn a_full_workspace_is_said_in_a_sentence_with_the_three_sizes_and_no_status_code() {
        let body = storage_full(&(5120 * MB - 120 * MB).to_string(), &(5120 * MB).to_string(), &(310 * MB).to_string(), "this tour needs 310 MB of room");
        // A new tour, a new chapter and an update all read the same.
        let dir = distinct_bundle(2, true);
        let mut errors = Vec::new();
        for target in [Target::New, Target::Chapter(TOUR.into())] {
            let s = full_create(body.clone());
            errors.push(upload_bundle(&s.base, TOKEN, dir.path(), &target, |_| true).unwrap_err());
            assert_eq!(s.lines(), [CREATE], "not sent again, nothing was uploaded and there is nothing to abort");
        }
        let s = full_create(body);
        errors.push(update(&s, dir.path()).unwrap_err());
        assert_eq!(s.lines(), [CREATE]);
        for e in errors {
            assert_eq!(e, FULL_SENTENCE);
            assert!(!e.contains("HTTP") && !e.contains("413") && !e.contains("Could not start"), "{e}");
            assert_eq!(full_workspace(&e), Some("This tour needs 310 MB, and your workspace has 120 MB left of its 5.0 GB. Delete tours you no longer need, or ask for more room, and publish again."));
            assert!(!is_gone(&e) && !is_chapter_gone(&e) && !needs_newer_site(&e));
        }
        assert_eq!(FULL_HEADING, "The tour was not uploaded");
        assert_eq!(FULL_NOTE, "Tours that are already online are not affected.");
    }

    #[test]
    fn the_sizes_come_from_the_numbers_never_from_the_sites_text() {
        let dir = distinct_bundle(2, false);
        let detail = format!("this tour needs 999 TB of room, says {TOKEN}");
        let s = full_create(storage_full(&(900 * MB).to_string(), &(1024 * MB).to_string(), &(14 * MB + MB / 5).to_string(), &detail));
        let e = update(&s, dir.path()).unwrap_err();
        assert_eq!(full_workspace(&e), Some("This tour needs 14.2 MB, and your workspace has 124 MB left of its 1.0 GB. Delete tours you no longer need, or ask for more room, and publish again."));
        assert!(!e.contains("999") && !e.contains("says") && !e.contains(TOKEN) && !e.contains("0123456789abcdef0123"), "{e}");
        // More used than allowed (the allowance was lowered): nothing is left, never a negative size.
        let s = full_create(storage_full(&(3 * 1024 * MB).to_string(), &(1024 * MB).to_string(), "2048", "x"));
        let e = update(&s, dir.path()).unwrap_err();
        assert_eq!(full_workspace(&e), Some("This tour needs 2 KB, and your workspace has 0 KB left of its 1.0 GB. Delete tours you no longer need, or ask for more room, and publish again."));
    }

    #[test]
    fn a_storage_full_answer_without_usable_numbers_keeps_the_sites_own_sentence() {
        let dir = distinct_bundle(2, false);
        let detail = "this tour needs 14.2 MB of room, but the workspace has 3.10 MB left of its 1.00 GB allowance. Nothing was uploaded. Delete tours you no longer need, or ask the site's admin for more room";
        let said = format!("Could not start the upload: {detail} (HTTP 413).");
        let bodies = [
            format!(r#"{{"error":"storage_full","detail":"{detail}"}}"#),
            format!(r#"{{"error":"storage_full","detail":"{detail}","used_bytes":1,"allowance_bytes":2}}"#),
            storage_full("\"1\"", "2", "3", detail),
            storage_full("1", "null", "3", detail),
            storage_full("1", "2", "-3", detail),
            storage_full("1.5", "2", "3", detail),
            storage_full("1", "2", "true", detail),
        ];
        for body in bodies {
            let s = full_create(body.clone());
            let e = update(&s, dir.path()).unwrap_err();
            assert_eq!(e, said, "{body}");
            assert_eq!(full_workspace(&e), None);
            assert_eq!(s.lines(), [CREATE]);
        }
        // Without a sentence either, it is the plain status line.
        let s = full_create(r#"{"detail":7,"error":"storage_full"}"#.to_string());
        assert_eq!(update(&s, dir.path()).unwrap_err(), "Could not start the upload: storage_full (HTTP 413).");
    }

    #[test]
    fn a_413_with_any_other_code_reads_as_before() {
        let dir = distinct_bundle(2, false);
        // The numbers alone do not make it the full-workspace sentence: the code does.
        let s = full_create(r#"{"error":"too_large","detail":"the request is over 262144 bytes","used_bytes":1,"allowance_bytes":2,"needed_bytes":3}"#.to_string());
        let e = update(&s, dir.path()).unwrap_err();
        assert_eq!(e, "Could not start the upload: the request is over 262144 bytes (HTTP 413).");
        assert_eq!(full_workspace(&e), None);
        let s = full_create(String::new());
        assert_eq!(update(&s, dir.path()).unwrap_err(), "Could not start the upload: the server answered HTTP 413.");
        // The code under another status is not it either.
        let s = refusing_create(500, r#"{"error":"storage_full","detail":"no","used_bytes":1,"allowance_bytes":2,"needed_bytes":3}"#);
        assert_eq!(update(&s, dir.path()).unwrap_err(), "Could not start the upload: no (HTTP 500).");
    }

    #[test]
    fn only_the_create_gets_the_full_workspace_message() {
        let body = storage_full("1", "2", "3", "no room");
        let dir = distinct_bundle(2, false);
        // On a step and on the commit it is the site's sentence with its status, and the upload is aborted once.
        for (at, said) in [("/steps/1", "Could not send step 1: no room (HTTP 413)."), ("/commit", "Could not finish the upload: no room (HTTP 413).")] {
            let body = body.clone();
            let s = stub(move |req, seen| if req.line.ends_with(at) { reply(413, &body) } else { platform(req, seen) });
            let e = upload_bundle(&s.base, TOKEN, dir.path(), &Target::New, |_| true).unwrap_err();
            assert_eq!(e, said);
            assert_eq!(full_workspace(&e), None);
            assert_eq!(count(&s.lines(), |l| l.starts_with("DELETE ")), 1, "{at}");
        }
        // On the logo too.
        let with_logo = logo_bundle(1);
        let b = body.clone();
        let s = stub(move |req, seen| if req.line.ends_with("/logo") { reply(413, &b) } else { platform(req, seen) });
        assert_eq!(upload_bundle(&s.base, TOKEN, with_logo.path(), &Target::New, |_| true).unwrap_err(), "Could not send the logo: no room (HTTP 413).");
        assert_eq!(count(&s.lines(), |l| l.starts_with("DELETE ")), 1);
        // On the publish step: the upload is done, and the result carries the site's sentence.
        let b = body.clone();
        let s = platform_with(move |_| reply(413, &b));
        let done = upload_and_publish_to(&s, true, |_| true).unwrap();
        assert!(matches!(&done.outcome, Outcome::NotPublic { cancelled: false, error } if error == "Could not publish the tour: no room (HTTP 413)."), "{done:?}");
        // And on every other request.
        let b = body.clone();
        let s = stub(move |_, _| reply(413, &b));
        let c = client(&s);
        assert_eq!(c.workspace().unwrap_err(), "Could not check the server: no room (HTTP 413).");
        assert_eq!(c.list_tours().unwrap_err(), "Could not list the tours: no room (HTTP 413).");
        assert_eq!(c.rename_tour(TOUR, "New").unwrap_err(), "Could not rename the tour: no room (HTTP 413).");
        assert_eq!(c.unpublish_tour(TOUR, Duration::ZERO).unwrap_err(), "Could not unpublish the tour: no room (HTTP 413).");
        assert_eq!(c.delete_tour(TOUR, Duration::ZERO, |_| {}).unwrap_err().message, "Could not delete the tour: no room (HTTP 413).");
    }

    #[test]
    fn the_sizes_never_contradict_each_other_and_never_overflow() {
        let dir = distinct_bundle(2, false);
        let said = |used: u64, allowance: u64, needed: u64| {
            let s = full_create(storage_full(&used.to_string(), &allowance.to_string(), &needed.to_string(), "x"));
            let e = update(&s, dir.path()).unwrap_err();
            full_workspace(&e).map(str::to_string).unwrap_or(e)
        };
        let rest = "Delete tours you no longer need, or ask for more room, and publish again.";
        // What the tour needs is rounded up and what is left down: both would read 3.1 MB to the nearest.
        let left = 3 * MB + MB / 8;
        assert_eq!(said(1024 * MB - left, 1024 * MB, left + 1), format!("This tour needs 3.2 MB, and your workspace has 3.1 MB left of its 1.0 GB. {rest}"));
        // A need above nothing is never "0 KB", and a rest under one unit is none.
        assert_eq!(said(1024 * MB - 100, 1024 * MB, 101), format!("This tour needs 1 KB, and your workspace has 0 KB left of its 1.0 GB. {rest}"));
        // Rounding up moves to the next unit instead of writing 1024 of the smaller one.
        assert_eq!(said(0, 500 * 1024, MB - 1), format!("This tour needs 1.0 MB, and your workspace has 500 KB left of its 500 KB. {rest}"));
        assert_eq!(said(0, 99 * MB, 100 * MB - 1), format!("This tour needs 100 MB, and your workspace has 99.0 MB left of its 99.0 MB. {rest}"));
        assert_eq!(said(1, 1024 * MB, 1024 * MB), format!("This tour needs 1.0 GB, and your workspace has 1023 MB left of its 1.0 GB. {rest}"));
        assert_eq!(said(0, 2047 * MB, 1024 * MB + 1), format!("This tour needs 1.1 GB, and your workspace has 1.9 GB left of its 2.0 GB. {rest}"));
        // The largest numbers there are.
        let huge = said(0, u64::MAX, u64::MAX);
        assert!(huge.starts_with("This tour needs 17179869184.0 GB, and your workspace has 17179869183.9 GB left of its "), "{huge}");
        assert!(said(u64::MAX, u64::MAX, u64::MAX).contains("has 0 KB left"));
    }

    #[test]
    fn numbers_that_are_no_byte_counts_and_bodies_that_are_no_answers_read_as_before() {
        let dir = distinct_bundle(2, false);
        for odd in ["1e30", "18446744073709551616", "1e3", "3.0"] {
            let s = full_create(storage_full("1", "2", odd, "no room"));
            assert_eq!(update(&s, dir.path()).unwrap_err(), "Could not start the upload: no room (HTTP 413).", "{odd}");
        }
        // Not JSON at all: a hosting error page, or nothing.
        for body in ["<html><body>413 Request Entity Too Large</body></html>", "storage_full", "{\"error\":\"storage_full\""] {
            let s = full_create(body.to_string());
            assert_eq!(update(&s, dir.path()).unwrap_err(), "Could not start the upload: the server answered HTTP 413.", "{body}");
        }
        // The site's own sentence is cut like any other text from a site.
        let long = "room ".repeat(100);
        let s = full_create(format!(r#"{{"error":"storage_full","detail":"{long}"}}"#));
        let e = update(&s, dir.path()).unwrap_err();
        assert_eq!(e, format!("Could not start the upload: {} (HTTP 413).", long.chars().take(MAX_DETAIL_CHARS).collect::<String>().trim()));
        assert!(e.chars().count() < MAX_DETAIL_CHARS + 60);
    }

    #[test]
    fn the_sites_text_cannot_pass_for_the_full_workspace_sentence() {
        let dir = distinct_bundle(2, false);
        let body = json!({ "error": "x", "detail": FULL_SENTENCE }).to_string();
        let s = stub(move |req, seen| if req.line == CREATE { reply(500, &body) } else { platform(req, seen) });
        let e = update(&s, dir.path()).unwrap_err();
        assert!(e.starts_with("Could not start the upload: The tour was not uploaded."), "{e}");
        assert_eq!(full_workspace(&e), None);
        assert_eq!(full_workspace(FULL_SENTENCE).map(str::len), Some(FULL_SENTENCE.len() - FULL_HEADING.len() - FULL_NOTE.len() - 3));
        for other in ["", "The tour was not uploaded", "The tour was not uploaded. Tours that are already online are not affected.", CANCELLED, REJECTED_TOKEN] {
            assert_eq!(full_workspace(other), None, "{other}");
        }
    }

    // ---- The logo's size goes with the create ----

    const LOGO: &[u8] = b"\x89PNG\r\n\x1a\nfakepng, and a logo of its own length";

    /// A bundle like [`distinct_bundle`] with a logo that is no step's size.
    fn logo_bundle(steps: usize) -> tempfile::TempDir {
        let dir = distinct_bundle(steps, true);
        std::fs::write(dir.path().join("logo.png"), LOGO).unwrap();
        dir
    }

    fn create_body(s: &Stub) -> Value {
        serde_json::from_slice(&s.requests()[0].body).unwrap()
    }

    #[test]
    fn a_create_that_carries_a_logo_says_how_many_bytes_the_logo_is() {
        let dir = logo_bundle(2);
        // A new tour.
        let s = stub(platform);
        upload_bundle(&s.base, TOKEN, dir.path(), &Target::New, |_| true).unwrap();
        assert_eq!(create_body(&s)["logo_bytes"], json!(LOGO.len()));
        let sent = s.requests().into_iter().find(|r| r.line.ends_with("/logo")).unwrap();
        assert_eq!(sent.body, LOGO, "what is sent is as long as what was said");
        // An update: the site only says in its answer whether it takes the logo, so the size always goes.
        for settings in [true, false] {
            let s = replace_stub("[]", settings, "draft", |_| reply(200, LIVE));
            update(&s, dir.path()).unwrap();
            assert_eq!(create_body(&s)["logo_bytes"], json!(LOGO.len()), "{settings}");
            assert_eq!(count(&s.lines(), |l| l.ends_with("/logo")), usize::from(settings));
        }
    }

    #[test]
    fn a_create_without_a_logo_never_names_a_logo_size() {
        // No logo in the tour.
        let plain = distinct_bundle(2, false);
        let s = stub(platform);
        upload_bundle(&s.base, TOKEN, plain.path(), &Target::New, |_| true).unwrap();
        assert!(create_body(&s).get("logo_bytes").is_none());
        let s = replace_stub("[]", true, "draft", |_| reply(200, LIVE));
        update(&s, plain.path()).unwrap();
        assert!(create_body(&s).get("logo_bytes").is_none());
        // A new chapter never sends the bundle's logo, so it does not name its size.
        let dir = logo_bundle(2);
        let s = stub(platform);
        upload_bundle(&s.base, TOKEN, dir.path(), &Target::Chapter(TOUR.into()), |_| true).unwrap();
        assert!(create_body(&s).get("logo_bytes").is_none());
        // An empty logo file has no size the site takes: the field stays out, and the site refuses the file as before.
        std::fs::write(dir.path().join("logo.png"), b"").unwrap();
        let s = stub(platform);
        upload_bundle(&s.base, TOKEN, dir.path(), &Target::New, |_| true).unwrap();
        assert!(create_body(&s).get("logo_bytes").is_none());
    }

    #[test]
    fn a_logo_that_changed_size_after_the_create_is_not_sent() {
        let dir = logo_bundle(1);
        let path = dir.path().join("logo.png");
        let s = stub(move |req, seen| {
            if req.line == CREATE {
                // Still within the limit, but no longer the size the create named.
                std::fs::write(&path, PNG).unwrap();
            }
            platform(req, seen)
        });
        let e = upload_bundle(&s.base, TOKEN, dir.path(), &Target::New, |_| true).unwrap_err();
        assert_eq!(e, "Could not send the logo: logo.png changed since it was checked.");
        assert_eq!(count(&s.lines(), |l| l.ends_with("/logo")), 0);
        assert_eq!(count(&s.lines(), |l| l.starts_with("DELETE ")), 1);
    }

    #[test]
    fn a_chapter_changed_meanwhile_is_a_sentence_and_aborts_once() {
        let s = publish_stub_over(
            |req, seen| {
                if req.line == CREATE {
                    reply(201, &created("[1]", false))
                } else if req.line.ends_with("/commit") {
                    reply(409, r#"{"error":"chapter_changed","detail":"the chapter was changed by another upload since this one started; start again"}"#)
                } else {
                    platform(req, seen)
                }
            },
            |_| reply(200, LIVE),
        );
        let dir = distinct_bundle(3, false);
        let e = update(&s, dir.path()).unwrap_err();
        assert_eq!(e, "Could not finish the upload: the chapter was changed by another upload meanwhile. Update again.");
        let lines = s.lines();
        assert_eq!(count(&lines, |l| l.ends_with("/commit")), 1, "{lines:?}");
        assert_eq!(count(&lines, |l| l.starts_with("DELETE ")), 1, "{lines:?}");
        assert_eq!(count(&lines, |l| l == PUBLISH), 0);
    }

    #[test]
    fn an_answer_naming_a_step_the_bundle_does_not_have_is_refused() {
        let dir = distinct_bundle(2, false);
        for have in ["[3]", "[0]", "[1,200]", "[-1]", "[1.5]", r#"["1"]"#, "[[1]]", r#""1""#, "1", "null", "{}", "true"] {
            let s = replace_stub(have, false, "ready", |_| reply(200, LIVE));
            let e = update(&s, dir.path()).unwrap_err();
            assert_eq!(e, "Could not start the upload: the server's answer was not understood.", "{have}");
            // The upload exists by then, so it is aborted; no picture, no commit, no publish.
            assert_eq!(s.lines(), [CREATE.to_string(), format!("DELETE /api/v1/uploads/{UP}")], "{have}");
        }
        // A step named twice is that step, once.
        let s = replace_stub("[2,2]", false, "draft", |_| reply(200, LIVE));
        update(&s, dir.path()).unwrap();
        assert_eq!(s.lines(), [CREATE.to_string(), format!("PUT /api/v1/uploads/{UP}/steps/1"), format!("POST /api/v1/uploads/{UP}/commit")]);
    }

    #[test]
    fn only_0_1_4_and_newer_support_update() {
        for (version, yes) in [("0.1.3", false), ("0.1.4", true), ("0.2.0", true), ("1.0.0", true), ("abc", false), ("0.1.10", true), ("0.0.9", false), ("0.1", false), ("", false), ("0.1.4.1", false), ("0.1.x", false), ("1.-1.0", false), ("+1.0.0", false)] {
            assert_eq!(supports_update(Some(version)), yes, "{version}");
        }
        assert!(!supports_update(None));
    }

    #[test]
    fn the_commit_answer_gives_the_chapter_id_for_every_target() {
        for target in [Target::New, Target::Chapter(TOUR.into())] {
            let s = stub(platform);
            let out = upload(&s, 2, false, &target).unwrap();
            assert_eq!(out, Published { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 2, publish: None }, "{target:?}");
        }
        for (advice, means) in [("draft", PublishAdvice::Draft), ("ready", PublishAdvice::Ready), ("other_changes", PublishAdvice::OtherChanges)] {
            let s = replace_stub("[]", false, advice, |_| reply(200, LIVE));
            let out = upload(&s, 2, false, &replace_target()).unwrap();
            assert_eq!(out, Published { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 3, publish: Some(means) });
        }
        // An id that is not one is never handed on: it names a folder later.
        let other_tour = r#"{"tour_id":"tour_bbbbbbbbbbbbbbbbbbbbbbbb","chapter_id":"ch_0123456789abcdef01234567","steps":2,"publish":"ready"}"#;
        let other_chapter = r#"{"tour_id":"tour_aaaaaaaaaaaaaaaaaaaaaaaa","chapter_id":"ch_ffffffffffffffffffffffff","steps":2,"publish":"ready"}"#;
        let cases: [(&str, bool); 7] = [
            (r#"{"tour_id":"tour_aaaaaaaaaaaaaaaaaaaaaaaa","steps":2}"#, false),
            (r#"{"tour_id":"tour_aaaaaaaaaaaaaaaaaaaaaaaa","chapter_id":"ch_../../x","steps":2}"#, false),
            (r#"{"tour_id":"tour_aaaaaaaaaaaaaaaaaaaaaaaa","chapter_id":7,"steps":2}"#, false),
            (r#"{"tour_id":"..\\x","chapter_id":"ch_0123456789abcdef01234567","steps":2}"#, false),
            // A replace must be answered about the chapter it replaced, and say what to do about publishing.
            (other_tour, true),
            (other_chapter, true),
            (r#"{"tour_id":"tour_aaaaaaaaaaaaaaaaaaaaaaaa","chapter_id":"ch_0123456789abcdef01234567","steps":2,"publish":"maybe"}"#, true),
        ];
        for (body, replace) in cases {
            let s = stub(move |req, seen| {
                if req.line == CREATE && replace {
                    reply(201, &created("[]", false))
                } else if req.line.ends_with("/commit") {
                    reply(201, body)
                } else {
                    platform(req, seen)
                }
            });
            let target = if replace { replace_target() } else { Target::New };
            assert_eq!(upload(&s, 2, false, &target).unwrap_err(), "Could not finish the upload: the server's answer was not understood.", "{body}");
        }
        // A replace answer without the advice is not understood either.
        let s = stub(|req, seen| if req.line == CREATE { reply(201, &created("[]", false)) } else { platform(req, seen) });
        assert!(upload(&s, 2, false, &replace_target()).unwrap_err().ends_with("not understood."));
    }

    #[test]
    fn the_token_is_never_in_an_update_error() {
        let echo = format!(r#"{{"error":"x","detail":"you sent {TOKEN}"}}"#);
        let dir = distinct_bundle(2, false);
        let mut errors = Vec::new();
        // Refused at create, at a step, at the commit; an answer that is not understood; no answer at all.
        for at in ["create", "/steps/1", "/commit"] {
            let echo = echo.clone();
            let s = stub(move |req, seen| {
                let here = if at == "create" { req.line == CREATE } else { req.line.ends_with(at) };
                if here {
                    Answer::Reply(500, echo.clone(), vec![])
                } else if req.line == CREATE {
                    reply(201, &created("[]", false))
                } else {
                    platform(req, seen)
                }
            });
            errors.push(update(&s, dir.path()).unwrap_err());
        }
        let e2 = echo.clone();
        errors.push(update(&stub(move |_, _| Answer::Reply(201, e2.clone(), vec![])), dir.path()).unwrap_err());
        errors.push(update(&stub(|_, _| Answer::Drop), dir.path()).unwrap_err());
        errors.push(update(&stub(|_, _| reply(302, "")), dir.path()).unwrap_err());
        errors.push(update_tour("http://example.com", TOKEN, dir.path(), TOUR, CH, |_| true, |_| {}, |_| true).unwrap_err());
        // The publish step's error is part of a result that is shown, too.
        let s = replace_stub("[]", false, "ready", move |_| Answer::Reply(500, echo.clone(), vec![]));
        match update(&s, dir.path()).unwrap().outcome {
            Outcome::NotPublic { error, .. } => errors.push(error),
            other => panic!("{other:?}"),
        }
        assert_eq!(errors.len(), 8);
        for e in &errors {
            assert!(!e.contains(TOKEN) && !format!("{e:?}").contains(TOKEN), "{e}");
            assert!(!e.contains("0123456789abcdef0123"), "{e}");
        }
        assert!(errors[0].contains("you sent [token]"), "{}", errors[0]);
        // A 401 anywhere is the token sentence, and an upload that exists is still aborted once.
        let s = stub(|req, seen| if req.line == CREATE { reply(201, &created("[]", false)) } else if req.line.ends_with("/commit") { reply(401, r#"{"error":"unauthorized"}"#) } else { platform(req, seen) });
        assert_eq!(update(&s, dir.path()).unwrap_err(), REJECTED_TOKEN);
        assert_eq!(count(&s.lines(), |l| l.starts_with("DELETE ")), 1);
    }

    #[test]
    fn a_step_file_changed_after_it_was_hashed_is_not_sent() {
        // The hash the site matched against must be of the bytes that are sent.
        let dir = distinct_bundle(2, false);
        let path = dir.path().join("step-002.png");
        let other: Vec<u8> = picture(2).iter().rev().copied().collect();
        let s = stub(move |req, seen| {
            if req.line == CREATE {
                // Same size, other bytes, between the check and the send.
                std::fs::write(&path, &other).unwrap();
                reply(201, &created("[1]", false))
            } else {
                platform(req, seen)
            }
        });
        let e = update(&s, dir.path()).unwrap_err();
        assert_eq!(e, "Could not send step 2: step-002.png changed since it was checked.");
        let lines = s.lines();
        assert_eq!(lines, [CREATE.to_string(), format!("DELETE /api/v1/uploads/{UP}")]);
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
    fn a_token_the_site_refuses_is_remembered_by_its_fingerprint_until_the_site_takes_it_again() {
        // A token of this test's own, so that tests running beside it cannot change the answer.
        const MINE: &str = "dp_abad1deaabad1deaabad1deaabad1deaabad1dea";
        let fingerprint = token_fingerprint(MINE);
        assert!(!fingerprint.contains(MINE) && fingerprint.len() == 64);
        assert_eq!(token_fingerprint(&format!(" {MINE}\n")), fingerprint);
        assert_ne!(token_fingerprint(TOKEN), fingerprint);
        assert!(!is_refused(&fingerprint));
        let refuses = stub(|_, _| reply(401, r#"{"error":"unauthorized"}"#));
        assert_eq!(workspace(&refuses.base, MINE).unwrap_err(), REJECTED_TOKEN);
        assert!(is_refused(&fingerprint));
        // Taken again: no longer refused.
        let takes = stub(|_, _| reply(200, r#"{"workspace":"Acme"}"#));
        workspace(&takes.base, MINE).unwrap();
        assert!(!is_refused(&fingerprint));
    }

    #[test]
    fn a_401_reads_as_a_token_problem_without_the_token() {
        let s = stub(|_, _| reply(401, r#"{"error":"unauthorized"}"#));
        assert_eq!(workspace(&s.base, TOKEN).unwrap_err(), "The server rejected the API token.");
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
                RemoteTour { id: "tour_1".into(), title: "Onboarding".into(), status: "draft".into(), chapters: 2, steps: 9, link: None, updated_at: Some(1700000000000), published_at: None, unpublished_changes: false },
                RemoteTour { id: "tour_2".into(), title: "Billing".into(), status: "published".into(), chapters: 1, steps: 3, link: None, updated_at: Some(1), published_at: None, unpublished_changes: false },
            ]
        );
        // A list that names a tour's slug or url gives its link, by the same rule as a publish answer.
        let s2 = stub(|_, _| reply(200, r#"{"tours":[{"id":"a","title":"A","status":"published","chapters":1,"steps":3,"slug":"abc"},{"id":"b","title":"B","status":"published","chapters":1,"steps":3,"url":"https://evil.example/t/x"}]}"#));
        let links: Vec<Option<String>> = list_tours(&s2.base, TOKEN).unwrap().into_iter().map(|t| t.link).collect();
        assert_eq!(links, [Some(format!("{}/t/abc", s2.base)), None]);
        assert_eq!(s.lines(), ["GET /api/v1/tours"]);
        assert_eq!(workspace(&stub(platform).base, TOKEN).unwrap().name, "Acme");

        let s = stub(|_, _| reply(200, r#"{"tours":[{"id":"t"}]}"#));
        assert_eq!(list_tours(&s.base, TOKEN).unwrap_err(), "Could not list the tours: the server's answer was not understood.");
    }

    #[test]
    fn the_token_is_never_in_an_error_or_a_debug_print() {
        let echo = format!(r#"{{"error":"x","detail":"you sent {TOKEN}"}}"#);
        let mut errors = vec![
            check_server("http://example.com").unwrap_err(),
            workspace("http://example.com", TOKEN).unwrap_err(),
            workspace("https://x.dev", "dp_short").unwrap_err(),
            store_token_in(&FakeStore::default(), &format!("{TOKEN}x")).unwrap_err(),
        ];
        let e2 = echo.clone();
        let s = stub(move |req, seen| if req.line.ends_with("/steps/1") { Answer::Reply(500, e2.clone(), vec![]) } else { platform(req, seen) });
        errors.push(upload(&s, 1, false, &Target::New).unwrap_err());
        let s = stub(move |_, _| Answer::Reply(400, echo.clone(), vec![]));
        errors.push(workspace(&s.base, TOKEN).unwrap_err());
        errors.push(list_tours(&s.base, TOKEN).unwrap_err());
        errors.push(upload(&s, 1, false, &Target::New).unwrap_err());
        errors.push(upload(&stub(|_, _| reply(302, "")), 1, false, &Target::New).unwrap_err());
        errors.push(workspace(&stub(|_, _| Answer::Drop).base, TOKEN).unwrap_err());
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
        let err = workspace(&s.base, TOKEN).unwrap_err();
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
        assert_eq!(client.workspace().unwrap_err(), "Could not check the server: the server did not answer in time.");
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn a_huge_answer_is_cut_off() {
        let big = format!(r#"{{"workspace":"{}"}}"#, "x".repeat(MAX_RESPONSE_BYTES as usize + 10));
        let s = stub(move |_, _| Answer::Reply(200, big.clone(), vec![]));
        assert_eq!(workspace(&s.base, TOKEN).unwrap_err(), "Could not check the server: the server's answer was too large.");
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
        let err = workspace(&s.base, TOKEN).unwrap_err();
        assert!(err.contains("abcdefgh"), "{err}");
        assert!(!err.chars().any(|c| is_invisible(c) || c.is_control()), "{err:?}");
        assert!(err.chars().count() < MAX_DETAIL_CHARS + 60, "{}", err.len());
    }

    // ---- Managing a tour on the site: rename, delete, unpublish ----

    const RENAME: &str = "PATCH /api/v1/tours/tour_aaaaaaaaaaaaaaaaaaaaaaaa";
    const DELETE: &str = "DELETE /api/v1/tours/tour_aaaaaaaaaaaaaaaaaaaaaaaa";
    const UNPUBLISH: &str = "POST /api/v1/tours/tour_aaaaaaaaaaaaaaaaaaaaaaaa/unpublish";
    /// The server's 404 for a tour it does not have, and its 404 for a route it does not have.
    const NO_TOUR: &str = r#"{"error":"not_found","detail":"no such tour"}"#;
    const NO_ROUTE: &str = r#"{"error":"not_found"}"#;
    const CLEANUP_PENDING: &str = r#"{"error":"cleanup_pending","detail":"the tour is deleted, but some of its files could not be removed yet; delete it again to retry"}"#;
    const GONE_TRAILER: &str = " The tour itself is gone from the site, but some of its files remain. Use Finish deleting on the Tours page to remove them.";

    fn deleting(done: u32, total: u32) -> Answer {
        reply(202, &format!(r#"{{"status":"in_progress","done":{done},"total":{total}}}"#))
    }

    /// Deletes TOUR against the stub with no pause, recording every progress report.
    fn delete(stub: &Stub) -> (Result<Deleted, DeleteFailed>, Vec<Option<Images>>) {
        let mut seen = Vec::new();
        let r = client(stub).delete_tour(TOUR, Duration::ZERO, |p| seen.push(p));
        (r, seen)
    }

    fn unpublish(stub: &Stub) -> Result<(), String> {
        client(stub).unpublish_tour(TOUR, Duration::ZERO)
    }

    #[test]
    fn rename_sends_the_title_and_returns_what_the_site_saved() {
        let s = stub(|_, _| reply(200, &format!(r#"{{"id":"{TOUR}","title":"New name"}}"#)));
        assert_eq!(rename_tour(&s.base, TOKEN, TOUR, "  New name ").unwrap(), "New name");
        let reqs = s.requests();
        assert_eq!(s.lines(), [RENAME]);
        assert_eq!(reqs[0].auth.as_deref(), Some(format!("Bearer {TOKEN}").as_str()));
        assert_eq!(reqs[0].content_type.as_deref(), Some("application/json"));
        assert_eq!(serde_json::from_slice::<Value>(&reqs[0].body).unwrap(), json!({ "title": "  New name " }));
        // A title with quotes and non-ASCII letters goes through as JSON.
        let s = stub(|req, _| {
            let title = serde_json::from_slice::<Value>(&req.body).unwrap()["title"].clone();
            reply(200, &json!({ "id": TOUR, "title": title }).to_string())
        });
        assert_eq!(rename_tour(&s.base, TOKEN, TOUR, "Say \"hi\" · café").unwrap(), "Say \"hi\" · café");
    }

    #[test]
    fn rename_refusals_are_sentences_and_are_sent_once() {
        let cases: [(u16, &str, &str); 9] = [
            (400, r#"{"error":"bad_title","detail":"the title must not be empty"}"#, "Could not rename the tour: the title must not be empty (HTTP 400)."),
            (404, NO_TOUR, "Could not rename the tour: that tour no longer exists."),
            // A site of version 0.1.0 has no rename route.
            (404, NO_ROUTE, "Could not rename the tour: the site needs an update to version 0.1.1 or newer."),
            (404, "404 Not Found", "Could not rename the tour: the site needs an update to version 0.1.1 or newer."),
            (405, "", "Could not rename the tour: the site needs an update to version 0.1.1 or newer."),
            (429, r#"{"error":"rate_limited"}"#, "Could not rename the tour: the server is busy (HTTP 429). Try again later."),
            (500, r#"{"error":"internal"}"#, "Could not rename the tour: internal (HTTP 500)."),
            (503, "", "Could not rename the tour: the server answered HTTP 503."),
            (401, r#"{"error":"unauthorized"}"#, REJECTED_TOKEN),
        ];
        for (status, body, message) in cases {
            let s = stub(move |_, _| reply(status, body));
            let e = rename_tour(&s.base, TOKEN, TOUR, "New").unwrap_err();
            assert_eq!(e, message);
            assert_eq!(s.lines().len(), 1, "{status} must not be sent again");
            assert_eq!(is_gone(&e), body == NO_TOUR);
        }
        // No answer at all, an answer about another tour, a web page: none is taken as a rename.
        let s = stub(|_, _| Answer::Drop);
        let e = rename_tour(&s.base, TOKEN, TOUR, "New").unwrap_err();
        assert!(e.starts_with("Could not rename the tour: ") && e.ends_with('.') && !e.contains("dp_"), "{e}");
        assert_eq!(s.lines().len(), 1);
        for body in [r#"{"id":"tour_bbbbbbbbbbbbbbbbbbbbbbbb","title":"New"}"#, "<html>Sign in</html>", r#"{"id":"tour_aaaaaaaaaaaaaaaaaaaaaaaa"}"#, r#"{"id":"tour_aaaaaaaaaaaaaaaaaaaaaaaa","title":7}"#] {
            let s = stub(move |_, _| reply(200, body));
            assert_eq!(rename_tour(&s.base, TOKEN, TOUR, "New").unwrap_err(), "Could not rename the tour: the server's answer was not understood.", "{body}");
        }
        // An id that is not a tour id is never sent anywhere.
        let s = stub(platform);
        for id in ["", "tour_1", "../uploads/x", "tour_AAAAAAAAAAAAAAAAAAAAAAAA"] {
            assert!(rename_tour(&s.base, TOKEN, id, "New").is_err());
            assert!(delete_tour(&s.base, TOKEN, id, |_| {}).is_err());
            assert!(unpublish_tour(&s.base, TOKEN, id).is_err());
        }
        assert!(s.lines().is_empty());
    }

    #[test]
    fn a_small_tour_is_deleted_in_one_call() {
        let s = stub(|_, _| reply(200, r#"{"deleted":true}"#));
        let (r, seen) = delete(&s);
        assert_eq!(r, Ok(Deleted::BySite));
        assert_eq!(seen, [None]);
        assert_eq!(s.lines(), [DELETE]);
        let req = &s.requests()[0];
        assert_eq!(req.auth.as_deref(), Some(format!("Bearer {TOKEN}").as_str()));
        assert!(req.body.is_empty());
    }

    #[test]
    fn delete_calls_until_the_site_says_nothing_is_left() {
        let s = stub(|_, seen| match seen {
            0 => deleting(250, 601),
            1 => deleting(500, 601),
            _ => reply(200, r#"{"deleted":true}"#),
        });
        let (r, seen) = delete(&s);
        assert_eq!(r, Ok(Deleted::BySite));
        assert_eq!(seen, [None, Some((250, 601)), Some((500, 601))]);
        assert_eq!(s.lines(), [DELETE; 3]);
    }

    #[test]
    fn a_tour_that_is_already_gone_counts_as_deleted() {
        // Deleted elsewhere before the first call. The site says the same of another workspace's
        // tour, so this answer alone does not say the tour was ever the token's.
        let s = stub(|_, _| reply(404, NO_TOUR));
        assert_eq!(delete(&s).0, Ok(Deleted::NotThere));
        assert_eq!(s.lines().len(), 1);
        // Or finished by someone else between two calls: the site had answered for this tour.
        let s = stub(|_, seen| if seen == 0 { deleting(250, 601) } else { reply(404, NO_TOUR) });
        assert_eq!(delete(&s).0, Ok(Deleted::BySite));
        assert_eq!(s.lines().len(), 2);
        // The same after files that could not be removed yet.
        let s = stub(|_, seen| if seen == 0 { reply(500, CLEANUP_PENDING) } else { reply(404, NO_TOUR) });
        assert_eq!(delete(&s).0, Ok(Deleted::BySite));
    }

    #[test]
    fn a_site_without_a_delete_route_is_not_mistaken_for_a_deleted_tour() {
        for (status, body) in [(404, NO_ROUTE), (404, "404 Not Found"), (404, ""), (404, r#"{"error":"not_found","detail":"no such page"}"#), (405, "")] {
            let s = stub(move |_, _| reply(status, body));
            let e = delete(&s).0.unwrap_err();
            assert_eq!(e, DeleteFailed { message: "Could not delete the tour: the site needs an update to version 0.1.1 or newer.".into(), unfinished: false, not_started: true }, "{status} {body}");
            assert_eq!(s.lines().len(), 1);
        }
    }

    #[test]
    fn delete_refusals_are_sentences_and_are_not_retried() {
        let cases: [(u16, &str, &str); 6] = [
            (409, r#"{"error":"publish_busy","detail":"a publish of this tour is in progress; wait"}"#, "Could not delete the tour: a publish of this tour is in progress. Wait for it to finish, or unpublish the tour, then delete it again."),
            (429, r#"{"error":"rate_limited"}"#, "Could not delete the tour: the server is busy (HTTP 429). Try again later."),
            (500, r#"{"error":"internal"}"#, "Could not delete the tour: internal (HTTP 500)."),
            (503, "", "Could not delete the tour: the server answered HTTP 503."),
            (302, "", "Could not delete the tour: the server answered with a redirect (HTTP 302), which is not followed. Check the server address."),
            (401, r#"{"error":"unauthorized"}"#, REJECTED_TOKEN),
        ];
        for (status, body, message) in cases {
            let s = stub(move |_, _| reply(status, body));
            let (r, _) = delete(&s);
            assert_eq!(r.unwrap_err(), DeleteFailed { message: message.into(), unfinished: false, not_started: matches!(status, 401 | 409) }, "{status}");
            assert_eq!(s.lines().len(), 1, "{status} must not be sent again");
        }
        let s = stub(|_, _| Answer::Drop);
        let e = delete(&s).0.unwrap_err();
        assert!(e.message.starts_with("Could not delete the tour: ") && e.message.ends_with('.') && !e.message.contains("dp_") && !e.unfinished, "{e:?}");
        assert!(!e.not_started, "a lost answer says nothing about what the site did");
        assert_eq!(s.lines().len(), 1);
    }

    #[test]
    fn a_delete_whose_files_could_not_be_removed_yet_is_called_again_and_resumes() {
        let s = stub(|_, seen| match seen {
            0 => deleting(250, 601),
            1 => reply(500, CLEANUP_PENDING),
            2 => deleting(500, 601),
            _ => reply(200, r#"{"deleted":true}"#),
        });
        let (r, seen) = delete(&s);
        assert_eq!(r, Ok(Deleted::BySite));
        assert_eq!(s.lines().len(), 4);
        assert_eq!(seen, [None, Some((250, 601)), Some((250, 601)), Some((500, 601))]);
        // On the very first call too: the tour is deleted by then, only files are left.
        let s = stub(|_, seen| if seen == 0 { reply(500, CLEANUP_PENDING) } else { reply(200, r#"{"deleted":true}"#) });
        assert_eq!(delete(&s).0, Ok(Deleted::BySite));
        // A site that keeps failing is given up, and the error says what is left.
        let s = stub(|_, _| reply(500, CLEANUP_PENDING));
        let e = delete(&s).0.unwrap_err();
        assert_eq!(e.message, format!("Could not delete the tour completely: the site could not remove some of its files yet.{GONE_TRAILER}"));
        assert!(e.unfinished);
        assert_eq!(s.lines().len(), MAX_STALLED_CALLS as usize);
    }

    #[test]
    fn a_delete_that_stops_making_progress_is_given_up() {
        let s = stub(|_, _| deleting(250, 601));
        let (r, _) = delete(&s);
        let e = r.unwrap_err();
        assert_eq!(e.message, format!("Could not delete the tour completely: the site stopped making progress at 250 of 601 files.{GONE_TRAILER}"));
        assert!(e.unfinished);
        assert_eq!(s.lines().len(), 1 + MAX_STALLED_CALLS as usize);
        // Progress in between starts the count again.
        let s = stub(|_, seen| match seen {
            0 | 1 | 2 => deleting(250, 1000),
            3 | 4 | 5 => deleting(500, 1000),
            _ => reply(200, r#"{"deleted":true}"#),
        });
        assert_eq!(delete(&s).0, Ok(Deleted::BySite));
        // An answer without counts is not progress.
        let s = stub(|_, _| reply(202, r#"{"status":"in_progress"}"#));
        let (r, seen) = delete(&s);
        assert!(r.unwrap_err().unfinished);
        assert!(seen.iter().all(Option::is_none));
        assert_eq!(s.lines().len(), MAX_STALLED_CALLS as usize);
    }

    #[test]
    fn a_delete_that_never_finishes_stops_at_the_hard_limit() {
        let s = stub(|_, seen| deleting(seen as u32 + 1, 100_000));
        let e = delete(&s).0.unwrap_err();
        assert_eq!(e.message, format!("Could not delete the tour completely: the site did not finish within {MAX_PUBLISH_CALLS} calls.{GONE_TRAILER}"));
        assert!(e.unfinished);
        assert_eq!(s.lines().len(), MAX_PUBLISH_CALLS as usize);
    }

    #[test]
    fn a_delete_that_fails_half_way_says_the_tour_is_gone_but_files_remain() {
        let s = stub(|_, seen| if seen == 0 { deleting(250, 601) } else { Answer::Drop });
        let e = delete(&s).0.unwrap_err();
        assert!(e.unfinished && e.message.starts_with("Could not delete the tour: ") && e.message.ends_with(GONE_TRAILER) && !e.message.contains("dp_"), "{e:?}");
        assert_eq!(s.lines().len(), 2);
        let s = stub(|_, seen| if seen == 0 { deleting(250, 601) } else { reply(503, "") });
        let e = delete(&s).0.unwrap_err();
        assert_eq!(e.message, format!("Could not delete the tour: the server answered HTTP 503.{GONE_TRAILER}"));
        assert!(e.unfinished);
        assert_eq!(s.lines().len(), 2, "a failure that is not cleanup_pending is not sent again");
    }

    #[test]
    fn a_web_page_404_in_the_middle_of_a_delete_does_not_claim_the_tour_is_gone() {
        for (status, body) in [(404, "<html>Not Found</html>"), (404, NO_ROUTE), (405, "")] {
            let s = stub(move |_, seen| if seen == 0 { deleting(250, 601) } else { reply(status, body) });
            let e = delete(&s).0.unwrap_err();
            assert_eq!(e, DeleteFailed { message: "Could not delete the tour: the site needs an update to version 0.1.1 or newer.".into(), unfinished: false, not_started: false }, "{status} {body}");
            assert!(!e.message.contains("gone from the site"));
            assert_eq!(s.lines().len(), 2);
        }
        // The sentence for a real unfinished delete names something the user can do.
        assert!(GONE_TRAILER.contains("Finish deleting on the Tours page") && !GONE_TRAILER.contains("delete it again"));
    }

    #[test]
    fn the_host_of_a_site_address_is_its_name_only() {
        assert_eq!(host_of(" https://Tours.Example.com/demo/ ").as_deref(), Some("tours.example.com"));
        assert_eq!(host_of("http://localhost:8787").as_deref(), Some("localhost"));
        assert_eq!(host_of("http://tours.example.com"), None);
        assert!(is_tour_id(TOUR) && !is_tour_id("tour_1") && !is_tour_id("../x"));
    }

    #[test]
    fn delete_answers_that_make_no_sense_are_refused() {
        for (status, body) in [(200, "{}"), (200, r#"{"deleted":false}"#), (200, "<html>Sign in</html>"), (202, "{}"), (202, r#"{"status":"done"}"#), (204, ""), (201, r#"{"deleted":true}"#)] {
            let s = stub(move |_, _| reply(status, body));
            let e = delete(&s).0.unwrap_err();
            assert_eq!(e, DeleteFailed { message: "Could not delete the tour: the server's answer was not understood.".into(), unfinished: false, not_started: false }, "{status} {body}");
            assert_eq!(s.lines().len(), 1);
        }
        // Counts that make no sense are not shown, but the loop goes on.
        let s = stub(|_, seen| match seen {
            0 => deleting(9, 4),
            1 => deleting(10, 0),
            _ => reply(200, r#"{"deleted":true}"#),
        });
        let (r, seen) = delete(&s);
        assert_eq!(r, Ok(Deleted::BySite));
        assert_eq!(seen, [None, None, None]);
    }

    #[test]
    fn unpublish_calls_until_the_public_images_are_gone() {
        let s = stub(|_, _| reply(204, ""));
        assert_eq!(unpublish(&s), Ok(()));
        assert_eq!(s.lines(), [UNPUBLISH]);
        assert!(s.requests()[0].body.is_empty());
        assert_eq!(s.requests()[0].auth.as_deref(), Some(format!("Bearer {TOKEN}").as_str()));
        let more = r#"{"status":"in_progress","detail":"the tour is unpublished; some of its public images are still to be removed; unpublish again to finish"}"#;
        let s = stub(move |_, seen| if seen < 2 { reply(202, more) } else { reply(204, "") });
        assert_eq!(unpublish(&s), Ok(()));
        assert_eq!(s.lines(), [UNPUBLISH; 3]);
        // The public function is the same loop.
        let s = stub(|_, _| reply(204, ""));
        assert_eq!(unpublish_tour(&s.base, TOKEN, TOUR), Ok(()));
    }

    #[test]
    fn unpublish_refusals_are_sentences_and_are_not_retried() {
        let cases: [(u16, &str, &str); 6] = [
            (404, NO_TOUR, "Could not unpublish the tour: that tour no longer exists."),
            (404, NO_ROUTE, "Could not unpublish the tour: not_found (HTTP 404)."),
            (429, r#"{"error":"rate_limited"}"#, "Could not unpublish the tour: the server is busy (HTTP 429). Try again later."),
            (500, r#"{"error":"internal"}"#, "Could not unpublish the tour: internal (HTTP 500)."),
            (503, "", "Could not unpublish the tour: the server answered HTTP 503."),
            (401, r#"{"error":"unauthorized"}"#, REJECTED_TOKEN),
        ];
        for (status, body, message) in cases {
            let s = stub(move |_, _| reply(status, body));
            let e = unpublish(&s).unwrap_err();
            assert_eq!(e, message);
            assert_eq!(is_gone(&e), body == NO_TOUR);
            assert_eq!(s.lines().len(), 1, "{status} must not be sent again");
        }
        let s = stub(|_, _| Answer::Drop);
        let e = unpublish(&s).unwrap_err();
        assert!(e.starts_with("Could not unpublish the tour: ") && e.ends_with('.') && !e.contains("dp_"), "{e}");
        for (status, body) in [(200, "{}"), (200, "<html>Sign in</html>"), (202, "{}"), (201, "")] {
            let s = stub(move |_, _| reply(status, body));
            assert_eq!(unpublish(&s).unwrap_err(), "Could not unpublish the tour: the server's answer was not understood.", "{status} {body}");
        }
    }

    #[test]
    fn an_unpublish_whose_clean_up_does_not_finish_says_the_tour_is_no_longer_public() {
        let pending = r#"{"error":"cleanup_pending","detail":"the tour is unpublished, but some of its public images could not be removed yet; unpublish again to retry"}"#;
        // Called again, it resumes.
        let s = stub(move |_, seen| if seen == 0 { reply(500, pending) } else { reply(204, "") });
        assert_eq!(unpublish(&s), Ok(()));
        assert_eq!(s.lines().len(), 2);
        // A site that keeps failing.
        let s = stub(move |_, _| reply(500, pending));
        assert_eq!(unpublish(&s).unwrap_err(), UNPUBLISHED_NOT_CLEARED);
        assert_eq!(s.lines().len(), MAX_STALLED_CALLS as usize);
        // A lost connection or a failure after the tour went off the web.
        let more = r#"{"status":"in_progress"}"#;
        let s = stub(move |_, seen| if seen == 0 { reply(202, more) } else { Answer::Drop });
        assert_eq!(unpublish(&s).unwrap_err(), UNPUBLISHED_NOT_CLEARED);
        let s = stub(move |_, seen| if seen == 0 { reply(202, more) } else { reply(503, "") });
        assert_eq!(unpublish(&s).unwrap_err(), UNPUBLISHED_NOT_CLEARED);
        // One that never finishes stops at the hard limit.
        let s = stub(move |_, _| reply(202, more));
        assert_eq!(unpublish(&s).unwrap_err(), UNPUBLISHED_NOT_CLEARED);
        assert_eq!(s.lines().len(), MAX_PUBLISH_CALLS as usize);
        assert!(!is_gone(UNPUBLISHED_NOT_CLEARED));
    }

    #[test]
    fn publishing_a_tour_that_is_gone_says_so() {
        let s = publish_stub(|_| reply(404, NO_TOUR));
        let e = publish(&s).0.unwrap_err();
        assert_eq!(e, "Could not publish the tour: that tour no longer exists.");
        assert!(is_gone(&e));
        // Text from the server can never pass for it.
        let s = publish_stub(|_| reply(500, r#"{"error":"x","detail":"Could not publish the tour: that tour no longer exists."}"#));
        assert!(!is_gone(&publish(&s).0.unwrap_err()));
        assert!(!is_gone(TOUR_GONE) && !is_gone("that tour no longer exists."));
    }

    #[test]
    fn the_token_is_never_in_a_manage_error() {
        // A server that echoes the Authorization header back in every error.
        let s = stub(|req, _| reply(500, &format!(r#"{{"error":"internal","detail":"failed for {}"}}"#, req.auth.clone().unwrap_or_default())));
        let errors = [rename_tour(&s.base, TOKEN, TOUR, "New").unwrap_err(), delete(&s).0.unwrap_err().message, unpublish(&s).unwrap_err(), workspace(&s.base, TOKEN).unwrap_err()];
        for e in errors {
            assert!(!e.contains(TOKEN) && !e.contains("dp_") && !e.contains(&TOKEN[3..]), "{e}");
        }
    }

    #[test]
    fn the_workspace_answer_gives_the_name_and_the_version_when_the_site_tells() {
        let s = stub(|_, _| reply(200, r#"{"workspace":"Acme","version":"0.1.1"}"#));
        assert_eq!(workspace(&s.base, TOKEN).unwrap(), Workspace { name: "Acme".into(), version: Some("0.1.1".into()) });
        assert_eq!(s.lines(), ["GET /api/v1/me"]);
        // A site of version 0.1.0 does not say.
        let s = stub(platform);
        assert_eq!(workspace(&s.base, TOKEN).unwrap(), Workspace { name: "Acme".into(), version: None });
        // Something that is not a version is not shown as one.
        for odd in [r#""""#, r#""latest""#, r#""1.0 <b>""#, "7", r#""0.1.1-with-a-very-long-suffix""#, "null"] {
            let s = stub(move |_, _| reply(200, &format!(r#"{{"workspace":"Acme","version":{odd}}}"#)));
            assert_eq!(workspace(&s.base, TOKEN).unwrap().version, None, "{odd}");
        }
        let s = stub(|_, _| reply(200, r#"{"version":"0.1.1"}"#));
        assert_eq!(workspace(&s.base, TOKEN).unwrap_err(), "Could not check the server: the server's answer was not understood.");
    }

    #[test]
    fn the_tour_list_gives_links_and_dates_and_an_older_site_gives_neither() {
        let s = stub(|_, _| {
            reply(
                200,
                r#"{"tours":[
                {"id":"tour_1","title":"Onboarding","status":"published","slug":"abc123","url":"/t/abc123","chapters":2,"steps":22,"updated_at":1700000000000,"published_at":1700000100000},
                {"id":"tour_2","title":"Testing","status":"draft","slug":"def456","url":null,"chapters":1,"steps":9,"updated_at":1700000200000,"published_at":null}]}"#,
            )
        });
        let tours = list_tours(&s.base, TOKEN).unwrap();
        assert_eq!(tours[0].link, Some(format!("{}/t/abc123", s.base)));
        assert_eq!((tours[0].updated_at, tours[0].published_at), (Some(1700000000000), Some(1700000100000)));
        assert!(tours[0].is_public() && !tours[1].is_public());
        // A draft has a slug but no link.
        assert_eq!(tours[1].link, None);
        assert_eq!((tours[1].updated_at, tours[1].published_at), (Some(1700000200000), None));
        // Version 0.1.0: no slug, no url, no published_at.
        let s = stub(|_, _| reply(200, r#"{"tours":[{"id":"tour_1","title":"Onboarding","status":"published","chapters":2,"steps":22,"updated_at":1700000000000}]}"#));
        let t = &list_tours(&s.base, TOKEN).unwrap()[0];
        assert_eq!((t.link.clone(), t.updated_at, t.published_at), (None, Some(1700000000000), None));
        // Dates that make no sense are left out.
        let s = stub(|_, _| reply(200, r#"{"tours":[{"id":"t","title":"T","status":"published","chapters":1,"steps":1,"updated_at":-5,"published_at":"yesterday"}]}"#));
        let t = &list_tours(&s.base, TOKEN).unwrap()[0];
        assert_eq!((t.updated_at, t.published_at), (None, None));
    }

    #[test]
    fn the_tour_list_says_which_public_tours_have_changes_waiting() {
        let row = |id: &str, status: &str, extra: &str| format!(r#"{{"id":"{id}","title":"T","status":"{status}","slug":"abc123","url":"/t/abc123","chapters":1,"steps":3,"updated_at":1700000000000{extra}}}"#);
        let rows = [
            row("yes", "published", r#","unpublished_changes":true"#),
            row("no", "published", r#","unpublished_changes":false"#),
            // An older site sends no such field.
            row("absent", "published", ""),
            // Anything that is not a yes or a no reads as no.
            row("null", "published", r#","unpublished_changes":null"#),
            row("string", "published", r#","unpublished_changes":"true""#),
            row("number", "published", r#","unpublished_changes":1"#),
            // A draft has nothing public to be behind, whatever the row says.
            row("draft", "draft", r#","unpublished_changes":true"#),
        ];
        let body = format!(r#"{{"tours":[{}]}}"#, rows.join(","));
        let s = stub(move |_, _| reply(200, &body));
        let tours = list_tours(&s.base, TOKEN).unwrap();
        let read: Vec<(&str, bool)> = tours.iter().map(|t| (t.id.as_str(), t.unpublished_changes)).collect();
        assert_eq!(read, [("yes", true), ("no", false), ("absent", false), ("null", false), ("string", false), ("number", false), ("draft", false)]);
        // The rest of each row is read as before.
        assert!(tours.iter().all(|t| t.title == "T" && t.steps == 3 && t.updated_at == Some(1700000000000)));
        assert!(tours[..6].iter().all(|t| t.link.is_some()) && tours[6].link.is_none());
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
