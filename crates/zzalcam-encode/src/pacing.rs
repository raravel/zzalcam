//! Converts variable-rate frames into display slots on a fixed time grid.
//!
//! Start times are quantised to `unit_ms` on the absolute timeline, so rounding never accumulates:
//! total output duration always equals the recording length (to one unit). A frame that would
//! start less than `min_units` after the pending one replaces it (newest content wins).

use zzalcam_core::{Error, Frame, TimedFrame};

pub struct Paced {
    pub frame: Frame,
    /// Display time in `unit_ms` units.
    pub delay: u64,
}

pub struct Pacer<I> {
    inner: I,
    unit_ms: u64,
    min_units: u64,
    pending: Option<(Frame, u64)>,
    end_units: u64,
    done: bool,
    consumed: usize,
}

impl<I: Iterator<Item = Result<TimedFrame, Error>>> Pacer<I> {
    pub fn new(inner: I, unit_ms: u64, min_units: u64) -> Self {
        Self {
            inner,
            unit_ms,
            min_units: min_units.max(1),
            pending: None,
            end_units: 0,
            done: false,
            consumed: 0,
        }
    }

    /// Input frames read so far (for progress; outputs can be fewer).
    pub fn consumed(&self) -> usize {
        self.consumed
    }

    fn quantise(&self, ms: u64) -> u64 {
        (ms + self.unit_ms / 2) / self.unit_ms
    }
}

impl<I: Iterator<Item = Result<TimedFrame, Error>>> Iterator for Pacer<I> {
    type Item = Result<Paced, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        loop {
            match self.inner.next() {
                Some(Err(e)) => {
                    self.done = true;
                    return Some(Err(e));
                }
                Some(Ok(tf)) => {
                    self.consumed += 1;
                    let start = self.quantise(tf.frame.timestamp_ms);
                    self.end_units = self.quantise(tf.frame.timestamp_ms + tf.duration_ms);
                    match self.pending.take() {
                        None => self.pending = Some((tf.frame, start)),
                        Some((_, pstart)) if start < pstart + self.min_units => {
                            self.pending = Some((tf.frame, pstart));
                        }
                        Some((pframe, pstart)) => {
                            self.pending = Some((tf.frame, start));
                            return Some(Ok(Paced {
                                frame: pframe,
                                delay: start - pstart,
                            }));
                        }
                    }
                }
                None => {
                    self.done = true;
                    let (frame, pstart) = self.pending.take()?;
                    let delay = self.end_units.saturating_sub(pstart).max(self.min_units);
                    return Some(Ok(Paced { frame, delay }));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tf(ts: u64, dur: u64, tag: u8) -> Result<TimedFrame, Error> {
        Ok(TimedFrame {
            frame: Frame::new(1, 1, vec![tag, 0, 0, 255], ts),
            duration_ms: dur,
        })
    }

    fn run(frames: Vec<Result<TimedFrame, Error>>, unit: u64, min: u64) -> Vec<(u8, u64)> {
        Pacer::new(frames.into_iter(), unit, min)
            .map(|p| p.unwrap())
            .map(|p| (p.frame.rgba[0], p.delay))
            .collect()
    }

    #[test]
    fn rounding_error_does_not_accumulate() {
        // 15 fps: 66.67 ms per frame, 15 frames = 1000 ms = 100 cs.
        let frames = (0..15)
            .map(|i| tf(i * 1000 / 15, (i + 1) * 1000 / 15 - i * 1000 / 15, i as u8))
            .collect();
        let out = run(frames, 10, 2);
        assert_eq!(out.len(), 15);
        assert_eq!(out.iter().map(|o| o.1).sum::<u64>(), 100);
    }

    #[test]
    fn too_fast_frames_collapse_to_newest() {
        // 100 fps input, GIF min 20 ms.
        let frames = (0..10).map(|i| tf(i * 10, 10, i as u8)).collect();
        let out = run(frames, 10, 2);
        assert_eq!(out.iter().map(|o| o.1).sum::<u64>(), 10);
        assert!(out.iter().all(|o| o.1 >= 2));
        assert_eq!(out.last().unwrap().0, 9, "latest content is shown last");
    }

    #[test]
    fn last_frame_gets_minimum() {
        let out = run(vec![tf(0, 1, 1)], 10, 2);
        assert_eq!(out, vec![(1, 2)]);
    }
}
