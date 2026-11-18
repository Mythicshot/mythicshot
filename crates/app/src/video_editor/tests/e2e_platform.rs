//! One real pass of publish, edit, update and delete against a real platform server that
//! runs on this computer. The normal suite never runs it: it needs that server.
//!
//! Start the platform locally with an empty data folder, claim its setup for a token, then:
//!
//! ```text
//! MYTHICSHOT_E2E_SERVER=http://127.0.0.1:<port> MYTHICSHOT_E2E_TOKEN=<token> \
//!   cargo test -p mythicshot one_real_pass_against_a_local_platform_server -- --ignored --nocapture
//! ```
//!
//! The jobs are the editor's own (`publish_job`, `update_job`) and the calls are the client's
//! own (`crate::publish`), with no window. Every call of the client goes through a small relay
//! on 127.0.0.1 that passes it on unchanged and notes its request line and the answer, so the
//! pass can count requests and show what the site really said. What a visitor would see is
//! asked of the site directly. The token is never printed and never part of a noted call.
//!
//! The pass leaves the site with one draft tour; give it a fresh data folder every time.

use super::*;
use crate::project::ProjectKey;
use crate::publish::{self, Deleted, RemoteTour};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::time::Duration;

const ENV_SERVER: &str = "MYTHICSHOT_E2E_SERVER";
const ENV_TOKEN: &str = "MYTHICSHOT_E2E_TOKEN";
/// Optional: the version the site must report, when it is not [`SITE_VERSION`].
const ENV_VERSION: &str = "MYTHICSHOT_E2E_VERSION";
const SITE_VERSION: &str = "0.1.6";

const TOUR_A: &str = "E2E onboarding";
const CHAPTER_B: &str = "E2E second chapter";
const TOUR_C: &str = "E2E draft tour";
const TOUR_D: &str = "E2E long tour";
/// More pictures than one publish request copies (12).
const LONG_STEPS: usize = 14;
const RENAMED: &str = "E2E onboarding, renamed";
const FIRST: [&str; 3] = ["First card, as published", "Second card, soon removed", "Third card, as published"];
const SECOND: [&str; 3] = ["Chapter two, card one", "Chapter two, card two", "Chapter two, card three"];
const DRAFT: [&str; 3] = ["Draft, card one", "Draft, card two", "Draft, card three"];
const EDITED: &str = "First card, edited once";
const EDITED_AGAIN: &str = "First card, edited twice";
const DRAFT_EDITED: &str = "Draft, card one, edited";

/// The host and port of `server` when it is plain http on this computer; else why the pass does not run.
fn local_only(server: &str) -> Result<(String, u16), String> {
    let url = publish::check_server(server)?;
    let rest = url.strip_prefix("http://").ok_or_else(|| format!("{ENV_SERVER} must be a plain http:// address on this computer."))?;
    let host = publish::host_of(&url).unwrap_or_default();
    if host != "127.0.0.1" && host != "localhost" {
        return Err(format!("{ENV_SERVER} must be on 127.0.0.1 or localhost: this pass never contacts another host."));
    }
    if rest.contains('/') {
        return Err(format!("{ENV_SERVER} must be just the address, without a path."));
    }
    let port = match rest.split_once(':') {
        Some((_, port)) => port.parse::<u16>().map_err(|_| format!("{ENV_SERVER} has an invalid port."))?,
        None => 80,
    };
    Ok((host, port))
}

#[test]
fn the_real_pass_refuses_any_host_but_this_computer() {
    assert_eq!(local_only("http://127.0.0.1:3000"), Ok(("127.0.0.1".to_string(), 3000)));
    assert_eq!(local_only("HTTP://LocalHost:8080/"), Ok(("localhost".to_string(), 8080)));
    for other in ["https://tours.example.com", "https://127.0.0.1:3000", "http://127.0.0.1.example.com:3000", "http://192.168.1.10:3000", "http://[::1]:3000", "http://127.0.0.1:3000/x", "http://localhost.example.com", ""] {
        assert!(local_only(other).is_err(), "{other}");
    }
}

// ---- The relay ----

/// One call of the client, as the relay saw it. Headers are never kept, so neither is the token.
#[derive(Debug, Clone)]
struct Call {
    /// `METHOD path`.
    line: String,
    status: u16,
    /// The request's body when it was JSON.
    sent: Option<Value>,
    /// The answer's body when it was JSON, else its first characters.
    answer: Option<Value>,
    text: String,
}

struct Relay {
    /// The address the client is given, as the address rule writes it.
    base: String,
    calls: Arc<Mutex<Vec<Call>>>,
}

