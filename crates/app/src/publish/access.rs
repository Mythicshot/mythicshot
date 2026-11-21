//! Who can view a shared item or a tour, as the site's API says it (platform 0.1.51).
//!
//! Everything that touches the wire for "Who can view" is here and only here: the version that
//! knows the fields, how they are written into a request, how an answer says they were applied,
//! and the error codes a refusal comes with. Should the contract change, this file changes.
//!
//! The rules that keep a restricted item from going online as public:
//! - Nothing is sent for [`Access::Public`]: requests stay as they were before this existed.
//! - A site older than [`SINCE`] ignores the fields and would make the item public, so the
//!   windows offer a restriction only to a site that says it is new enough ([`supported`]).
//! - Every request that carries a restriction is padded past the body limit of a site up to
//!   0.1.50 ([`body_bytes`]), so such a site refuses it with 413 before it does anything. This is
//!   the main guard: for a small tour an older site would publish on its very first answer.
//! - When a restriction was sent, the answer must say the same back ([`applied`]); anything
//!   else means the site did not apply it.
//!
//! The addresses are a list of people's email addresses: they are never printed by `Debug`,
//! and their memory is overwritten when they go.

use std::fmt;

use serde_json::{Value, json};

/// The first site version that takes `access` and `viewers`.
pub const SINCE: (u32, u32, u32) = (0, 1, 51);
/// The most addresses one item or tour may have, once repeats are left out.
pub const MAX_VIEWERS: usize = 200;
/// The most entries a list may have before repeats are left out: the site refuses more than
/// this many raw `viewers` entries as `too_many_viewers` before it checks any of them.
pub const MAX_ENTRIES: usize = 1_000;

/// Whether a site of this version takes who can view; None (a site that tells none) is a no.
pub fn supported(version: Option<&str>) -> bool {
    super::version_at_least(version, SINCE)
}

/// The email addresses of "Only people I choose", as typed (trimmed, one per entry). Never
/// shown by `Debug`; overwritten when dropped.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Viewers(Vec<String>);

impl Viewers {
    pub fn new(addresses: Vec<String>) -> Viewers {
        Viewers(addresses)
    }
}

impl fmt::Debug for Viewers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Viewers({} addresses)", self.0.len())
    }
}

impl Drop for Viewers {
    fn drop(&mut self) {
        for a in &mut self.0 {
            crate::publish_ui::wipe(a);
        }
    }
}

/// Who can view what is created. The default is anyone with the link.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Access {
    #[default]
    Public,
    People(Viewers),
    Me,
}

impl Access {
    /// The value of `access` on the wire; None for public, which is never sent.
    fn wire(&self) -> Option<&'static str> {
        match self {
            Access::Public => None,
            Access::People(_) => Some("people"),
            Access::Me => Some("me"),
        }
    }

    pub fn is_public(&self) -> bool {
        matches!(self, Access::Public)
    }
}

/// What the site says it applied: only the people chosen (how many, after it trimmed,
/// lowercased and removed repeats), or only the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granted {
    People(u64),
    Me,
}

/// Write `access` into a create (`POST /api/v1/media`) or the call that starts a tour's publish.
/// Public writes nothing; `viewers` goes only with "people".
pub fn add_to(body: &mut Value, access: &Access) {
    let Some(name) = access.wire() else { return };
    body["access"] = json!(name);
    if let Access::People(v) = access {
        body["viewers"] = json!(v.0);
    }
}

/// Which request a body is for: the limits of the body differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// `POST /api/v1/media`.
    Media,
    /// The first `POST /api/v1/tours/:id/publish`.
    Publish,
}

impl Route {
    /// The largest body a site up to 0.1.50 takes: anything larger it refuses with 413, before
    /// it creates or publishes anything.
    pub fn old_limit(self) -> usize {
        match self {
            Route::Media => 4_096,
            Route::Publish => 1_024,
        }
    }

    /// The largest body a site of 0.1.51 or newer takes when `access` is in it: what a padded
    /// body must stay under.
    pub fn new_limit(self) -> usize {
        match self {
            Route::Media => 69_632,
            Route::Publish => 66_560,
        }
    }
}

/// How far above [`Route::old_limit`] a body with `access` is made.
pub const PAD_ABOVE: usize = 512;

