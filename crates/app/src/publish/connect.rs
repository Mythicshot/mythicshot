//! Sign in: this computer gets its own token from the site, after the person confirms a short
//! code in their browser, instead of a token being pasted by hand.
//!
//! The routine is: [`start`] asks the configured site for a code (`POST /api/v1/connect/start`
//! with the computer's name), then [`wait`] asks it every `interval` seconds whether the code
//! was confirmed (`POST /api/v1/connect/token`), until it answers with the token, refuses the
//! code (410: ran out, turned down, used or unknown alike), the code's time is up, or the person
//! cancels. Neither request carries a credential ([`Client::anonymous`]), and like every other
//! request they go to the configured site only and never follow a redirect.
//!
//! What is asked a second time, and only once per ask: a code check that got no answer at all.
//! A `slow_down` (429) makes the asks further apart. Nothing is ever asked after the code's time
//! is up, and the time is never longer than [`MAX_DEADLINE`] whatever the site says.
//!
//! The device code and the token are secrets: they live only in memory ([`Secret`], wiped when
//! dropped), are never part of an error, a log line or a `Debug` print, and never reach the
//! settings. The short code (`K7QM-2HXD`) is shown to the person and is never put into the
//! address of the page that is opened.
//!
//! The page the code is typed into may be on another host than the site (the dashboard's own
//! address). It is accepted only from the site's own answer and only as a plain web address
//! ([`verify_page`]); nothing is ever sent to it by Mythicshot.

use std::fmt;
use std::time::Duration;

use serde_json::{Value, json};

use super::{Client, Method, Reply, Timeouts, Token, share};

pub const START_PATH: &str = "/api/v1/connect/start";
pub const TOKEN_PATH: &str = "/api/v1/connect/token";
/// The asks are never closer together than this, nor further apart than [`MAX_INTERVAL`],
/// whatever the site says.
pub const MIN_INTERVAL: Duration = Duration::from_secs(2);
pub const MAX_INTERVAL: Duration = Duration::from_secs(30);
/// The longest a code is waited for, whatever the site says.
pub const MAX_DEADLINE: Duration = Duration::from_secs(15 * 60);
/// The longest name the site takes for a computer, in characters.
pub const DEVICE_NAME_MAX: usize = 60;
/// The name sent when the computer's own cannot be read.
pub const FALLBACK_DEVICE_NAME: &str = "This computer";
/// How often cancel is looked at while waiting.
const WAIT_STEP: Duration = Duration::from_millis(100);
/// The longest device code taken from the site.
const MAX_DEVICE_CODE: usize = 512;
/// The longest short code taken from the site.
const MAX_USER_CODE: usize = 16;

/// The sentence for a site that has no Sign in (an older site: 404 or 405).
pub const NOT_OFFERED: &str = "This site does not offer Sign in yet. Paste a token instead.";
/// The heading and the sentence for a code that ran out or was turned down.
pub const NOT_CONNECTED_HEADING: &str = "This computer was not connected";
pub const NOT_CONNECTED_TEXT: &str = "The code ran out or was turned down in the browser. Nothing was changed.";
/// What every other failure starts with.
const WHAT: &str = "Could not sign in";

/// A secret from the site (the device code, the token): its `Debug` hides it, and its bytes are
/// overwritten when it is dropped. It cannot be cloned.
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(hidden)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        crate::publish_ui::wipe(&mut self.0);
    }
}

/// The site gave a code: what the window shows, and what [`wait`] needs.
#[derive(Debug)]
pub struct Started {
    /// The configured site, as the address rule writes it: the token belongs to it.
    pub server: String,
    /// The short code the person types in the browser.
    pub user_code: String,
    /// The page to open, when the site named one Mythicshot can open ([`verify_page`]).
    pub page: Option<String>,
    /// How long the code is waited for: the site's `expires_in`, at most [`MAX_DEADLINE`].
    pub expires_in: Duration,
    /// Time between asks, within [`MIN_INTERVAL`] and [`MAX_INTERVAL`].
    pub interval: Duration,
    device_code: Secret,
}

