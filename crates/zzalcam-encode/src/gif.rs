//! GIF encoder: libimagequant + our own inter-frame optimisation.
//!
//! Pipeline:
//! 1. **Palette pass** – changed regions of every frame feed one histogram, producing a global palette
//!    with a reserved transparent slot.
//! 2. **Diff pass** (sequential, cheap) – each paced frame is diffed against a reference canvas holding
//!    the source colour last emitted for every pixel. Pixels within `tolerance` are treated as unchanged,
//!    the frame is cropped to the changed bounding box, and fully unchanged frames extend the previous delay.
//! 3. **Remap pass** (parallel) – each cropped frame is remapped to the global palette with the reference
//!    as imagequant's background, so unchanged pixels become transparent and keep their previously dithered
//!    output (no dither flicker, maximal transparency). Frames that fit the global palette poorly get a local
//!    palette. LZW compression also runs here.
//! 4. **Write** in order.

use std::borrow::Cow;
use std::io::Write;

use ::gif::{DisposalMethod, Encoder, Frame as GifFrame, Repeat};
use imagequant::{Attributes, Histogram, QuantizationResult, RGBA};
use rayon::prelude::*;
use zzalcam_core::diff::{changed_bbox, pixel_changed};
use zzalcam_core::{FrameSource, Rect};

use crate::pacing::Pacer;
use crate::{EncodeError, EncodeStats, Progress};

/// Largest delay one GIF frame can hold (~655 s); longer stills are split.
const MAX_DELAY_CS: u64 = u16::MAX as u64;
/// Pixels of queued frame jobs (8 bytes each) before the remap pass is flushed.
const JOB_BUDGET_PX: usize = 32 * 1024 * 1024;
const TRANSPARENT: RGBA = RGBA {
    r: 0,
    g: 0,
    b: 0,
    a: 0,
};
/// Histogram input budget before palette sampling starts skipping frames.
const PALETTE_SAMPLE_BUDGET_PX: usize = 8 * 1024 * 1024;
/// Max pixels sampled from a single frame.
const PALETTE_SAMPLE_PER_FRAME_PX: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct GifOptions {
    /// Target quality 0–100 (imagequant). The minimum accepted is 30 below this.
    pub quality: u8,
    /// Palette size including the transparent slot, 2–256.
    pub max_colors: u32,
    /// Floyd–Steinberg strength 0.0–1.0.
    pub dithering: f32,
    /// Per-channel difference treated as "unchanged". 0 is lossless w.r.t. change detection.
    pub tolerance: u8,
    /// imagequant speed 1 (slow, best) – 10 (fast).
    pub speed: i32,
    /// Use a per-frame palette when remapping to the global one scores below this quality.
    pub local_palette_min_quality: Option<u8>,
    /// Minimum frame delay in centiseconds. Browsers turn delays < 2 into 10, so 2 (50 fps) is the floor.
    pub min_delay_cs: u16,
    /// `None` loops forever.
    pub loop_count: Option<u16>,
}

impl Default for GifOptions {
    fn default() -> Self {
        Self {
            quality: 90,
            max_colors: 256,
            dithering: 0.8,
            tolerance: 2,
            speed: 4,
            local_palette_min_quality: Some(60),
            min_delay_cs: 2,
            loop_count: None,
        }
    }
}

impl GifOptions {
    fn attributes(&self) -> Result<Attributes, EncodeError> {
        let mut attr = Attributes::new();
        attr.set_max_colors(self.max_colors.clamp(2, 256))?;
        // No minimum: imagequant would otherwise fail the whole encode with QualityTooLow on
        // colour-rich content. Poor fits are handled by local palettes instead.
        attr.set_quality(0, self.quality.min(100))?;
        attr.set_speed(self.speed.clamp(1, 10))?;
        Ok(attr)
    }
}

struct Job {
    rect: Rect,
    pixels: Vec<RGBA>,
    background: Option<Vec<RGBA>>,
    delay: u64,
}

struct Encoded {
    frame: GifFrame<'static>,
    local_palette: bool,
    /// Full delay in centiseconds; may exceed what one GIF frame can hold.
    delay: u64,
}

fn to_rgba(bytes: &[u8]) -> Vec<RGBA> {
    bytes
        .chunks_exact(4)
        .map(|c| RGBA::new(c[0], c[1], c[2], c[3]))
        .collect()
}

fn palette_rgb(p: &[RGBA]) -> Vec<u8> {
    p.iter().flat_map(|c| [c.r, c.g, c.b]).collect()
}

