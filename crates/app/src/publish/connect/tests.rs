//! Signing in against a stand-in site on 127.0.0.1, in the style of the share tests. The clock
//! is a fake one: no test really waits for an interval.

use super::*;
use crate::publish::tests::{Answer, Req, Stub, reply, stub};
use std::sync::{Arc, Mutex};

const START: &str = "POST /api/v1/connect/start";
const ASK: &str = "POST /api/v1/connect/token";
const DEVICE_CODE: &str = "dc_Zm9vYmFyYmF6cXV4LXNlY3JldC1kZXZpY2UtY29kZQ";
const USER_CODE: &str = "K7QM-2HXD";
/// A token in the platform's form, used by these tests only.
const NEW_TOKEN: &str = "dp_feedfacefeedfacefeedfacefeedfacefeedface";

/// A clock that only moves when it is slept on, shared with the stand-in so it can note when it was asked.
#[derive(Clone, Default)]
struct FakeClock(Arc<Mutex<Duration>>);

impl FakeClock {
    fn at(&self) -> Duration {
        *self.0.lock().unwrap()
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Duration {
        self.at()
    }
    fn sleep(&self, d: Duration) {
        *self.0.lock().unwrap() += d;
    }
}

fn started_body(expires_in: u64, interval: u64, verify_url: &str) -> String {
    json!({ "device_code": DEVICE_CODE, "user_code": USER_CODE, "verify_url": verify_url, "expires_in": expires_in, "interval": interval }).to_string()
}

fn connected_body() -> String {
    json!({ "status": "connected", "token": NEW_TOKEN, "workspace": { "name": "Cowfee", "id": "ws_1" } }).to_string()
}

const PENDING: &str = r#"{"status":"pending"}"#;

/// The stand-in site: start answers a code (600 s, every 5 s); each code check is answered by
/// `asks`, given how many came before, with the fake clock's time noted.
fn site(asks: impl Fn(usize) -> Answer + Send + 'static) -> (Stub, FakeClock, Arc<Mutex<Vec<Duration>>>) {
    let clock = FakeClock::default();
    let times: Arc<Mutex<Vec<Duration>>> = Arc::default();
    let (c, t) = (clock.clone(), times.clone());
    let s = stub(move |req: &Req, seen| match req.line.as_str() {
        START => reply(201, &started_body(600, 5, "https://app.example.com/connections")),
        ASK => {
            t.lock().unwrap().push(c.at());
            asks(seen)
        }
        _ => reply(404, r#"{"error":"not_found"}"#),
    });
    (s, clock, times)
}

fn anon(s: &Stub) -> Client {
    Client::anonymous(&s.base, Timeouts::default()).unwrap()
}

/// Start, then wait with the fake clock, never cancelled.
fn sign_in(s: &Stub, clock: &FakeClock) -> Result<Connected, ConnectError> {
    let started = start_with(&anon(s), "ALEX-DESKTOP").expect("a code");
    wait_with(&anon(s), &started, &|| false, clock)
}

fn secs(list: &[u64]) -> Vec<Duration> {
    list.iter().map(|s| Duration::from_secs(*s)).collect()
}

/// No request carries a credential, and nothing a request carries holds anything but what it should.
fn no_credential_sent(s: &Stub) {
    for r in s.requests() {
        assert_eq!(r.auth, None, "{} carried a credential", r.line);
    }
}

#[test]
fn pending_then_connected_answers_the_token_asking_every_interval_without_a_credential() {
    let (s, clock, times) = site(|seen| if seen < 2 { reply(200, PENDING) } else { reply(200, &connected_body()) });
    let started = start_with(&anon(&s), "ALEX-DESKTOP").unwrap();
    assert_eq!(started.user_code, USER_CODE);
    assert_eq!(started.page.as_deref(), Some("https://app.example.com/connections"));
    assert_eq!((started.expires_in, started.interval), (Duration::from_secs(600), Duration::from_secs(5)));
    assert_eq!(started.minutes().as_deref(), Some("10 minutes"));
    assert_eq!(started.server, s.base);
    let connected = wait_with(&anon(&s), &started, &|| false, &clock).unwrap();
    assert_eq!(connected.token.expose(), NEW_TOKEN);
    assert_eq!(connected.workspace.as_deref(), Some("Cowfee"));
    assert_eq!(*times.lock().unwrap(), secs(&[5, 10, 15]), "one ask per interval, the first after one interval");
    assert_eq!(s.lines(), [START, ASK, ASK, ASK]);
    no_credential_sent(&s);
    // What each request carried: the computer's name, then the device code and nothing else.
    let reqs = s.requests();
    let start_body: Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(start_body, json!({ "device_name": "ALEX-DESKTOP" }));
    assert_eq!(reqs[0].content_type.as_deref(), Some("application/json"));
    for r in &reqs[1..] {
        let body: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(body, json!({ "device_code": DEVICE_CODE }));
    }
}

#[test]
fn a_410_at_once_or_after_pending_is_not_connected_and_nothing_more_is_asked() {
    let (s, clock, _) = site(|_| reply(410, r#"{"error":"expired_token","detail":"The code expired."}"#));
    assert_eq!(sign_in(&s, &clock).unwrap_err(), ConnectError::NotConnected);
    assert_eq!(s.lines(), [START, ASK]);

    let (s, clock, _) = site(|seen| if seen < 3 { reply(200, PENDING) } else { reply(410, r#"{"error":"access_denied"}"#) });
    assert_eq!(sign_in(&s, &clock).unwrap_err(), ConnectError::NotConnected);
    assert_eq!(s.lines(), [START, ASK, ASK, ASK, ASK]);
    no_credential_sent(&s);
    let (heading, text) = ConnectError::NotConnected.text().unwrap();
    assert_eq!(heading, "This computer was not connected");
    assert_eq!(text, "The code ran out or was turned down in the browser. Nothing was changed.");
    assert!(ConnectError::NotConnected.can_try_again());
}

#[test]
fn the_deadline_ends_the_wait_and_nothing_is_asked_after_it() {
    let clock = FakeClock::default();
    let times: Arc<Mutex<Vec<Duration>>> = Arc::default();
    let (c, t) = (clock.clone(), times.clone());
    let s = stub(move |req: &Req, _| match req.line.as_str() {
        START => reply(201, &started_body(12, 5, "/connections")),
        _ => {
            t.lock().unwrap().push(c.at());
            reply(200, PENDING)
        }
    });
    assert_eq!(sign_in(&s, &clock).unwrap_err(), ConnectError::NotConnected);
    assert_eq!(*times.lock().unwrap(), secs(&[5, 10]));
    assert_eq!(clock.at(), Duration::from_secs(12), "it ended at the deadline, not an interval later");
    // Less than a minute is said so.
    assert_eq!(minutes_text(Duration::from_secs(12)), None);
    assert_eq!(minutes_text(Duration::from_secs(119)).as_deref(), Some("1 minute"));
    assert_eq!(minutes_text(Duration::from_secs(600)).as_deref(), Some("10 minutes"));
}

#[test]
fn the_sites_times_are_kept_within_sane_bounds() {
    for (expires_in, interval, want_expires, want_interval) in [(600, 5, 600, 5), (86_400, 1, 900, 2), (60, 0, 60, 5), (300, 3600, 300, 30)] {
        let s = stub(move |_, _| reply(201, &started_body(expires_in, interval, "/connections")));
        let started = start_with(&anon(&s), "PC").unwrap();
        assert_eq!((started.expires_in.as_secs(), started.interval.as_secs()), (want_expires, want_interval), "{expires_in} {interval}");
    }
    // A code that never expires is not a code Mythicshot waits on.
    let s = stub(|_, _| reply(201, &started_body(0, 5, "/connections")));
    assert!(matches!(start_with(&anon(&s), "PC"), Err(ConnectError::Failed(_))));
}

#[test]
fn slow_down_makes_the_asks_further_apart() {
    let (s, clock, times) = site(|seen| match seen {
        0 => reply(200, PENDING),
        1 => Answer::Reply(429, r#"{"error":"slow_down"}"#.into(), vec![("retry-after", "12".into())]),
        2 => Answer::Reply(429, r#"{"error":"slow_down"}"#.into(), vec![]),
        3 => reply(200, PENDING),
        _ => reply(200, &connected_body()),
    });
    assert_eq!(sign_in(&s, &clock).unwrap().token.expose(), NEW_TOKEN);
    // 5, 10; then the site asks for 12 s (the interval is 10 by then); then 15, 15.
    assert_eq!(*times.lock().unwrap(), secs(&[5, 10, 22, 37, 52]));
    // Never further apart than 30 seconds, whatever the site says.
    let (s, clock, times) = site(|seen| if seen < 4 { Answer::Reply(429, r#"{"error":"slow_down"}"#.into(), vec![("retry-after", "3600".into())]) } else { reply(200, &connected_body()) });
    assert!(sign_in(&s, &clock).is_ok());
    assert_eq!(*times.lock().unwrap(), secs(&[5, 35, 65, 95, 125]));
}

#[test]
fn a_lost_answer_is_asked_once_more_and_a_second_loss_ends_with_a_sentence() {
    let (s, clock, times) = site(|seen| if seen == 0 { Answer::Drop } else { reply(200, &connected_body()) });
    assert_eq!(sign_in(&s, &clock).unwrap().token.expose(), NEW_TOKEN);
    assert_eq!(s.lines(), [START, ASK, ASK]);
    assert_eq!(*times.lock().unwrap(), secs(&[5, 5]), "the resend goes at once");

    // Lost per ask, not per sign-in: a later loss gets its own resend.
    let (s, clock, _) = site(|seen| match seen {
        0 | 2 => Answer::Drop,
        1 => reply(200, PENDING),
        _ => reply(200, &connected_body()),
    });
    assert!(sign_in(&s, &clock).is_ok());
    assert_eq!(s.lines().len(), 5);

    let (s, clock, _) = site(|_| Answer::Drop);
    let ConnectError::Failed(e) = sign_in(&s, &clock).unwrap_err() else { panic!("not a failure") };
    assert!(e.starts_with("Could not sign in: ") && e.ends_with('.'), "{e}");
    assert_eq!(s.lines(), [START, ASK, ASK]);
    // A lost answer to the start is not asked again: that would be a second code.
    let s = stub(|_, _| Answer::Drop);
    assert!(matches!(start_with(&anon(&s), "PC"), Err(ConnectError::Failed(e)) if e.starts_with("Could not sign in: ")));
    assert_eq!(s.lines(), [START]);
}

#[test]
fn cancel_mid_wait_stops_asking_and_answers_cancelled() {
    let (s, clock, _) = site(|_| reply(200, PENDING));
    let started = start_with(&anon(&s), "PC").unwrap();
    let log = s.requests_handle();
    let cancelled = move || log.lock().unwrap().len() >= 3;
    assert_eq!(wait_with(&anon(&s), &started, &cancelled, &clock).unwrap_err(), ConnectError::Cancelled);
    assert_eq!(s.lines(), [START, ASK, ASK]);
    assert!(clock.at() < Duration::from_secs(11), "it stopped within a tenth of a second of the cancel");
    assert_eq!(ConnectError::Cancelled.text(), None);
    // Cancelled before the first ask: nothing is asked.
    let (s, clock, _) = site(|_| reply(200, PENDING));
    let started = start_with(&anon(&s), "PC").unwrap();
    assert_eq!(wait_with(&anon(&s), &started, &|| true, &clock).unwrap_err(), ConnectError::Cancelled);
    assert_eq!(s.lines(), [START]);
}

#[test]
fn an_older_site_without_sign_in_is_said_in_mythicshots_words() {
    for status in [404, 405] {
        let s = stub(move |_, _| reply(status, r#"{"error":"not_found","detail":"Route does not exist."}"#));
        assert_eq!(start_with(&anon(&s), "PC").unwrap_err(), ConnectError::NotOffered);
    }
    assert_eq!(ConnectError::NotOffered.text(), Some(("This computer was not connected", "This site does not offer Sign in yet. Paste a token instead.".to_string())));
    assert!(!ConnectError::NotOffered.can_try_again());
}

#[test]
fn a_busy_site_and_other_answers_are_said_without_the_sites_text() {
    let s = stub(|_, _| Answer::Reply(429, r#"{"error":"rate_limited","detail":"Calm down."}"#.into(), vec![("retry-after", "40".into())]));
    let e = start_with(&anon(&s), "PC").unwrap_err();
    assert_eq!(e, ConnectError::Busy(Some(40)));
    assert_eq!(e.text().unwrap().1, "Could not sign in: the site asks to wait 40 seconds. Sign in again later.");
    assert_eq!(ConnectError::Busy(None).text().unwrap().1, "Could not sign in: the site is busy. Sign in again later.");
    let s = stub(|_, _| reply(500, r#"{"error":"boom","detail":"Database password is hunter2."}"#));
    assert_eq!(start_with(&anon(&s), "PC").unwrap_err(), ConnectError::Failed("Could not sign in: the server answered HTTP 500.".into()));
    // A redirect is not followed.
    let s = stub(|_, _| Answer::Reply(302, String::new(), vec![("location", "https://elsewhere.example.com/connect".into())]));
    assert_eq!(start_with(&anon(&s), "PC").unwrap_err(), ConnectError::Failed("Could not sign in: the server answered with a redirect (HTTP 302), which is not followed. Check the server address.".into()));
    assert_eq!(s.lines(), [START]);
    // During the wait too.
    let (s, clock, _) = site(|_| reply(503, r#"{"detail":"maintenance"}"#));
    assert_eq!(sign_in(&s, &clock).unwrap_err(), ConnectError::Failed("Could not sign in: the server answered HTTP 503.".into()));
}

#[test]
fn an_answer_that_makes_no_sense_is_refused() {
    let not_understood = ConnectError::Failed("Could not sign in: the server's answer was not understood.".into());
    for body in [
        "not json".to_string(),
        json!({ "user_code": USER_CODE, "expires_in": 600, "interval": 5 }).to_string(),
        json!({ "device_code": DEVICE_CODE, "expires_in": 600, "interval": 5 }).to_string(),
        json!({ "device_code": DEVICE_CODE, "user_code": USER_CODE, "interval": 5 }).to_string(),
        json!({ "device_code": "has space", "user_code": USER_CODE, "expires_in": 600 }).to_string(),
        json!({ "device_code": DEVICE_CODE, "user_code": "k7qm<script>", "expires_in": 600 }).to_string(),
        json!({ "device_code": DEVICE_CODE, "user_code": "----", "expires_in": 600 }).to_string(),
        json!({ "device_code": "x".repeat(513), "user_code": USER_CODE, "expires_in": 600 }).to_string(),
    ] {
        let s = stub(move |_, _| reply(201, &body));
        assert_eq!(start_with(&anon(&s), "PC").unwrap_err(), not_understood);
    }
    // A connected answer without a token in the platform's form: refused, and the text is never repeated.
    for answer in [json!({ "status": "connected" }), json!({ "status": "connected", "token": "dp_short" }), json!({ "status": "connected", "token": "not-a-token-at-all" }), json!({ "status": "what" })] {
        let body = answer.to_string();
        let (s, clock, _) = site(move |_| reply(200, &body));
        let e = sign_in(&s, &clock).unwrap_err();
        assert_eq!(e, not_understood, "{answer}");
    }
}

#[test]
fn every_verify_url_form() {
    let server = "https://tours.example.com";
    // A path on the site, and a full address on the dashboard's own host.
    assert_eq!(verify_page(server, "/connections", USER_CODE).as_deref(), Some("https://tours.example.com/connections"));
    assert_eq!(verify_page(server, "https://app.example.com/connections", USER_CODE).as_deref(), Some("https://app.example.com/connections"));
    assert_eq!(verify_page(server, "https://app.example.com:8443/dash/connections", USER_CODE).as_deref(), Some("https://app.example.com:8443/dash/connections"));
    assert_eq!(verify_page("http://127.0.0.1:8787", "/connections", USER_CODE).as_deref(), Some("http://127.0.0.1:8787/connections"));
    for bad in [
        "",
        "connections",
        "//evil.example.com/connections",
        "http://app.example.com/connections",
        "https://user:pass@app.example.com/connections",
        "https://app.example.com/connections?next=1",
        "https://app.example.com/connections#code",
        "https://app.example.com/a b",
        "https://app.exämple.com/connections",
        "https://xn--app-.example.com/%2e%2e/connections",
        "javascript:alert(1)",
        "file:///C:/Windows/system32/calc.exe",
        "https://app.example.com/connections\u{202e}",
        &format!("https://app.example.com/{}", "a".repeat(400)),
        // The code is only ever typed by the person: a page with it in the address is refused.
        "https://app.example.com/connect/K7QM-2HXD",
        "https://app.example.com/connect/k7qm2hxd",
    ] {
        assert_eq!(verify_page(server, bad, USER_CODE), None, "{bad:?}");
    }
    // From the start answer: a page refused there leaves the window to say where to go.
    let s = stub(|_, _| reply(201, &started_body(600, 5, "ftp://app.example.com/connections")));
    assert_eq!(start_with(&anon(&s), "PC").unwrap().page, None);
    let s = stub(|_, _| reply(201, &json!({ "device_code": DEVICE_CODE, "user_code": USER_CODE, "expires_in": 600, "interval": 5 }).to_string()));
    assert_eq!(start_with(&anon(&s), "PC").unwrap().page, None);
    // The address the window writes: no scheme.
    assert_eq!(shown_address("https://app.mythicshot.example/connections"), "app.mythicshot.example/connections");
    assert_eq!(shown_address("http://127.0.0.1:8787/connections"), "127.0.0.1:8787/connections");
}

#[test]
fn neither_the_device_code_nor_the_token_is_in_any_error_or_debug_print() {
    let (s, clock, _) = site(|seen| if seen == 0 { reply(200, PENDING) } else { reply(200, &connected_body()) });
    let started = start_with(&anon(&s), "PC").unwrap();
    let debug = format!("{started:?}");
    assert!(!debug.contains(DEVICE_CODE) && debug.contains(USER_CODE), "{debug}");
    let connected = wait_with(&anon(&s), &started, &|| false, &clock).unwrap();
    let debug = format!("{connected:?} {:?}", connected.token);
    assert!(!debug.contains(NEW_TOKEN) && !debug.contains("feedface"), "{debug}");
    // The site's answers that hold either never come back as text.
    let echo = format!(r#"{{"error":"{NEW_TOKEN}","detail":"device_code {DEVICE_CODE}","token":"{NEW_TOKEN}"}}"#);
    for status in [400, 401, 403, 409, 418, 500, 302] {
        let e = echo.clone();
        let (s, clock, _) = site(move |_| reply(status, &e));
        let err = sign_in(&s, &clock).unwrap_err();
        let text = err.text().map(|(h, t)| format!("{h} {t}")).unwrap_or_default();
        let all = format!("{err:?} {text}");
        assert!(!all.contains(NEW_TOKEN) && !all.contains(DEVICE_CODE) && !all.contains("feedface"), "{status}: {all}");
    }
    let e = echo.clone();
    let s = stub(move |_, _| reply(400, &e));
    let err = start_with(&anon(&s), "PC").unwrap_err();
    assert!(!format!("{err:?}").contains(DEVICE_CODE) && !format!("{err:?}").contains(NEW_TOKEN));
}

#[test]
fn the_computers_name_is_cleaned_and_cut_to_60_characters() {
    assert_eq!(clean_device_name("ALEX-DESKTOP"), "ALEX-DESKTOP");
    assert_eq!(clean_device_name("  Alex's PC \n"), "Alex's PC");
    assert_eq!(clean_device_name("PC\u{202e}evil\u{200b}\u{0007}"), "PCevil");
    assert_eq!(clean_device_name(&"x".repeat(80)).chars().count(), 60);
    assert_eq!(clean_device_name(&format!("{}  y", "x".repeat(59))), "x".repeat(59));
    assert_eq!(clean_device_name(""), "This computer");
    assert_eq!(clean_device_name("\u{200b}\t"), "This computer");
    assert_eq!(clean_device_name("Bürorechner"), "Bürorechner");
    // What is sent is cleaned the same way.
    let s = stub(|_, _| reply(201, &started_body(600, 5, "/c")));
    start_with(&anon(&s), "  PC\u{202e}\u{0000}  ").unwrap();
    let body: Value = serde_json::from_slice(&s.requests()[0].body).unwrap();
    assert_eq!(body, json!({ "device_name": "PC" }));
    // This computer's own name reads as something, cleaned.
    let name = device_name();
    assert!(!name.is_empty() && name.chars().count() <= 60 && name == clean_device_name(&name));
}
