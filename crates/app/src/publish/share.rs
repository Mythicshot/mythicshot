//! "Publish to": one image or one clip shared with a link (the site's "Items").
//!
//! A share is: create (what the item is), its parts in order, the preview when there is one,
//! then commit, which answers with the link. Progress is reported as bytes sent of the total,
//! and cancel is checked before every request. Once the item exists, any failure or cancel
//! sends exactly one DELETE for it ([`AbortGuard`]), a panic included; a commit the site
//! answered with the item's id never does.
//!
//! What is sent a second time, and only once each:
//! - a part, after a transport failure (no answer at all), and after a 409 `superseded` (that
//!   send did not take its place; sending a part again replaces it);
//! - the preview, the same way; a preview that still fails is left out, and never fails the share;
//! - the commit, after a transport failure or a 409 `incomplete` (the site answers a repeated
//!   commit the same way);
//! - any request answered 429 with a `retry-after` of at most a minute, after that wait. A
//!   longer wait, or a second 429, ends the share with a sentence that says so.
//!
//! The create is never sent again after a lost answer: a second item would count against the
//! workspace's allowance until the site sweeps it a day later.
//!
//! The token goes only to the configured server, like every request of [`Client`]. The link the
//! commit answers with may be on another host (the site's own address for images and clips): it
//! is accepted only from that authenticated answer and only as a plain web address
//! ([`item_link`]), and nothing is ever sent to it. Tour links keep their stricter rule
//! ([`super::public_url`]).

use std::time::Duration;

use serde_json::{Value, json};

use super::{AbortGuard, Client, Method, Reply, Timeouts, check_server, is_id, is_invisible, not_understood, safe_link, size_rounded};

/// The largest image or clip any site can be set to take: 38 parts of 2,000,000 bytes. A
/// site's own limit (by default 10 MiB for an image and 50 MiB for a clip) is lower or equal,
/// and is said by its `media_too_large` answer; only past this one is an item refused here.
pub const MOST_BYTES: u64 = 76_000_000;
/// The most pixels on a side any site can be set to take for an image.
pub const IMAGE_MOST_SIDE: u32 = 65_535;
/// The most pixels on a side a clip may have on every site: a fixed ceiling, not a setting.
pub const CLIP_MAX_SIDE: u32 = 8192;
/// The longest clip the site takes: 10 minutes.
pub const CLIP_MAX_DURATION_MS: u64 = 600_000;
/// The longest title the site takes, in characters.
pub const TITLE_MAX: usize = 120;
/// The preview: a PNG or JPEG of at most 100,000 bytes and 512 pixels on a side.
pub const PREVIEW_MAX_BYTES: usize = 100_000;
pub const PREVIEW_MAX_SIDE: u32 = 512;
/// The most parts an item may have.
const MAX_PARTS: u64 = 999;
/// The longest `retry-after` that is waited for.
const MAX_WAIT_SECONDS: u32 = 60;
/// How often cancel is checked while waiting.
const WAIT_STEP: Duration = Duration::from_millis(100);
const PART_TYPE: &str = "application/octet-stream";
/// The small line under the message for an item the workspace has no room for.
pub const FULL_NOTE: &str = "Everything that is already online is not affected.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Image,
    Clip,
}

impl Kind {
    /// The word the windows use: "image" or "clip".
    pub fn noun(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Clip => "clip",
        }
    }
}

/// What the file is. Mythicshot shares a screenshot as PNG and a recording as MP4 or GIF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    Png,
    Gif,
    Mp4,
}

impl MediaType {
    pub fn content_type(self) -> &'static str {
        match self {
            MediaType::Png => "image/png",
            MediaType::Gif => "image/gif",
            MediaType::Mp4 => "video/mp4",
        }
    }

    /// The name the details line shows: "PNG", "GIF", "MP4".
    pub fn label(self) -> &'static str {
        match self {
            MediaType::Png => "PNG",
            MediaType::Gif => "GIF",
            MediaType::Mp4 => "MP4",
        }
    }

    pub fn kind(self) -> Kind {
        match self {
            MediaType::Png => Kind::Image,
            MediaType::Gif | MediaType::Mp4 => Kind::Clip,
        }
    }
}