/// Rows of `rect`, taking every `stride`-th one, as imagequant pixels.
fn sample_rows(canvas: &[u8], width: u32, rect: Rect, stride: usize) -> (Vec<RGBA>, usize) {
    let w = width as usize;
    let mut out = Vec::new();
    let mut rows = 0;
    for y in (rect.y as usize..(rect.y + rect.h) as usize).step_by(stride) {
        let start = (y * w + rect.x as usize) * 4;
        out.extend(to_rgba(&canvas[start..start + rect.w as usize * 4]));
        rows += 1;
    }
    (out, rows)
}

fn build_global_palette(
    src: &dyn FrameSource,
    opts: &GifOptions,
) -> Result<Vec<RGBA>, EncodeError> {
    let attr = opts.attributes()?;
    let mut hist = Histogram::new(&attr);
    hist.add_fixed_color(TRANSPARENT, 0.0)?;
    let (w, h) = (src.width(), src.height());
    let mut prev: Option<Vec<u8>> = None;
    let mut sampled = 0usize;
    for (i, tf) in src.frames().enumerate() {
        let tf = tf?;
        let rect = match &prev {
            None => Some(Rect::new(0, 0, w, h)),
            Some(p) => changed_bbox(p, &tf.frame.rgba, w, h, opts.tolerance),
        };
        if let Some(rect) = rect.filter(|_| sampled < PALETTE_SAMPLE_BUDGET_PX || i % 4 == 0) {
            // Large changes are subsampled by rows; colour statistics barely change.
            let px = rect.w as usize * rect.h as usize;
            let stride = px.div_ceil(PALETTE_SAMPLE_PER_FRAME_PX).max(1);
            let (pixels, rows) = sample_rows(&tf.frame.rgba, w, rect, stride);
            let mut img = attr.new_image(pixels, rect.w as usize, rows, 0.0)?;
            hist.add_image(&attr, &mut img)?;
            sampled += rect.w as usize * rows;
        }
        prev = Some(tf.frame.rgba);
    }
    let mut res = hist.quantize(&attr)?;
    Ok(res.palette_vec())
}

fn remap(
    res: &mut QuantizationResult,
    img: &mut imagequant::Image<'_>,
    dithering: f32,
) -> Result<(Vec<RGBA>, Vec<u8>), EncodeError> {
    res.set_dithering_level(dithering)?;
    Ok(res.remapped(img)?)
}

fn encode_job(job: Job, global: &[RGBA], opts: &GifOptions) -> Result<Encoded, EncodeError> {
    let attr = opts.attributes()?;
    let (w, h) = (job.rect.w as usize, job.rect.h as usize);
    let mut img = attr.new_image(job.pixels, w, h, 0.0)?;
    if let Some(bg) = job.background {
        img.set_background(attr.new_image(bg, w, h, 0.0)?)?;
    }

    let mut res = QuantizationResult::from_palette(&attr, global, 0.0)?;
    let (mut palette, mut indices) = remap(&mut res, &mut img, opts.dithering)?;
    let poor_fit = match (opts.local_palette_min_quality, res.remapping_quality()) {
        (Some(min), Some(q)) => q < min,
        _ => false,
    };
    if poor_fit {
        img.add_fixed_color(TRANSPARENT)?;
        match attr.quantize(&mut img) {
            Ok(mut local) => (palette, indices) = remap(&mut local, &mut img, opts.dithering)?,
            // Keep the global remap rather than failing the whole encode over one frame.
            Err(imagequant::Error::QualityTooLow) => {}
            Err(e) => return Err(e.into()),
        }
    }

    let local_palette = palette != global;
    let mut frame = GifFrame {
        left: job.rect.x as u16,
        top: job.rect.y as u16,
        width: job.rect.w as u16,
        height: job.rect.h as u16,
        delay: job.delay.min(MAX_DELAY_CS) as u16,
        dispose: DisposalMethod::Keep,
        transparent: palette.iter().position(|c| c.a == 0).map(|i| i as u8),
        palette: local_palette.then(|| palette_rgb(&palette)),
        buffer: Cow::Owned(indices),
        ..GifFrame::default()
    };
    frame.make_lzw_pre_encoded();
    Ok(Encoded {
        frame,
        local_palette,
        delay: job.delay,
    })
}

