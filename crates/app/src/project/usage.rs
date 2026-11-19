//! What the saved recordings take, in words and numbers: the limit the user sets, whether the
//! total is past it, and the texts of the usage line and of a row in the list. Pure functions
//! only; nothing here reads the disk, and nothing here ever removes anything.

use super::store::{ConvertState, Form, Problem, ProjectInfo};
use crate::publish_ui::{short_title, size_text};

/// The highest limit that can be set, in GB: four digits.
pub const LIMIT_MAX_GB: u32 = 9999;

/// How many characters of a title the remove question shows.
const QUESTION_TITLE_MAX: usize = 60;

const GB: u64 = 1 << 30;

/// The limit typed into the field, in GB. Empty means no warning, as 0 does. `None` for anything
/// that is not a whole number of one to four digits: the caller keeps the value it had.
pub fn parse_limit(text: &str) -> Option<u32> {
    let text = text.trim();
    if text.is_empty() {
        return Some(0);
    }
    // Digits only, so no sign, no decimal and no other script's numerals; four at most, so it fits.
    let digits = text.len() <= 4 && text.bytes().all(|b| b.is_ascii_digit());
    digits.then(|| text.parse().ok()).flatten()
}

/// Whether the saved recordings take more than the limit. Without a limit they never do.
pub fn over_limit(bytes: u64, limit_gb: u32) -> bool {
    limit_gb > 0 && bytes > u64::from(limit_gb) * GB
}

/// Whether going from `before` to `after` crossed the limit: under or at it before, over it after.
// The publish result screen is the caller; until it asks, only the tests do.
#[cfg_attr(not(test), allow(dead_code))]
pub fn pushed_over(before: u64, after: u64, limit_gb: u32) -> bool {
    !over_limit(before, limit_gb) && over_limit(after, limit_gb)
}

/// The line under the usage bar: how many recordings, what they take, and of how much when a
/// limit is set.
pub fn usage_line(count: usize, bytes: u64, limit_gb: u32) -> String {
    let size = size_text(bytes);
    let of = if limit_gb > 0 { format!(" of {limit_gb} GB") } else { String::new() };
    match count {
        0 => "No saved recordings".to_string(),
        1 => format!("1 saved recording · {size}{of}"),
        n => format!("{n} saved recordings · {size}{of}"),
    }
}

/// How full the usage bar is, 0.0 to 1.0. `None` without a limit: there is no bar then.
pub fn usage_fraction(bytes: u64, limit_gb: u32) -> Option<f32> {
    (limit_gb > 0).then(|| (bytes as f64 / (u64::from(limit_gb) * GB) as f64).min(1.0) as f32)
}

/// What is said when the saved recordings take more than the limit.
pub fn over_limit_sentence(bytes: u64, limit_gb: u32) -> String {
    format!("Saved recordings take {}, more than the {limit_gb} GB you set. Nothing is removed automatically: remove recordings you no longer need in Preferences.", size_text(bytes))
}

/// The form column of a row. `converting`: this project is the one being converted right now.
/// `will_convert`: the converter would take it (the switch is on, ffmpeg was found, and the
/// recording is large enough); without that, "yet" would promise something that never comes.
pub fn form_text(form: Form, convert: ConvertState, converting: bool, will_convert: bool) -> &'static str {
    match (form, convert) {
        (Form::Video, _) => "video",
        (Form::Frames, _) if converting => "compressing…",
        // A try that ended with the frames kept is not made again by this version.
        (Form::Frames, ConvertState::Failed | ConvertState::NotSmaller) => "not compressed",
        (Form::Frames, ConvertState::NotTried | ConvertState::Done) if will_convert => "not compressed yet",
        (Form::Frames, ConvertState::NotTried | ConvertState::Done) => "not compressed",
    }
}

/// The question before a recording is removed.
pub fn remove_question(title: &str) -> String {
    format!("Remove the recording of \"{}\"? The tour stays online, but it can no longer be edited from this computer.", short_title(title, QUESTION_TITLE_MAX))
}

