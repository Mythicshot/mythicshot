//! File name patterns, Greenshot style: `${YYYY}-${MM}-${DD} ${hh}-${mm}-${ss}`.
//!
//! Tokens: `${YYYY}` `${MM}` `${DD}` `${hh}` `${mm}` `${ss}` `${title}` `${user}`.
//! Unknown tokens are kept as written. `/` and `\` make subfolders; each part
//! is cleaned of characters that file systems refuse.

use chrono::{DateTime, Datelike, Local, Timelike};

pub const DEFAULT_PATTERN: &str = "Mythicshot ${YYYY}-${MM}-${DD} ${hh}-${mm}-${ss}";

pub fn expand_pattern(pattern: &str, title: &str, now: DateTime<Local>) -> String {
    let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default();
    let text = pattern
        .replace("${YYYY}", &format!("{:04}", now.year()))
        .replace("${MM}", &format!("{:02}", now.month()))
        .replace("${DD}", &format!("{:02}", now.day()))
        .replace("${hh}", &format!("{:02}", now.hour()))
        .replace("${mm}", &format!("{:02}", now.minute()))
        .replace("${ss}", &format!("{:02}", now.second()))
        .replace("${title}", title)
        .replace("${user}", &user);
    let parts: Vec<String> = text.split(['/', '\\']).map(clean).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() { "Mythicshot".into() } else { parts.join(std::path::MAIN_SEPARATOR_STR) }
}

/// One path segment without characters Windows, macOS or Linux refuse, and
/// without the trailing dots and spaces Windows strips.
fn clean(part: &str) -> String {
    let s: String = part.chars().map(|c| if c.is_control() || "<>:\"|?*".contains(c) { '_' } else { c }).collect();
    let s = s.trim().trim_end_matches('.').trim();
    if s == ".." || s == "." { String::new() } else { s.chars().take(120).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn when() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 23, 9, 5, 7).unwrap()
    }

    #[test]
    fn default_pattern() {
        assert_eq!(expand_pattern(DEFAULT_PATTERN, "x", when()), "Mythicshot 2026-10-23 09-05-07");
    }

    #[test]
    fn title_is_cleaned_and_folders_kept() {
        let sep = std::path::MAIN_SEPARATOR_STR;
        assert_eq!(expand_pattern("${YYYY}/${title}", "a<b>:c", when()), format!("2026{sep}a_b__c"));
        assert_eq!(expand_pattern("../${title}.", "x", when()), "x");
        assert_eq!(expand_pattern("", "", when()), "Mythicshot");
    }
}