/// One thing to share: the file's bytes, its size in pixels, a clip's duration, the title
/// (empty for none) and the preview picture, when there is one.
pub struct Item {
    pub media: MediaType,
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Required for a clip; never sent for an image.
    pub duration_ms: Option<u64>,
    pub title: String,
    pub preview: Option<Vec<u8>>,
}

/// A share that worked.
#[derive(Debug, Clone, PartialEq)]
pub struct Shared {
    pub id: String,
    /// The link, when the site gave one Mythicshot can use ([`item_link`]). None: the item is
    /// online all the same, and is found in the dashboard.
    pub link: Option<String>,
}

/// Which limit an item is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TooLarge {
    /// `bytes` over `max`; `exact` is false while `bytes` is an estimate.
    Bytes { bytes: u64, max: u64, exact: bool },
    Side { width: u32, height: u32, max: u32 },
    Duration { ms: u64, max: u64 },
}

/// Why a share did not happen.
#[derive(Debug, Clone, PartialEq)]
pub enum ShareError {
    /// Over a limit: found before anything was sent, or said by the site at the create.
    TooLarge(TooLarge),
    /// The workspace has no room for it: what it needs, what is left, the allowance, in bytes.
    Full { needed: u64, left: u64, allowance: u64 },
    Cancelled,
    /// Anything else, as a sentence of Mythicshot's.
    Failed(String),
}

/// A refusal as the window shows it: a heading, the sentence, and a small line under it.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub heading: String,
    pub text: String,
    pub note: Option<&'static str>,
}

impl ShareError {
    /// What the window says for this error about a `kind`; None for a cancel, which says nothing.
    pub fn refusal(&self, kind: Kind) -> Option<Refusal> {
        let noun = kind.noun();
        let not_uploaded = format!("The {noun} was not uploaded");
        match self {
            ShareError::Cancelled => None,
            ShareError::TooLarge(t) => Some(Refusal { heading: format!("This {noun} is too large to share"), text: too_large_text(kind, *t), note: None }),
            ShareError::Full { needed, left, allowance } => Some(Refusal {
                heading: not_uploaded,
                // What the item needs is rounded up and what is left down, as for a tour, so the two never read as if it would fit.
                text: format!(
                    "This {noun} needs {}, and your workspace has {} left of its {}. Delete things you no longer need, or ask for more room, and publish again.",
                    size_rounded(*needed, true),
                    size_rounded(*left, false),
                    crate::publish_ui::size_text(*allowance)
                ),
                note: Some(FULL_NOTE),
            }),
            ShareError::Failed(message) => Some(Refusal { heading: not_uploaded, text: message.clone(), note: None }),
        }
    }
}

/// The sentence for an item over a limit. A size over the limit is rounded up and the limit
/// down, so the two never read as if it would fit.
fn too_large_text(kind: Kind, t: TooLarge) -> String {
    let make_smaller = match kind {
        Kind::Image => "Crop it, or make it smaller, and publish again.",
        Kind::Clip => "Trim it, or lower the size or quality in Export, and publish again.",
    };
    let noun = kind.noun();
    match t {
        TooLarge::Bytes { bytes, max, exact } => {
            let is = if exact { "It is" } else { "It would be" };
            format!("{is} {}, and a shared {noun} can be at most {}. {make_smaller}", size_rounded(bytes, true), size_rounded(max, false))
        }
        TooLarge::Side { width, height, max } => {
            let smaller = match kind {
                Kind::Image => make_smaller,
                Kind::Clip => "Lower the size in Export, and publish again.",
            };
            format!("It is {width} × {height} pixels, and a shared {noun} can be at most {max} pixels on a side. {smaller}")
        }
        TooLarge::Duration { ms, max } => format!("It is {} long, and a shared {noun} can be at most {} long. Trim it, and publish again.", duration_words(ms, true), duration_words(max, false)),
    }
}

