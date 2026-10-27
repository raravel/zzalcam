//! Native overlays: the target picker and the recording border.
//!
//! Everything here is drawn on click-through layered windows that are excluded from screen capture,
//! so they never show up in recordings.

mod frame;
mod gfx;
mod hooks;
mod select;
mod uia;
mod win;

pub use frame::RecordingFrame;
pub use select::{Kind, Mode, SelectHandle, Selection, select};
pub use win::exclude_from_capture;

/// Initialise Direct2D / DirectWrite and the render device ahead of time (~150 ms on first use), so
/// the first picker opens without delay.
pub fn warm_up() {
    let r = zzalcam_capture::ScreenRect {
        x: 0,
        y: 0,
        w: 1,
        h: 1,
    };
    if let Ok(s) = gfx::Surface::new(windows::Win32::Foundation::HWND::default(), r) {
        // Drawing creates the device; presenting to the null window then fails harmlessly.
        let _ = s.paint(|_| Ok(()));
    }
}

/// Localised text shown by the picker.
#[derive(Debug, Clone)]
pub struct Strings {
    pub region: String,
    pub window: String,
    pub control: String,
    pub monitor: String,
    /// Bottom hint in window/region mode.
    pub help: String,
    /// Bottom hint in control mode.
    pub help_control: String,
}

impl Default for Strings {
    fn default() -> Self {
        Self {
            region: "Region".into(),
            window: "Window".into(),
            control: "Control".into(),
            monitor: "Monitor".into(),
            help: "Drag: region · Click: window · Tab: control · F: monitor · A: all · Esc: cancel"
                .into(),
            help_control: "Click: control · Wheel: parent/child · Tab: window · Esc: cancel".into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("windows: {0}")]
    Win(#[from] windows::core::Error),
    #[error(transparent)]
    Capture(#[from] zzalcam_capture::Error),
    #[error("overlay thread stopped")]
    Thread,
}
