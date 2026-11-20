//! The sign-in window's wording and steps, without a window and without the real browser.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const SERVER: &str = "https://tours.example.com";
const PAGE: &str = "https://app.mythicshot.example/connections";

fn code(page: Option<&str>, minutes: Option<&str>) -> Code {
    Code { user_code: "K7QM-2HXD".into(), page: page.map(str::to_string), minutes: minutes.map(str::to_string) }
}

/// An opener that records what it was asked to open.
fn opener() -> (Arc<Mutex<Vec<String>>>, Opener) {
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let l = log.clone();
    (log, Arc::new(move |url: &str| {
        l.lock().unwrap().push(url.to_string());
        Ok(())
    }))
}

/// What a job does: after reporting `code` (when there is one), it ends with what `release`
/// sends, or with a cancel. `cancelled` notes that it saw the cancel.
struct Script {
    code: Option<Code>,
    release: std::sync::mpsc::Receiver<Result<Option<String>, ConnectError>>,
    cancelled: Arc<AtomicBool>,
}

fn run_script(s: Script, r: &Reporter) -> Result<Option<String>, ConnectError> {
    if let Some(c) = s.code {
        r.code(c);
    }
    loop {
        if r.cancelled() {
            s.cancelled.store(true, Ordering::SeqCst);
            return Err(ConnectError::Cancelled);
        }
        if let Ok(result) = s.release.recv_timeout(Duration::from_millis(10)) {
            return result;
        }
    }
}

/// A dialog whose jobs report `code` and end when told. Answers the dialog, the sender that ends
/// the newest job, how many jobs were started, whether the last one saw a cancel, and the opener's log.
#[allow(clippy::type_complexity)]
fn dialog(code: Option<Code>) -> (SignInDialog, Arc<Mutex<Option<std::sync::mpsc::Sender<Result<Option<String>, ConnectError>>>>>, Arc<AtomicUsize>, Arc<AtomicBool>, Arc<Mutex<Vec<String>>>) {
    let sender: Arc<Mutex<Option<std::sync::mpsc::Sender<Result<Option<String>, ConnectError>>>>> = Arc::default();
    let started = Arc::new(AtomicUsize::new(0));
    let cancelled = Arc::new(AtomicBool::new(false));
    let (s, n, c) = (sender.clone(), started.clone(), cancelled.clone());
    let starter: Starter = Arc::new(move |server: String| {
        assert_eq!(server, SERVER, "the window starts for its own address");
        n.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        *s.lock().unwrap() = Some(tx);
        let script = Script { code: code.clone(), release: rx, cancelled: c.clone() };
        SignInJob::spawn(move |r| run_script(script, r))
    });
    let (log, open) = opener();
    (SignInDialog::open(SERVER.into(), starter, open), sender, started, cancelled, log)
}

