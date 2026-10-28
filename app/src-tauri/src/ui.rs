//! The recording control bar and the toast window (both Tauri windows, excluded from capture).

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize};
use zzalcam_capture::ScreenRect;

use crate::i18n::I18n;

/// Logical sizes; the physical size follows the monitor's scale.
const BAR_SIZE: (f64, f64) = (360.0, 48.0);
const TOAST_SIZE: (f64, f64) = (360.0, 96.0);
const GAP: f64 = 12.0;

/// Keep the bar and toast out of every recording. (They are also `focusable: false` in
/// tauri.conf.json, so clicking "pause" never steals focus from the app being recorded.)
pub fn exclude_windows(app: &AppHandle) {
    for label in ["controlbar", "toast"] {
        if let Some(hwnd) = app.get_webview_window(label).and_then(|w| w.hwnd().ok()) {
            zzalcam_overlay::exclude_from_capture(hwnd.0 as isize);
        }
    }
}

/// Place the bar just below the recorded area, else above it, else inside its bottom edge.
pub fn show_bar(app: &AppHandle, area: ScreenRect) {
    let Some(w) = app.get_webview_window("controlbar") else {
        return;
    };
    let Some((mon, scale)) = home_monitor(area) else {
        return;
    };
    let (bw, bh, gap) = (BAR_SIZE.0 * scale, BAR_SIZE.1 * scale, GAP * scale);
    // max/min rather than clamp: a monitor narrower than the bar must not panic.
    let x = (area.x as f64 + (area.w as f64 - bw) / 2.0)
        .min(mon.right() as f64 - bw)
        .max(mon.x as f64);
    let y = if area.bottom() as f64 + gap + bh <= mon.bottom() as f64 {
        area.bottom() as f64 + gap
    } else if area.y as f64 - gap - bh >= mon.y as f64 {
        area.y as f64 - gap - bh
    } else {
        area.bottom().min(mon.bottom()) as f64 - gap - bh
    };
    let _ = w.set_size(PhysicalSize::new(bw.round() as u32, bh.round() as u32));
    let _ = w.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
    let _ = w.emit("rec-start", ());
    let _ = w.show();
}

/// The monitor showing most of `area`, or the nearest one if `area` is entirely off-screen.
fn home_monitor(area: ScreenRect) -> Option<(ScreenRect, f64)> {
    let monitors = zzalcam_capture::monitors().ok()?;
    let overlap = |m: &zzalcam_capture::Monitor| {
        m.rect
            .intersect(&area)
            .map_or(0, |r| r.w as u64 * r.h as u64)
    };
    let center = |r: ScreenRect| (r.x as i64 + r.w as i64 / 2, r.y as i64 + r.h as i64 / 2);
    let (ax, ay) = center(area);
    let distance = |m: &zzalcam_capture::Monitor| {
        let (mx, my) = center(m.rect);
        (mx - ax).pow(2) + (my - ay).pow(2)
    };
    let best = monitors
        .iter()
        .max_by_key(|m| overlap(m))
        .filter(|m| overlap(m) > 0)
        .or_else(|| monitors.iter().min_by_key(|m| distance(m)))?;
    Some((best.rect, best.scale as f64))
}

pub fn hide_bar(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("controlbar") {
        let _ = w.hide();
    }
}

#[derive(Serialize, Clone)]
pub struct Toast {
    /// `encoding`, `saved`, `cancelled`, `failed`, `warning` or `recovery`; decides the buttons
    /// and auto-hide.
    pub kind: &'static str,
    pub title: String,
    pub detail: String,
    pub path: Option<PathBuf>,
}

impl Toast {
    pub fn encoding(i: &I18n) -> Self {
        Self {
            kind: "encoding",
            title: i.t("toast.encoding"),
            detail: String::new(),
            path: None,
        }
    }

    pub fn saved(i: &I18n, path: &Path, copied: bool) -> Self {
        let size = std::fs::metadata(path)
            .map(|m| human_size(m.len()))
            .unwrap_or_default();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut detail = format!("{name} · {size}");
        if copied {
            detail = format!("{detail} · {}", i.t("toast.copied"));
        }
        Self {
            kind: "saved",
            title: i.t("toast.saved"),
            detail,
            path: Some(path.to_path_buf()),
        }
    }

    pub fn cancelled(i: &I18n, session: PathBuf) -> Self {
        Self {
            kind: "cancelled",
            title: i.t("toast.cancelled"),
            detail: String::new(),
            path: Some(session),
        }
    }

    pub fn with_detail(mut self, path: &Path) -> Self {
        self.detail = path.display().to_string();
        self
    }

    /// Something the user should know that is not a failure.
    pub fn warning(title: String) -> Self {
        Self {
            kind: "warning",
            title,
            detail: String::new(),
            path: None,
        }
    }

    /// Unsaved recordings found at startup.
    pub fn recovery(i: &I18n, count: usize) -> Self {
        Self {
            kind: "recovery",
            title: i.tf("toast.recovery", &[("n", &count.to_string())]),
            detail: String::new(),
            path: None,
        }
    }

    pub fn failed(i: &I18n, error: &str) -> Self {
        Self {
            kind: "failed",
            title: i.tf("toast.failed", &[("error", error)]),
            detail: String::new(),
            path: None,
        }
    }
}

/// Show `t` in the bottom-right corner of the primary monitor's work area.
pub fn toast(app: &AppHandle, t: Toast) {
    let Some(w) = app.get_webview_window("toast") else {
        return;
    };
    if let Ok(Some(m)) = w.primary_monitor() {
        let scale = m.scale_factor();
        let area = m.work_area();
        let (tw, th, gap) = (TOAST_SIZE.0 * scale, TOAST_SIZE.1 * scale, GAP * scale);
        let x = area.position.x as f64 + area.size.width as f64 - tw - gap;
        let y = area.position.y as f64 + area.size.height as f64 - th - gap;
        let _ = w.set_size(PhysicalSize::new(tw.round() as u32, th.round() as u32));
        let _ = w.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
    }
    let _ = w.emit("toast", t);
    let _ = w.show();
}

fn human_size(b: u64) -> String {
    match b {
        b if b >= 1 << 20 => format!("{:.2} MB", b as f64 / (1 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}