/// "10 minutes", "10 minutes 5 seconds", "45 seconds": whole seconds, rounded up or down.
fn duration_words(ms: u64, up: bool) -> String {
    let s = if up { ms.div_ceil(1000) } else { ms / 1000 };
    let unit = |n: u64, one: &str, many: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
    match (s / 60, s % 60) {
        (0, sec) => unit(sec, "second", "seconds"),
        (min, 0) => unit(min, "minute", "minutes"),
        (min, sec) => format!("{} {}", unit(min, "minute", "minutes"), unit(sec, "second", "seconds")),
    }
}

/// Whether an item of `kind` is over a limit no site can be set past: its size in bytes
/// ([`MOST_BYTES`]; `exact` false when that is an estimate, None when not known yet), its sides
/// and a clip's duration. None when it is within them all: whether it is within the configured
/// site's own limits only the site's answer can say. Needs no encoding: the sides alone can be
/// checked before the file exists.
pub fn too_large(kind: Kind, bytes: Option<u64>, exact: bool, width: u32, height: u32, duration_ms: Option<u64>) -> Option<TooLarge> {
    let max_side = match kind {
        Kind::Image => IMAGE_MOST_SIDE,
        Kind::Clip => CLIP_MAX_SIDE,
    };
    if let Some(bytes) = bytes.filter(|b| *b > MOST_BYTES) {
        return Some(TooLarge::Bytes { bytes, max: MOST_BYTES, exact });
    }
    if width > max_side || height > max_side {
        return Some(TooLarge::Side { width, height, max: max_side });
    }
    match (kind, duration_ms) {
        (Kind::Clip, Some(ms)) if ms > CLIP_MAX_DURATION_MS => Some(TooLarge::Duration { ms, max: CLIP_MAX_DURATION_MS }),
        _ => None,
    }
}

/// The title as it is sent: [`plain_text`], at most [`TITLE_MAX`] characters. Empty means no title.
pub fn clean_title(title: &str) -> String {
    plain_text(title).chars().take(TITLE_MAX).collect::<String>().trim_end().to_string()
}

/// `text` without control characters and without characters that show nothing but can hide or
/// reorder text (bidi controls, zero-width marks), trimmed.
pub fn plain_text(text: &str) -> String {
    let plain: String = text.chars().filter(|&c| !c.is_control() && !is_invisible(c)).collect();
    plain.trim().to_string()
}

/// The link to a shared item from the commit's answer, which came from the configured `server`
/// over the authenticated connection. A path (`/i/…`) is put after the server address. A full
/// address may be on any host, since a site can serve images and clips from an address of its
/// own: it must be https (plain http only when the configured server is itself plain http on
/// this computer) and a plain web address, with no user name, query, fragment, escape, space or
/// control character. None for anything else. Mythicshot never sends anything to that host.
pub fn item_link(server: &str, link: &str) -> Option<String> {
    let base = check_server(server).ok()?;
    if link.chars().any(|c| c.is_control() || c.is_whitespace() || is_invisible(c)) {
        return None;
    }
    if link.starts_with('/') {
        return (!link.starts_with("//")).then(|| format!("{base}{link}")).and_then(|u| safe_link(&u));
    }
    let plain_ok = base.starts_with("http://");
    if !(link.starts_with("https://") || (plain_ok && link.starts_with("http://"))) {
        return None;
    }
    safe_link(link)
}

/// `img` as a PNG file, the way "Save" writes a PNG.
pub fn png_bytes(img: &image::RgbaImage) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out).write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgba8).map_err(|e| format!("could not encode the image: {e}"))?;
    Ok(out)
}