impl Relay {
    fn start(upstream: (String, u16)) -> Relay {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let log = calls.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let (log, upstream) = (log.clone(), upstream.clone());
                std::thread::spawn(move || {
                    let _ = pass_on(conn, &upstream, &log);
                });
            }
        });
        Relay { base: format!("http://127.0.0.1:{port}"), calls }
    }

    fn mark(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn since(&self, mark: usize) -> Vec<Call> {
        self.calls.lock().unwrap()[mark..].to_vec()
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The body of a chunked answer, put together; `body` as it is when it is not chunked after all.
fn dechunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(end) = find(body, b"\r\n") {
        let Ok(size) = usize::from_str_radix(String::from_utf8_lossy(&body[..end]).trim(), 16) else { return body.to_vec() };
        let start = end + 2;
        if size == 0 || body.len() < start + size {
            break;
        }
        out.extend_from_slice(&body[start..start + size]);
        body = &body[(start + size + 2).min(body.len())..];
    }
    out
}

/// One request per connection: read it whole, send it on with `Connection: close`, note the
/// answer, then hand the answer back byte for byte and close.
fn pass_on(mut conn: TcpStream, upstream: &(String, u16), log: &Mutex<Vec<Call>>) -> std::io::Result<()> {
    conn.set_read_timeout(Some(Duration::from_secs(60)))?;
    let mut buf = Vec::new();
    let mut piece = [0u8; 64 * 1024];
    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i + 4;
        }
        let n = conn.read(&mut piece)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&piece[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_string();
    let line = request_line.rsplit_once(' ').map_or(request_line.as_str(), |(l, _)| l).to_string();
    let (mut length, mut expects, mut json, mut chunked) = (0usize, false, false, false);
    let mut out = format!("{request_line}\r\n");
    for header in lines.filter(|l| !l.is_empty()) {
        let (name, value) = header.split_once(':').unwrap_or((header, ""));
        match name.to_ascii_lowercase().as_str() {
            "content-length" => length = value.trim().parse().unwrap_or(0),
            "content-type" => json = value.contains("json"),
            "transfer-encoding" => chunked = true,
            "expect" => {
                expects = true;
                continue;
            }
            "connection" | "host" => continue,
            _ => {}
        }
        out.push_str(header);
        out.push_str("\r\n");
    }
    if chunked {
        // The client sends every body with its length; a chunked one is not passed on, and the pass fails on it.
        log.lock().unwrap().push(Call { line: format!("{line} (chunked request, not passed on)"), status: 0, sent: None, answer: None, text: String::new() });
        return Ok(());
    }
    out.push_str(&format!("Host: {}:{}\r\nConnection: close\r\n\r\n", upstream.0, upstream.1));
    if expects {
        conn.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
    }
    let mut body = buf[head_end..].to_vec();
    while body.len() < length {
        let n = conn.read(&mut piece)?;
        if n == 0 {
            return Ok(());
        }
        body.extend_from_slice(&piece[..n]);
    }

    let mut up = TcpStream::connect((upstream.0.as_str(), upstream.1))?;
    up.set_read_timeout(Some(Duration::from_secs(60)))?;
    up.write_all(out.as_bytes())?;
    up.write_all(&body)?;
    let mut answer = Vec::new();
    up.read_to_end(&mut answer)?;

    let status = String::from_utf8_lossy(&answer[..answer.len().min(16)]).split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let split = find(&answer, b"\r\n\r\n").map_or(answer.len(), |i| i + 4);
    let answer_head = String::from_utf8_lossy(&answer[..split]).to_ascii_lowercase();
    let answer_body = if answer_head.contains("transfer-encoding: chunked") { dechunk(&answer[split..]) } else { answer[split..].to_vec() };
    let text: String = String::from_utf8_lossy(&answer_body).chars().take(400).collect();
    let call = Call { line, status, sent: if json { serde_json::from_slice(&body).ok() } else { None }, answer: serde_json::from_slice(&answer_body).ok(), text };
    // Noted before the client has its answer, so a count taken right after a call holds it.
    log.lock().unwrap().push(call);
    conn.write_all(&answer)?;
    conn.flush()?;
    conn.shutdown(Shutdown::Write)
}

// ---- What the site shows, asked directly ----

/// A GET to the site itself, not through the relay: status, content type and body.
fn fetch(url: &str, token: Option<&str>) -> (u16, String, Vec<u8>) {
    let agent = ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).proxy(None).timeout_global(Some(Duration::from_secs(30))).build().new_agent();
    let mut request = agent.get(url);
    if let Some(token) = token {
        request = request.header("Authorization", &format!("Bearer {token}"));
    }
    // The error is not printed: it is the library's, and this request may carry the token.
    let mut answer = request.call().unwrap_or_else(|_| panic!("the site did not answer a GET"));
    let kind = answer.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    let body = answer.body_mut().read_to_vec().unwrap_or_default();
    (answer.status().as_u16(), kind, body)
}

/// One step of a public page: its picture's key and its card's text.
#[derive(Debug, Clone, PartialEq)]
struct PageStep {
    image: String,
    card: Option<String>,
}

/// A tour's public page, `GET /t/<slug>`.
#[derive(Debug, Clone, PartialEq)]
struct Page {
    status: u16,
    title: String,
    chapters: Vec<Vec<PageStep>>,
    html: Vec<u8>,
}

impl Page {
    fn steps(&self) -> Vec<usize> {
        self.chapters.iter().map(Vec::len).collect()
    }

    fn cards(&self) -> Vec<&str> {
        self.chapters.iter().flatten().filter_map(|s| s.card.as_deref()).collect()
    }

