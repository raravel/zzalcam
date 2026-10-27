//! Shared types for zzalcam: frames, rectangles and the on-disk session format.

pub mod diff;
pub mod session;

pub use diff::changed_bbox;
pub use session::{SessionReader, SessionWriter};

/// Axis-aligned pixel rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = (self.x + self.w).max(other.x + other.w);
        let y1 = (self.y + self.h).max(other.y + other.h);
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }
}

/// A full-canvas RGBA8 frame with its capture timestamp.
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
    /// Milliseconds since the start of the recording (pauses excluded).
    pub timestamp_ms: u64,
}

impl Frame {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>, timestamp_ms: u64) -> Self {
        debug_assert_eq!(rgba.len(), width as usize * height as usize * 4);
        Self {
            width,
            height,
            rgba,
            timestamp_ms,
        }
    }
}

/// A frame paired with how long it stays on screen.
#[derive(Debug, Clone)]
pub struct TimedFrame {
    pub frame: Frame,
    pub duration_ms: u64,
}

/// Something that can replay a recording from the start, any number of times.
///
/// Encoders take two passes (palette sampling, then encoding) so sources must be restartable.
pub trait FrameSource: Sync {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    fn frame_count(&self) -> usize;
    fn frames(&self) -> Box<dyn Iterator<Item = Result<TimedFrame, Error>> + '_>;
}

/// In-memory source, mainly for tests and short clips.
pub struct VecSource {
    pub width: u32,
    pub height: u32,
    pub frames: Vec<TimedFrame>,
}

impl FrameSource for VecSource {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
    fn frame_count(&self) -> usize {
        self.frames.len()
    }
    fn frames(&self) -> Box<dyn Iterator<Item = Result<TimedFrame, Error>> + '_> {
        Box::new(self.frames.iter().cloned().map(Ok))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid session file: {0}")]
    InvalidSession(&'static str),
    #[error("frame size {got_w}x{got_h} does not match session {want_w}x{want_h}")]
    SizeMismatch {
        got_w: u32,
        got_h: u32,
        want_w: u32,
        want_h: u32,
    },
}
