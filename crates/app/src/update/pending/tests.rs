//! The note an in-app update leaves for the next start: what it holds, how it is read, and
//! what the start makes of it. Every file here is in a temporary folder of the test's own.

use std::time::{Duration, SystemTime};

use super::*;

fn v(s: &str) -> Version {
    Version::parse(s).unwrap_or_else(|| panic!("{s} is a version"))
}

fn note(from: &str, to: &str) -> Pending {
    Pending { from: v(from), to: v(to) }
}

const FRESH: Option<Duration> = Some(Duration::from_secs(90));

// ---- The decision ----

#[test]
fn the_new_version_running_is_an_update_that_worked() {
    let text = note("0.2.38", "0.2.39").text();
    assert_eq!(decide(Some(text.as_bytes()), FRESH, v("0.2.39")), Some(AfterUpdate::Updated { from: v("0.2.38"), to: v("0.2.39") }));
}

#[test]
fn the_old_version_running_is_an_update_that_did_not_finish() {
    let text = note("0.2.38", "0.2.39").text();
    assert_eq!(decide(Some(text.as_bytes()), FRESH, v("0.2.38")), Some(AfterUpdate::NotFinished { from: v("0.2.38") }));
}

#[test]
fn any_other_version_running_shows_nothing() {
    let text = note("0.2.38", "0.2.39").text();
    for running in ["0.2.37", "0.2.40", "0.3.0", "1.0.0"] {
        assert_eq!(decide(Some(text.as_bytes()), FRESH, v(running)), None, "{running}");
    }
}

#[test]
fn no_note_shows_nothing() {
    assert_eq!(decide(None, FRESH, v("0.2.39")), None);
    assert_eq!(decide(None, None, v("0.2.39")), None);
}

#[test]
fn a_note_that_is_not_what_the_app_writes_shows_nothing() {
    let bad: &[&[u8]] = &[
        b"",
        b"\xff\xfe\x00garbage",
        b"not toml at all",
        b"from = \"0.2.38\"\n",
        b"to = \"0.2.39\"\n",
        b"from = 38\nto = 39\n",
        b"from = \"0.2.38\"\nto = \"0.2.39\"\nwhere = \"C:\\\\\"\n",
        b"from = \"0.2.38\"\nto = \"0.2.39\"\nto = \"0.2.40\"\n",
        b"[pending]\nfrom = \"0.2.38\"\nto = \"0.2.39\"\n",
        // The same version twice, or backwards: an update goes to a newer version.
        b"from = \"0.2.39\"\nto = \"0.2.39\"\n",
        b"from = \"0.2.39\"\nto = \"0.2.38\"\n",
    ];
    for bytes in bad {
        for running in ["0.2.38", "0.2.39"] {
            assert_eq!(decide(Some(bytes), FRESH, v(running)), None, "{:?} at {running}", String::from_utf8_lossy(bytes));
        }
        assert_eq!(Pending::parse(bytes), None, "{:?}", String::from_utf8_lossy(bytes));
    }
}

#[test]
fn versions_in_the_note_meet_the_update_codes_strict_rule() {
    for (from, to) in [("v0.2.38", "0.2.39"), ("0.2", "0.2.39"), ("0.2.38", "0.2.39.1"), (" 0.2.38", "0.2.39"), ("0.2.38", "0.2.39 "), ("+0.2.38", "0.2.39"), ("0.2.038", "0.2.39"), ("0.2.38", "0.2.-1"), ("0.2.38", "1234567890.0.0"), ("0.2.38", "")] {
        let text = format!("from = \"{from}\"\nto = \"{to}\"\n");
        assert_eq!(Pending::parse(text.as_bytes()), None, "{text}");
    }
    // What the version rule allows, the note allows.
    assert_eq!(Pending::parse(b"from = \"0.9.99\"\nto = \"0.10.0\"\n"), Some(note("0.9.99", "0.10.0")));
}

#[test]
fn an_oversized_note_shows_nothing() {
    // A note the app wrote, padded past the cap with a TOML comment.
    let mut text = note("0.2.38", "0.2.39").text();
    while text.len() as u64 <= MAX_BYTES {
        text.push_str("# padding\n");
    }
    assert_eq!(decide(Some(text.as_bytes()), FRESH, v("0.2.39")), None);
    // At the cap it is still read.
    let mut text = note("0.2.38", "0.2.39").text();
    text.push_str(&"#".repeat(MAX_BYTES as usize - text.len()));
    assert_eq!(text.len() as u64, MAX_BYTES);
    assert!(decide(Some(text.as_bytes()), FRESH, v("0.2.39")).is_some());
}

#[test]
fn a_note_older_than_a_day_or_of_unknown_age_shows_nothing() {
    let text = note("0.2.38", "0.2.39").text();
    assert!(decide(Some(text.as_bytes()), Some(Duration::ZERO), v("0.2.39")).is_some());
    assert!(decide(Some(text.as_bytes()), Some(KEEP_FOR), v("0.2.39")).is_some(), "a day is still fresh");
    assert_eq!(decide(Some(text.as_bytes()), Some(KEEP_FOR + Duration::from_secs(1)), v("0.2.39")), None);
    assert_eq!(decide(Some(text.as_bytes()), Some(KEEP_FOR + Duration::from_secs(1)), v("0.2.38")), None);
    assert_eq!(decide(Some(text.as_bytes()), None, v("0.2.39")), None);
}