    fn images(&self) -> Vec<String> {
        self.chapters.iter().flatten().map(|s| s.image.clone()).collect()
    }

    /// What a visitor sees, without the bytes: a re-publish may name the same pictures anew.
    fn seen(&self) -> (u16, &str, Vec<usize>, Vec<&str>) {
        (self.status, &self.title, self.steps(), self.cards())
    }
}

fn page(site: &str, slug: &str) -> Page {
    let (status, kind, html) = fetch(&format!("{site}/t/{slug}"), None);
    assert!(kind.starts_with("text/html"), "the page is HTML: {kind}");
    let text = String::from_utf8_lossy(&html).into_owned();
    const OPEN: &str = r#"<script type="application/json" id="tour">"#;
    let data: Option<Value> = text.split_once(OPEN).and_then(|(_, rest)| rest.split_once("</script>")).and_then(|(json, _)| serde_json::from_str(json).ok());
    let Some(data) = data else { return Page { status, title: String::new(), chapters: Vec::new(), html } };
    assert_eq!(data["base"], "", "the site serves its own pictures");
    let tour = &data["tour"];
    let chapters = tour["chapters"]
        .as_array()
        .expect("the page has chapters")
        .iter()
        .map(|c| c["steps"].as_array().expect("a chapter has steps").iter().map(|s| PageStep { image: s["image"].as_str().expect("a step has a picture").to_string(), card: s["card"]["body"].as_str().map(str::to_string) }).collect())
        .collect();
    Page { status, title: tour["title"].as_str().unwrap_or_default().to_string(), chapters, html }
}

/// Every picture of the page answers 200 as a PNG or JPEG with bytes of that kind.
fn pictures_are_served(site: &str, page: &Page) {
    for key in page.images() {
        let (status, kind, bytes) = fetch(&format!("{site}/{key}"), None);
        assert_eq!(status, 200, "GET /{key}");
        let png = bytes.starts_with(&[0x89, b'P', b'N', b'G']);
        let jpeg = bytes.starts_with(&[0xFF, 0xD8]);
        assert!((kind == "image/png" && png) || (kind == "image/jpeg" && jpeg), "GET /{key}: {kind}, {} bytes", bytes.len());
    }
}

fn pictures_are_gone(site: &str, keys: &[String]) {
    for key in keys {
        assert_eq!(fetch(&format!("{site}/{key}"), None).0, 404, "GET /{key}");
    }
}

/// The tour's slug as the site's own list gives it: the client keeps none for a draft.
fn slug_of(site: &str, token: &str, tour_id: &str) -> String {
    let (status, _, body) = fetch(&format!("{site}/api/v1/tours"), Some(token));
    assert_eq!(status, 200);
    let list: Value = serde_json::from_slice(&body).unwrap();
    let row = list["tours"].as_array().unwrap().iter().find(|t| t["id"] == tour_id).expect("the list has the tour");
    row["slug"].as_str().expect("the list gives a slug").to_string()
}

// ---- The recordings ----

/// A real recording of six frames with two clicks: three steps (two clicks and the last
/// frame), each with a guide card.
fn recording(root: &Path, name: &str, cards: [&str; 3]) -> PublishState {
    let state = recording_of(root, name, &cards);
    assert_eq!(tour::plan_steps(&state.rec, &state.edit, &state.events).unwrap().iter().map(|s| s.src).collect::<Vec<_>>(), [1, 3, 5]);
    state
}

/// A real recording with one step per card: a click on every second frame, and the last frame.
fn recording_of(root: &Path, name: &str, cards: &[&str]) -> PublishState {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let times: Vec<u64> = (0..cards.len() as u64 * 2).map(|i| i * 500).collect();
    let rec = Arc::new(Recording::synthetic_in(&dir, 200, 160, &times, times.len() as u64 * 500).unwrap());
    let events: Vec<InputEvent> = (1..cards.len() as u64).map(|n| left(n * 1000 - 250, 30 + n as i32 * 10, 20 + n as i32 * 8)).collect();
    let mut edit = Edit::new(&rec, OverlayStyle::default());
    let steps = tour::plan_steps(&rec, &edit, &events).unwrap();
    assert_eq!(steps.len(), cards.len());
    for (n, (step, body)) in steps.iter().zip(cards.iter().copied()).enumerate() {
        let card = Card { title: format!("Step {}", n + 1), body: body.into(), next_label: "Next".into(), anchor: Default::default(), point: None, placement: Default::default() };
        edit.tour_doc.steps.insert(step.src, StepExtras { card: Some(card), ..Default::default() });
    }
    PublishState { rec, items: Vec::new(), events, edit }
}

/// What an editor opened from the saved project would send: the project as loaded.
fn state_from(loaded: &crate::project::Loaded) -> PublishState {
    PublishState { rec: loaded.rec.clone(), items: loaded.restored.items.clone(), events: loaded.restored.events.clone(), edit: loaded.restored.edit.clone() }
}

fn card_body(state: &PublishState, src: usize) -> &str {
    state.edit.tour_doc.steps[&src].card.as_ref().map(|c| c.body.as_str()).unwrap_or_default()
}

