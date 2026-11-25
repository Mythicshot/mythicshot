//! The update check and download against a stand-in site on 127.0.0.1: the exact request, every
//! rule the release file must meet, the size and hash rules of the download, and the start of
//! the installer through a stand-in that runs nothing.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;

// ---- The stand-in site ----

/// One request as the stand-in site read it: the request line and every header, names in lower case.
#[derive(Debug, Clone)]
struct Seen {
    line: String,
    headers: BTreeMap<String, String>,
}

/// What the stand-in site answers.
enum Serve {
    /// Status, headers, body, and how long to wait between pieces of 16 KB (none: all at once).
    Bytes(u16, Vec<(&'static str, String)>, Vec<u8>, Option<Duration>),
    /// Headers, then pieces of the body forever, without a length.
    Endless,
    /// Headers announcing `0` bytes, `1` of them sent, then the connection is closed.
    Short(usize, usize),
    /// Headers announcing `0` bytes, `1` of them sent, then nothing for a long time.
    Stall(usize, usize),
    /// Nothing at all for a long time.
    Hang,
}

struct Site {
    origin: Origin,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Site {
    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
    fn lines(&self) -> Vec<String> {
        self.seen().into_iter().map(|s| s.line).collect()
    }
}

fn read_head(conn: &TcpStream) -> Option<Seen> {
    let mut r = BufReader::new(conn.try_clone().ok()?);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let line = line.trim_end().to_string();
    let mut headers = BTreeMap::new();
    loop {
        let mut h = String::new();
        r.read_line(&mut h).ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':')?;
        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
    }
    Some(Seen { line, headers })
}

fn head(status: u16, extra: &[(&'static str, String)], length: Option<usize>) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status} X\r\nConnection: close\r\n");
    if let Some(n) = length {
        out.push_str(&format!("Content-Length: {n}\r\n"));
    }
    for (k, v) in extra {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    out.into_bytes()
}

/// A stand-in site on 127.0.0.1 that answers each request with `serve(request line)`.
fn site(serve: impl Fn(&str) -> Serve + Send + Sync + 'static) -> Site {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let serve = Arc::new(serve);
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let Some(req) = read_head(&conn) else { continue };
            log.lock().unwrap().push(req.clone());
            let serve = serve.clone();
            std::thread::spawn(move || match serve(&req.line) {
                Serve::Bytes(status, extra, body, pace) => {
                    let _ = conn.write_all(&head(status, &extra, Some(body.len())));
                    for piece in body.chunks(16 * 1024) {
                        if conn.write_all(piece).is_err() {
                            return;
                        }
                        if let Some(p) = pace {
                            std::thread::sleep(p);
                        }
                    }
                    let _ = conn.flush();
                }
                Serve::Endless => {
                    let _ = conn.write_all(&head(200, &[], None));
                    let piece = vec![7u8; 16 * 1024];
                    for _ in 0..100_000 {
                        if conn.write_all(&piece).is_err() {
                            return;
                        }
                    }
                }
                Serve::Short(announced, sent) => {
                    let _ = conn.write_all(&head(200, &[], Some(announced)));
                    let _ = conn.write_all(&vec![1u8; sent]);
                    let _ = conn.flush();
                }
                Serve::Stall(announced, sent) => {
                    let _ = conn.write_all(&head(200, &[], Some(announced)));
                    let _ = conn.write_all(&vec![1u8; sent]);
                    let _ = conn.flush();
                    std::thread::sleep(Duration::from_secs(10));
                }
                Serve::Hang => std::thread::sleep(Duration::from_secs(10)),
            });
        }
    });
    Site { origin: Origin::stub(&base), seen }
}

fn json(status: u16, body: &str) -> Serve {
    Serve::Bytes(status, vec![("Content-Type", "application/json".into())], body.as_bytes().to_vec(), None)
}

fn v(s: &str) -> Version {
    Version::parse(s).unwrap_or_else(|| panic!("{s} is a version"))
}

const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn release_json(version: &str, file: &str, size: u64, sha: &str) -> String {
    format!(r#"{{"version":"{version}","file":"{file}","size":{size},"sha256":"{sha}"}}"#)
}

fn sha_of(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn installer_bytes(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 31 % 251) as u8).collect()
}

fn release_of(bytes: &[u8]) -> Release {
    Release { version: v("0.2.35"), size: bytes.len() as u64, sha256: sha_of(bytes) }
}

/// The quick timeouts the tests use, so a site that does not answer fails within a second.
const QUICK: Timeouts = Timeouts { connect: Duration::from_millis(500), whole: Duration::from_millis(800), stall: Duration::from_millis(400) };

// ---- Versions ----

#[test]
fn versions_compare_by_number_part_by_part_never_as_text() {
    assert!(v("0.2.10") > v("0.2.9"));
    assert!(v("0.10.0") > v("0.9.99"));
    assert!(v("1.0.0") > v("0.99.99"));
    assert!(v("0.2.35") > v("0.2.34"));
    assert_eq!(v("0.2.34"), v("0.2.34"));
    assert!(v("0.2.33") < v("0.2.34"));
    assert_eq!(v("0.2.35").to_string(), "0.2.35");
    assert_eq!(v("0.0.0").to_string(), "0.0.0");
}

#[test]
fn a_version_is_three_plain_decimal_numbers_and_nothing_else() {
    for bad in ["", "0.2", "0.2.35.1", "+0.2.35", " 0.2.35", "0.2.35 ", "0.2.35\n", "0..35", ".2.35", "0.2.", "0.2.x", "v0.2.35", "0.02.35", "00.2.35", "0.2.-1", "0.2.+1", "0,2,35", "0.2.3a", "0.2.٣", "0.2.１", "0.2.12345678901", "0.2.35/..", "0.2.35-beta"] {
        assert_eq!(Version::parse(bad), None, "{bad:?}");
    }
    assert_eq!(Version::running().to_string(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn the_changes_page_and_the_installer_are_named_from_the_version_alone() {
    assert_eq!(changes_url(v("0.2.35")), "https://mythicshot.com/changes/#v0-2-35");
    assert_eq!(installer_name(v("0.2.35")), "Mythicshot-0.2.35-x64.msi");
    assert_eq!(HOST, "https://mythicshot.com");
    assert_eq!(DOWNLOAD_PAGE, "https://mythicshot.com/download/");
    // Opened through the app's own helper, as the page and its place on it.
    assert_eq!(changes_anchor(v("0.2.35")), "v0-2-35");
    assert!(mythicshot_destinations::is_web_link(CHANGES_PAGE));
    assert_eq!(mythicshot_destinations::open_url_at(CHANGES_PAGE, &changes_anchor(v("0.2.35"))), Ok(()));
    assert_eq!(mythicshot_destinations::stubs::take(), ["open url https://mythicshot.com/changes/#v0-2-35"]);
}

// ---- The release file ----

#[test]
fn a_good_release_file_is_read_and_unknown_fields_are_ignored() {
    let text = release_json("0.2.35", "Mythicshot-0.2.35-x64.msi", 41_000_000, SHA);
    assert_eq!(parse_release(text.as_bytes()), Ok(Release { version: v("0.2.35"), size: 41_000_000, sha256: SHA.into() }));
    let more = r#"{"version":"0.2.35","notes":"<b>hi</b>","file":"Mythicshot-0.2.35-x64.msi","size":1048576,"sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","url":"https://elsewhere.example/x.msi"}"#;
    let r = parse_release(more.as_bytes()).unwrap();
    assert_eq!((r.size, r.file_name()), (1_048_576, "Mythicshot-0.2.35-x64.msi".to_string()));
}

#[test]
fn every_other_release_file_is_a_failed_check() {
    let good_file = "Mythicshot-0.2.35-x64.msi";
    let cases: Vec<String> = vec![
        // The file must be exactly the name the version gives.
        release_json("0.2.35", "Mythicshot-0.2.36-x64.msi", 2_000_000, SHA),
        release_json("0.2.35", "mythicshot-0.2.35-x64.msi", 2_000_000, SHA),
        release_json("0.2.35", "../Mythicshot-0.2.35-x64.msi", 2_000_000, SHA),
        release_json("0.2.35", "sub/Mythicshot-0.2.35-x64.msi", 2_000_000, SHA),
        release_json("0.2.35", "https://elsewhere.example/Mythicshot-0.2.35-x64.msi", 2_000_000, SHA),
        release_json("0.2.35", "Mythicshot-0.2.35-x64.msi?x=1", 2_000_000, SHA),
        release_json("0.2.35", "Mythicshot-0.2.35-x64.exe", 2_000_000, SHA),
        // A version that is not three plain numbers.
        release_json("+0.2.35", good_file, 2_000_000, SHA),
        release_json("0.2.35.1", good_file, 2_000_000, SHA),
        release_json(" 0.2.35", good_file, 2_000_000, SHA),
        // The hash: 64 lowercase hex digits.
        release_json("0.2.35", good_file, 2_000_000, &SHA.to_uppercase()),
        release_json("0.2.35", good_file, 2_000_000, &SHA[..63]),
        release_json("0.2.35", good_file, 2_000_000, &format!("{SHA}0")),
        release_json("0.2.35", good_file, 2_000_000, &SHA.replace('a', "g")),
        // The size: 1 MB to 64 MB.
        release_json("0.2.35", good_file, 1_048_575, SHA),
        release_json("0.2.35", good_file, 0, SHA),
        release_json("0.2.35", good_file, 67_108_865, SHA),
        format!(r#"{{"version":"0.2.35","file":"{good_file}","size":-5,"sha256":"{SHA}"}}"#),
        format!(r#"{{"version":"0.2.35","file":"{good_file}","size":2000000.5,"sha256":"{SHA}"}}"#),
        format!(r#"{{"version":"0.2.35","file":"{good_file}","size":"2000000","sha256":"{SHA}"}}"#),
        // Missing or wrongly typed fields, and what is not JSON at all.
        format!(r#"{{"version":"0.2.35","file":"{good_file}","sha256":"{SHA}"}}"#),
        format!(r#"{{"version":0.2,"file":"{good_file}","size":2000000,"sha256":"{SHA}"}}"#),
        "[]".into(),
        "<html>Not here</html>".into(),
        String::new(),
    ];
    for text in cases {
        assert!(parse_release(text.as_bytes()).is_err(), "{text}");
    }
    // The bounds themselves are allowed.
    for size in [1_048_576, 67_108_864] {
        assert!(parse_release(release_json("0.2.35", good_file, size, SHA).as_bytes()).is_ok(), "{size}");
    }
}

// ---- The check ----

#[test]
fn the_check_is_one_plain_get_that_says_nothing_but_the_version_it_runs() {
    let s = site(|_| json(200, &release_json("0.2.35", "Mythicshot-0.2.35-x64.msi", 2_000_000, SHA)));
    assert!(matches!(check_with(&s.origin, v("0.2.34"), QUICK), Ok(Checked::Newer(_))));
    let seen = s.seen();
    assert_eq!(seen.len(), 1, "one request, no retry");
    assert_eq!(seen[0].line, "GET /download/release.json HTTP/1.1", "no query string");
    let port = s.origin.0.rsplit(':').next().unwrap().to_string();
    let want: BTreeMap<String, String> = [("host".to_string(), format!("127.0.0.1:{port}")), ("user-agent".to_string(), format!("Mythicshot/{}", env!("CARGO_PKG_VERSION")))].into_iter().collect();
    assert_eq!(seen[0].headers, want, "no token, no cookie, no accept, nothing else");
}

#[test]
fn only_a_strictly_newer_version_is_offered() {
    let s = site(|_| json(200, &release_json("0.2.35", "Mythicshot-0.2.35-x64.msi", 2_000_000, SHA)));
    match check_with(&s.origin, v("0.2.34"), QUICK) {
        Ok(Checked::Newer(r)) => assert_eq!(r, Release { version: v("0.2.35"), size: 2_000_000, sha256: SHA.into() }),
        other => panic!("{other:?}"),
    }
    assert_eq!(check_with(&s.origin, v("0.2.35"), QUICK), Ok(Checked::UpToDate), "the same version");
    assert_eq!(check_with(&s.origin, v("0.2.36"), QUICK), Ok(Checked::UpToDate), "never a downgrade");
    assert_eq!(check_with(&s.origin, v("0.10.0"), QUICK), Ok(Checked::UpToDate), "0.10.0 is newer than 0.2.35");
    let s = site(|_| json(200, &release_json("0.2.10", "Mythicshot-0.2.10-x64.msi", 2_000_000, SHA)));
    assert!(matches!(check_with(&s.origin, v("0.2.9"), QUICK), Ok(Checked::Newer(_))), "0.2.10 is newer than 0.2.9");
}

#[test]
fn a_bad_answer_is_a_failed_check() {
    let big = format!(r#"{{"version":"0.2.35","file":"Mythicshot-0.2.35-x64.msi","size":2000000,"sha256":"{SHA}","pad":"{}"}}"#, "x".repeat(4096));
    let answers: Vec<Serve> = vec![
        json(404, "{}"),
        json(500, ""),
        json(200, "not json"),
        json(200, &release_json("0.2.35", "../evil.msi", 2_000_000, SHA)),
        json(200, &big),
        Serve::Bytes(302, vec![("Location", "https://elsewhere.example/release.json".into())], Vec::new(), None),
        Serve::Bytes(301, vec![("Location", "/download/other.json".into())], Vec::new(), None),
    ];
    for answer in answers {
        let answer = Mutex::new(Some(answer));
        let s = site(move |_| answer.lock().unwrap().take().unwrap_or(Serve::Hang));
        assert!(check_with(&s.origin, v("0.2.34"), QUICK).is_err());
        assert_eq!(s.lines(), ["GET /download/release.json HTTP/1.1"], "a redirect is never followed");
    }
}

#[test]
fn a_site_that_does_not_answer_fails_the_check_within_its_time() {
    let s = site(|_| Serve::Hang);
    let started = Instant::now();
    assert!(check_with(&s.origin, v("0.2.34"), QUICK).is_err());
    assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    // Nobody listening at all.
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    assert!(check_with(&Origin::stub(&format!("http://127.0.0.1:{port}")), v("0.2.34"), QUICK).is_err());
}

#[test]
fn the_real_check_has_a_short_whole_timeout_and_the_download_a_stall_timeout() {
    assert_eq!(CHECK.whole, Duration::from_secs(5));
    assert!(CHECK.connect <= CHECK.whole);
    assert!(DOWNLOAD.stall <= Duration::from_secs(60) && DOWNLOAD.whole >= Duration::from_secs(600), "a slow line still finishes; a stalled one stops");
}

#[test]
fn a_test_build_never_reaches_the_real_site() {
    let official = Origin::official();
    assert_eq!(official.0, HOST);
    assert!(check_with(&official, v("0.2.34"), QUICK).is_err());
    let dir = tempfile::tempdir().unwrap();
    let r = release_of(b"x");
    assert!(matches!(download_with(&official, &r, dir.path(), 1, &AtomicU64::new(0), &AtomicBool::new(false), QUICK), Downloaded::Failed(_)));
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
}

#[test]
fn the_client_follows_no_redirect_and_trusts_the_system_store() {
    let config = agent(&Origin::official(), CHECK);
    let config = config.config();
    assert_eq!(config.max_redirects(), 0);
    assert!(matches!(config.tls_config().root_certs(), ureq::tls::RootCerts::PlatformVerifier));
    // The plain address of a test site is never handed to a proxy.
    assert!(agent(&Origin::stub("http://127.0.0.1:1"), CHECK).config().proxy().is_none());
}

// ---- The download ----

fn names(dir: &Path) -> Vec<String> {
    let mut n: Vec<String> = std::fs::read_dir(dir).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    n.sort();
    n
}

#[test]
fn a_download_that_matches_is_kept_under_the_name_the_app_gives_it() {
    let bytes = installer_bytes(200_000);
    let served = bytes.clone();
    let s = site(move |_| Serve::Bytes(200, vec![], served.clone(), None));
    let dir = tempfile::tempdir().unwrap();
    let updates = dir.path().join("updates");
    let done = AtomicU64::new(0);
    let got = download_with(&s.origin, &release_of(&bytes), &updates, 1, &done, &AtomicBool::new(false), QUICK);
    let path = updates.join("Mythicshot-0.2.35-x64.msi");
    assert_eq!(got, Downloaded::Ready(path.clone()));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(names(&updates), ["Mythicshot-0.2.35-x64.msi"], "no partial file is left");
    assert_eq!(done.load(Ordering::SeqCst), bytes.len() as u64);
    let seen = s.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].line, "GET /download/Mythicshot-0.2.35-x64.msi HTTP/1.1");
    assert_eq!(seen[0].headers.keys().cloned().collect::<Vec<_>>(), ["host", "user-agent"]);
}

#[test]
fn a_download_whose_hash_differs_is_deleted() {
    let bytes = installer_bytes(100_000);
    let mut other = bytes.clone();
    other[500] ^= 1;
    let s = site(move |_| Serve::Bytes(200, vec![], other.clone(), None));
    let dir = tempfile::tempdir().unwrap();
    let got = download_with(&s.origin, &release_of(&bytes), dir.path(), 1, &AtomicU64::new(0), &AtomicBool::new(false), QUICK);
    assert_eq!(got, Downloaded::Mismatch);
    assert!(names(dir.path()).is_empty(), "{:?}", names(dir.path()));
}

#[test]
fn a_download_larger_than_listed_stops_as_soon_as_more_arrives() {
    let bytes = installer_bytes(100_000);
    // Announced larger than listed: nothing is read.
    let mut more = bytes.clone();
    more.push(0);
    let s = site(move |_| Serve::Bytes(200, vec![], more.clone(), None));
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(download_with(&s.origin, &release_of(&bytes), dir.path(), 1, &AtomicU64::new(0), &AtomicBool::new(false), QUICK), Downloaded::Mismatch);
    assert!(names(dir.path()).is_empty());
    // No length at all, and bytes without end: stopped at the listed size, not at the end.
    let s = site(|_| Serve::Endless);
    let done = AtomicU64::new(0);
    let started = Instant::now();
    assert_eq!(download_with(&s.origin, &release_of(&bytes), dir.path(), 2, &done, &AtomicBool::new(false), QUICK), Downloaded::Mismatch);
    assert!(done.load(Ordering::SeqCst) <= bytes.len() as u64, "never counted past the listed size");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(names(dir.path()).is_empty());
}

#[test]
fn a_download_smaller_than_listed_is_not_kept() {
    let bytes = installer_bytes(100_000);
    let fewer = bytes[..99_999].to_vec();
    let s = site(move |_| Serve::Bytes(200, vec![], fewer.clone(), None));
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(download_with(&s.origin, &release_of(&bytes), dir.path(), 1, &AtomicU64::new(0), &AtomicBool::new(false), QUICK), Downloaded::Mismatch);
    assert!(names(dir.path()).is_empty());
}

#[test]
fn a_connection_that_breaks_or_stalls_is_a_failed_download() {
    let bytes = installer_bytes(100_000);
    let r = release_of(&bytes);
    let dir = tempfile::tempdir().unwrap();
    let s = site(|_| Serve::Short(100_000, 30_000));
    assert!(matches!(download_with(&s.origin, &r, dir.path(), 1, &AtomicU64::new(0), &AtomicBool::new(false), QUICK), Downloaded::Failed(_)));
    assert!(names(dir.path()).is_empty());
    let s = site(|_| Serve::Stall(100_000, 30_000));
    let started = Instant::now();
    let done = AtomicU64::new(0);
    assert!(matches!(download_with(&s.origin, &r, dir.path(), 2, &done, &AtomicBool::new(false), QUICK), Downloaded::Failed(_)));
    assert!(started.elapsed() < Duration::from_secs(4), "the stall bound stops it: {:?}", started.elapsed());
    assert!(names(dir.path()).is_empty());
    for status in [404, 500, 302] {
        let s = site(move |_| Serve::Bytes(status, vec![("Location", "https://elsewhere.example/x.msi".into())], b"no".to_vec(), None));
        assert!(matches!(download_with(&s.origin, &r, dir.path(), 3, &AtomicU64::new(0), &AtomicBool::new(false), QUICK), Downloaded::Failed(_)), "{status}");
        assert_eq!(s.lines().len(), 1, "{status}: no redirect is followed");
        assert!(names(dir.path()).is_empty());
    }
}

#[test]
fn cancel_stops_the_download_and_deletes_the_partial_file() {
    let bytes = installer_bytes(400_000);
    let served = bytes.clone();
    let s = site(move |_| Serve::Bytes(200, vec![], served.clone(), Some(Duration::from_millis(40))));
    let dir = tempfile::tempdir().unwrap();
    let (done, cancel) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicBool::new(false)));
    let (d, c) = (done.clone(), cancel.clone());
    let watcher = std::thread::spawn(move || {
        while d.load(Ordering::SeqCst) == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
        c.store(true, Ordering::SeqCst);
    });
    let got = download_with(&s.origin, &release_of(&bytes), dir.path(), 1, &done, &cancel, QUICK);
    watcher.join().unwrap();
    assert_eq!(got, Downloaded::Cancelled);
    assert!(names(dir.path()).is_empty(), "{:?}", names(dir.path()));
}

// ---- The updates folder ----

#[test]
fn a_test_build_keeps_its_updates_folder_in_its_own_temp_folder() {
    assert_eq!(updates_dir(), Some(crate::settings::test_data_dir().join("updates")));
}

#[test]
fn old_installers_are_removed_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    for name in ["Mythicshot-0.2.34-x64.msi", "Mythicshot-0.2.35-x64.msi", "Mythicshot-0.2.35-x64.msi.3.partial", "notes.txt", "Mythicshot-0.2.35-x64.msi.txt", "other.msi", "Mythicshot-evil-x64.msi", "Mythicshot-0.02.35-x64.msi"] {
        std::fs::write(d.join(name), b"x").unwrap();
    }
    std::fs::create_dir(d.join("Mythicshot-0.2.33-x64.msi")).unwrap();
    assert_eq!(clean(d), 3);
    assert_eq!(names(d), ["Mythicshot-0.02.35-x64.msi", "Mythicshot-0.2.33-x64.msi", "Mythicshot-0.2.35-x64.msi.txt", "Mythicshot-evil-x64.msi", "notes.txt", "other.msi"]);
    assert_eq!(clean(&d.join("missing")), 0);
}

// ---- Starting the installer ----

#[derive(Default)]
struct FakeLaunch {
    system: Option<PathBuf>,
    started: Mutex<Vec<(PathBuf, Vec<OsString>)>>,
}

impl Launch for FakeLaunch {
    fn system_dir(&self) -> Option<PathBuf> {
        self.system.clone()
    }
    fn spawn(&self, program: &Path, args: &[OsString]) -> std::io::Result<()> {
        self.started.lock().unwrap().push((program.to_path_buf(), args.to_vec()));
        Ok(())
    }
}

fn verified_on_disk(bytes: &[u8]) -> (tempfile::TempDir, PathBuf, Release) {
    let dir = tempfile::tempdir().unwrap();
    let r = release_of(bytes);
    let path = dir.path().join(r.file_name());
    std::fs::write(&path, bytes).unwrap();
    (dir, path, r)
}

#[test]
fn msiexec_from_the_system_folder_gets_the_verified_file_as_real_arguments() {
    let (dir, path, r) = verified_on_disk(&installer_bytes(5000));
    let sys = dir.path().join("System32");
    let launch = FakeLaunch { system: Some(sys.clone()), ..Default::default() };
    let held = open_verified(&path, &r).unwrap();
    start_installer(&held, &launch).unwrap();
    let started = launch.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].0, sys.join("msiexec.exe"));
    let want: Vec<OsString> = vec!["/i".into(), path.clone().into_os_string(), "/passive".into(), "/norestart".into(), "MYTHICSHOT_RELAUNCH=1".into()];
    assert_eq!(started[0].1, want);
}

#[test]
fn a_file_that_changed_since_it_was_checked_is_never_started() {
    let (_dir, path, r) = verified_on_disk(&installer_bytes(5000));
    let mut changed = installer_bytes(5000);
    changed[10] ^= 1;
    std::fs::write(&path, &changed).unwrap();
    assert!(open_verified(&path, &r).is_err());
    // Another size, another name, a folder, a missing file.
    std::fs::write(&path, installer_bytes(4999)).unwrap();
    assert!(open_verified(&path, &r).is_err());
    let other = path.with_file_name("Mythicshot-0.2.36-x64.msi");
    std::fs::write(&other, installer_bytes(5000)).unwrap();
    assert!(open_verified(&other, &r).is_err(), "only the name the version gives");
    std::fs::remove_file(&path).unwrap();
    assert!(open_verified(&path, &r).is_err());
    std::fs::create_dir(&path).unwrap();
    assert!(open_verified(&path, &r).is_err());
}

#[test]
fn without_a_full_system_folder_nothing_is_started() {
    let (_dir, path, r) = verified_on_disk(&installer_bytes(5000));
    let held = open_verified(&path, &r).unwrap();
    for system in [None, Some(PathBuf::from("System32"))] {
        let launch = FakeLaunch { system, ..Default::default() };
        assert!(start_installer(&held, &launch).is_err());
        assert!(launch.started.lock().unwrap().is_empty());
    }
}

#[cfg(windows)]
#[test]
fn while_it_is_held_the_verified_file_cannot_be_changed_moved_or_deleted() {
    let (_dir, path, r) = verified_on_disk(&installer_bytes(5000));
    let held = open_verified(&path, &r).unwrap();
    assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err(), "no writer");
    assert!(std::fs::rename(&path, path.with_extension("old")).is_err(), "no rename");
    assert!(std::fs::remove_file(&path).is_err(), "no delete");
    // Reading it, as msiexec does, still works.
    assert_eq!(std::fs::read(&path).unwrap(), installer_bytes(5000));
    drop(held);
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn an_unsigned_installer_is_allowed_until_a_certificate_exists() {
    let (_dir, path, _r) = verified_on_disk(&installer_bytes(10));
    assert_eq!(check_signature(&path), SignatureCheck::NotSignedAllowed);
    assert!(SignatureCheck::NotSignedAllowed.allows());
}

#[cfg(windows)]
#[test]
fn the_system_folder_is_asked_of_windows_as_a_full_path() {
    let sys = WindowsLaunch.system_dir().expect("Windows names its system folder");
    assert!(sys.is_absolute(), "{sys:?}");
    assert!(sys.ends_with("System32") || sys.ends_with("system32"), "{sys:?}");
}

#[test]
fn log_lines_name_the_step_and_nothing_personal() {
    let line = log_line("2026-11-24 18:13:00.000", "check: 0.2.35 offered");
    assert_eq!(line, "== 2026-11-24 18:13:00.000 update · check: 0.2.35 offered");
}