/// The bytes of a request: `body` with `access` written in ([`add_to`]). With a restriction the
/// body also gets a `pad` of spaces, so that it is at least [`PAD_ABOVE`] bytes larger than an
/// older site takes: such a site answers 413 and creates or publishes nothing, rather than
/// ignoring `access` and making it public. A newer site ignores `pad`. Without a restriction the
/// bytes are exactly what they were before `access` existed.
///
/// Nothing is answered that would not do: a body that could not be written, and one that would
/// reach the route's new limit (the site would refuse it with a 413 that reads like an older
/// site's), are errors, and nothing is sent.
pub fn body_bytes(mut body: Value, access: &Access, route: Route) -> Result<Vec<u8>, BodyError> {
    let write = |body: &Value| serde_json::to_vec(body).ok().filter(|b| !b.is_empty()).ok_or(BodyError::Unreadable);
    if access.is_public() {
        return write(&body);
    }
    add_to(&mut body, access);
    body["pad"] = json!("");
    let bare = write(&body)?.len();
    let want = route.old_limit() + PAD_ABOVE;
    body["pad"] = json!(" ".repeat(want.saturating_sub(bare)));
    let bytes = write(&body)?;
    if bytes.len() >= route.new_limit() {
        return Err(BodyError::TooLong);
    }
    Ok(bytes)
}

/// Why a body is not sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyError {
    /// It would reach the route's limit: the list of addresses is too long.
    TooLong,
    /// It could not be written.
    Unreadable,
}

/// Room kept in a body for everything but the addresses (an item's title, the pad's quotes).
const ROOM_FOR_THE_REST: usize = 2_048;

/// Whether a list of addresses, as JSON writes it, leaves a body well under both routes' limits.
pub fn fits(viewers: &[String]) -> bool {
    let smallest = Route::Media.new_limit().min(Route::Publish.new_limit());
    serde_json::to_vec(viewers).is_ok_and(|v| v.len() + ROOM_FOR_THE_REST < smallest)
}

/// What an answer says was applied: None when it names no restriction (a public item, or an
/// answer without the key).
pub fn granted(answer: &Value) -> Option<Granted> {
    match answer.get("access").and_then(Value::as_str)? {
        "people" => Some(Granted::People(answer.get("viewers").and_then(Value::as_u64).unwrap_or(0))),
        "me" => Some(Granted::Me),
        _ => None,
    }
}

/// Whether the answer to a request that sent `access` says the site applied it: the same
/// `access` back. Always true for public, which sends nothing.
pub fn applied(answer: &Value, sent: &Access) -> bool {
    match sent.wire() {
        None => true,
        Some(name) => answer.get("access").and_then(Value::as_str) == Some(name),
    }
}

/// Why who can view was refused, or not applied. Nothing was created or published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// 400 `bad_access`.
    BadAccess,
    /// 400 `bad_viewer`: the position of the entry, from the site's detail when it can be read.
    BadViewer(Option<u32>),
    /// 400 `too_many_viewers`.
    TooManyViewers,
    /// 400 `bad_request` to a request that carried the fields.
    BadRequest,
    /// 409 `cannot_restrict`: the site cannot send mail.
    CannotRestrict,
    /// 409 `too_large_to_restrict`.
    TooLargeToRestrict,
    /// 409 `already_published`: the tour was published before, even if unpublished since.
    AlreadyPublished,
    /// 409 `access_already_set`: set in the dashboard, and not what was sent.
    AccessAlreadySet,
    /// The site answered yes but did not say back what was sent.
    NotApplied,
    /// 413 to a padded request: a site older than 0.1.51, which refused the body before doing anything.
    SiteTooOld,
    /// Not sent: the body would be over the route's limit.
    ListTooLong,
}

/// The refusal an answer to a request that carried the fields means, when it is one of who
/// can view's; None for any other answer, which the caller handles as before.
pub fn refused(status: u16, answer: Option<&Value>) -> Option<Refused> {
    let answer = answer?;
    let code = answer.get("error").and_then(Value::as_str)?;
    let detail = answer.get("detail").and_then(Value::as_str);
    // A body over the limit, which a site up to 0.1.50 says of a padded body. Every other 413
    // (a tour too large, an item over its size limit, a workspace without room) keeps the
    // meaning the client gives it without a restriction.
    if status == 413 {
        return (code == "too_large" && detail.is_some_and(over_the_limit)).then_some(Refused::SiteTooOld);
    }
    Some(match (status, code) {
        (400, "bad_access") => Refused::BadAccess,
        (400, "bad_viewer") => Refused::BadViewer(detail.and_then(position)),
        (400, "too_many_viewers") => Refused::TooManyViewers,
        (400, "bad_request") => Refused::BadRequest,
        (409, "cannot_restrict") => Refused::CannotRestrict,
        (409, "too_large_to_restrict") => Refused::TooLargeToRestrict,
        (409, "already_published") => Refused::AlreadyPublished,
        (409, "access_already_set") => Refused::AccessAlreadySet,
        _ => return None,
    })
}