/// Poll until the stage is no longer Starting.
fn until_code(d: &mut SignInDialog) {
    for _ in 0..500 {
        assert_eq!(d.poll(), None);
        if *d.stage() != Stage::Starting {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("no code came");
}

/// End the newest job with `result` and poll until the dialog has taken it in.
fn finish(d: &mut SignInDialog, sender: &Mutex<Option<std::sync::mpsc::Sender<Result<Option<String>, ConnectError>>>>, result: Result<Option<String>, ConnectError>) -> Option<SignInAction> {
    sender.lock().unwrap().as_ref().unwrap().send(result).unwrap();
    for _ in 0..500 {
        let action = d.poll();
        if action.is_some() || d.job().is_none() {
            return action;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the job did not end");
}

#[test]
fn the_waiting_window_says_what_the_mockup_says_and_opens_the_page_once() {
    let (mut d, _tx, _, _, opened) = dialog(Some(code(Some(PAGE), Some("10 minutes"))));
    assert_eq!(d.view().texts(), [CAPTION, "Asking the site for a code…", "Cancel"]);
    until_code(&mut d);
    assert_eq!(
        d.view().texts(),
        [
            "Sign in",
            "Type this code in your browser",
            "Your browser opened the Connections page of your dashboard. Type the code there and press Connect.",
            "Waiting for you to confirm… The code works for 10 minutes.",
            "Browser did not open? Open the page or go to app.mythicshot.example/connections.",
            "K7QM-2HXD",
            "Cancel",
        ]
    );
    // The code is its own line, after the first sentence; the address is shown without its scheme.
    let v = d.view();
    assert_eq!(v.code.as_deref(), Some("K7QM-2HXD"));
    assert_eq!(v.lines[2], Line::Help { address: "app.mythicshot.example/connections".into() });
    // The browser was opened once, at the page, and the code is not in its address.
    for _ in 0..5 {
        d.poll();
    }
    assert_eq!(*opened.lock().unwrap(), [PAGE]);
    // "Open the page" opens it again.
    d.open_page();
    assert_eq!(*opened.lock().unwrap(), [PAGE, PAGE]);
    assert!(opened.lock().unwrap().iter().all(|u| !u.contains("K7QM")));
}

#[test]
fn how_long_the_code_works_is_said_in_whole_minutes() {
    assert_eq!(waiting(Some("10 minutes")), "Waiting for you to confirm… The code works for 10 minutes.");
    assert_eq!(waiting(Some("1 minute")), "Waiting for you to confirm… The code works for 1 minute.");
    assert_eq!(waiting(None), "Waiting for you to confirm… The code works for less than a minute.");
    assert_eq!(browser_help("app.example.com/connections"), "Browser did not open? Open the page or go to app.example.com/connections.");
}

#[test]
fn without_a_page_the_window_says_where_to_go_and_opens_nothing() {
    let (mut d, _tx, _, _, opened) = dialog(Some(code(None, Some("10 minutes"))));
    until_code(&mut d);
    assert_eq!(
        d.view().texts(),
        [
            "Sign in",
            "Type this code in your browser",
            "Mythicshot cannot open the page for this. Open the Connections page of your dashboard yourself, type the code there and press Connect.",
            "Waiting for you to confirm… The code works for 10 minutes.",
            "K7QM-2HXD",
            "Cancel",
        ]
    );
    d.open_page();
    assert!(opened.lock().unwrap().is_empty());
}

#[test]
fn a_confirmed_code_closes_the_window_as_connected() {
    let (mut d, tx, _, _, _) = dialog(Some(code(Some(PAGE), Some("10 minutes"))));
    until_code(&mut d);
    assert_eq!(finish(&mut d, &tx, Ok(None)), Some(SignInAction::Connected(None)));
    // With the account the site named, which the page shows.
    let (mut d, tx, _, _, _) = dialog(Some(code(Some(PAGE), Some("10 minutes"))));
    until_code(&mut d);
    assert_eq!(finish(&mut d, &tx, Ok(Some("alex".into()))), Some(SignInAction::Connected(Some("alex".into()))));
}

#[test]
fn a_code_that_ran_out_says_so_and_try_again_starts_over() {
    let (mut d, tx, started, _, opened) = dialog(Some(code(Some(PAGE), Some("10 minutes"))));
    until_code(&mut d);
    assert_eq!(finish(&mut d, &tx, Err(ConnectError::NotConnected)), None);
    assert_eq!(d.view().texts(), ["Sign in", "This computer was not connected", "The code ran out or was turned down in the browser. Nothing was changed.", "Close", "Try again"]);
    assert_eq!(d.view().lines, [Line::Warning(connect::NOT_CONNECTED_TEXT.into())]);
    assert_eq!(d.escape_button(), Button::Close);
    // Try again: a new code, and the page opened for it again.
    assert_eq!(d.press(Button::TryAgain), None);
    assert_eq!(started.load(Ordering::SeqCst), 2);
    assert_eq!(*d.stage(), Stage::Starting);
    until_code(&mut d);
    assert_eq!(opened.lock().unwrap().len(), 2);
    assert_eq!(finish(&mut d, &tx, Err(ConnectError::NotConnected)), None);
    assert_eq!(d.press(Button::Close), Some(SignInAction::Closed));
}

#[test]
fn other_failures_are_said_in_mythicshots_words() {
    for (error, text, retry) in [
        (ConnectError::NotOffered, "This site does not offer Sign in yet. Paste a token instead.", false),
        (ConnectError::Busy(Some(40)), "Could not sign in: the site asks to wait 40 seconds. Sign in again later.", true),
        (ConnectError::LostAnswer, "The site's answer was lost on the way. If this computer shows on the Connections page of your dashboard, disconnect it there, then sign in again.", true),
        (ConnectError::Busy(None), "Could not sign in: the site is busy. Sign in again later.", true),
        (ConnectError::Failed("Could not sign in: could not connect to the server.".into()), "Could not sign in: could not connect to the server.", true),
        (ConnectError::Failed("Could not use the credential store: the credential store is locked or not reachable.".into()), "Could not use the credential store: the credential store is locked or not reachable.", true),
    ] {
        let (mut d, tx, started, _, _) = dialog(None);
        assert_eq!(finish(&mut d, &tx, Err(error.clone())), None);
        let mut want = vec!["Sign in", "This computer was not connected", text, "Close"];
        if retry {
            want.push("Try again");
        }
        assert_eq!(d.view().texts(), want, "{error:?}");
        // "Try again" means nothing where it cannot help.
        if !retry {
            assert_eq!(d.press(Button::TryAgain), None);
            assert_eq!(started.load(Ordering::SeqCst), 1);
        }
    }
}

#[test]
fn cancel_stops_the_sign_in_and_closes_the_window() {
    let (mut d, _tx, _, cancelled, _) = dialog(Some(code(Some(PAGE), Some("10 minutes"))));
    until_code(&mut d);
    assert_eq!(d.escape_button(), Button::Cancel);
    assert_eq!(d.press(Button::Cancel), Some(SignInAction::Closed));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !cancelled.load(Ordering::SeqCst) {
        assert!(std::time::Instant::now() < deadline, "the job never saw the cancel");
        std::thread::sleep(Duration::from_millis(5));
    }
    // Cancel while asking for the code works the same.
    let (mut d, _tx, _, cancelled, _) = dialog(None);
    assert_eq!(d.press(Button::Cancel), Some(SignInAction::Closed));
    while !cancelled.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(5));
    }
    // Buttons that are not shown do nothing.
    let (mut d, _tx, _, _, _) = dialog(None);
    assert_eq!(d.press(Button::Close), None);
    assert_eq!(d.press(Button::TryAgain), None);
}

#[test]
fn a_window_that_goes_away_cancels_its_sign_in() {
    let (d, _tx, _, cancelled, _) = dialog(Some(code(Some(PAGE), None)));
    drop(d);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !cancelled.load(Ordering::SeqCst) {
        assert!(std::time::Instant::now() < deadline, "the job never saw the cancel");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_job_that_panics_ends_with_a_sentence() {
    let (log, open) = opener();
    let starter: Starter = Arc::new(|_| SignInJob::spawn(|_| panic!("boom")));
    let mut d = SignInDialog::open(SERVER.into(), starter, open);
    for _ in 0..500 {
        d.poll();
        if d.job().is_none() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(d.view().texts(), ["Sign in", "This computer was not connected", "Signing in failed because of an internal error.", "Close", "Try again"]);
    assert!(log.lock().unwrap().is_empty());
}

#[test]
fn tests_never_reach_the_real_browser() {
    assert!(open_in_browser("https://app.example.com/connections").is_err());
}

#[test]
fn a_browser_that_did_not_open_is_said_and_the_page_can_still_be_opened() {
    let tries = Arc::new(AtomicUsize::new(0));
    let t = tries.clone();
    // The first open fails, the second works.
    let open: Opener = Arc::new(move |_: &str| if t.fetch_add(1, Ordering::SeqCst) == 0 { Err("Windows could not open the link (code 2)".into()) } else { Ok(()) });
    let c = code(Some(PAGE), Some("10 minutes"));
    let starter: Starter = Arc::new(move |_| {
        let c = c.clone();
        SignInJob::spawn(move |r| {
            r.code(c);
            while !r.cancelled() {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(ConnectError::Cancelled)
        })
    });
    let mut d = SignInDialog::open(SERVER.into(), starter, open);
    until_code(&mut d);
    assert_eq!(
        d.view().texts(),
        [
            "Sign in",
            "Type this code in your browser",
            "Mythicshot could not open your browser. Open the Connections page of your dashboard yourself, type the code there and press Connect.",
            "Waiting for you to confirm… The code works for 10 minutes.",
            "Browser did not open? Open the page or go to app.mythicshot.example/connections.",
            "K7QM-2HXD",
            "Cancel",
        ]
    );
    // "Open the page" worked this time: the window says the browser opened.
    d.open_page();
    assert_eq!(d.view().lines[0], Line::Text(OPENED.into()));
    assert_eq!(tries.load(Ordering::SeqCst), 2);
}

#[test]
fn the_page_on_another_host_is_shown_in_full() {
    let long = "https://connections-dashboard.a-rather-long-workspace-host-name.example.com:8443/connections";
    let (mut d, _tx, _, _, _) = dialog(Some(code(Some(long), Some("10 minutes"))));
    until_code(&mut d);
    let v = d.view();
    assert_eq!(v.lines[2], Line::Help { address: "connections-dashboard.a-rather-long-workspace-host-name.example.com:8443/connections".into() });
    assert!(v.texts().iter().any(|t| t.ends_with("or go to connections-dashboard.a-rather-long-workspace-host-name.example.com:8443/connections.")));
}