/// The preview for an item: `img` scaled to fit [`PREVIEW_MAX_SIDE`], as a PNG when that is at
/// most [`PREVIEW_MAX_BYTES`], else as a JPEG of falling quality, else smaller again. None
/// when no such picture can be made (an empty image).
pub fn preview_of(img: &image::RgbaImage) -> Option<Vec<u8>> {
    if img.width() == 0 || img.height() == 0 {
        return None;
    }
    let mut side = PREVIEW_MAX_SIDE;
    while side >= 32 {
        let small = fit(img, side);
        if let Some(png) = png_bytes(&small).ok().filter(|b| b.len() <= PREVIEW_MAX_BYTES) {
            return Some(png);
        }
        let rgb = image::DynamicImage::ImageRgba8(small).to_rgb8();
        for quality in [85u8, 70, 55, 40] {
            let mut out = Vec::new();
            let encoded = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).encode_image(&rgb);
            if encoded.is_ok() && out.len() <= PREVIEW_MAX_BYTES {
                return Some(out);
            }
        }
        side /= 2;
    }
    None
}

/// `img` scaled down to fit a square of `side` pixels, keeping its shape; unchanged when it fits.
fn fit(img: &image::RgbaImage, side: u32) -> image::RgbaImage {
    let (w, h) = (img.width(), img.height());
    if w <= side && h <= side {
        return img.clone();
    }
    let scale = |a: u32, b: u32| ((u64::from(a) * u64::from(side) + u64::from(b) / 2) / u64::from(b)).clamp(1, u64::from(side)) as u32;
    let (nw, nh) = if w >= h { (side, scale(h, w)) } else { (scale(w, h), side) };
    image::imageops::resize(img, nw, nh, image::imageops::FilterType::Triangle)
}

/// Shares `item` on `server`, which answers with the link. `progress` gets the bytes sent so
/// far and the total before every request (and while waiting) and returns false to cancel,
/// which ends in [`ShareError::Cancelled`] after the one clean-up DELETE. Blocks for as long as
/// the upload takes: run it off the UI thread.
pub fn share(server: &str, token: &str, item: &Item, mut progress: impl FnMut(u64, u64) -> bool) -> Result<Shared, ShareError> {
    let client = Client::new(server, token, Timeouts::default()).map_err(ShareError::Failed)?;
    share_with(&client, item, &mut progress, &std::thread::sleep)
}

/// [`share`] on a client, with the pause used for `retry-after` given.
fn share_with(client: &Client, item: &Item, progress: &mut dyn FnMut(u64, u64) -> bool, sleep: &dyn Fn(Duration)) -> Result<Shared, ShareError> {
    Sharing { client, item, progress, sleep, sent: 0 }.run()
}

/// One share in progress.
struct Sharing<'a> {
    client: &'a Client,
    item: &'a Item,
    progress: &'a mut dyn FnMut(u64, u64) -> bool,
    sleep: &'a dyn Fn(Duration),
    sent: u64,
}