fn set_card_body(state: &mut PublishState, src: usize, body: &str) {
    state.edit.tour_doc.steps.get_mut(&src).and_then(|s| s.card.as_mut()).expect("the step has a card").body = body.into();
}

// ---- Counting ----

fn step_puts(calls: &[Call]) -> usize {
    calls.iter().filter(|c| c.line.starts_with("PUT ") && c.line.contains("/steps/")).count()
}

fn answer_of(calls: &[Call], is: impl Fn(&str) -> bool) -> &Call {
    calls.iter().find(|c| is(&c.line)).unwrap_or_else(|| panic!("no such call among {:?}", calls.iter().map(|c| &c.line).collect::<Vec<_>>()))
}

fn create_of(calls: &[Call]) -> &Call {
    answer_of(calls, |l| l == "POST /api/v1/uploads")
}

fn commit_of(calls: &[Call]) -> &Call {
    answer_of(calls, |l| l.ends_with("/commit"))
}

/// The step numbers the site said it already has.
fn have_of(calls: &[Call]) -> Vec<u64> {
    create_of(calls).answer.as_ref().and_then(|a| a["have"].as_array().cloned()).unwrap_or_default().iter().filter_map(Value::as_u64).collect()
}

/// Wall time and the client's calls, step by step, printed as the pass goes.
struct Report {
    rows: Vec<(String, Duration, usize)>,
    started: Instant,
    mark: usize,
}

impl Report {
    fn begin(&mut self, relay: &Relay) {
        self.started = Instant::now();
        self.mark = relay.mark();
    }

    /// Close the step: note its time and print the calls the client made in it.
    fn end(&mut self, relay: &Relay, name: &str) {
        let took = self.started.elapsed();
        let calls = relay.since(self.mark);
        println!("\n== {name}: {} ms, {} client calls", took.as_millis(), calls.len());
        for c in &calls {
            let sent = c.sent.as_ref().map(|s| if s.get("manifest").is_some() { format!(" sent target {} sha256 {}", s["target"], s["sha256"].as_array().map_or(0, Vec::len)) } else { format!(" sent {s}") }).unwrap_or_default();
            println!("   {} -> {}{} answered {}", c.line, c.status, sent, c.text);
        }
        self.rows.push((name.to_string(), took, calls.len()));
        self.begin(relay);
    }
}

fn listed(server: &str, token: &str, tour_id: &str) -> Option<RemoteTour> {
    publish::list_tours(server, token).unwrap().into_iter().find(|t| t.id == tour_id)
}

fn fresh_run() -> Arc<Run> {
    Arc::new(Run::default())
}