/// What a row of the list is called: the tour's title, and the chapter's after it only when
/// several saved projects share the tour (`shared_tour`).
pub fn row_title(info: &ProjectInfo, shared_tour: bool) -> String {
    let (tour, chapter) = (info.tour_title.as_str(), info.title.as_str());
    // A project file that cannot be read may name only one of the two, or neither.
    match (tour.is_empty(), chapter.is_empty()) {
        (true, true) => UNTITLED.to_string(),
        (true, false) => chapter.to_string(),
        (false, false) if shared_tour => format!("{tour} · {chapter}"),
        (false, _) => tour.to_string(),
    }
}

/// What a row is called when its project file names no title at all.
pub const UNTITLED: &str = "Untitled recording";

/// The form column of a recording that cannot be opened: what is wrong with it.
pub fn problem_text(problem: Problem) -> &'static str {
    match problem {
        Problem::Damaged => "damaged",
        Problem::Newer => "saved by a newer version",
        Problem::FramesMissing => "frames missing",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectKey;

    const MB: u64 = 1 << 20;

    #[test]
    fn empty_and_0_mean_no_warning() {
        for text in ["", "   ", "\t", "0", " 0 ", "00", "0000"] {
            assert_eq!(parse_limit(text), Some(0), "{text:?}");
        }
        // No limit: nothing is ever over it, there is no bar, and the line names no limit.
        for bytes in [0, 1, 5 * GB, u64::MAX] {
            assert!(!over_limit(bytes, 0) && !pushed_over(0, bytes, 0));
            assert_eq!(usage_fraction(bytes, 0), None);
        }
        // A whole number of one to four digits is the limit, with spaces around it and zeros before it.
        for (text, gb) in [("1", 1), ("5", 5), (" 12 ", 12), ("007", 7), ("0050", 50), ("9999", 9999), ("5\n", 5)] {
            assert_eq!(parse_limit(text), Some(gb), "{text:?}");
        }
        assert_eq!(LIMIT_MAX_GB, 9999);
    }

    #[test]
    fn letters_a_decimal_a_minus_sign_and_five_digits_are_not_a_limit() {
        let not = [
            "five", "5 GB", "5GB", "GB", "5.0", "5,5", "2.5", ".5", "-1", "-0", "+5", "10000", "00005", "99999", "4294967296", "18446744073709551616", "1e3", "0x10", "5 5", "1_000", "١٢", "５", "²", "5;rm", "\u{0}",
            "saved_recordings_warn_gb = 5",
        ];
        for text in not {
            assert_eq!(parse_limit(text), None, "{text:?}");
        }
        // A long pasted string is refused as quickly as a short one.
        assert_eq!(parse_limit(&"9".repeat(100_000)), None);
    }

    #[test]
    fn exactly_the_limit_is_not_over_it() {
        assert!(!over_limit(5 * GB, 5));
        assert!(over_limit(5 * GB + 1, 5));
        assert!(!over_limit(5 * GB - 1, 5) && !over_limit(0, 5));
        assert!(!over_limit(GB, 1) && over_limit(GB + 1, 1));
        // The largest limit, and the largest size, without overflow.
        assert!(!over_limit(9999 * GB, LIMIT_MAX_GB) && over_limit(9999 * GB + 1, LIMIT_MAX_GB));
        assert!(over_limit(u64::MAX, u32::MAX));
        // The bar: empty, part full, full at the limit and never more than full.
        assert_eq!(usage_fraction(0, 5), Some(0.0));
        assert_eq!(usage_fraction(GB, 4), Some(0.25));
        assert_eq!(usage_fraction(5 * GB, 5), Some(1.0));
        assert_eq!(usage_fraction(6 * GB, 5), Some(1.0));
        assert_eq!(usage_fraction(u64::MAX, 1), Some(1.0));
        assert_eq!(usage_fraction(u64::MAX, u32::MAX), Some(1.0));
    }

    #[test]
    fn a_publish_pushes_over_only_when_it_crosses() {
        // Under before, over after: this publish did it.
        assert!(pushed_over(4 * GB, 5 * GB + 1, 5));
        assert!(pushed_over(5 * GB, 5 * GB + 1, 5), "exactly at the limit was not over it");
        assert!(pushed_over(0, 6 * GB, 5));
        // Already over, still under, or exactly reaching it: nothing to say.
        assert!(!pushed_over(5 * GB + 1, 7 * GB, 5));
        assert!(!pushed_over(GB, 2 * GB, 5));
        assert!(!pushed_over(4 * GB, 5 * GB, 5));
        // A save that made the total smaller, and no limit at all.
        assert!(!pushed_over(6 * GB, 4 * GB, 5) && !pushed_over(6 * GB, 5 * GB + 1, 5));
        assert!(!pushed_over(0, u64::MAX, 0));
    }

    #[test]
    fn the_usage_line_names_the_limit_only_when_there_is_one() {
        assert_eq!(usage_line(3, 1_288_490_189, 5), "3 saved recordings · 1.2 GB of 5 GB");
        assert_eq!(usage_line(3, 1_288_490_189, 0), "3 saved recordings · 1.2 GB");
        assert_eq!(usage_line(1, 172 * MB, 0), "1 saved recording · 172 MB");
        assert_eq!(usage_line(1, 172 * MB, 5), "1 saved recording · 172 MB of 5 GB");
        assert_eq!(usage_line(0, 0, 5), "No saved recordings");
        assert_eq!(usage_line(0, 0, 0), "No saved recordings");
        // Past the limit the line still only counts; the warning is its own sentence.
        assert_eq!(usage_line(40, 5_798_205_850, 5), "40 saved recordings · 5.4 GB of 5 GB");
        assert_eq!(usage_line(2, 3 * MB / 2, 9999), "2 saved recordings · 1.5 MB of 9999 GB");
        // Sizes are written as everywhere else in the app.
        for bytes in [0, 860_160, 13_002_342, 104_857_599, 1_073_741_823, 13_314_398_618] {
            assert_eq!(usage_line(2, bytes, 0), format!("2 saved recordings · {}", size_text(bytes)));
        }
    }

    #[test]
    fn the_over_limit_sentence_is_the_planned_one() {
        assert_eq!(over_limit_sentence(5_798_205_850, 5), "Saved recordings take 5.4 GB, more than the 5 GB you set. Nothing is removed automatically: remove recordings you no longer need in Preferences.");
        assert_eq!(over_limit_sentence(1_288_490_189, 1), "Saved recordings take 1.2 GB, more than the 1 GB you set. Nothing is removed automatically: remove recordings you no longer need in Preferences.");
    }

    #[test]
    fn the_form_reads_video_not_compressed_yet_or_compressing() {
        assert_eq!(form_text(Form::Video, ConvertState::Done, false, true), "video");
        assert_eq!(form_text(Form::Frames, ConvertState::NotTried, false, true), "not compressed yet");
        assert_eq!(form_text(Form::Frames, ConvertState::NotTried, true, true), "compressing…");
        // A video is a video whatever else is said about it.
        for convert in [ConvertState::NotTried, ConvertState::Failed, ConvertState::NotSmaller, ConvertState::Done] {
            for (converting, will_convert) in [(false, false), (false, true), (true, false), (true, true)] {
                assert_eq!(form_text(Form::Video, convert, converting, will_convert), "video");
            }
            // And the one being converted right now says so, whatever its last try was.
            assert_eq!(form_text(Form::Frames, convert, true, true), "compressing…");
        }
    }

    #[test]
    fn a_recording_that_will_never_be_compressed_says_not_compressed() {
        // The switch is off, ffmpeg was not found, or the recording is too small to be worth it.
        assert_eq!(form_text(Form::Frames, ConvertState::NotTried, false, false), "not compressed");
        // Its video failed verification or was not smaller: it is not tried again, so no "yet".
        for will_convert in [false, true] {
            assert_eq!(form_text(Form::Frames, ConvertState::Failed, false, will_convert), "not compressed");
            assert_eq!(form_text(Form::Frames, ConvertState::NotSmaller, false, will_convert), "not compressed");
        }
        assert_eq!(form_text(Form::Frames, ConvertState::Done, false, false), "not compressed");
    }

    #[test]
    fn the_question_names_the_title_cut_to_60_characters() {
        assert_eq!(remove_question("Customer onboarding"), "Remove the recording of \"Customer onboarding\"? The tour stays online, but it can no longer be edited from this computer.");
        // Exactly 60 characters stay whole; one more and the 60th is an ellipsis.
        let sixty = "a".repeat(60);
        assert!(remove_question(&sixty).starts_with(&format!("Remove the recording of \"{sixty}\"? ")));
        let cut = remove_question(&"a".repeat(61));
        assert!(cut.starts_with(&format!("Remove the recording of \"{}…\"? ", "a".repeat(59))), "{cut}");
        // Characters are counted, not bytes.
        let long = remove_question(&"é".repeat(500));
        let shown = long.strip_prefix("Remove the recording of \"").unwrap().split('"').next().unwrap();
        assert_eq!((shown.chars().count(), shown.ends_with('…')), (60, true));
        assert!(long.ends_with("? The tour stays online, but it can no longer be edited from this computer."));
    }

    fn info(tour_title: &str, title: &str) -> ProjectInfo {
        let key = ProjectKey { server: "https://tours.example.com".into(), tour_id: "tour_bbbbbbbbbbbbbbbbbbbbbbbb".into(), chapter_id: "chap_cccccccccccccccccccccccc".into() };
        ProjectInfo { key, title: title.into(), tour_title: tour_title.into(), saved_at: 1_763_424_000_000, bytes: 172 * MB, dir: std::path::PathBuf::from("chapter"), problem: None, form: Form::Frames, convert: ConvertState::NotTried }
    }

    #[test]
    fn a_chapter_is_named_only_when_the_tour_has_several() {
        // The only saved recording of a tour: the tour's title, whatever the chapter is called.
        assert_eq!(row_title(&info("Customer onboarding", "Customer onboarding"), false), "Customer onboarding");
        assert_eq!(row_title(&info("Customer onboarding", "Invite your team"), false), "Customer onboarding");
        // Several saved recordings of one tour: each names its chapter, also one titled as the tour.
        assert_eq!(row_title(&info("Customer onboarding", "Invite your team"), true), "Customer onboarding · Invite your team");
        assert_eq!(row_title(&info("Customer onboarding", "Customer onboarding"), true), "Customer onboarding · Customer onboarding");
        // A project whose file names only one of the two (a damaged one): what there is, with no stray dot.
        assert_eq!(row_title(&info("Customer onboarding", ""), true), "Customer onboarding");
        assert_eq!(row_title(&info("Customer onboarding", ""), false), "Customer onboarding");
        assert_eq!(row_title(&info("", "Invite your team"), false), "Invite your team");
        assert_eq!(row_title(&info("", "Invite your team"), true), "Invite your team");
    }

    #[test]
    fn a_recording_without_any_title_is_called_untitled() {
        for shared in [false, true] {
            assert_eq!(row_title(&info("", ""), shared), "Untitled recording");
        }
        assert_eq!(UNTITLED, "Untitled recording");
        // And the question reads with it.
        assert!(remove_question(&row_title(&info("", ""), false)).starts_with("Remove the recording of \"Untitled recording\"? "));
    }

    #[test]
    fn a_recording_with_a_problem_names_it_in_the_form_column() {
        assert_eq!(problem_text(Problem::Damaged), "damaged");
        assert_eq!(problem_text(Problem::Newer), "saved by a newer version");
        assert_eq!(problem_text(Problem::FramesMissing), "frames missing");
    }
}