/// Encode `src` as an animated GIF into `out`.
pub fn encode<W: Write>(
    src: &dyn FrameSource,
    opts: &GifOptions,
    out: W,
    progress: Progress,
) -> Result<EncodeStats, EncodeError> {
    let (w, h) = (src.width(), src.height());
    if w == 0 || h == 0 || w > u16::MAX as u32 || h > u16::MAX as u32 {
        return Err(EncodeError::Invalid("GIF dimensions must be 1..=65535"));
    }
    let total = src.frame_count();
    let global = build_global_palette(src, opts)?;

    let mut enc = Encoder::new(out, w as u16, h as u16, &palette_rgb(&global))?;
    enc.set_repeat(match opts.loop_count {
        None => Repeat::Infinite,
        Some(n) => Repeat::Finite(n),
    })?;

    let mut stats = EncodeStats {
        input_frames: total,
        ..Default::default()
    };
    let chunk = rayon::current_num_threads() * 2;
    let transparent = global.iter().position(|c| c.a == 0).map(|i| i as u8);

    std::thread::scope(|scope| -> Result<(), EncodeError> {
        // The diff pass runs on its own thread so decoding and diffing overlap the parallel remap.
        // The queue is short: memory is bounded by JOB_BUDGET_PX on the consumer side.
        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Job, EncodeError>>(2);
        scope.spawn(move || {
            if let Err(e) = diff_pass(src, opts, progress, &tx) {
                let _ = tx.send(Err(e));
            }
        });

        let mut jobs: Vec<Job> = Vec::with_capacity(chunk);
        let mut queued_px = 0usize;
        loop {
            let received = rx.recv().ok();
            let finished = received.is_none();
            if let Some(job) = received {
                let job = job?;
                queued_px += job.pixels.len();
                jobs.push(job);
            }
            if jobs.len() == chunk || queued_px >= JOB_BUDGET_PX || (finished && !jobs.is_empty()) {
                queued_px = 0;
                let encoded: Vec<Encoded> = std::mem::take(&mut jobs)
                    .into_par_iter()
                    .map(|j| encode_job(j, &global, opts))
                    .collect::<Result<_, _>>()?;
                for e in encoded {
                    stats.output_frames += 1;
                    stats.local_palettes += e.local_palette as usize;
                    stats.duration_ms += e.delay * 10;
                    enc.write_lzw_pre_encoded_frame(&e.frame)?;
                    // Split very long stills with invisible 1x1 frames so total time stays exact.
                    let mut rest = e.delay.saturating_sub(MAX_DELAY_CS);
                    while rest > 0 {
                        let d = rest.min(MAX_DELAY_CS);
                        rest -= d;
                        let mut pad = GifFrame {
                            width: 1,
                            height: 1,
                            delay: d as u16,
                            dispose: DisposalMethod::Keep,
                            transparent,
                            buffer: Cow::Owned(vec![transparent.unwrap_or(0)]),
                            ..GifFrame::default()
                        };
                        pad.make_lzw_pre_encoded();
                        enc.write_lzw_pre_encoded_frame(&pad)?;
                    }
                }
            }
            if finished {
                return Ok(());
            }
        }
    })?;
    enc.into_inner()?.flush()?;
    Ok(stats)
}

/// An opaque colour at least 128 away from `c` in every channel.
fn far_from(c: [u8; 4]) -> RGBA {
    let f = |v: u8| if v < 128 { 255 } else { 0 };
    RGBA::new(f(c[0]), f(c[1]), f(c[2]), 255)
}