#[test]
#[ignore = "needs a real demo-platform server on 127.0.0.1 with a fresh data folder: set MYTHICSHOT_E2E_SERVER and MYTHICSHOT_E2E_TOKEN, then run with --ignored --nocapture"]
fn one_real_pass_against_a_local_platform_server() {
    let site = std::env::var(ENV_SERVER).unwrap_or_else(|_| panic!("{ENV_SERVER} is not set"));
    let token = std::env::var(ENV_TOKEN).unwrap_or_else(|_| panic!("{ENV_TOKEN} is not set")).trim().to_string();
    let upstream = local_only(&site).unwrap_or_else(|why| panic!("{why}"));
    let site = format!("http://{}:{}", upstream.0, upstream.1);
    let version = std::env::var(ENV_VERSION).unwrap_or_else(|_| SITE_VERSION.to_string());

    let relay = Relay::start(upstream);
    // The address as someone might type it; the client and the store both use it as the address rule writes it.
    let typed = format!("HTTP://127.0.0.1:{}/", relay.base.rsplit(':').next().unwrap());
    let server = publish::check_server(&typed).unwrap();
    assert_eq!(server, relay.base);

    let tmp = tempfile::tempdir().unwrap();
    let store = Store::at(tmp.path().join("projects"));
    let keep = keeping(true, Some(store.root().to_path_buf()));
    let ask = || -> Result<String, String> { Ok(token.clone()) };
    let mut report = Report { rows: Vec::new(), started: Instant::now(), mark: 0 };
    let all = Instant::now();

    // 1. The site says what it is.
    report.begin(&relay);
    let ws = publish::workspace(&typed, &token).unwrap();
    assert_eq!(ws.version.as_deref(), Some(version.as_str()));
    assert!(publish::supports_update(ws.version.as_deref()));
    assert!(publish::list_tours(&typed, &token).unwrap().is_empty(), "the pass needs a site without tours: give it a fresh data folder");
    report.end(&relay, "1 me");

    // 2. A new tour of three steps, published, with its recording kept.
    let first = recording(tmp.path(), "rec-a", FIRST);
    let done = publish_job(&first, TOUR_A, TOUR_A, &typed, ask, &Target::New, true, &keep, &fresh_run(), &|_| {}).unwrap();
    let publish_calls = relay.since(report.mark);
    let Outcome::Public { url: Some(link) } = done.finished.outcome.clone() else { panic!("not public with a link: {done:?}") };
    assert_eq!((done.finished.steps, &done.kept), (3, &Kept::Saved(vec![])));
    assert!(link.starts_with(&format!("{server}/t/")), "{link}");
    let slug = link.rsplit('/').next().unwrap().to_string();
    let (tour_a, chapter_a) = (done.finished.tour_id.clone(), done.finished.chapter_id.clone());
    assert_eq!(step_puts(&publish_calls), 3);
    let live = page(&site, &slug);
    assert_eq!(live.seen(), (200, TOUR_A, vec![3], FIRST.to_vec()));
    pictures_are_served(&site, &live);
    let key_a = ProjectKey { server: server.clone(), tour_id: tour_a.clone(), chapter_id: chapter_a.clone() };
    let saved = store.list();
    assert_eq!(saved.iter().map(|p| (&p.key, p.title.as_str(), p.tour_title.as_str(), p.problem)).collect::<Vec<_>>(), [(&key_a, TOUR_A, TOUR_A, None)]);
    assert!(!project_text(&saved[0]).contains(&token));
    let row = listed(&typed, &token, &tour_a).expect("the list has the tour");
    assert_eq!((row.title.as_str(), row.status.as_str(), row.chapters, row.steps, row.link.as_deref(), row.unpublished_changes), (TOUR_A, "published", 1, 3, Some(link.as_str()), false));
    drop(first);
    report.end(&relay, "2 publish a new tour");

    // 3. The project opened, a card's text changed and a step removed, then "Update tour".
    let loaded = store.load(&key_a).unwrap();
    let mut state = state_from(&loaded);
    assert_eq!((card_body(&state, 1), loaded.restored.title.as_str()), (FIRST[0], TOUR_A));
    set_card_body(&mut state, 1, EDITED);
    state.edit.tour_removed.insert(3);
    let target = EditTarget { tour_id: tour_a.clone(), chapter_id: chapter_a.clone(), tour_title: row.title.clone() };
    let before = Before::of(&row);
    report.begin(&relay);
    let done = update_job(&state, &loaded.restored.title, &target, &typed, ask, &keep, &fresh_run(), &|_| {}).unwrap();
    let calls = relay.since(report.mark);
    // Steps 1 and 2 of the update are the pictures of the old steps 1 and 3: the site has both.
    let have = have_of(&calls);
    assert_eq!(create_of(&calls).sent.as_ref().unwrap()["target"], serde_json::json!({ "kind": "replace", "tour_id": tour_a, "chapter_id": chapter_a }));
    assert_eq!((have.clone(), step_puts(&calls)), (vec![1, 2], 0), "only pictures the site lacks are sent");
    assert_eq!(commit_of(&calls).answer.as_ref().unwrap()["publish"], "ready");
    assert_eq!(done.finished.outcome, Outcome::Public { url: Some(link.clone()) }, "the same link");
    assert_eq!((publish_ui::result_heading(&done.finished.outcome, true), publish_ui::result_status(&done.finished, &before, true).as_str()), ("Tour updated", "Tour updated: 2 steps"));
    assert_eq!(done.kept, Kept::Saved(vec![]));
    let live = page(&site, &slug);
    assert_eq!(live.seen(), (200, TOUR_A, vec![2], vec![EDITED, FIRST[2]]));
    pictures_are_served(&site, &live);
    drop((state, loaded));
    let again = store.load(&key_a).unwrap();
    assert_eq!((card_body(&state_from(&again), 1), again.restored.edit.tour_removed.iter().copied().collect::<Vec<_>>()), (EDITED, vec![3]));
    assert_eq!(tour::plan_steps(&again.rec, &again.restored.edit, &again.restored.events).unwrap().len(), 2);
    assert_eq!(store.list().len(), 1, "the same project, not a second one");
    report.end(&relay, "3 update: text changed, a step removed");

    // 4. Updated again with nothing changed: no picture goes out, and the page shows the same.
    let state = state_from(&again);
    let before = Before::of(&listed(&typed, &token, &tour_a).unwrap());
    report.begin(&relay);
    let done = update_job(&state, &again.restored.title, &target, &typed, ask, &keep, &fresh_run(), &|_| {}).unwrap();
    let calls = relay.since(report.mark);
    assert_eq!((have_of(&calls), step_puts(&calls)), (vec![1, 2], 0));
    assert_eq!(done.finished.outcome, Outcome::Public { url: Some(link.clone()) });
    assert_eq!(publish_ui::result_status(&done.finished, &before, true), "Tour updated: 2 steps");
    let same = page(&site, &slug);
    assert_eq!(same.seen(), live.seen());
    drop((state, again));
    report.end(&relay, "4 update: nothing changed");
    println!("   the page after an update without changes is {} the one before", if same.html == live.html { "byte for byte" } else { "not byte for byte" });
    println!("   its pictures are named {}", if same.images() == live.images() { "as before" } else { "anew" });

    // 5. A second chapter, uploaded as a private draft into the same tour.
    let second = recording(tmp.path(), "rec-b", SECOND);
    let done = publish_job(&second, CHAPTER_B, TOUR_A, &typed, ask, &Target::Chapter(tour_a.clone()), false, &keep, &fresh_run(), &|_| {}).unwrap();
    assert_eq!((done.finished.tour_id.as_str(), done.finished.steps, &done.finished.outcome, &done.kept), (tour_a.as_str(), 3, &Outcome::Draft, &Kept::Saved(vec![])));
    let chapter_b = done.finished.chapter_id.clone();
    assert_ne!(chapter_b, chapter_a);
    let row = listed(&typed, &token, &tour_a).unwrap();
    assert_eq!((row.status.as_str(), row.chapters, row.steps, row.unpublished_changes, row.changes_waiting()), ("published", 2, 5, true, true));
    assert_eq!(page(&site, &slug).seen(), live.seen(), "the public page still shows only the first chapter");
    assert_eq!(store.for_tour(&server, &tour_a).len(), 2);
    drop(second);
    report.end(&relay, "5 a second chapter as a draft");

    // 6. The first chapter updated with a change while the other chapter waits: held back.
    let loaded = store.load(&key_a).unwrap();
    let mut state = state_from(&loaded);
    set_card_body(&mut state, 1, EDITED_AGAIN);
    // A rectangle on the first step's frame: that step's picture is another one now.
    state.items.push(Item { id: 1, shape: Shape::Rect(Bounds::new(pt(30.0, 20.0), pt(90.0, 70.0))), style: Style::default(), frames: Some((1, 1)), pin: None });
    let before = Before::of(&row);
    report.begin(&relay);
    let done = update_job(&state, &loaded.restored.title, &target, &typed, ask, &keep, &fresh_run(), &|_| {}).unwrap();
    let calls = relay.since(report.mark);
    assert_eq!(commit_of(&calls).answer.as_ref().unwrap()["publish"], "other_changes");
    assert_eq!((have_of(&calls), step_puts(&calls)), (vec![2], 1), "only the changed picture is sent");
    assert_eq!(done.finished.outcome, Outcome::Held);
    assert!(!calls.iter().any(|c| c.line.ends_with("/publish")), "nothing was published");
    assert_eq!(publish_ui::result_status(&done.finished, &before, true), "Update uploaded: the public page still shows the previous version");
    assert_eq!(publish_ui::result_note(&done.finished.outcome, &before, true), Some((publish_ui::UPDATE_HELD.to_string(), false)));
    assert_eq!(publish_ui::result_link(&done.finished.outcome, &before), Some(link.as_str()));
    assert_eq!(page(&site, &slug).seen(), live.seen(), "the public page still shows the previous version");
    assert_eq!(done.kept, Kept::Saved(vec![]));
    drop((state, loaded));
    let again = store.load(&key_a).unwrap();
    assert_eq!((card_body(&state_from(&again), 1), again.restored.items.len()), (EDITED_AGAIN, 1), "the project is rewritten");
    drop(again);
    assert!(listed(&typed, &token, &tour_a).unwrap().changes_waiting());
    report.end(&relay, "6 update while other changes wait: held");

    // 7. "Publish changes" on the Tours page: the publish call alone.
    let live_now = publish::publish_tour(&typed, &token, &tour_a, |_| true).unwrap();
    assert_eq!((live_now.steps, live_now.url.as_deref()), (5, Some(link.as_str())));
    let both = page(&site, &slug);
    assert_eq!(both.seen(), (200, TOUR_A, vec![2, 3], vec![EDITED_AGAIN, FIRST[2], SECOND[0], SECOND[1], SECOND[2]]));
    pictures_are_served(&site, &both);
    let row = listed(&typed, &token, &tour_a).unwrap();
    assert_eq!((row.status.as_str(), row.chapters, row.steps, row.unpublished_changes), ("published", 2, 5, false));
    report.end(&relay, "7 publish the waiting changes");

    // 8. Renamed on the site; the update dialog then names the tour as the site does, and an
    // update carries that title into the saved recording.
    assert_eq!(publish::rename_tour(&typed, &token, &tour_a, RENAMED).as_deref(), Ok(RENAMED));
    let tours = publish::list_tours(&typed, &token).unwrap();
    assert_eq!(publish_ui::update_offer(true, ws.version.as_deref(), Some(&tours), &tour_a), publish_ui::UpdateOffer::Offered { public: true });
    let row = tours.iter().find(|t| t.id == tour_a).unwrap().clone();
    assert_eq!((row.title.as_str(), row.link.as_deref(), row.unpublished_changes), (RENAMED, Some(link.as_str()), false), "a rename keeps the link and is live at once");
    assert_eq!(page(&site, &slug).title, RENAMED);
    assert_eq!(store.for_tour(&server, &tour_a).iter().map(|p| p.tour_title.as_str()).collect::<Vec<_>>(), [TOUR_A, TOUR_A], "the saved recordings still have the old title");
    let loaded = store.load(&key_a).unwrap();
    let renamed = EditTarget { tour_title: row.title.clone(), ..target.clone() };
    let done = update_job(&state_from(&loaded), &loaded.restored.title, &renamed, &typed, ask, &keep, &fresh_run(), &|_| {}).unwrap();
    assert_eq!(done.finished.outcome, Outcome::Public { url: Some(link.clone()) });
    assert_eq!(publish_ui::result_line(&renamed.tour_title, &done.finished, &Before::of(&row), true), format!("{RENAMED} · 5 steps · public"));
    drop(loaded);
    let titles: Vec<(String, String)> = store.for_tour(&server, &tour_a).into_iter().map(|p| (p.key.chapter_id, p.tour_title)).collect();
    assert!(titles.contains(&(chapter_a.clone(), RENAMED.to_string())) && titles.contains(&(chapter_b.clone(), TOUR_A.to_string())), "{titles:?}");
    let both = page(&site, &slug);
    assert_eq!(both.seen(), (200, RENAMED, vec![2, 3], vec![EDITED_AGAIN, FIRST[2], SECOND[0], SECOND[1], SECOND[2]]));
    report.end(&relay, "8 rename, then update under the site's title");

    // 9. A tour that is a draft stays one through an update.
    let third = recording(tmp.path(), "rec-c", DRAFT);
    let done = publish_job(&third, TOUR_C, TOUR_C, &typed, ask, &Target::New, false, &keep, &fresh_run(), &|_| {}).unwrap();
    assert_eq!((&done.finished.outcome, &done.kept), (&Outcome::Draft, &Kept::Saved(vec![])));
    let (tour_c, chapter_c) = (done.finished.tour_id.clone(), done.finished.chapter_id.clone());
    let key_c = ProjectKey { server: server.clone(), tour_id: tour_c.clone(), chapter_id: chapter_c.clone() };
    let slug_c = slug_of(&site, &token, &tour_c);
    assert_eq!(page(&site, &slug_c).status, 404);
    let row_c = listed(&typed, &token, &tour_c).unwrap();
    assert_eq!((row_c.status.as_str(), row_c.steps, row_c.link.as_deref(), row_c.unpublished_changes), ("draft", 3, None, false));
    assert_eq!(publish_ui::update_offer(true, ws.version.as_deref(), Some(&publish::list_tours(&typed, &token).unwrap()), &tour_c), publish_ui::UpdateOffer::Offered { public: false });
    drop(third);
    let loaded = store.load(&key_c).unwrap();
    let mut state = state_from(&loaded);
    set_card_body(&mut state, 1, DRAFT_EDITED);
    let target_c = EditTarget { tour_id: tour_c.clone(), chapter_id: chapter_c.clone(), tour_title: row_c.title.clone() };
    report.end(&relay, "9a upload a draft tour");
    let done = update_job(&state, &loaded.restored.title, &target_c, &typed, ask, &keep, &fresh_run(), &|_| {}).unwrap();
    let calls = relay.since(report.mark);
    assert_eq!(commit_of(&calls).answer.as_ref().unwrap()["publish"], "draft");
    assert_eq!((have_of(&calls), step_puts(&calls)), (vec![1, 2, 3], 0));
    assert_eq!(done.finished.outcome, Outcome::Draft);
    assert!(!calls.iter().any(|c| c.line.ends_with("/publish")));
    assert_eq!((publish_ui::result_heading(&done.finished.outcome, true), publish_ui::result_status(&done.finished, &Before::of(&row_c), true).as_str()), ("Draft updated", "Draft updated: 3 steps"));
    drop((state, loaded));
    let row_c = listed(&typed, &token, &tour_c).unwrap();
    assert_eq!((row_c.status.as_str(), row_c.steps, row_c.unpublished_changes), ("draft", 3, false));
    assert_eq!(page(&site, &slug_c).status, 404, "a draft has no public page");
    report.end(&relay, "9b update the draft tour");

    // 10. The first tour deleted until the site says nothing is left; only then its recordings go.
    let pictures = both.images();
    let draft_files = files_of(&store.for_tour(&server, &tour_c)[0].dir);
    let mut asked = 0;
    let how = publish::delete_tour(&typed, &token, &tour_a, |_| asked += 1);
    assert_eq!(how, Ok(Deleted::BySite));
    assert_eq!(page(&site, &slug).status, 404);
    pictures_are_gone(&site, &pictures);
    assert!(listed(&typed, &token, &tour_a).is_none());
    assert_eq!(store.for_tour(&server, &tour_a).len(), 2, "nothing on this computer went before the site said the tour is gone");
    assert_eq!(store.remove_tour(&server, &tour_a), Ok(2));
    assert!(store.for_tour(&server, &tour_a).is_empty());
    assert_eq!(store.list().iter().map(|p| &p.key).collect::<Vec<_>>(), [&key_c]);
    assert_eq!(files_of(&store.list()[0].dir), draft_files, "the draft tour's recording is untouched");
    report.end(&relay, "10 delete the first tour");
    println!("   delete: {asked} requests");

    // 11a. An update aimed at a chapter the tour does not have.
    let loaded = store.load(&key_c).unwrap();
    let mut state = state_from(&loaded);
    set_card_body(&mut state, 1, "Never arrives");
    let nowhere = EditTarget { chapter_id: "ch_0123456789abcdef01234567".into(), ..target_c.clone() };
    let error = update_job(&state, &loaded.restored.title, &nowhere, &typed, ask, &keep, &fresh_run(), &|_| {}).unwrap_err();
    let calls = relay.since(report.mark);
    assert_eq!(error, "Could not start the upload: that chapter is no longer part of the tour.");
    assert!(publish::is_chapter_gone(&error) && !publish_ui::update_can_be_tried_again(&error));
    assert_eq!(calls.iter().map(|c| (c.line.as_str(), c.status)).collect::<Vec<_>>(), [("POST /api/v1/uploads", 404)]);
    assert_eq!(calls[0].answer, Some(serde_json::json!({ "error": "not_found", "detail": "no such chapter" })));
    assert_eq!(listed(&typed, &token, &tour_c).unwrap(), row_c, "nothing changed on the site");
    assert_eq!(files_of(&store.list()[0].dir), draft_files, "and nothing on this computer");
    report.end(&relay, "11a update a chapter that is not there");

    // 11b. An update with a token the site does not know.
    let wrong = [format!("dp_{}", "0123456789".repeat(4)), format!("dp_{}", "f".repeat(40))].into_iter().find(|w| *w != token).unwrap();
    let error = update_job(&state, &loaded.restored.title, &target_c, &typed, || Ok(wrong.clone()), &keep, &fresh_run(), &|_| {}).unwrap_err();
    let calls = relay.since(report.mark);
    assert_eq!(error, publish::REJECTED_TOKEN);
    assert!(!error.contains(&wrong) && !error.contains(&token) && !error.contains("dp_"));
    assert_eq!(calls.iter().map(|c| (c.line.as_str(), c.status)).collect::<Vec<_>>(), [("POST /api/v1/uploads", 401)]);
    assert!(!calls[0].text.contains(&wrong), "the site does not echo the token either");
    assert_eq!(listed(&typed, &token, &tour_c).unwrap(), row_c);
    assert_eq!(files_of(&store.list()[0].dir), draft_files);
    drop((state, loaded));
    report.end(&relay, "11b update with a wrong token");

    // 12. Beyond the eleven: a tour with more pictures than one publish request copies, so the
    // publish is called again while the site answers that it is under way. Then it is deleted.
    let cards: Vec<String> = (1..=LONG_STEPS).map(|n| format!("Long tour, card {n}")).collect();
    let long = recording_of(tmp.path(), "rec-d", &cards.iter().map(String::as_str).collect::<Vec<_>>());
    let run = fresh_run();
    let done = publish_job(&long, TOUR_D, TOUR_D, &typed, ask, &Target::New, true, &keep, &run, &|_| {}).unwrap();
    let calls = relay.since(report.mark);
    let Outcome::Public { url: Some(link_d) } = done.finished.outcome.clone() else { panic!("not public with a link: {done:?}") };
    let slug_d = link_d.rsplit('/').next().unwrap().to_string();
    assert_eq!((done.finished.steps, step_puts(&calls)), (LONG_STEPS, LONG_STEPS));
    let publishes: Vec<&Call> = calls.iter().filter(|c| c.line.ends_with("/publish")).collect();
    assert!(publishes.len() >= 2, "the publish took more than one call");
    assert_eq!(publishes.iter().map(|c| c.status).collect::<Vec<_>>(), [vec![202; publishes.len() - 1], vec![200]].concat());
    let started = publishes[0].answer.as_ref().unwrap();
    assert_eq!((publishes[0].sent.is_none(), started["status"].as_str(), started["total"].as_u64()), (true, Some("in_progress"), Some(LONG_STEPS as u64)));
    assert_eq!(publishes[1].sent.as_ref().unwrap()["publish_id"], started["publish_id"], "the later calls name the publish the first one started");
    assert!(matches!(run.publishing(), Some(Some((done, total))) if done <= total && total == LONG_STEPS as u64), "the count the site gave is what the editor shows: {:?}", run.publishing());
    let long_page = page(&site, &slug_d);
    assert_eq!((long_page.status, long_page.steps(), long_page.cards()), (200, vec![LONG_STEPS], cards.iter().map(String::as_str).collect()));
    pictures_are_served(&site, &long_page);
    drop(long);
    report.end(&relay, "12a a tour that publishes over several calls");
    let tour_d = done.finished.tour_id.clone();
    assert_eq!(publish::delete_tour(&typed, &token, &tour_d, |_| {}), Ok(Deleted::BySite));
    assert_eq!(page(&site, &slug_d).status, 404);
    pictures_are_gone(&site, &long_page.images());
    assert_eq!(store.remove_tour(&server, &tour_d), Ok(1));
    assert_eq!(store.list().iter().map(|p| &p.key).collect::<Vec<_>>(), [&key_c]);
    report.end(&relay, "12b delete it");

    println!("\n== Wall time and client calls per step (the pass: {} ms)", all.elapsed().as_millis());
    for (name, took, calls) in &report.rows {
        println!("{:>7} ms  {calls:>3} calls  {name}", took.as_millis());
    }
    println!("   a new 3-step tour, uploaded and published: {} calls ({})", publish_calls.len(), publish_calls.iter().map(|c| c.line.split(' ').next().unwrap_or_default()).collect::<Vec<_>>().join(" "));
}
