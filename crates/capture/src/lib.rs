//! Screen and window capture.
//!
//! A capture starts by freezing the whole desktop into one image
//! ([`grab_desktop`]). Region, window and full-screen captures are crops of that
//! frozen image, so what the user saw when they pressed the key is what they get.
//! Coordinates are desktop pixels, which can be negative on monitors left of or
//! above the primary one.

use image::RgbaImage;

pub use image;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PxRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl PxRect {
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    pub fn from_corners(ax: i32, ay: i32, bx: i32, by: i32) -> Self {
        let (x0, x1) = (ax.min(bx), ax.max(bx));
        let (y0, y1) = (ay.min(by), ay.max(by));
        Self::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
    }

    pub fn right(&self) -> i32 {
        self.x + self.w as i32
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h as i32
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub fn intersect(&self, o: &PxRect) -> Option<PxRect> {
        let (x0, y0) = (self.x.max(o.x), self.y.max(o.y));
        let (x1, y1) = (self.right().min(o.right()), self.bottom().min(o.bottom()));
        (x1 > x0 && y1 > y0).then(|| PxRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
    }

    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
}

#[derive(Debug, Clone)]
pub struct MonitorInfo {
    pub name: String,
    pub primary: bool,
    pub rect: PxRect,
    pub scale: f32,
}

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub title: String,
    pub app: String,
    pub rect: PxRect,
    pub focused: bool,
}

/// The whole desktop frozen at one moment, plus what was on it.
pub struct Desktop {
    pub bounds: PxRect,
    pub image: RgbaImage,
    pub monitors: Vec<MonitorInfo>,
    /// Visible top-level windows, topmost first.
    pub windows: Vec<WindowInfo>,
}

#[derive(Debug)]
pub struct CaptureError(pub String);

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CaptureError {}

fn err(e: impl std::fmt::Display) -> CaptureError {
    CaptureError(e.to_string())
}

/// Monitors in the OS's order, without capturing them.
pub fn list_monitors() -> Vec<MonitorInfo> {
    xcap::Monitor::all()
        .unwrap_or_default()
        .iter()
        .filter_map(|m| {
            Some(MonitorInfo {
                name: m.name().unwrap_or_default(),
                primary: m.is_primary().unwrap_or_default(),
                rect: PxRect::new(m.x().ok()?, m.y().ok()?, m.width().ok()?, m.height().ok()?),
                scale: m.scale_factor().unwrap_or(1.0),
            })
        })
        .collect()
}

pub fn grab_desktop() -> Result<Desktop, CaptureError> {
    let monitors = xcap::Monitor::all().map_err(err)?;
    let mut infos = Vec::new();
    let mut shots = Vec::new();
    for m in &monitors {
        let shot = m.capture_image().map_err(err)?;
        let rect = PxRect::new(m.x().map_err(err)?, m.y().map_err(err)?, shot.width(), shot.height());
        infos.push(MonitorInfo {
            name: m.name().unwrap_or_default(),
            primary: m.is_primary().unwrap_or_default(),
            rect,
            scale: m.scale_factor().unwrap_or(1.0),
        });
        shots.push(shot);
    }
    if infos.is_empty() {
        return Err(CaptureError("no monitors found".into()));
    }
    let x0 = infos.iter().map(|m| m.rect.x).min().unwrap_or(0);
    let y0 = infos.iter().map(|m| m.rect.y).min().unwrap_or(0);
    let x1 = infos.iter().map(|m| m.rect.right()).max().unwrap_or(0);
    let y1 = infos.iter().map(|m| m.rect.bottom()).max().unwrap_or(0);
    let bounds = PxRect::from_corners(x0, y0, x1, y1);

    let mut image = RgbaImage::new(bounds.w, bounds.h);
    for (info, shot) in infos.iter().zip(&shots) {
        let shot = RgbaImage::from_raw(shot.width(), shot.height(), shot.as_raw().clone()).ok_or_else(|| err("bad monitor image"))?;
        image::imageops::replace(&mut image, &shot, (info.rect.x - x0) as i64, (info.rect.y - y0) as i64);
    }

    Ok(Desktop { bounds, image, monitors: infos, windows: list_windows(&bounds) })
}

fn list_windows(bounds: &PxRect) -> Vec<WindowInfo> {
    let own_pid = std::process::id();
    let Ok(all) = xcap::Window::all() else { return Vec::new() };
    all.into_iter()
        .filter(|w| !w.is_minimized().unwrap_or(true) && w.pid().unwrap_or(0) != own_pid)
        .filter_map(|w| {
            let rect = PxRect::new(w.x().ok()?, w.y().ok()?, w.width().ok()?, w.height().ok()?);
            let rect = rect.intersect(bounds)?;
            (rect.w > 8 && rect.h > 8).then(|| WindowInfo {
                title: w.title().unwrap_or_default(),
                app: w.app_name().unwrap_or_default(),
                rect,
                focused: w.is_focused().unwrap_or(false),
            })
        })
        .collect()
}

impl Desktop {
    /// Crop a rectangle (desktop pixels) out of the frozen image.
    pub fn crop(&self, rect: PxRect) -> Option<RgbaImage> {
        let r = rect.intersect(&self.bounds)?;
        let (x, y) = ((r.x - self.bounds.x) as u32, (r.y - self.bounds.y) as u32);
        Some(image::imageops::crop_imm(&self.image, x, y, r.w, r.h).to_image())
    }

    /// The topmost window under a desktop point.
    pub fn window_at(&self, x: i32, y: i32) -> Option<&WindowInfo> {
        self.windows.iter().find(|w| w.rect.contains(x, y))
    }

    pub fn focused_window(&self) -> Option<&WindowInfo> {
        self.windows.iter().find(|w| w.focused)
    }

    pub fn monitor_at(&self, x: i32, y: i32) -> Option<&MonitorInfo> {
        self.monitors.iter().find(|m| m.rect.contains(x, y))
    }
}