/// Sequential stage: pace frames, diff them against the reference canvas and emit crop jobs.
fn diff_pass(
    src: &dyn FrameSource,
    opts: &GifOptions,
    progress: Progress,
    tx: &std::sync::mpsc::SyncSender<Result<Job, EncodeError>>,
) -> Result<(), EncodeError> {
    let (w, h) = (src.width(), src.height());
    let total = src.frame_count();
    let mut reference: Vec<u8> = Vec::new();
    // Held back one step: a following unchanged frame may still extend its delay.
    let mut held: Option<Job> = None;
    let send = |job: Job| {
        tx.send(Ok(job))
            .map_err(|_| EncodeError::Invalid("encoder stopped"))
    };

    let mut pacer = Pacer::new(src.frames(), 10, opts.min_delay_cs.max(1) as u64);
    while let Some(paced) = pacer.next() {
        let paced = paced?;
        let cur = paced.frame.rgba;
        progress(pacer.consumed(), total);

        if reference.is_empty() {
            held = Some(Job {
                rect: Rect::new(0, 0, w, h),
                pixels: to_rgba(&cur),
                background: None,
                delay: paced.delay,
            });
            reference = cur;
            continue;
        }
        let Some(rect) = changed_bbox(&reference, &cur, w, h, opts.tolerance) else {
            // Nothing visibly changed: show the previous frame longer.
            held.as_mut().expect("first frame is always held").delay += paced.delay;
            continue;
        };

        // Within the box, pixels inside tolerance are snapped to the reference so imagequant makes them
        // transparent. Changed pixels get a background far from their colour so imagequant can never
        // pick transparency for them: otherwise the screen would keep showing the old colour while the
        // reference already moved on, and slow fades would drift.
        let stride = w as usize * 4;
        let row_len = rect.w as usize * 4;
        let mut pixels = Vec::with_capacity(rect.w as usize * rect.h as usize);
        let mut background = Vec::with_capacity(pixels.capacity());
        for y in rect.y as usize..(rect.y + rect.h) as usize {
            let start = y * stride + rect.x as usize * 4;
            let ref_row = &mut reference[start..start + row_len];
            let cur_row = &cur[start..start + row_len];
            for (r, c) in ref_row.chunks_exact_mut(4).zip(cur_row.chunks_exact(4)) {
                let rp = [r[0], r[1], r[2], r[3]];
                let cp = [c[0], c[1], c[2], c[3]];
                if pixel_changed(rp, cp, opts.tolerance) {
                    r.copy_from_slice(c);
                    pixels.push(RGBA::new(cp[0], cp[1], cp[2], cp[3]));
                    background.push(far_from(cp));
                } else {
                    pixels.push(RGBA::new(rp[0], rp[1], rp[2], rp[3]));
                    background.push(RGBA::new(rp[0], rp[1], rp[2], rp[3]));
                }
            }
        }
        if let Some(prev) = held.replace(Job {
            rect,
            pixels,
            background: Some(background),
            delay: paced.delay,
        }) {
            send(prev)?;
        }
    }
    if let Some(last) = held {
        send(last)?;
    }
    progress(total, total);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zzalcam_core::{Frame, TimedFrame, VecSource};

    fn decode(bytes: &[u8]) -> Vec<(Vec<u8>, u16)> {
        let mut opts = ::gif::DecodeOptions::new();
        opts.set_color_output(::gif::ColorOutput::RGBA);
        let mut dec = opts.read_info(bytes).unwrap();
        let (w, h) = (dec.width() as usize, dec.height() as usize);
        let mut canvas = vec![0u8; w * h * 4];
        let mut out = Vec::new();
        while let Some(f) = dec.read_next_frame().unwrap() {
            for y in 0..f.height as usize {
                for x in 0..f.width as usize {
                    let s = (y * f.width as usize + x) * 4;
                    if f.buffer[s + 3] == 0 {
                        continue;
                    }
                    let d = ((y + f.top as usize) * w + x + f.left as usize) * 4;
                    canvas[d..d + 4].copy_from_slice(&f.buffer[s..s + 4]);
                }
            }
            out.push((canvas.clone(), f.delay));
        }
        out
    }

    /// Flat UI-like canvas with a moving box, like a typical screen recording.
    fn ui_frames(w: u32, h: u32, n: u64, step_ms: u64) -> VecSource {
        let frames = (0..n)
            .map(|i| {
                let mut rgba = Vec::with_capacity((w * h * 4) as usize);
                for y in 0..h {
                    for x in 0..w {
                        let in_box = (x as u64).wrapping_sub(i * 3) < 10 && (10..20).contains(&y);
                        let c = if in_box {
                            [220, 40, 40, 255]
                        } else {
                            [(x * 255 / w) as u8, 128, (y * 255 / h) as u8, 255]
                        };
                        rgba.extend_from_slice(&c);
                    }
                }
                TimedFrame {
                    frame: Frame::new(w, h, rgba, i * step_ms),
                    duration_ms: step_ms,
                }
            })
            .collect();
        VecSource {
            width: w,
            height: h,
            frames,
        }
    }

    #[test]
    fn roundtrip_preserves_timing_and_content() {
        let src = ui_frames(64, 32, 10, 100);
        let mut out = Vec::new();
        let stats = encode(
            &src,
            &GifOptions {
                tolerance: 0,
                ..Default::default()
            },
            &mut out,
            &|_, _| {},
        )
        .unwrap();
        assert_eq!(stats.output_frames, 10);
        assert_eq!(stats.duration_ms, 1000);

        let frames = decode(&out);
        assert_eq!(frames.len(), 10);
        assert!(frames.iter().all(|f| f.1 == 10));
        // Quantised output should stay close to the source.
        for (dec, src) in frames.iter().zip(&src.frames) {
            let err: u64 = dec
                .0
                .iter()
                .zip(&src.frame.rgba)
                .map(|(a, b)| a.abs_diff(*b) as u64)
                .sum();
            let mean = err as f64 / dec.0.len() as f64;
            assert!(mean < 6.0, "mean abs error {mean}");
        }
    }

    #[test]
    fn subsequent_frames_are_cropped_to_changes() {
        let src = ui_frames(64, 32, 5, 100);
        let mut out = Vec::new();
        encode(&src, &GifOptions::default(), &mut out, &|_, _| {}).unwrap();
        let mut dec = ::gif::DecodeOptions::new().read_info(&out[..]).unwrap();
        let first = dec.read_next_frame().unwrap().unwrap();
        assert_eq!((first.width, first.height), (64, 32));
        let second = dec.read_next_frame().unwrap().unwrap();
        assert!(
            second.width < 20 && second.height == 10,
            "got {}x{}",
            second.width,
            second.height
        );
    }

    #[test]
    fn identical_frames_merge_into_delay() {
        let mut src = ui_frames(32, 32, 1, 100);
        let f = src.frames[0].clone();
        for i in 1..5 {
            let mut g = f.clone();
            g.frame.timestamp_ms = i * 100;
            src.frames.push(g);
        }
        let mut out = Vec::new();
        let stats = encode(&src, &GifOptions::default(), &mut out, &|_, _| {}).unwrap();
        assert_eq!(stats.output_frames, 1);
        assert_eq!(decode(&out), vec![(decode(&out)[0].0.clone(), 50)]);
    }

    #[test]
    fn colour_rich_content_never_fails_quantization() {
        let (w, h) = (64u32, 64u32);
        let mut seed = 1u32;
        let frames = (0..3u64)
            .map(|i| {
                let rgba = (0..w * h)
                    .flat_map(|_| {
                        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        let b = seed.to_le_bytes();
                        [b[1], b[2], b[3], 255]
                    })
                    .collect();
                TimedFrame {
                    frame: Frame::new(w, h, rgba, i * 100),
                    duration_ms: 100,
                }
            })
            .collect();
        let src = VecSource {
            width: w,
            height: h,
            frames,
        };
        for colors in [256, 16] {
            let opts = GifOptions {
                max_colors: colors,
                ..Default::default()
            };
            encode(&src, &opts, &mut Vec::new(), &|_, _| {}).unwrap();
        }
    }

    #[test]
    fn slow_fade_does_not_drift() {
        // Right half fades grey 100 -> 217 in steps larger than the tolerance but smaller than the
        // palette spacing; the static left half spends palette entries elsewhere.
        let (w, h) = (64u32, 64u32);
        let frames: Vec<TimedFrame> = (0..40u64)
            .map(|i| {
                let g = 100 + i as u8 * 3;
                let rgba = (0..h)
                    .flat_map(|y| {
                        (0..w).flat_map(move |x| {
                            if x < 32 {
                                [(x * 8) as u8, (y * 4) as u8, 200, 255]
                            } else {
                                [g, g, g, 255]
                            }
                        })
                    })
                    .collect();
                TimedFrame {
                    frame: Frame::new(w, h, rgba, i * 100),
                    duration_ms: 100,
                }
            })
            .collect();
        let src = VecSource {
            width: w,
            height: h,
            frames,
        };
        let opts = GifOptions {
            max_colors: 16,
            quality: 30,
            dithering: 0.0,
            local_palette_min_quality: None,
            ..Default::default()
        };
        let mut out = Vec::new();
        encode(&src, &opts, &mut out, &|_, _| {}).unwrap();

        let dec = ::gif::DecodeOptions::new().read_info(&out[..]).unwrap();
        let dist = |c: &[u8]| c.iter().map(|&v| v.abs_diff(217)).max().unwrap();
        let best = dec
            .global_palette()
            .unwrap()
            .chunks(3)
            .map(dist)
            .min()
            .unwrap();
        let p = ((10 * w + 50) * 4) as usize;
        let shown = dist(&decode(&out).last().unwrap().0[p..p + 3]);
        assert!(
            shown <= best,
            "shown is {shown} off, nearest palette entry {best} off"
        );
    }

    #[test]
    fn uses_global_palette_for_similar_frames() {
        let src = ui_frames(64, 32, 8, 100);
        let mut out = Vec::new();
        let stats = encode(&src, &GifOptions::default(), &mut out, &|_, _| {}).unwrap();
        assert_eq!(stats.local_palettes, 0);
    }
}
