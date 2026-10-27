//! Screen capture backends for Windows.
//!
//! * Monitors, the whole virtual desktop and free regions use **DXGI Desktop Duplication**, one
//!   duplication per intersecting output, composited in physical pixels.
//! * Windows use **Windows.Graphics.Capture**, which follows the window and sees occluded content.
//!
//! Every backend yields full RGBA8 canvases; [`record`] samples them at a fixed rate into a
//! [`SessionWriter`], which drops unchanged frames (variable frame rate).

mod convert;
mod cursor;
mod dxgi;
mod enumerate;
mod timer;
mod wgc;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use zzalcam_core::SessionWriter;

pub use enumerate::{Monitor, WindowInfo, monitors, virtual_desktop, windows};

/// Rectangle in virtual-desktop physical pixels (may be negative left of / above the primary monitor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl ScreenRect {
    pub fn right(&self) -> i32 {
        self.x.saturating_add_unsigned(self.w)
    }
    pub fn bottom(&self) -> i32 {
        self.y.saturating_add_unsigned(self.h)
    }
    pub fn intersect(&self, o: &ScreenRect) -> Option<ScreenRect> {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        (x1 > x0 && y1 > y0).then(|| ScreenRect {
            x: x0,
            y: y0,
            w: (x1 - x0) as u32,
            h: (y1 - y0) as u32,
        })
    }
}

#[derive(Debug, Clone)]
pub enum Target {
    /// Index into [`monitors()`].
    Monitor(usize),
    /// Every monitor composited into one canvas.
    AllMonitors,
    /// Fixed screen rectangle. Also used for window controls, whose rect is resolved before recording.
    Region(ScreenRect),
    /// A top-level window, followed as it moves.
    Window(isize),
}

#[derive(Debug, Clone)]
pub struct CaptureOptions {
    pub fps: u32,
    pub cursor: bool,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            fps: 15,
            cursor: true,
        }
    }
}

/// A live capture producing RGBA8 canvases of a fixed size.
pub trait Capturer {
    fn size(&self) -> (u32, u32);
    /// Refresh the canvas with the latest screen content. Returns `false` if nothing changed.
    fn grab(&mut self) -> Result<bool, Error>;
    fn canvas(&self) -> &[u8];
    /// The target went away (e.g. the window was closed); recording should stop.
    fn ended(&self) -> bool {
        false
    }
}

pub fn open(target: &Target, opts: &CaptureOptions) -> Result<Box<dyn Capturer>, Error> {
    Ok(match *target {
        Target::Monitor(i) => {
            let m = monitors()?
                .into_iter()
                .nth(i)
                .ok_or(Error::NotFound("monitor"))?;
            Box::new(dxgi::DesktopCapturer::new(m.rect, opts.cursor)?)
        }
        Target::AllMonitors => {
            Box::new(dxgi::DesktopCapturer::new(virtual_desktop()?, opts.cursor)?)
        }
        Target::Region(r) => Box::new(dxgi::DesktopCapturer::new(r, opts.cursor)?),
        Target::Window(hwnd) => Box::new(wgc::WindowCapturer::new(hwnd, opts.cursor)?),
    })
}

/// Controls a running [`record`] call from another thread.
#[derive(Default)]
pub struct RecordControl {
    pub stop: AtomicBool,
    pub paused: AtomicBool,
}

#[derive(Debug, Clone, Default)]
pub struct RecordStats {
    pub ticks: u64,
    pub frames_written: usize,
    pub duration_ms: u64,
}

/// Sample `cap` at `fps` into `writer` until `ctl.stop` is set or `max_duration` elapses.
/// Paused time is excluded from timestamps.
pub fn record(
    cap: &mut dyn Capturer,
    mut writer: SessionWriter,
    fps: u32,
    max_duration: Option<Duration>,
    ctl: &RecordControl,
) -> Result<RecordStats, Error> {
    let interval = Duration::from_secs_f64(1.0 / fps.clamp(1, 120) as f64);
    let timer = timer::HighResTimer::new()?;
    let start = Instant::now();
    let mut paused_total = Duration::ZERO;
    let mut paused_since: Option<Instant> = None;
    let mut next_tick = start;
    let mut stats = RecordStats::default();
    let mut first = true;

    loop {
        let now = Instant::now();
        if ctl.paused.load(Ordering::Relaxed) {
            paused_since.get_or_insert(now);
        } else if let Some(p) = paused_since.take() {
            paused_total += now - p;
        }
        let elapsed = now - start - paused_total - paused_since.map_or(Duration::ZERO, |p| now - p);
        if ctl.stop.load(Ordering::Relaxed)
            || cap.ended()
            || max_duration.is_some_and(|d| elapsed >= d)
        {
            stats.duration_ms = max_duration.map_or(elapsed, |d| elapsed.min(d)).as_millis() as u64;
            writer.finish(stats.duration_ms)?;
            return Ok(stats);
        }

        if paused_since.is_none() {
            let changed = cap.grab()?;
            stats.ticks += 1;
            if changed || first {
                if writer.push(cap.canvas(), elapsed.as_millis() as u64)? {
                    stats.frames_written += 1;
                }
                first = false;
            }
        }

        next_tick += interval;
        let now = Instant::now();
        if next_tick > now {
            timer.sleep(next_tick - now)?;
        } else {
            // Fell behind (slow grab); don't try to catch up with a burst.
            next_tick = now;
        }
    }
}

/// Largest canvas we allocate (16384 x 16384); beyond this a typo, not a monitor wall.
const MAX_CANVAS_PX: usize = 1 << 28;

/// Pixel count of a `w`×`h` canvas, rejecting sizes that overflow or are absurdly large.
pub(crate) fn checked_area(w: u32, h: u32) -> Result<usize, Error> {
    (w as usize)
        .checked_mul(h as usize)
        .filter(|&n| n > 0 && n <= MAX_CANVAS_PX)
        .ok_or(Error::Unsupported("capture area is too large"))
}

/// Opt the process into per-monitor DPI awareness so all coordinates are physical pixels.
pub fn init_dpi_awareness() {
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    // Fails harmlessly if the manifest already set it.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("windows: {0}")]
    Win(#[from] windows::core::Error),
    #[error(transparent)]
    Session(#[from] zzalcam_core::Error),
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("capture region is empty or off-screen")]
    EmptyRegion,
    #[error("{0}")]
    Unsupported(&'static str),
}