impl Sharing<'_> {
    fn total(&self) -> u64 {
        self.item.data.len() as u64
    }

    fn kind(&self) -> Kind {
        self.item.media.kind()
    }

    /// Report progress; false when the user cancelled.
    fn go_on(&mut self) -> bool {
        let (sent, total) = (self.sent, self.total());
        (self.progress)(sent, total)
    }

    fn run(mut self) -> Result<Shared, ShareError> {
        let item = self.item;
        let total = self.total();
        if total == 0 {
            return Err(ShareError::Failed(format!("The {} is empty, so there is nothing to share.", self.kind().noun())));
        }
        if let Some(t) = too_large(self.kind(), Some(total), true, item.width, item.height, item.duration_ms) {
            return Err(ShareError::TooLarge(t));
        }
        if !self.go_on() {
            return Err(ShareError::Cancelled);
        }
        let (id, answer) = self.create()?;
        // From here every exit but a commit the site answered, a panic included, sends exactly one DELETE.
        let guard = AbortGuard::new(self.client, format!("/api/v1/media/{id}"));
        let part_bytes = parts_of(&answer, total).ok_or_else(|| ShareError::Failed(not_understood("Could not start the upload")))?;
        for (n, part) in item.data.chunks(part_bytes).enumerate() {
            if !self.go_on() {
                return Err(ShareError::Cancelled);
            }
            self.send_part(&id, n, part)?;
            self.sent += part.len() as u64;
        }
        if let Some(preview) = &item.preview {
            if !self.go_on() {
                return Err(ShareError::Cancelled);
            }
            self.send_preview(&id, preview);
        }
        if !self.go_on() {
            return Err(ShareError::Cancelled);
        }
        let answer = self.commit(&id)?;
        guard.disarm();
        // A link Mythicshot cannot use (plain http on another host, too long) falls back to the
        // path on the configured server, which serves the item's page too.
        let usable = |key: &str| answer.get(key).and_then(Value::as_str).and_then(|l| item_link(&self.client.base, l));
        let link = usable("link").or_else(|| usable("path"));
        Ok(Shared { id, link })
    }

    /// One request, sent again at most once after a transport failure when `resend` allows it,
    /// and at most once after a 429 whose `retry-after` is at most a minute, after that wait.
    fn send(&mut self, what: &str, method: Method, path: &str, body: Option<(&str, &[u8])>, resend: bool) -> Result<Reply, ShareError> {
        let mut resend = resend;
        let mut waited = false;
        loop {
            match self.client.send(method, path, body) {
                Err(_) if resend => resend = false,
                Err(t) => return Err(ShareError::Failed(format!("{what}: {}.", t.0))),
                Ok(reply) if reply.status == 429 => {
                    let wait = reply.retry_after.filter(|s| *s <= MAX_WAIT_SECONDS && !waited);
                    let Some(seconds) = wait else { return Err(ShareError::Failed(busy(what, reply.retry_after, waited))) };
                    waited = true;
                    if !self.pause(Duration::from_secs(u64::from(seconds))) {
                        return Err(ShareError::Cancelled);
                    }
                }
                Ok(reply) => return Ok(reply),
            }
        }
    }

    /// Wait `d`, checking for a cancel every tenth of a second; false when cancelled.
    fn pause(&mut self, d: Duration) -> bool {
        let mut left = d;
        loop {
            if !self.go_on() {
                return false;
            }
            if left.is_zero() {
                return true;
            }
            let step = left.min(WAIT_STEP);
            (self.sleep)(step);
            left -= step;
        }
    }

    /// The create: what the item is. Answers the item's id and the site's answer.
    fn create(&mut self) -> Result<(String, Value), ShareError> {
        let what = "Could not start the upload";
        let item = self.item;
        let kind = self.kind();
        let mut body = json!({
            "kind": match kind { Kind::Image => "image", Kind::Clip => "clip" },
            "content_type": item.media.content_type(),
            "bytes": self.total(),
            "width": item.width,
            "height": item.height,
        });
        if kind == Kind::Clip {
            body["duration_ms"] = json!(item.duration_ms.unwrap_or(1).max(1));
        }
        let title = clean_title(&item.title);
        if !title.is_empty() {
            body["title"] = json!(title);
        }
        let body = serde_json::to_vec(&body).map_err(|_| ShareError::Failed(not_understood(what)))?;
        let reply = self.send(what, Method::Post, "/api/v1/media", Some(("application/json", &body)), false)?;
        if reply.status == 413 {
            match reply.error_code().as_deref() {
                Some("media_too_large") => {
                    if let Some(t) = self.over_the_sites_limit(&reply) {
                        return Err(ShareError::TooLarge(t));
                    }
                }
                // Said with the site's numbers, never its text. Without all three numbers the
                // site's own sentence stays, as for a tour.
                Some("storage_full") => {
                    if let Some((needed, left, allowance)) = Client::room(&reply) {
                        return Err(ShareError::Full { needed, left, allowance });
                    }
                }
                _ => {}
            }
        }
        let reply = self.client.check(what, reply).map_err(ShareError::Failed)?;
        let answer = reply.json().ok_or_else(|| ShareError::Failed(not_understood(what)))?;
        let id = answer.get("id").and_then(Value::as_str).filter(|id| is_id(id, "med_")).ok_or_else(|| ShareError::Failed(not_understood(what)))?.to_string();
        Ok((id, answer))
    }

    /// Which of the site's limits a `media_too_large` answer means, from its numbers; None when
    /// they are missing or the item is within them both.
    fn over_the_sites_limit(&self, reply: &Reply) -> Option<TooLarge> {
        let v = reply.json()?;
        let max_bytes = v.get("max_bytes").and_then(Value::as_u64)?;
        let max_side = v.get("max_side").and_then(Value::as_u64).and_then(|s| u32::try_from(s).ok())?;
        let (bytes, item) = (self.total(), self.item);
        if bytes > max_bytes {
            Some(TooLarge::Bytes { bytes, max: max_bytes, exact: true })
        } else if item.width > max_side || item.height > max_side {
            Some(TooLarge::Side { width: item.width, height: item.height, max: max_side })
        } else {
            None
        }
    }

    /// PUTs part `n`, again once after a 409 `superseded`.
    fn send_part(&mut self, id: &str, n: usize, data: &[u8]) -> Result<(), ShareError> {
        let what = format!("Could not send the {}", self.kind().noun());
        let path = format!("/api/v1/media/{id}/parts/{n}");
        let mut again = true;
        loop {
            let reply = self.send(&what, Method::Put, &path, Some((PART_TYPE, data)), true)?;
            if again && reply.status == 409 && reply.error_code().as_deref() == Some("superseded") {
                again = false;
                if !self.go_on() {
                    return Err(ShareError::Cancelled);
                }
                continue;
            }
            return self.client.check(&what, reply).map(drop).map_err(ShareError::Failed);
        }
    }

    /// PUTs the preview, at most twice (after a transport failure or a `superseded`). Whatever
    /// happens, the share goes on: a preview only stands for the item in the dashboard's lists.
    fn send_preview(&mut self, id: &str, data: &[u8]) {
        let content_type = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
            "image/jpeg"
        } else {
            return;
        };
        if data.len() > PREVIEW_MAX_BYTES {
            return;
        }
        let path = format!("/api/v1/media/{id}/preview");
        for _ in 0..2 {
            match self.client.send(Method::Put, &path, Some((content_type, data))) {
                Err(_) => {}
                Ok(reply) if reply.status == 409 && reply.error_code().as_deref() == Some("superseded") => {}
                Ok(_) => return,
            }
        }
    }

    /// The commit, sent again once after a transport failure or a 409 `incomplete`. Answers the
    /// site's answer, which must be about this item.
    fn commit(&mut self, id: &str) -> Result<Value, ShareError> {
        let what = "Could not finish the upload";
        let path = format!("/api/v1/media/{id}/commit");
        let mut again = true;
        loop {
            let reply = self.send(what, Method::Post, &path, None, true)?;
            if again && reply.status == 409 && reply.error_code().as_deref() == Some("incomplete") {
                again = false;
                continue;
            }
            let reply = self.client.check(what, reply).map_err(ShareError::Failed)?;
            let answer = reply.json().filter(|v| v.get("id").and_then(Value::as_str) == Some(id)).ok_or_else(|| ShareError::Failed(not_understood(what)))?;
            return Ok(answer);
        }
    }
}

/// The length of every part but the last, from the create's answer: it must name at least one
/// part, at most [`MAX_PARTS`], and as many as `total` bytes in parts of that length make.
fn parts_of(answer: &Value, total: u64) -> Option<usize> {
    let parts = answer.get("parts").and_then(Value::as_u64)?;
    let part_bytes = answer.get("part_bytes").and_then(Value::as_u64).filter(|b| *b > 0)?;
    ((1..=MAX_PARTS).contains(&parts) && total.div_ceil(part_bytes) == parts).then(|| usize::try_from(part_bytes).ok()).flatten()
}

/// What a 429 that is not waited for says: the wait the site asked for when it is too long,
/// else that the site is busy.
fn busy(what: &str, retry_after: Option<u32>, waited: bool) -> String {
    match retry_after {
        Some(s) if s > MAX_WAIT_SECONDS && !waited => format!("{what}: the site asks to wait {} seconds before more is sent. Publish again later.", s.min(3600)),
        _ => format!("{what}: the site is busy. Publish again later."),
    }
}

#[cfg(test)]
mod tests;
