//! Export pipeline: frame-rate cap and scale on top of a source, optionally squeezed under a
//! maximum file size by stepping down a ladder of cheaper settings.

use zzalcam_core::FrameSource;

use crate::gif::GifOptions;
use crate::transform::{FpsCapped, Prefix, Scaled};
use crate::webp::{WebpMode, WebpOptions};
use crate::{EncodeError, EncodeStats, Format, Preset, Progress};

/// Frames encoded to estimate the final size.
const SAMPLE_FRAMES: usize = 60;
/// Aim a little under the target: estimates are approximate.
const ESTIMATE_HEADROOM: f64 = 0.9;
/// Full re-encodes allowed after the first one overshoots.
const MAX_RETRIES: usize = 2;

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub format: Format,
    pub preset: Preset,
    /// 1.0 keeps the recorded size.
    pub scale: f32,
    pub max_fps: Option<u32>,
    pub max_bytes: Option<u64>,
}

/// A setting the size target forced, for the user to see.
#[derive(Debug, Clone, PartialEq)]
pub enum Adjustment {
    Quality(u8),
    Colors(u32),
    Tolerance(u8),
    Fps(u32),
    Scale(f32),
}

#[derive(Debug, Clone, Default)]
pub struct ExportReport {
    pub stats: EncodeStats,
    pub adjustments: Vec<Adjustment>,
    /// The size target could not be met even at the cheapest step.
    pub over_target: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct Step {
    gif: GifOptions,
    webp: WebpOptions,
    scale: f32,
    fps: Option<u32>,
}

fn webp_quality(m: WebpMode) -> f32 {
    match m {
        WebpMode::Lossy { quality } | WebpMode::Mixed { quality } => quality,
        WebpMode::Lossless => 100.0,
    }
}

/// Distinct settings from best to cheapest. Step 0 is the user's own choice.
fn ladder(opts: &ExportOptions) -> Vec<Step> {
    let base = Step {
        gif: opts.preset.gif(),
        webp: opts.preset.webp(),
        scale: opts.scale,
        fps: opts.max_fps,
    };
    let mut steps = vec![base];
    let mut push = |f: &dyn Fn(&mut Step)| {
        let mut s = steps.last().unwrap().clone();
        f(&mut s);
        // A step identical to the previous one would only waste an encode.
        if Some(&s) != steps.last() {
            steps.push(s);
        }
    };
    let fps = |s: &mut Step, cap: u32| s.fps = Some(s.fps.map_or(cap, |f| f.min(cap)));
    let scale = |s: &mut Step, factor: f32| s.scale = (opts.scale * factor).min(s.scale);
    match opts.format {
        Format::Gif => {
            push(&|s| {
                s.gif.quality = s.gif.quality.min(80);
                s.gif.max_colors = s.gif.max_colors.min(192);
                s.gif.dithering = s.gif.dithering.min(0.5);
            });
            push(&|s| {
                s.gif.tolerance = s.gif.tolerance.max(6);
                s.gif.max_colors = s.gif.max_colors.min(128);
                s.gif.dithering = 0.0;
            });
        }
        Format::WebP => {
            for q in [70.0, 55.0, 40.0] {
                push(&|s| {
                    s.webp.mode = WebpMode::Mixed {
                        quality: webp_quality(s.webp.mode).min(q),
                    }
                });
            }
        }
    }
    push(&|s| fps(s, 10));
    push(&|s| scale(s, 0.75));
    push(&|s| {
        scale(s, 0.5);
        fps(s, 8);
    });
    push(&|s| {
        scale(s, 0.35);
        fps(s, 6);
        s.gif.max_colors = s.gif.max_colors.min(64);
    });
    steps
}

/// What `step` changed relative to the user's settings.
fn adjustments(format: Format, base: &Step, step: &Step) -> Vec<Adjustment> {
    let mut out = Vec::new();
    match format {
        Format::Gif => {
            if step.gif.quality != base.gif.quality {
                out.push(Adjustment::Quality(step.gif.quality));
            }
            if step.gif.max_colors != base.gif.max_colors {
                out.push(Adjustment::Colors(step.gif.max_colors));
            }
            if step.gif.tolerance != base.gif.tolerance {
                out.push(Adjustment::Tolerance(step.gif.tolerance));
            }
        }
        Format::WebP => {
            let (q, b) = (webp_quality(step.webp.mode), webp_quality(base.webp.mode));
            if q != b {
                out.push(Adjustment::Quality(q as u8));
            }
        }
    }
    if step.fps != base.fps {
        out.extend(step.fps.map(Adjustment::Fps));
    }
    if step.scale != base.scale {
        out.push(Adjustment::Scale(step.scale));
    }
    out
}

fn encode_step(
    src: &dyn FrameSource,
    format: Format,
    step: &Step,
    progress: Progress,
) -> Result<(Vec<u8>, EncodeStats), EncodeError> {
    // Drop frames before resizing them.
    let capped;
    let source: &dyn FrameSource = match step.fps {
        Some(f) => {
            capped = FpsCapped::new(src, f);
            &capped
        }
        None => src,
    };
    let scaled = Scaled::new(source, step.scale);
    let mut out = Vec::new();
    let stats = match format {
        Format::Gif => crate::gif::encode(&scaled, &step.gif, &mut out, progress)?,
        Format::WebP => crate::webp::encode(&scaled, &step.webp, &mut out, progress)?,
    };
    Ok((out, stats))
}

/// Predicted output size of a whole source from its first frames. The first frame (full canvas,
/// palette) is measured separately so it isn't multiplied along with the rest.
fn estimate(src: &dyn FrameSource, format: Format, step: &Step) -> Result<u64, EncodeError> {
    let total = src.frame_count();
    let sample = Prefix::new(src, SAMPLE_FRAMES);
    let n = sample.frame_count();
    let (bytes, _) = encode_step(&sample, format, step, &|_, _| {})?;
    if n == total || n < 2 {
        return Ok(bytes.len() as u64);
    }
    let (first, _) = encode_step(&Prefix::new(src, 1), format, step, &|_, _| {})?;
    let rest = bytes.len().saturating_sub(first.len()) as f64;
    Ok((first.len() as f64 + rest * (total - 1) as f64 / (n - 1) as f64) as u64)
}

/// Encode `src` with `opts`, returning the file bytes.
pub fn export(
    src: &dyn FrameSource,
    opts: &ExportOptions,
    progress: Progress,
) -> Result<(Vec<u8>, ExportReport), EncodeError> {
    let steps = ladder(opts);
    let Some(target) = opts.max_bytes else {
        let (bytes, stats) = encode_step(src, opts.format, &steps[0], progress)?;
        return Ok((
            bytes,
            ExportReport {
                stats,
                ..Default::default()
            },
        ));
    };
    let goal = target as f64 * ESTIMATE_HEADROOM;
    let last = steps.len() - 1;

    let mut estimates: Vec<Option<u64>> = vec![None; steps.len()];
    let mut estimate_at = |i: usize| -> Result<u64, EncodeError> {
        if let Some(e) = estimates[i] {
            return Ok(e);
        }
        let e = estimate(src, opts.format, &steps[i])?;
        estimates[i] = Some(e);
        Ok(e)
    };

    // First step predicted to fit; `correction` scales predictions by how far off they proved.
    let mut chosen = last;
    for i in 0..steps.len() {
        if estimate_at(i)? as f64 <= goal {
            chosen = i;
            break;
        }
    }
    let mut attempt = 0;
    loop {
        let (bytes, stats) = encode_step(src, opts.format, &steps[chosen], progress)?;
        let fits = bytes.len() as u64 <= target;
        if fits || chosen == last {
            let report = ExportReport {
                stats,
                adjustments: adjustments(opts.format, &steps[0], &steps[chosen]),
                over_target: !fits,
            };
            return Ok((bytes, report));
        }
        attempt += 1;
        if attempt == MAX_RETRIES {
            chosen = last; // out of retries: the cheapest step is the best remaining chance
            continue;
        }
        let correction = (bytes.len() as f64 / estimate_at(chosen)?.max(1) as f64).max(1.0);
        let mut next = last;
        for i in chosen + 1..steps.len() {
            if estimate_at(i)? as f64 * correction <= goal {
                next = i;
                break;
            }
        }
        chosen = next;
    }
}

/// Predicted size with the user's settings (no size target applied).
pub fn estimate_size(src: &dyn FrameSource, opts: &ExportOptions) -> Result<u64, EncodeError> {
    estimate(src, opts.format, &ladder(opts)[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use zzalcam_core::{Frame, TimedFrame, VecSource};

    /// Noisy frames compress badly, so a size target has real work to do.
    fn noisy(n: u64) -> VecSource {
        let (w, h) = (96u32, 64u32);
        let mut seed = 7u32;
        let frames = (0..n)
            .map(|i| {
                let rgba = (0..w * h)
                    .flat_map(|_| {
                        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        let b = seed.to_le_bytes();
                        [b[1], b[2], b[3], 255]
                    })
                    .collect();
                TimedFrame {
                    frame: Frame::new(w, h, rgba, i * 66),
                    duration_ms: 66,
                }
            })
            .collect();
        VecSource {
            width: w,
            height: h,
            frames,
        }
    }

    fn opts(format: Format, preset: Preset) -> ExportOptions {
        ExportOptions {
            format,
            preset,
            scale: 1.0,
            max_fps: None,
            max_bytes: None,
        }
    }

    #[test]
    fn plain_export_matches_preset() {
        let src = noisy(4);
        let (bytes, report) =
            export(&src, &opts(Format::Gif, Preset::Balanced), &|_, _| {}).unwrap();
        assert!(report.adjustments.is_empty());
        assert_eq!(&bytes[..6], b"GIF89a");
    }

    #[test]
    fn size_target_steps_down_until_it_fits() {
        let src = noisy(20);
        let base = opts(Format::Gif, Preset::Balanced);
        let (full, _) = export(&src, &base, &|_, _| {}).unwrap();
        let target = full.len() as u64 / 3;
        let (bytes, report) = export(
            &src,
            &ExportOptions {
                max_bytes: Some(target),
                ..base
            },
            &|_, _| {},
        )
        .unwrap();
        assert!(bytes.len() as u64 <= target, "{} > {target}", bytes.len());
        assert!(!report.over_target);
        assert!(!report.adjustments.is_empty());
    }

    #[test]
    fn ladder_is_distinct_and_only_gets_cheaper() {
        for format in [Format::Gif, Format::WebP] {
            for preset in [Preset::Best, Preset::Balanced, Preset::Smallest] {
                let o = ExportOptions {
                    max_fps: Some(12),
                    scale: 0.8,
                    max_bytes: Some(1),
                    ..opts(format, preset)
                };
                let steps = ladder(&o);
                for w in steps.windows(2) {
                    assert_ne!(w[0], w[1], "{format:?} {preset:?} has a duplicate step");
                    assert!(w[1].scale <= w[0].scale);
                    assert!(w[1].fps.unwrap_or(u32::MAX) <= w[0].fps.unwrap_or(u32::MAX));
                }
                assert_eq!(steps.last().unwrap().fps, Some(6));
            }
        }
    }

    #[test]
    fn adjustments_list_only_real_changes() {
        // Smallest GIF already uses 128 colours and tolerance 6; the target must not claim it changed them.
        let o = opts(Format::Gif, Preset::Smallest);
        let steps = ladder(&o);
        for s in &steps[1..] {
            let a = adjustments(o.format, &steps[0], s);
            assert!(
                !a.contains(&Adjustment::Colors(128)) && !a.contains(&Adjustment::Tolerance(6)),
                "{a:?}"
            );
        }
    }
}
