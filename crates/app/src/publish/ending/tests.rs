//! Ending a token against a stand-in site on 127.0.0.1: every answer the call can get, a site
//! that does not answer, and a redirect, which is never followed.

use std::time::Instant;

use super::*;
use crate::publish::tests::{Answer, TOKEN, reply, stub};

const LINE: &str = "DELETE /api/v1/token";

#[test]
fn the_call_is_one_delete_with_the_token_and_no_body() {
    let s = stub(|_, _| reply(204, ""));
    assert_eq!(end_token(&s.base, TOKEN), Outcome::Ended);
    let reqs = s.requests();
    assert_eq!(reqs.len(), 1, "one call, no retry");
    assert_eq!(reqs[0].line, LINE);
    assert_eq!(reqs[0].auth.as_deref(), Some(format!("Bearer {TOKEN}").as_str()));
    assert!(reqs[0].body.is_empty() && reqs[0].content_type.is_none());
}

#[test]
fn a_site_under_a_path_is_asked_under_that_path() {
    let s = stub(|_, _| reply(204, ""));
    assert_eq!(end_token(&format!("{}/demo/", s.base), TOKEN), Outcome::Ended);
    assert_eq!(s.lines(), ["DELETE /demo/api/v1/token"]);
}

#[test]
fn each_answer_means_what_the_contract_says() {
    let cases: [(u16, &str, Outcome); 9] = [
        (204, "", Outcome::Ended),
        (401, r#"{"error":"unauthorized"}"#, Outcome::AlreadyEnded),
        (404, r#"{"error":"not_found"}"#, Outcome::SiteTooOld),
        (405, "", Outcome::SiteTooOld),
        (200, r#"{"ok":true}"#, Outcome::NotReached("the site answered HTTP 200".into())),
        (403, r#"{"error":"forbidden"}"#, Outcome::NotReached("the site answered HTTP 403".into())),
        (429, "", Outcome::NotReached("the site answered HTTP 429".into())),
        (500, "", Outcome::NotReached("the site answered HTTP 500".into())),
        (503, "", Outcome::NotReached("the site answered HTTP 503".into())),
    ];
    for (status, body, want) in cases {
        let body = body.to_string();
        let s = stub(move |_, _| reply(status, &body));
        assert_eq!(end_token(&s.base, TOKEN), want, "HTTP {status}");
        assert_eq!(s.lines(), [LINE], "HTTP {status}: one call, no retry");
    }
}

#[test]
fn a_site_that_drops_the_connection_is_not_reached() {
    let s = stub(|_, _| Answer::Drop);
    assert!(matches!(end_token(&s.base, TOKEN), Outcome::NotReached(_)));
    assert_eq!(s.lines(), [LINE], "no retry");
}

#[test]
fn nobody_listening_is_not_reached() {
    // A port that was free a moment ago: nothing answers there.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    assert!(matches!(end_token(&format!("http://127.0.0.1:{port}"), TOKEN), Outcome::NotReached(_)));
}

#[test]
fn a_site_that_does_not_answer_is_given_up_at_the_timeout() {
    assert!(END_TIMEOUT <= Duration::from_secs(5), "the call must never hold anyone up");
    let s = stub(|_, _| Answer::Hang);
    let started = Instant::now();
    let short = Timeouts { connect: Duration::from_millis(300), request: Duration::from_millis(300) };
    assert!(matches!(end_with(&s.base, TOKEN, short), Outcome::NotReached(_)));
    assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
}

#[test]
fn a_redirect_is_not_followed_and_the_token_goes_nowhere_else() {
    let other = stub(|_, _| reply(204, ""));
    for status in [301, 302, 307, 308] {
        let to = format!("{}/api/v1/token", other.base);
        let s = stub(move |_, _| Answer::Reply(status, String::new(), vec![("Location", to.clone())]));
        assert_eq!(end_token(&s.base, TOKEN), Outcome::NotReached(format!("the site answered with a redirect (HTTP {status}), which is not followed")));
        assert_eq!(s.lines(), [LINE]);
    }
    assert!(other.lines().is_empty(), "the other host was never asked");
}

#[test]
fn what_it_says_never_holds_the_token() {
    let echo = stub(|req, _| Answer::Reply(500, format!(r#"{{"error":"{}"}}"#, req.auth.clone().unwrap_or_default()), vec![]));
    let outcomes = [end_token(&echo.base, TOKEN), end_token("http://127.0.0.1:1", TOKEN), end_token(&echo.base, "dp_not_a_token")];
    for o in outcomes {
        let text = format!("{o:?}");
        assert!(!text.contains(TOKEN) && !text.contains(&TOKEN[3..15]), "{text}");
    }
}

#[test]
fn something_that_is_no_token_is_never_sent() {
    let s = stub(|_, _| reply(204, ""));
    assert!(matches!(end_token(&s.base, "dp_short"), Outcome::NotReached(_)));
    assert!(matches!(end_token("ftp://127.0.0.1:1", TOKEN), Outcome::NotReached(_)));
    assert!(s.lines().is_empty());
}

#[test]
fn a_test_build_sends_nothing_to_a_real_site() {
    let started = Instant::now();
    assert!(matches!(end_token("https://tours.example.com", TOKEN), Outcome::NotReached(_)));
    assert!(matches!(end_token("http://localhost:3000", TOKEN), Outcome::NotReached(_)));
    assert!(started.elapsed() < Duration::from_secs(1));
}
