//! A site's pages: the index listing every tour, each tour's page, and the shared player files.
//!
//! A tour's page is the local preview's own player, cut from the preview template between its markers
//! (see `tour::player_css`, `player_html` and `player_js`), so the preview, the single file and a site
//! can never show a tour differently. Every string from a tour or the site reaches a page as text: the
//! index is escaped here, and a tour's page carries its manifest in a block the browser never runs.

use sha2::{Digest, Sha256};

use super::{SiteList, THUMB};
use crate::tour;

const TOUR_TEMPLATE: &str = include_str!("../site_tour.html");
const INDEX_TEMPLATE: &str = include_str!("../site_index.html");

/// Reads the manifest from the page's data block. Anything but an object gives an empty tour, which the
/// player shows as one with no steps.
const READ_DATA_JS: &str = r#"
// The tour comes from the page's data block, which the browser never runs.
function readTourData() {
  var block = document.getElementById("tour-data"), data = null;
  try { data = JSON.parse(block ? block.textContent : ""); } catch (e) { data = null; }
  return data && typeof data === "object" && !Array.isArray(data) ? data : { title: "", steps: [] };
}
"#;

/// Rules a site's page needs beyond the preview's: the link back to the index, which sits where the
/// preview's label does and looks like it.
const SITE_CSS: &str = "\n  a.tag { text-decoration: none; white-space: nowrap; }\n  a.tag:hover { color: #d8dbe2; border-color: #5a6378; }\n  #needs-script { display: block; max-width: 34em; margin: 0; padding: 32px 24px; line-height: 1.5; color: #d8dbe2; white-space: normal; }\n";

/// Shown inside the stage until the player starts, so a page whose script a browser blocks is not just empty.
pub const NEEDS_SCRIPT: &str = "This tour needs scripts enabled. If you opened it from a folder and it stays empty, put the site on a web server.";
/// Where the message goes: first thing in the player's stage.
const STAGE_OPEN: &str = "<div id=\"stage\">";
/// Removes that message once the shared script runs, just before the player starts.
const DROP_MESSAGE_JS: &str = "(function () { var m = document.getElementById(\"needs-script\"); if (m) m.remove(); })();\n";

/// The shared player files of a site, with the names they are served under.
#[derive(Debug, Clone, PartialEq)]
pub struct Assets {
    pub js_name: String,
    pub js: String,
    pub css_name: String,
    pub css: String,
}

/// `player.<first 16 hex digits of the content's SHA-256>.<ext>`
fn hashed_name(content: &str, ext: &str) -> String {
    let hex: String = Sha256::digest(content.as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("player.{hex}.{ext}")
}

/// The shared player files: "player.<16 hex of SHA-256>.js" and ".css". The script is the shared rules,
/// the preview's player as a function of the tour, and a last line that starts it with the page's data.
pub fn assets() -> Assets {
    let js = [tour::tour_core_js(), READ_DATA_JS, "function startTour(TOUR) {\n", &tour::player_js(), "\n}\n", DROP_MESSAGE_JS, "startTour(readTourData());\n"].concat();
    let css = [tour::player_css(), SITE_CSS].concat();
    Assets { js_name: hashed_name(&js, "js"), js, css_name: hashed_name(&css, "css"), css }
}

/// Text made safe for HTML text and double- or single-quoted attributes.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// The template with each `{{key}}` replaced by its value. Values are never searched for keys, so no
/// text from a tour can be taken for one.
fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len() + values.iter().map(|(_, v)| v.len()).sum::<usize>());
    let mut rest = template;
    while let Some(at) = rest.find("{{") {
        out.push_str(&rest[..at]);
        let (key, after) = rest[at + 2..].split_once("}}").expect("a placeholder is closed");
        let value = values.iter().find(|(k, _)| *k == key).unwrap_or_else(|| panic!("no value for the placeholder {key}")).1;
        out.push_str(value);
        rest = after;
    }
    out.push_str(rest);
    out
}

