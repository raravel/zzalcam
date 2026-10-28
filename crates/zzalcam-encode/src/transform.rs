//! Export-time transforms layered over a [`FrameSource`]: scaling, frame-rate caps and prefixes.

use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use zzalcam_core::{Error, Frame, FrameSource, TimedFrame};

use crate::pacing::Pacer;

/// Output size for `scale`, never below 1×1.
pub fn scaled_size(w: u32, h: u32, scale: f32) -> (u32, u32) {
    let s = scale.clamp(0.01, 1.0) as f64;
    (
        ((w as f64 * s).round() as u32).max(1),
        ((h as f64 * s).round() as u32).max(1),
    )
}

/// Downscale every frame by a factor (Lanczos3, SIMD).
pub struct Scaled<'a> {
    src: &'a dyn FrameSource,
    w: u32,
    h: u32,
}

impl<'a> Scaled<'a> {
    pub fn new(src: &'a dyn FrameSource, scale: f32) -> Self {
        let (w, h) = scaled_size(src.width(), src.height(), scale);
        Self { src, w, h }
    }
}

/// Resize one RGBA8 buffer.
pub fn resize_rgba(
    rgba: Vec<u8>,
    w: u32,
    h: u32,
    dw: u32,
    dh: u32,
    resizer: &mut Resizer,
) -> Result<Vec<u8>, Error> {
    let bad = |_| Error::InvalidSession("frame buffer does not match its size");
    let src = Image::from_vec_u8(w, h, rgba, PixelType::U8x4).map_err(bad)?;
    let mut dst = Image::new(dw, dh, PixelType::U8x4);
    // Frames are opaque, so skip alpha pre-multiplication.
    let opts = ResizeOptions::new()
        .resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3))
        .use_alpha(false);
    resizer
        .resize(&src, &mut dst, &opts)
        .map_err(|_| Error::InvalidSession("resize failed"))?;
    Ok(dst.into_vec())
}

impl FrameSource for Scaled<'_> {
    fn width(&self) -> u32 {
        self.w
    }
    fn height(&self) -> u32 {
        self.h
    }
    fn frame_count(&self) -> usize {
        self.src.frame_count()
    }
    fn frames(&self) -> Box<dyn Iterator<Item = Result<TimedFrame, Error>> + '_> {
        let (sw, sh, w, h) = (self.src.width(), self.src.height(), self.w, self.h);
        if (sw, sh) == (w, h) {
            return self.src.frames();
        }
        let mut resizer = Resizer::new();
        Box::new(self.src.frames().map(move |tf| {
            let tf = tf?;
            let rgba = resize_rgba(tf.frame.rgba, sw, sh, w, h, &mut resizer)?;
            Ok(TimedFrame {
                frame: Frame::new(w, h, rgba, tf.frame.timestamp_ms),
                duration_ms: tf.duration_ms,
            })
        }))
    }
}

/// Cap the frame rate: frames closer together than `1 / fps` collapse into the newest one,
/// keeping the total duration.
pub struct FpsCapped<'a> {
    src: &'a dyn FrameSource,
    min_ms: u64,
}

impl<'a> FpsCapped<'a> {
    pub fn new(src: &'a dyn FrameSource, fps: u32) -> Self {
        Self {
            src,
            min_ms: 1000 / fps.max(1) as u64,
        }
    }
}

impl FrameSource for FpsCapped<'_> {
    fn width(&self) -> u32 {
        self.src.width()
    }
    fn height(&self) -> u32 {
        self.src.height()
    }
    fn frame_count(&self) -> usize {
        // Upper bound; only used for progress.
        self.src.frame_count()
    }
    fn frames(&self) -> Box<dyn Iterator<Item = Result<TimedFrame, Error>> + '_> {
        let mut clock = 0u64;
        Box::new(Pacer::new(self.src.frames(), 1, self.min_ms).map(move |p| {
            let p = p?;
            let f = Frame::new(p.frame.width, p.frame.height, p.frame.rgba, clock);
            clock += p.delay;
            Ok(TimedFrame {
                frame: f,
                duration_ms: p.delay,
            })
        }))
    }
}

/// The first `count` frames only (for size estimates).
pub struct Prefix<'a> {
    src: &'a dyn FrameSource,
    count: usize,
}

impl<'a> Prefix<'a> {
    pub fn new(src: &'a dyn FrameSource, count: usize) -> Self {
        Self {
            src,
            count: count.min(src.frame_count()),
        }
    }
}

impl FrameSource for Prefix<'_> {
    fn width(&self) -> u32 {
        self.src.width()
    }
    fn height(&self) -> u32 {
        self.src.height()
    }
    fn frame_count(&self) -> usize {
        self.count
    }
    fn frames(&self) -> Box<dyn Iterator<Item = Result<TimedFrame, Error>> + '_> {
        Box::new(self.src.frames().take(self.count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zzalcam_core::VecSource;

    fn src(n: u64, step: u64) -> VecSource {
        let frames = (0..n)
            .map(|i| TimedFrame {
                frame: Frame::new(8, 4, vec![i as u8; 8 * 4 * 4], i * step),
                duration_ms: step,
            })
            .collect();
        VecSource {
            width: 8,
            height: 4,
            frames,
        }
    }

    #[test]
    fn scales_frames() {
        let s = src(2, 100);
        let sc = Scaled::new(&s, 0.5);
        assert_eq!((sc.width(), sc.height()), (4, 2));
        let f = sc.frames().next().unwrap().unwrap();
        assert_eq!(f.frame.rgba.len(), 4 * 2 * 4);
    }

    #[test]
    fn caps_fps_and_keeps_duration() {
        let s = src(30, 1000 / 30); // ~30 fps for ~1 s
        let capped = FpsCapped::new(&s, 10);
        let out: Vec<_> = capped.frames().map(Result::unwrap).collect();
        let total: u64 = out.iter().map(|f| f.duration_ms).sum();
        let src_total: u64 = s.frames.iter().map(|f| f.duration_ms).sum();
        assert!(out.len() <= 11, "got {} frames", out.len());
        // The last frame still gets the minimum slot, so the end may move by less than one slot.
        assert!(
            (src_total..src_total + 100).contains(&total),
            "{total} vs {src_total}"
        );
        assert!(out.iter().all(|f| f.duration_ms >= 100));
    }
}