impl Started {
    /// "10 minutes", "1 minute": how long the code works, in whole minutes rounded down. None
    /// for less than a minute.
    pub fn minutes(&self) -> Option<String> {
        minutes_text(self.expires_in)
    }
}

/// "10 minutes", "1 minute", or None under a minute.
pub fn minutes_text(d: Duration) -> Option<String> {
    match d.as_secs() / 60 {
        0 => None,
        1 => Some("1 minute".to_string()),
        n => Some(format!("{n} minutes")),
    }
}

/// The code was confirmed: the token. (The workspace's name in the same answer is not kept: the
/// Workspace page asks the site for it with the new token, like for a pasted one.)
#[derive(Debug)]
pub struct Connected {
    pub token: Secret,
}

/// Why this computer was not connected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectError {
    /// The site has no Sign in (404 or 405): [`NOT_OFFERED`].
    NotOffered,
    /// The code ran out, was turned down, or its time is up: [`NOT_CONNECTED_TEXT`].
    NotConnected,
    /// The site asked to wait (429 when a code is asked for): the seconds, when it said.
    Busy(Option<u32>),
    Cancelled,
    /// Anything else, as a sentence of Mythicshot's: never the site's text, never a secret.
    Failed(String),
}

impl ConnectError {
    /// What the window says: a heading and a sentence. None for a cancel, which says nothing.
    pub fn text(&self) -> Option<(&'static str, String)> {
        let text = match self {
            ConnectError::Cancelled => return None,
            ConnectError::NotOffered => NOT_OFFERED.to_string(),
            ConnectError::NotConnected => NOT_CONNECTED_TEXT.to_string(),
            ConnectError::Busy(Some(s)) => format!("{WHAT}: the site asks to wait {} seconds. Sign in again later.", (*s).min(3600)),
            ConnectError::Busy(None) => format!("{WHAT}: the site is busy. Sign in again later."),
            ConnectError::Failed(text) => text.clone(),
        };
        Some((NOT_CONNECTED_HEADING, text))
    }

    /// Whether "Try again" can help: not for a site that has no Sign in.
    pub fn can_try_again(&self) -> bool {
        !matches!(self, ConnectError::NotOffered | ConnectError::Cancelled)
    }
}

/// The time [`wait`] goes by, and how it waits: the real clock, or a fake one in tests.
pub trait Clock {
    /// Time since some fixed moment.
    fn now(&self) -> Duration;
    fn sleep(&self, d: Duration);
}

/// The real clock, from the moment it is made.
pub struct RealClock(std::time::Instant);

impl RealClock {
    pub fn new() -> RealClock {
        RealClock(std::time::Instant::now())
    }
}

impl Default for RealClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for RealClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// The name this computer is shown with on the site's Connections page: the Windows computer
/// name ([`clean_device_name`]), or [`FALLBACK_DEVICE_NAME`] when it cannot be read.
pub fn device_name() -> String {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| clean_device_name(&computer_name().unwrap_or_default())).clone()
}

/// `raw` without control and format characters (bidi marks, zero-width marks), trimmed, at most
/// [`DEVICE_NAME_MAX`] characters; [`FALLBACK_DEVICE_NAME`] when nothing is left.
pub fn clean_device_name(raw: &str) -> String {
    let plain: String = raw.chars().filter(|&c| !c.is_control() && !super::is_invisible(c)).collect();
    let cut: String = plain.trim().chars().take(DEVICE_NAME_MAX).collect();
    let cut = cut.trim_end();
    if cut.is_empty() { FALLBACK_DEVICE_NAME.to_string() } else { cut.to_string() }
}

