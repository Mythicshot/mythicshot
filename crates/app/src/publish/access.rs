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
/// The most addresses one item or tour may have.
pub const MAX_VIEWERS: usize = 200;

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
    /// body must stay under (pinned by the tests).
    #[cfg_attr(not(test), allow(dead_code))]
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
pub fn body_bytes(mut body: Value, access: &Access, route: Route) -> Vec<u8> {
    if access.is_public() {
        return serde_json::to_vec(&body).unwrap_or_default();
    }
    add_to(&mut body, access);
    body["pad"] = json!("");
    let bare = serde_json::to_vec(&body).unwrap_or_default().len();
    let want = route.old_limit() + PAD_ABOVE;
    body["pad"] = json!(" ".repeat(want.saturating_sub(bare)));
    serde_json::to_vec(&body).unwrap_or_default()
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
}

/// The refusal an answer to a request that carried the fields means, when it is one of who
/// can view's; None for any other answer, which the caller handles as before.
pub fn refused(status: u16, answer: Option<&Value>) -> Option<Refused> {
    let code = answer.and_then(|a| a.get("error")).and_then(Value::as_str);
    // A body over an older site's limit. A newer site's own 413s (an item over its size limit,
    // a workspace without room) keep their meaning.
    if status == 413 && !matches!(code, Some("media_too_large" | "storage_full")) {
        return Some(Refused::SiteTooOld);
    }
    let (answer, code) = (answer?, code?);
    Some(match (status, code) {
        (400, "bad_access") => Refused::BadAccess,
        (400, "bad_viewer") => Refused::BadViewer(answer.get("detail").and_then(Value::as_str).and_then(position)),
        (400, "too_many_viewers") => Refused::TooManyViewers,
        (400, "bad_request") => Refused::BadRequest,
        (409, "cannot_restrict") => Refused::CannotRestrict,
        (409, "too_large_to_restrict") => Refused::TooLargeToRestrict,
        (409, "already_published") => Refused::AlreadyPublished,
        (409, "access_already_set") => Refused::AccessAlreadySet,
        _ => return None,
    })
}

/// The position a `bad_viewer` detail names: its first number, when it is one of the positions
/// a list can have (1 to [`MAX_VIEWERS`]). Nothing else of the detail is ever used.
fn position(detail: &str) -> Option<u32> {
    let digits: String = detail.chars().skip_while(|c| !c.is_ascii_digit()).take_while(char::is_ascii_digit).collect();
    digits.parse::<u32>().ok().filter(|n| (1..=MAX_VIEWERS as u32).contains(n))
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
        assert_eq!(r(400, json!({ "error": "bad_viewer", "detail": "viewers[3] is not an email address" })), Some(Refused::BadViewer(Some(3))));
        assert_eq!(r(400, json!({ "error": "bad_viewer", "detail": "entry 0" })), Some(Refused::BadViewer(None)));
        assert_eq!(r(400, json!({ "error": "bad_viewer", "detail": "entry 9999" })), Some(Refused::BadViewer(None)));
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
                let bytes = body_bytes(body.clone(), &access, route);
                assert!(bytes.len() >= route.old_limit() + PAD_ABOVE, "{route:?} {}", bytes.len());
                assert!(bytes.len() < route.new_limit(), "{route:?} {}", bytes.len());
                let v: Value = serde_json::from_slice(&bytes).unwrap();
                assert!(v["pad"].as_str().unwrap().bytes().all(|b| b == b' '));
                assert!(applied(&v, &access));
            }
            // A small restricted body is padded to exactly the target.
            assert_eq!(body_bytes(body.clone(), &Access::Me, route).len(), route.old_limit() + PAD_ABOVE);
            // Public: the bytes as before, no pad.
            assert_eq!(body_bytes(body.clone(), &Access::Public, route), serde_json::to_vec(&body).unwrap());
        }
        assert_eq!((Route::Media.old_limit(), Route::Media.new_limit(), Route::Publish.old_limit(), Route::Publish.new_limit()), (4_096, 69_632, 1_024, 66_560));
    }

    #[test]
    fn a_413_to_a_padded_request_means_the_site_is_too_old() {
        assert_eq!(refused(413, None), Some(Refused::SiteTooOld));
        assert_eq!(refused(413, Some(&json!({ "error": "payload_too_large" }))), Some(Refused::SiteTooOld));
        assert_eq!(refused(413, Some(&json!({ "error": "media_too_large" }))), None);
        assert_eq!(refused(413, Some(&json!({ "error": "storage_full" }))), None);
    }

    #[test]
    fn debug_never_shows_an_address() {
        let a = people(&["someone@example.com"]);
        let text = format!("{a:?} {:?}", Viewers::new(vec!["someone@example.com".into()]));
        assert!(!text.contains("someone") && !text.contains('@'), "{text}");
        assert!(text.contains("1 addresses"), "{text}");
    }
}