#[test]
fn the_age_is_how_long_ago_the_note_was_written() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    assert_eq!(age(now - Duration::from_secs(60), now), Some(Duration::from_secs(60)));
    assert_eq!(age(now, now), Some(Duration::ZERO));
    // A clock set back a little since it was written: just now.
    assert_eq!(age(now + Duration::from_secs(30), now), Some(Duration::ZERO));
    // Far in the future: not a note this start can place in time.
    assert_eq!(age(now + FUTURE_SLACK + Duration::from_secs(1), now), None);
}

// ---- What is written ----

#[test]
fn the_note_holds_the_two_versions_and_nothing_else() {
    let p = note("0.2.38", "0.2.39");
    assert_eq!(p.text(), "from = \"0.2.38\"\nto = \"0.2.39\"\n");
    assert_eq!(Pending::parse(p.text().as_bytes()), Some(p));
    assert!(p.text().len() as u64 <= MAX_BYTES);
}

#[test]
fn the_note_lives_beside_the_settings_file() {
    assert_eq!(path(), crate::settings::Settings::path().with_file_name(FILE_NAME));
    assert!(path().starts_with(crate::settings::test_data_dir()), "a test build never touches the user's folder");
}

#[test]
fn the_note_is_written_whole_or_not_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let at = dir.path().join("config").join(FILE_NAME);
    write(&at, note("0.2.38", "0.2.39")).unwrap();
    assert_eq!(std::fs::read_to_string(&at).unwrap(), note("0.2.38", "0.2.39").text());
    // Written again: replaced, and no temporary file is left beside it.
    write(&at, note("0.2.39", "0.2.40")).unwrap();
    assert_eq!(std::fs::read_to_string(&at).unwrap(), note("0.2.39", "0.2.40").text());
    let names: Vec<String> = std::fs::read_dir(at.parent().unwrap()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, [FILE_NAME]);
}

// ---- Reading it at start ----

#[test]
fn the_note_is_read_once_and_removed() {
    let dir = tempfile::tempdir().unwrap();
    let at = dir.path().join(FILE_NAME);
    write(&at, note("0.2.38", "0.2.39")).unwrap();
    let now = SystemTime::now();
    assert_eq!(take(&at, now, v("0.2.39")), Some(AfterUpdate::Updated { from: v("0.2.38"), to: v("0.2.39") }));
    assert!(!at.exists(), "removed as soon as it is read");
    assert_eq!(take(&at, now, v("0.2.39")), None, "the popup shows once");
    // The did-not-finish case is removed just the same.
    write(&at, note("0.2.38", "0.2.39")).unwrap();
    assert_eq!(take(&at, now, v("0.2.38")), Some(AfterUpdate::NotFinished { from: v("0.2.38") }));
    assert!(!at.exists());
}

#[test]
fn a_missing_note_is_nothing() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(take(&dir.path().join(FILE_NAME), SystemTime::now(), v("0.2.39")), None);
    assert_eq!(take(&dir.path().join("no folder").join(FILE_NAME), SystemTime::now(), v("0.2.39")), None);
}

#[test]
fn a_note_for_another_version_corrupt_or_oversized_is_removed_silently() {
    let dir = tempfile::tempdir().unwrap();
    let at = dir.path().join(FILE_NAME);
    let big = vec![b'#'; 4 * 1024 * 1024];
    let cases: Vec<Vec<u8>> = vec![note("0.2.38", "0.2.39").text().into_bytes(), b"\x00\x01 not a note".to_vec(), big];
    for (i, bytes) in cases.iter().enumerate() {
        std::fs::write(&at, bytes).unwrap();
        assert_eq!(take(&at, SystemTime::now(), v("0.3.0")), None, "case {i}");
        assert!(!at.exists(), "case {i} removed");
    }
}

#[test]
fn a_stale_note_is_removed_silently() {
    let dir = tempfile::tempdir().unwrap();
    let at = dir.path().join(FILE_NAME);
    write(&at, note("0.2.38", "0.2.39")).unwrap();
    let written = std::fs::metadata(&at).unwrap().modified().unwrap();
    // A start more than a day after it was written.
    assert_eq!(take(&at, written + KEEP_FOR + Duration::from_secs(60), v("0.2.39")), None);
    assert!(!at.exists());
}

#[test]
fn something_else_at_the_notes_place_never_stops_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let at = dir.path().join(FILE_NAME);
    std::fs::create_dir(&at).unwrap();
    std::fs::write(at.join("inside.txt"), b"x").unwrap();
    assert_eq!(take(&at, SystemTime::now(), v("0.2.39")), None);
    assert!(at.join("inside.txt").exists(), "a folder that is not the app's note is left alone");
}