#[cfg(windows)]
fn computer_name() -> Option<String> {
    use windows::Win32::System::WindowsProgramming::GetComputerNameW;
    use windows::core::PWSTR;
    // MAX_COMPUTERNAME_LENGTH is 15; a little more room costs nothing.
    let mut buf = [0u16; 64];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` is valid for `len` UTF-16 units, and `len` is updated to what was written.
    unsafe { GetComputerNameW(Some(PWSTR(buf.as_mut_ptr())), &mut len) }.ok()?;
    let len = (len as usize).min(buf.len());
    String::from_utf16(&buf[..len]).ok()
}

#[cfg(not(windows))]
fn computer_name() -> Option<String> {
    std::env::var("HOSTNAME").ok()
}

/// The page the code is typed into, from the site's `verify_url`: a path (`/connections`) is put
/// after the configured site's address; a full address may be on any host (the dashboard's), as
/// strictly as a shared item's link ([`share::item_link`]: https, plain address characters, no
/// user name, query or fragment, bounded length). A page whose address holds the code itself is
/// refused: the code is only ever typed by the person. None for anything else.
pub fn verify_page(server: &str, verify_url: &str, user_code: &str) -> Option<String> {
    let page = share::item_link(server, verify_url)?;
    let lower = page.to_ascii_lowercase();
    let code = user_code.to_ascii_lowercase();
    let bare: String = code.chars().filter(|c| *c != '-').collect();
    let holds_code = (!code.is_empty() && lower.contains(&code)) || (!bare.is_empty() && lower.contains(&bare));
    (!holds_code).then_some(page)
}

/// The page's address as the window writes it: without `https://` (`app.example.com/connections`).
pub fn shown_address(page: &str) -> String {
    page.strip_prefix("https://").or_else(|| page.strip_prefix("http://")).unwrap_or(page).to_string()
}

/// Ask `server` for a code for this computer, named `device_name`.
pub fn start(server: &str, device_name: &str) -> Result<Started, ConnectError> {
    let client = Client::anonymous(server, Timeouts::default()).map_err(ConnectError::Failed)?;
    start_with(&client, device_name)
}

/// Ask the site whether the code of `started` was confirmed until it says, the time is up or
/// `cancelled` says so, going by `clock` ([`RealClock`] outside tests). Blocks for as long as
/// that takes: run it off the UI thread.
pub fn wait(started: &Started, cancelled: &dyn Fn() -> bool, clock: &dyn Clock) -> Result<Connected, ConnectError> {
    let client = Client::anonymous(&started.server, Timeouts::default()).map_err(ConnectError::Failed)?;
    wait_with(&client, started, cancelled, clock)
}

/// [`start`] on a client.
fn start_with(client: &Client, device_name: &str) -> Result<Started, ConnectError> {
    let name = clean_device_name(device_name);
    let body = serde_json::to_vec(&json!({ "device_name": name })).map_err(|_| ConnectError::Failed(not_understood()))?;
    let reply = client.send(Method::Post, START_PATH, Some(("application/json", &body))).map_err(|t| ConnectError::Failed(format!("{WHAT}: {}.", t.0)))?;
    match reply.status {
        200..=299 => {}
        404 | 405 => return Err(ConnectError::NotOffered),
        429 => return Err(ConnectError::Busy(reply.retry_after)),
        s => return Err(ConnectError::Failed(status_text(s))),
    }
    let v = reply.json().ok_or_else(|| ConnectError::Failed(not_understood()))?;
    let text = |key: &str| v.get(key).and_then(Value::as_str);
    let device_code = text("device_code").filter(|c| is_device_code(c)).ok_or_else(|| ConnectError::Failed(not_understood()))?;
    let user_code = text("user_code").filter(|c| is_user_code(c)).ok_or_else(|| ConnectError::Failed(not_understood()))?.to_string();
    let seconds = |key: &str| v.get(key).and_then(Value::as_u64).filter(|s| *s > 0);
    let expires_in = Duration::from_secs(seconds("expires_in").ok_or_else(|| ConnectError::Failed(not_understood()))?.min(MAX_DEADLINE.as_secs()));
    // A site that names no interval gets the slowest one the contract allows for, not the fastest.
    let interval = Duration::from_secs(seconds("interval").unwrap_or(5).min(MAX_INTERVAL.as_secs())).max(MIN_INTERVAL);
    let page = text("verify_url").and_then(|u| verify_page(&client.base, u, &user_code));
    Ok(Started { server: client.base.clone(), user_code, page, expires_in, interval, device_code: Secret(device_code.to_string()) })
}

/// [`wait`] on a client and a clock.
fn wait_with(client: &Client, started: &Started, cancelled: &dyn Fn() -> bool, clock: &dyn Clock) -> Result<Connected, ConnectError> {
    let deadline = clock.now() + started.expires_in;
    let mut interval = started.interval;
    let mut next = interval;
    let body = serde_json::to_vec(&json!({ "device_code": started.device_code.expose() })).map_err(|_| ConnectError::Failed(not_understood()))?;
    // The body holds the device code: it is overwritten when this function ends, however it ends.
    let body = Secret(String::from_utf8(body).map_err(|_| ConnectError::Failed(not_understood()))?);
    loop {
        // Wait, looking at cancel every tenth of a second, and never past the deadline.
        let until = (clock.now() + next).min(deadline);
        loop {
            if cancelled() {
                return Err(ConnectError::Cancelled);
            }
            let now = clock.now();
            if now >= until {
                break;
            }
            clock.sleep((until - now).min(WAIT_STEP));
        }
        if clock.now() >= deadline {
            return Err(ConnectError::NotConnected);
        }
        let mut reply = ask(client, body.expose().as_bytes())?;
        next = interval;
        match reply.status {
            200 => {
                let v = reply.json().ok_or_else(|| ConnectError::Failed(not_understood()))?;
                match v.get("status").and_then(Value::as_str) {
                    Some("pending") => {}
                    Some("connected") => {
                        // A token that is not one is never stored, and never said.
                        let token = v.get("token").and_then(Value::as_str).and_then(|t| Token::parse(t).ok()).map(|t| Secret(t.0));
                        // The answer held the token: its bytes are overwritten now.
                        for b in reply.body.iter_mut() {
                            // SAFETY: `b` is a valid, aligned, exclusive reference into the body.
                            unsafe { std::ptr::write_volatile(b, 0) };
                        }
                        let token = token.ok_or_else(|| ConnectError::Failed(not_understood()))?;
                        return Ok(Connected { token });
                    }
                    _ => return Err(ConnectError::Failed(not_understood())),
                }
            }
            410 => return Err(ConnectError::NotConnected),
            // Asked too fast: one more interval from now on, and at least the wait the site asks for.
            429 => {
                interval = (interval + started.interval).min(MAX_INTERVAL);
                let asked = reply.retry_after.map(|s| Duration::from_secs(u64::from(s)).min(MAX_INTERVAL)).unwrap_or(Duration::ZERO);
                next = interval.max(asked);
            }
            404 | 405 => return Err(ConnectError::NotOffered),
            s => return Err(ConnectError::Failed(status_text(s))),
        }
    }
}

/// One code check, sent once more when the first got no answer at all.
fn ask(client: &Client, body: &[u8]) -> Result<Reply, ConnectError> {
    let send = || client.send(Method::Post, TOKEN_PATH, Some(("application/json", body)));
    send().or_else(|_| send()).map_err(|t| ConnectError::Failed(format!("{WHAT}: {}.", t.0)))
}

/// A status the routine has no answer for, in Mythicshot's words; never the site's sentence.
fn status_text(status: u16) -> String {
    match status {
        300..=399 => format!("{WHAT}: the server answered with a redirect (HTTP {status}), which is not followed. Check the server address."),
        s => format!("{WHAT}: the server answered HTTP {s}."),
    }
}

fn not_understood() -> String {
    super::not_understood(WHAT)
}

/// A device code as the site gives it: printable ASCII without spaces, at most 512 characters.
fn is_device_code(code: &str) -> bool {
    (1..=MAX_DEVICE_CODE).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_graphic())
}

/// A short code: capital letters, digits and hyphens (`K7QM-2HXD`), at most 16 characters.
fn is_user_code(code: &str) -> bool {
    (1..=MAX_USER_CODE).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-') && code.bytes().any(|b| b != b'-')
}

#[cfg(test)]
mod tests;