/// Whether a 413's detail is exactly "the request is over <digits> bytes".
fn over_the_limit(detail: &str) -> bool {
    detail.strip_prefix("the request is over ").and_then(|d| d.strip_suffix(" bytes")).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The position (from 1) a `bad_viewer` detail names. The site writes exactly
/// "viewers[<i>] is not an email address; nothing was changed", `<i>` from 0 in the list as
/// sent; only a plain number from 0 to 999 in exactly that text is read, and nothing else of
/// the detail is ever used, shown or logged.
fn position(detail: &str) -> Option<u32> {
    let i = detail.strip_prefix("viewers[")?.strip_suffix("] is not an email address; nothing was changed")?;
    if i.is_empty() || i.len() > 3 || !i.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    i.parse::<u32>().ok().filter(|n| *n < MAX_ENTRIES as u32).map(|n| n + 1)
}

/// Overwrite a request body that held addresses, once it is sent.
pub fn wipe_body(body: &mut Vec<u8>) {
    for b in body.iter_mut() {
        // SAFETY: `b` is a valid, aligned, exclusive reference into `body`.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    body.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn people(list: &[&str]) -> Access {
        Access::People(Viewers::new(list.iter().map(|s| s.to_string()).collect()))
    }

    #[test]
    fn only_a_site_of_0_1_51_or_newer_takes_the_fields() {
        for (v, yes) in [(Some("0.1.50"), false), (Some("0.1.51"), true), (Some("0.1.52"), true), (Some("0.2.0"), true), (Some("1.0.0"), true), (None, false), (Some("0.1"), false), (Some("abc"), false), (Some("0.1.51-beta"), false)] {
            assert_eq!(supported(v), yes, "{v:?}");
        }
    }

    #[test]
    fn public_writes_nothing_and_viewers_go_only_with_people() {
        let base = json!({ "kind": "image" });
        let mut b = base.clone();
        add_to(&mut b, &Access::Public);
        assert_eq!(b, base);
        let mut b = base.clone();
        add_to(&mut b, &Access::Me);
        assert_eq!(b, json!({ "kind": "image", "access": "me" }));
        let mut b = base.clone();
        add_to(&mut b, &people(&["a@example.com", "b@example.com"]));
        assert_eq!(b, json!({ "kind": "image", "access": "people", "viewers": ["a@example.com", "b@example.com"] }));
    }

    #[test]
    fn an_answer_is_applied_only_when_it_says_the_same_back() {
        let me = Access::Me;
        assert!(applied(&json!({ "access": "me" }), &me));
        assert!(!applied(&json!({}), &me));
        assert!(!applied(&json!({ "access": "public" }), &me));
        assert!(!applied(&json!({ "access": "people", "viewers": 1 }), &me));
        assert!(applied(&json!({ "access": "people", "viewers": 2 }), &people(&["a@example.com"])));
        assert!(applied(&json!({}), &Access::Public));
        assert_eq!(granted(&json!({ "access": "people", "viewers": 2 })), Some(Granted::People(2)));
        assert_eq!(granted(&json!({ "access": "me", "viewers": 0 })), Some(Granted::Me));
        assert_eq!(granted(&json!({ "access": "public" })), None);
        assert_eq!(granted(&json!({})), None);
    }

    #[test]
    fn every_refusal_code_is_known_by_its_status_and_name() {
        let r = |status, body: Value| refused(status, Some(&body));
        assert_eq!(r(400, json!({ "error": "bad_access" })), Some(Refused::BadAccess));
        assert_eq!(r(400, json!({ "error": "bad_viewer", "detail": "viewers[2] is not an email address; nothing was changed" })), Some(Refused::BadViewer(Some(3))));
        assert_eq!(r(400, json!({ "error": "bad_viewer", "detail": "viewers[0] is not an email address; nothing was changed" })), Some(Refused::BadViewer(Some(1))));
        assert_eq!(r(400, json!({ "error": "bad_viewer", "detail": "viewers[999] is not an email address; nothing was changed" })), Some(Refused::BadViewer(Some(1000))));
        for odd in ["viewers[1000] is not an email address; nothing was changed", "viewers[-1] is not an email address; nothing was changed", "viewers[03x] is not an email address; nothing was changed", "viewers[3] is not an email address", "entry 3", "viewers[] is not an email address; nothing was changed", "a viewers[3] is not an email address; nothing was changed"] {
            assert_eq!(r(400, json!({ "error": "bad_viewer", "detail": odd })), Some(Refused::BadViewer(None)), "{odd}");
        }
        assert_eq!(r(400, json!({ "error": "bad_viewer" })), Some(Refused::BadViewer(None)));
        assert_eq!(r(400, json!({ "error": "too_many_viewers" })), Some(Refused::TooManyViewers));
        assert_eq!(r(400, json!({ "error": "bad_request" })), Some(Refused::BadRequest));
        assert_eq!(r(409, json!({ "error": "cannot_restrict" })), Some(Refused::CannotRestrict));
        assert_eq!(r(409, json!({ "error": "too_large_to_restrict" })), Some(Refused::TooLargeToRestrict));
        assert_eq!(r(409, json!({ "error": "already_published" })), Some(Refused::AlreadyPublished));
        assert_eq!(r(409, json!({ "error": "access_already_set" })), Some(Refused::AccessAlreadySet));
        // Another status, another code, or no JSON: not who can view's, handled as before.
        assert_eq!(r(409, json!({ "error": "bad_access" })), None);
        assert_eq!(refused(400, None), None);
    }

    /// 200 addresses as long as an address can be (254 characters).
    fn longest_list() -> Access {
        people(&(0..200).map(|i| format!("{i:03}{}@{}.example", "a".repeat(60), "b".repeat(180))).collect::<Vec<_>>().iter().map(String::as_str).collect::<Vec<_>>())
    }

    #[test]
    fn a_restricted_body_is_too_large_for_an_older_site_and_small_enough_for_a_newer_one() {
        let create = json!({ "kind": "image", "content_type": "image/png", "bytes": 1000, "width": 1920, "height": 1080 });
        for (route, body) in [(Route::Media, create.clone()), (Route::Publish, json!({}))] {
            for access in [Access::Me, people(&["a@example.com"]), longest_list()] {
                let bytes = body_bytes(body.clone(), &access, route).unwrap();
                assert!(bytes.len() >= route.old_limit() + PAD_ABOVE, "{route:?} {}", bytes.len());
                assert!(bytes.len() < route.new_limit(), "{route:?} {}", bytes.len());
                let v: Value = serde_json::from_slice(&bytes).unwrap();
                assert!(v["pad"].as_str().unwrap().bytes().all(|b| b == b' '));
                assert!(applied(&v, &access));
            }
            // A small restricted body is padded to exactly the target.
            assert_eq!(body_bytes(body.clone(), &Access::Me, route).unwrap().len(), route.old_limit() + PAD_ABOVE);
            // Public: the bytes as before, no pad.
            assert_eq!(body_bytes(body.clone(), &Access::Public, route), Ok(serde_json::to_vec(&body).unwrap()));
        }
        assert_eq!((Route::Media.old_limit(), Route::Media.new_limit(), Route::Publish.old_limit(), Route::Publish.new_limit()), (4_096, 69_632, 1_024, 66_560));
    }

    #[test]
    fn only_a_413_about_the_requests_size_means_the_site_is_too_old() {
        let r = |body: Value| refused(413, Some(&body));
        assert_eq!(r(json!({ "error": "too_large", "detail": "the request is over 1024 bytes" })), Some(Refused::SiteTooOld));
        assert_eq!(r(json!({ "error": "too_large", "detail": "the request is over 4096 bytes" })), Some(Refused::SiteTooOld));
        // The tour's own size keeps the meaning it has today.
        assert_eq!(r(json!({ "error": "too_large", "detail": "the tour has more than 400 steps" })), None);
        assert_eq!(r(json!({ "error": "too_large", "detail": "the tour's text is over 512000 bytes" })), None);
        assert_eq!(r(json!({ "error": "too_large", "detail": "the request is over bytes" })), None);
        assert_eq!(r(json!({ "error": "too_large", "detail": "the request is over 1024 bytes!" })), None);
        assert_eq!(r(json!({ "error": "media_too_large", "detail": "the request is over 4096 bytes" })), None);
        assert_eq!(r(json!({ "error": "storage_full" })), None);
        assert_eq!(r(json!({ "error": "too_large" })), None);
        assert_eq!(refused(413, None), None);
    }

    #[test]
    fn a_body_that_would_reach_the_routes_limit_is_never_sent() {
        // Past what the address check allows: 300 addresses of 254 characters.
        let long: Vec<String> = (0..300).map(|i| format!("{i:03}{}@{}.example", "a".repeat(60), "b".repeat(182))).collect();
        assert!(long.iter().all(|a| a.len() == 254));
        let access = Access::People(Viewers::new(long.clone()));
        for route in [Route::Media, Route::Publish] {
            assert_eq!(body_bytes(json!({}), &access, route), Err(BodyError::TooLong), "{route:?}");
        }
        assert!(!fits(&long));
        // 200 such addresses fit; 200 that are mostly quotes, which JSON writes twice as long, do not.
        assert!(fits(&long[..200]));
        let quotes: Vec<String> = (0..200).map(|i| format!("{i:03}{}@x", "\"".repeat(249))).collect();
        assert!(!fits(&quotes));
    }

    #[test]
    fn debug_never_shows_an_address() {
        let a = people(&["someone@example.com"]);
        let text = format!("{a:?} {:?}", Viewers::new(vec!["someone@example.com".into()]));
        assert!(!text.contains("someone") && !text.contains('@'), "{text}");
        assert!(text.contains("1 addresses"), "{text}");
    }
}