/// A tour's page in a site: data in a non-executing block, the two shared files, a link back to the index.
/// `compact_json` is the manifest; any `<` in it is written as a JSON escape so nothing can close the block.
pub fn tour_page(compact_json: &str, title: &str, assets: &Assets) -> String {
    let data = compact_json.replace('<', r"\u003c");
    let title = escape_html(title);
    let player = tour::player_html().replacen(STAGE_OPEN, &format!("{STAGE_OPEN}\n    <p id=\"needs-script\">{NEEDS_SCRIPT}</p>"), 1);
    fill(TOUR_TEMPLATE, &[("title", &title), ("css", &escape_html(&assets.css_name)), ("js", &escape_html(&assets.js_name)), ("player", &player), ("data", &data)])
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

/// The index page for a site list. Plain HTML, no script: one card per tour, in the list's order.
pub fn index_page(list: &SiteList) -> String {
    let mut cards = String::with_capacity(list.tours.len() * 160);
    for t in &list.tours {
        let slug = escape_html(&t.slug);
        cards.push_str(&format!(
            "<a class=\"tour\" href=\"tours/{slug}/index.html\"><img src=\"tours/{slug}/{THUMB}\" alt=\"\" loading=\"lazy\"><b>{}</b><span>{}</span></a>\n",
            escape_html(&t.title),
            plural(t.steps as usize, "step", "steps")
        ));
    }
    let title = escape_html(&list.title);
    fill(INDEX_TEMPLATE, &[("title", &title), ("count", &plural(list.tours.len(), "tour", "tours")), ("cards", &cards)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::list;

    #[test]
    fn shared_files_are_named_by_their_content() {
        let a = assets();
        assert!(a.js_name.starts_with("player.") && a.js_name.ends_with(".js") && a.js_name.len() == "player..js".len() + 16);
        assert!(a.css_name.starts_with("player.") && a.css_name.ends_with(".css"));
        assert!(a.js_name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.'));
        assert!(a.css_name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.'));
        assert_eq!(assets().js_name, a.js_name);
        assert_eq!(assets(), a);
        assert!(a.js.contains("TourCore") && a.js.contains("startTour") && a.js.contains("tour-data"));
        assert!(!a.js.contains("/*TOUR_JSON*/") && !a.js.contains("Local preview") && !a.js.contains("local preview"));
        assert!(!a.css.contains("</style"));
        // The name follows the content.
        assert_ne!(hashed_name("a", "js"), hashed_name("b", "js"));
        assert_eq!(a.js_name, hashed_name(&a.js, "js"));
        assert_eq!(a.css_name, hashed_name(&a.css, "css"));
        // The shared script holds the rules and the preview's player as they are, and asks nothing of the network.
        assert!(a.js.starts_with(tour::tour_core_js()) && a.js.contains(&tour::player_js()));
        for banned in ["fetch(", "XMLHttpRequest", "WebSocket", "EventSource", "sendBeacon", "import(", "localStorage", "document.cookie", "innerHTML", "document.write", "eval("] {
            assert!(!a.js.contains(banned), "{banned}");
        }
        assert!(a.css.starts_with(tour::player_css()) && !a.css.contains("url(") && !a.css.contains("@import"));
    }

    #[test]
    fn a_site_tour_page_has_data_not_code() {
        let a = assets();
        let json = r#"{"title":"x\u003c/script>"}"#;
        let html = tour_page(json, "Raid <groups>", &a);
        assert!(html.contains(r#"<script type="application/json" id="tour-data">"#));
        assert!(html.contains(&format!(r#"src="../../assets/{}""#, a.js_name)));
        assert!(html.contains(&format!(r#"href="../../assets/{}""#, a.css_name)));
        assert!(html.contains("<title>Raid &lt;groups&gt;</title>"));
        assert!(!html.contains("Local preview") && !html.contains("unsafe-inline"));
        assert_eq!(html.matches("<script").count(), 2); // the data block and the shared file
        // A raw `<` in the data is escaped too, so the block can only end where the page ends it.
        let html = tour_page(r#"{"title":"</script><script>alert(1)</script>"}"#, "t", &a);
        assert_eq!(html.matches("</script").count(), 2);
        assert!(html.contains(r#"{"title":"\u003c/script>\u003cscript>alert(1)\u003c/script>"}"#));
        // A link back to the index, by its file name, and the preview's player body.
        assert!(html.contains(r#"href="../../index.html""#));
        // The preview's player body, with a message inside the stage that the shared script removes before it starts.
        let stage = html.split_once(STAGE_OPEN).unwrap().1;
        assert!(stage.trim_start().starts_with(&format!(r#"<p id="needs-script">{NEEDS_SCRIPT}</p>"#)));
        assert_eq!(html.replacen(&format!("\n    <p id=\"needs-script\">{NEEDS_SCRIPT}</p>"), "", 1).matches(tour::player_html()).count(), 1);
        assert!(a.js.find("needs-script").unwrap() < a.js.rfind("startTour(readTourData())").unwrap() && a.js.contains("m.remove()"));
        assert!(!tour::player_html().contains("needs-script"), "the preview page is unchanged");
        // Text that looks like a placeholder stays text.
        let html = tour_page(r#"{"title":"{{data}}"}"#, "{{js}}", &a);
        assert!(html.contains("<title>{{js}}</title>") && html.contains(r#"{"title":"{{data}}"}"#));
    }

    #[test]
    fn pages_ask_for_nothing_outside_the_site() {
        let a = assets();
        let tour = tour_page("{}", "t", &a);
        let index = index_page(&list(&[("a", "A")]));
        let csp = |html: &str| html.split("Content-Security-Policy\" content=\"").nth(1).unwrap().split('"').next().unwrap().to_string();
        let tc = csp(&tour);
        assert_eq!(tc, "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; base-uri 'none'; form-action 'none'");
        let ic = csp(&index);
        for part in ["default-src 'none'", "img-src 'self'", "base-uri 'none'", "form-action 'none'"] {
            assert!(ic.contains(part), "{part}");
        }
        assert!(!ic.contains("script-src") && !ic.contains("connect-src"), "{ic}");
        // Both name the site's own icon, so a browser does not ask the host's root for one.
        assert!(index.contains(r#"<link rel="icon" href="favicon.ico">"#) && tour.contains(r#"<link rel="icon" href="../../favicon.ico">"#));
        for html in [&tour, &index] {
            assert!(html.find("Content-Security-Policy").unwrap() < html.find("<title>").unwrap());
            assert!(!html.contains("http:") && !html.contains("https:") && !html.contains("//"), "only relative addresses");
            for attr in [" src=\"", " href=\""] {
                for part in html.split(attr).skip(1) {
                    let url = part.split('"').next().unwrap();
                    assert!(!url.starts_with('/') && !url.contains(':') && !url.contains('\\') && !url.ends_with('/'), "{url}");
                }
            }
        }
    }

    #[test]
    fn the_index_lists_every_tour_as_text() {
        let mut l = list(&[("raid-groups", "Setting up raid groups"), ("x", "</script><img src=x onerror=alert(1)>")]);
        l.title = "Guides & <more>".into();
        let html = index_page(&l);
        assert!(html.contains("<h1>Guides &amp; &lt;more&gt;</h1>") && html.contains("2 tours"));
        assert!(html.contains(r#"href="tours/raid-groups/index.html""#) && html.contains(r#"src="tours/raid-groups/thumb.jpg""#));
        assert!(html.contains("3 steps") && html.contains(r#"loading="lazy""#));
        assert!(!html.contains("<script") && !html.contains("onerror=alert(1)>") && html.contains("&lt;/script&gt;"));
        assert!(index_page(&list(&[("a", "A")])).contains("1 tour<"));
        assert!(index_page(&list(&[])).contains("0 tours"));
        assert!(html.contains("<title>Guides &amp; &lt;more&gt;</title>"));
        // In the list's order, and one step is "1 step".
        assert!(html.find("raid-groups").unwrap() < html.find("tours/x/").unwrap());
        let mut one = list(&[("a", "A")]);
        one.tours[0].steps = 1;
        assert!(index_page(&one).contains("<span>1 step</span>"));
        // An attribute cannot be broken out of either.
        let odd = list(&[("a\" onmouseover=\"x", "A")]);
        assert!(!index_page(&odd).contains("\" onmouseover=\""));
        assert_eq!(escape_html(r#"<a href='x'>"&"</a>"#), "&lt;a href=&#39;x&#39;&gt;&quot;&amp;&quot;&lt;/a&gt;");
    }

    #[test]
    fn an_index_of_three_hundred_tours_stays_small() {
        let tours: Vec<(String, String)> = (0..300).map(|i| (format!("tour-{i}"), "T".repeat(200))).collect();
        let refs: Vec<(&str, &str)> = tours.iter().map(|(s, t)| (s.as_str(), t.as_str())).collect();
        let html = index_page(&list(&refs));
        assert_eq!(html.matches("<a class=\"tour\"").count(), 300);
        assert_eq!(html.matches(r#"loading="lazy""#).count(), 300);
        assert!(html.len() < 150_000, "{}", html.len());
    }

    #[test]
    fn long_titles_are_cut_by_the_layout() {
        assert!(INDEX_TEMPLATE.contains("-webkit-line-clamp: 2") && INDEX_TEMPLATE.contains("overflow: hidden"));
        assert!(INDEX_TEMPLATE.contains("aspect-ratio: 16 / 10") && INDEX_TEMPLATE.contains("prefers-color-scheme: dark"));
        // The tour page's title is the preview's, which ends long titles with an ellipsis.
        assert!(tour::player_css().contains("header h1 { flex: 1; margin: 0; font-size: 15px; font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }"));
    }

    #[test]
    fn templates_have_only_known_placeholders() {
        // `fill` panics on an unknown key; these render every placeholder of both templates.
        let _ = tour_page("{}", "t", &assets());
        let _ = index_page(&list(&[]));
        assert_eq!(fill("a{{x}}b{{y}}c", &[("x", "{{y}}"), ("y", "2")]), "a{{y}}b2c");
    }
}
