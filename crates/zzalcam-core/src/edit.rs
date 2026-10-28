//! Non-destructive edits applied on the fly while reading a recording.

use crate::diff::crop;
use crate::{Error, Frame, FrameSource, Rect, TimedFrame};

/// What the editor changed. Indices refer to frames of the underlying source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditSpec {
    /// Keep frames `start..end`; `None` keeps everything.
    pub trim: Option<(usize, usize)>,
    /// Frames removed individually (sorted or not).
    pub deleted: Vec<usize>,
    /// Keep only this part of every frame.
    pub crop: Option<Rect>,
}

impl EditSpec {
    pub fn is_identity(&self) -> bool {
        self.trim.is_none() && self.deleted.is_empty() && self.crop.is_none()
    }
}

/// A source with an [`EditSpec`] applied. Removed frames take their display time with them, so
/// the remaining frames are re-timed back to back from zero.
pub struct EditedSource<'a> {
    src: &'a dyn FrameSource,
    keep: Vec<bool>,
    kept: usize,
    crop: Rect,
}

impl<'a> EditedSource<'a> {
    pub fn new(src: &'a dyn FrameSource, spec: &EditSpec) -> Result<Self, Error> {
        let n = src.frame_count();
        let (start, end) = spec.trim.unwrap_or((0, n));
        let mut keep: Vec<bool> = (0..n).map(|i| i >= start && i < end.min(n)).collect();
        for &d in &spec.deleted {
            if let Some(k) = keep.get_mut(d) {
                *k = false;
            }
        }
        let full = Rect::new(0, 0, src.width(), src.height());
        let crop = match spec.crop {
            None => full,
            Some(c) => {
                let fits = c.x.checked_add(c.w).is_some_and(|r| r <= full.w)
                    && c.y.checked_add(c.h).is_some_and(|b| b <= full.h);
                if c.is_empty() || !fits {
                    return Err(Error::InvalidSession("crop rectangle outside the frame"));
                }
                c
            }
        };
        let kept = keep.iter().filter(|k| **k).count();
        Ok(Self {
            src,
            keep,
            kept,
            crop,
        })
    }
}

impl FrameSource for EditedSource<'_> {
    fn width(&self) -> u32 {
        self.crop.w
    }
    fn height(&self) -> u32 {
        self.crop.h
    }
    fn frame_count(&self) -> usize {
        self.kept
    }
    fn frames(&self) -> Box<dyn Iterator<Item = Result<TimedFrame, Error>> + '_> {
        let full = self.crop == Rect::new(0, 0, self.src.width(), self.src.height());
        let mut clock = 0u64;
        // Skip straight to the first kept frame instead of decoding everything trimmed away.
        let first = self.keep.iter().position(|k| *k).unwrap_or(self.keep.len());
        Box::new(
            self.src
                .frames_from(first)
                .enumerate()
                .map(move |(i, tf)| (i + first, tf))
                // Errors pass through even for dropped frames: a failed read leaves the canvas wrong.
                .filter(|(i, tf)| self.keep[*i] || tf.is_err())
                .map(move |(_, tf)| {
                    let tf = tf?;
                    let rgba = if full {
                        tf.frame.rgba
                    } else {
                        crop(&tf.frame.rgba, tf.frame.width, self.crop)
                    };
                    let out = TimedFrame {
                        frame: Frame::new(self.crop.w, self.crop.h, rgba, clock),
                        duration_ms: tf.duration_ms,
                    };
                    clock += tf.duration_ms;
                    Ok(out)
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VecSource;

    fn source() -> VecSource {
        let frames = (0..6u64)
            .map(|i| TimedFrame {
                frame: Frame::new(
                    4,
                    2,
                    (0..32).map(|p| (p + i as usize * 10) as u8).collect(),
                    i * 100,
                ),
                duration_ms: 100 + i,
            })
            .collect();
        VecSource {
            width: 4,
            height: 2,
            frames,
        }
    }

    #[test]
    fn trims_deletes_and_retimes() {
        let src = source();
        let spec = EditSpec {
            trim: Some((1, 5)),
            deleted: vec![2],
            crop: None,
        };
        let ed = EditedSource::new(&src, &spec).unwrap();
        let out: Vec<_> = ed.frames().map(Result::unwrap).collect();
        assert_eq!(ed.frame_count(), 3);
        assert_eq!(out.len(), 3);
        // Kept source frames 1, 3, 4 with their own durations, back to back from 0.
        assert_eq!(
            out.iter().map(|f| f.duration_ms).collect::<Vec<_>>(),
            vec![101, 103, 104]
        );
        assert_eq!(
            out.iter().map(|f| f.frame.timestamp_ms).collect::<Vec<_>>(),
            vec![0, 101, 204]
        );
        assert_eq!(out[1].frame.rgba, src.frames[3].frame.rgba);
    }

    #[test]
    fn crops() {
        let src = source();
        let spec = EditSpec {
            crop: Some(Rect::new(1, 1, 2, 1)),
            ..Default::default()
        };
        let ed = EditedSource::new(&src, &spec).unwrap();
        assert_eq!((ed.width(), ed.height()), (2, 1));
        let f = ed.frames().next().unwrap().unwrap();
        assert_eq!(f.frame.rgba, (20..28).collect::<Vec<u8>>());
    }

    #[test]
    fn rejects_crop_outside_frame() {
        let src = source();
        let spec = EditSpec {
            crop: Some(Rect::new(3, 0, 2, 1)),
            ..Default::default()
        };
        assert!(EditedSource::new(&src, &spec).is_err());
    }
}
