//! Parallel animated WebP: we find each frame's changed rectangle ourselves, encode the sub-frames
//! with `WebPEncode` on all cores, and assemble them with `WebPMux`. libwebp's own
//! `WebPAnimEncoder` does the same analysis but encodes strictly one frame at a time.
//!
//! Lossless sub-frames make every pixel unchanged since the previous frame transparent and are
//! alpha-blended over the canvas, so static parts of a changed rectangle cost almost nothing.
//! Lossy sub-frames only drop whole unchanged 8x8 blocks that way: a per-pixel mask under lossy
//! coding would put re-encoded pixels right next to old ones and mottle anti-aliased text.
//! Mixed encodes lossless first and only tries lossy when lossless came out large (photos, video,
//! games); on UI content lossless is both smaller and sharper, so the second encode is skipped.

use std::io::Write;

use libwebp_sys as sys;
use rayon::prelude::*;
use zzalcam_core::diff::{changed_bbox, crop};
use zzalcam_core::{FrameSource, Rect};

use crate::pacing::Pacer;
use crate::webp::{WebpMode, WebpOptions};
use crate::{EncodeError, EncodeStats, Progress};

/// Pixels encoded at once (mixed mode counts twice): bounds the encoders' working memory, which
/// is several times the input.
const JOB_BUDGET_PX: usize = 16 * 1024 * 1024;

/// Below this many bits per pixel a lossless sub-frame is kept without trying lossy.
const LOSSLESS_GOOD_BPP: f64 = 1.0;
/// Mixed qualities below this put size first and always try lossy too.
const SIZE_FIRST_QUALITY: f32 = 75.0;
/// Side of the blocks lossy sub-frames may leave transparent.
const LOSSY_BLOCK: u32 = 8;

/// Copy of `pixels` (`w`x`h`) where every `LOSSY_BLOCK`-square block that is entirely transparent
/// in `masked` (i.e. unchanged) is transparent too; `None` if no block qualifies.
fn block_mask(pixels: &[u8], masked: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    let mut out: Option<Vec<u8>> = None;
    for by in (0..h).step_by(LOSSY_BLOCK as usize) {
        for bx in (0..w).step_by(LOSSY_BLOCK as usize) {
            let (bw, bh) = (LOSSY_BLOCK.min(w - bx), LOSSY_BLOCK.min(h - by));
            let px = |x: u32, y: u32| (((by + y) * w + bx + x) * 4) as usize;
            let unchanged = (0..bh).all(|y| (0..bw).all(|x| masked[px(x, y) + 3] == 0));
            if unchanged {
                let o = out.get_or_insert_with(|| pixels.to_vec());
                for y in 0..bh {
                    let row = px(0, y);
                    o[row..row + bw as usize * 4].fill(0);
                }
            }
        }
    }
    out
}

/// Longest duration a WebP frame can hold (24 bits of milliseconds, ~4.6 h).
const MAX_DURATION_MS: u64 = 0xff_ffff;

struct Job {
    rect: Rect,
    /// Opaque RGBA of the rectangle.
    pixels: Vec<u8>,
    /// The rectangle with pixels unchanged since the previous frame made transparent; `None` for
    /// the first frame.
    masked: Option<Vec<u8>>,
    /// The rectangle with only fully unchanged 8x8 blocks transparent, if there are any.
    blocks: Option<Vec<u8>>,
    duration_ms: u64,
}

struct Encoded {
    rect: Rect,
    bitstream: Vec<u8>,
    blend: bool,
    duration_ms: u64,
}

/// One still image through `WebPEncode`.
fn encode_picture(
    rgba: &[u8],
    w: u32,
    h: u32,
    lossless: bool,
    opts: &WebpOptions,
) -> Result<Vec<u8>, EncodeError> {
    let mut cfg =
        sys::WebPConfig::new().map_err(|_| EncodeError::WebP("config init failed".into()))?;
    cfg.method = opts.method.clamp(0, 6);
    if lossless {
        cfg.lossless = 1;
        cfg.quality = 75.0;
        // Keep the RGB of transparent pixels out of the bitstream; they are never shown.
        cfg.exact = 0;
    } else {
        cfg.lossless = 0;
        if let WebpMode::Lossy { quality } | WebpMode::Mixed { quality } = opts.mode {
            cfg.quality = quality.clamp(0.0, 100.0);
        }
    }
    if unsafe { sys::WebPValidateConfig(&cfg) } == 0 {
        return Err(EncodeError::WebP("invalid config".into()));
    }
    unsafe {
        let mut pic =
            sys::WebPPicture::new().map_err(|_| EncodeError::WebP("picture init failed".into()))?;
        pic.use_argb = 1;
        pic.width = w as i32;
        pic.height = h as i32;
        let mut writer: sys::WebPMemoryWriter = std::mem::zeroed();
        sys::WebPMemoryWriterInit(&mut writer);
        pic.writer = Some(sys::WebPMemoryWrite);
        pic.custom_ptr = &mut writer as *mut _ as *mut _;
        let ok = sys::WebPPictureImportRGBA(&mut pic, rgba.as_ptr(), w as i32 * 4) != 0
            && sys::WebPEncode(&cfg, &mut pic) != 0;
        let err = pic.error_code;
        sys::WebPPictureFree(&mut pic);
        let out = ok.then(|| std::slice::from_raw_parts(writer.mem, writer.size).to_vec());
        sys::WebPMemoryWriterClear(&mut writer);
        out.ok_or_else(|| EncodeError::WebP(format!("encode failed ({err:?})")))
    }
}

fn encode_job(job: Job, opts: &WebpOptions) -> Result<Encoded, EncodeError> {
    let (w, h) = (job.rect.w, job.rect.h);
    let lossy = || match &job.blocks {
        Some(b) => encode_picture(b, w, h, false, opts).map(|bits| (bits, true)),
        None => encode_picture(&job.pixels, w, h, false, opts).map(|bits| (bits, false)),
    };
    // The first frame has nothing to blend over.
    let lossless = || match &job.masked {
        Some(m) => encode_picture(m, w, h, true, opts).map(|b| (b, true)),
        None => encode_picture(&job.pixels, w, h, true, opts).map(|b| (b, false)),
    };
    let (bitstream, blend) = match opts.mode {
        WebpMode::Lossless => lossless()?,
        WebpMode::Lossy { .. } => lossy()?,
        WebpMode::Mixed { quality } => {
            let exact = lossless()?;
            let bits_per_px = exact.0.len() as f64 * 8.0 / (w as f64 * h as f64);
            // Size-first settings always try lossy; it often wins on busy content.
            if quality >= SIZE_FIRST_QUALITY && bits_per_px <= LOSSLESS_GOOD_BPP {
                exact
            } else {
                let approx = lossy()?;
                if approx.0.len() < exact.0.len() {
                    approx
                } else {
                    exact
                }
            }
        }
    };
    Ok(Encoded {
        rect: job.rect,
        bitstream,
        blend,
        duration_ms: job.duration_ms,
    })
}

/// Grow `r` left/up to even coordinates (WebP stores frame offsets halved).
fn even(r: Rect) -> Rect {
    let (x, y) = (r.x & !1, r.y & !1);
    Rect::new(x, y, r.w + (r.x - x), r.h + (r.y - y))
}

/// Owns a `WebPMux` and frees it on drop.
struct Mux(*mut sys::WebPMux);

impl Drop for Mux {
    fn drop(&mut self) {
        unsafe { sys::WebPMuxDelete(self.0) }
    }
}

fn mux_ok(e: sys::WebPMuxError, what: &str) -> Result<(), EncodeError> {
    if e == sys::WebPMuxError::WEBP_MUX_OK {
        Ok(())
    } else {
        Err(EncodeError::WebP(format!("{what}: {e:?}")))
    }
}

/// Encode `src` as an animated WebP into `out`, encoding frames in parallel.
pub fn encode<W: Write>(
    src: &dyn FrameSource,
    opts: &WebpOptions,
    mut out: W,
    progress: Progress,
) -> Result<EncodeStats, EncodeError> {
    let (w, h) = (src.width(), src.height());
    if w == 0 || h == 0 || w > 16383 || h > 16383 {
        return Err(EncodeError::Invalid("WebP dimensions must be 1..=16383"));
    }
    let total = src.frame_count();
    let mux = Mux(sys::WebPMuxNew());
    if mux.0.is_null() {
        return Err(EncodeError::WebP("mux allocation failed".into()));
    }
    let params = sys::WebPMuxAnimParams {
        bgcolor: 0xffff_ffff,
        loop_count: opts.loop_count,
    };
    mux_ok(
        unsafe { sys::WebPMuxSetAnimationParams(mux.0, &params) },
        "animation params",
    )?;

    let mut stats = EncodeStats {
        input_frames: total,
        ..Default::default()
    };
    let chunk = rayon::current_num_threads() * 2;
    let push = |encoded: Vec<Encoded>, stats: &mut EncodeStats| -> Result<(), EncodeError> {
        for e in encoded {
            let info = sys::WebPMuxFrameInfo {
                bitstream: sys::WebPData {
                    bytes: e.bitstream.as_ptr(),
                    size: e.bitstream.len(),
                },
                x_offset: e.rect.x as i32,
                y_offset: e.rect.y as i32,
                duration: e.duration_ms.min(MAX_DURATION_MS) as i32,
                id: sys::WebPChunkId::WEBP_CHUNK_ANMF,
                dispose_method: sys::WebPMuxAnimDispose::WEBP_MUX_DISPOSE_NONE,
                blend_method: if e.blend {
                    sys::WebPMuxAnimBlend::WEBP_MUX_BLEND
                } else {
                    sys::WebPMuxAnimBlend::WEBP_MUX_NO_BLEND
                },
                pad: [0],
            };
            // copy_data = 1: the mux keeps its own copy of the bitstream.
            mux_ok(
                unsafe { sys::WebPMuxPushFrame(mux.0, &info, 1) },
                "push frame",
            )?;
            stats.output_frames += 1;
            stats.duration_ms += e.duration_ms.min(MAX_DURATION_MS);
        }
        Ok(())
    };

    let cost = |j: &Job| {
        (j.rect.w * j.rect.h) as usize
            * if matches!(opts.mode, WebpMode::Mixed { .. }) {
                2
            } else {
                1
            }
    };
    let mut prev: Option<Vec<u8>> = None;
    let mut jobs: Vec<Job> = Vec::with_capacity(chunk + 1);
    let mut queued_px = 0usize;
    let mut pacer = Pacer::new(src.frames(), 1, opts.min_delay_ms);
    while let Some(paced) = pacer.next() {
        let paced = paced?;
        progress(pacer.consumed(), total);
        let cur = paced.frame.rgba;
        let job = match &prev {
            None => Job {
                rect: Rect::new(0, 0, w, h),
                pixels: cur.clone(),
                masked: None,
                blocks: None,
                duration_ms: paced.delay,
            },
            Some(p) => match changed_bbox(p, &cur, w, h, 0) {
                // Nothing changed: the previous frame simply stays longer.
                None => {
                    jobs.last_mut().expect("first frame is queued").duration_ms += paced.delay;
                    continue;
                }
                Some(r) => {
                    let r = even(r);
                    let pixels = crop(&cur, w, r);
                    let mut masked = pixels.clone();
                    let before = crop(p, w, r);
                    for (m, b) in masked
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .zip(before.as_chunks::<4>().0.iter())
                    {
                        if m == b {
                            m.copy_from_slice(&[0, 0, 0, 0]);
                        }
                    }
                    // Lossless only ever encodes the per-pixel mask.
                    let lossless_only = opts.mode == WebpMode::Lossless;
                    let blocks = (!lossless_only)
                        .then(|| block_mask(&pixels, &masked, r.w, r.h))
                        .flatten();
                    Job {
                        rect: r,
                        pixels: if lossless_only { Vec::new() } else { pixels },
                        masked: Some(masked),
                        blocks,
                        duration_ms: paced.delay,
                    }
                }
            },
        };
        queued_px += cost(&job);
        jobs.push(job);
        prev = Some(cur);
        // Keep the newest job back: a following unchanged frame may still extend it.
        if jobs.len() > chunk || queued_px >= JOB_BUDGET_PX {
            let last = jobs.pop().unwrap();
            queued_px = cost(&last);
            let encoded: Vec<Encoded> = std::mem::take(&mut jobs)
                .into_par_iter()
                .map(|j| encode_job(j, opts))
                .collect::<Result<_, _>>()?;
            push(encoded, &mut stats)?;
            jobs.push(last);
        }
    }
    let encoded: Vec<Encoded> = jobs
        .into_par_iter()
        .map(|j| encode_job(j, opts))
        .collect::<Result<_, _>>()?;
    push(encoded, &mut stats)?;
    progress(total, total);

    mux_ok(
        unsafe { sys::WebPMuxSetCanvasSize(mux.0, w as i32, h as i32) },
        "canvas size",
    )?;
    let mut data: sys::WebPData = unsafe { std::mem::zeroed() };
    mux_ok(
        unsafe { sys::WebPMuxAssemble(mux.0, &mut data) },
        "assemble",
    )?;
    let bytes = unsafe { std::slice::from_raw_parts(data.bytes, data.size) };
    let res = out.write_all(bytes).and_then(|_| out.flush());
    stats.bytes = data.size as u64;
    unsafe { sys::WebPDataClear(&mut data) };
    res?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_mask_drops_only_fully_unchanged_blocks() {
        let (w, h) = (16u32, 8u32);
        let pixels = vec![9u8; (w * h * 4) as usize];
        let mut masked = vec![0u8; (w * h * 4) as usize]; // everything unchanged...
        masked[((3 * w + 12) * 4 + 3) as usize] = 255; // ...except one pixel in the right block
        let out = block_mask(&pixels, &masked, w, h).unwrap();
        assert!(
            out[..32].iter().all(|v| *v == 0),
            "left block becomes transparent"
        );
        assert_eq!(
            &out[((3 * w + 12) * 4) as usize..][..4],
            &[9, 9, 9, 9],
            "right block stays"
        );
        assert!(block_mask(&pixels, &vec![255u8; pixels.len()], w, h).is_none());
    }
    use zzalcam_core::{Frame, TimedFrame, VecSource};

    /// Decode with libwebp's demuxer and return (frames, end timestamp, last canvas).
    fn decode(bytes: &[u8]) -> (u32, i32, Vec<u8>) {
        unsafe {
            let data = sys::WebPData {
                bytes: bytes.as_ptr(),
                size: bytes.len(),
            };
            let mut dopts: sys::WebPAnimDecoderOptions = std::mem::zeroed();
            sys::WebPAnimDecoderOptionsInit(&mut dopts);
            dopts.color_mode = sys::WEBP_CSP_MODE::MODE_RGBA;
            let dec = sys::WebPAnimDecoderNew(&data, &dopts);
            assert!(!dec.is_null());
            let mut info: sys::WebPAnimInfo = std::mem::zeroed();
            sys::WebPAnimDecoderGetInfo(dec, &mut info);
            let (mut n, mut ts, mut buf) = (0, 0, std::ptr::null_mut());
            while sys::WebPAnimDecoderHasMoreFrames(dec) != 0 {
                sys::WebPAnimDecoderGetNext(dec, &mut buf, &mut ts);
                n += 1;
            }
            let last = std::slice::from_raw_parts(
                buf,
                (info.canvas_width * info.canvas_height * 4) as usize,
            )
            .to_vec();
            sys::WebPAnimDecoderDelete(dec);
            (n, ts, last)
        }
    }

    /// Moving box over a gradient, odd-sized so offsets need evening out.
    fn source(n: u64) -> VecSource {
        let (w, h) = (61u32, 37u32);
        let frames = (0..n)
            .map(|i| {
                let rgba = (0..h)
                    .flat_map(|y| {
                        (0..w).flat_map(move |x| {
                            let inside =
                                (x as u64).wrapping_sub(3 + i * 5) < 9 && (11..20).contains(&y);
                            if inside {
                                [230, 40, 40, 255]
                            } else {
                                [(x * 4) as u8, 120, (y * 6) as u8, 255]
                            }
                        })
                    })
                    .collect();
                TimedFrame {
                    frame: Frame::new(w, h, rgba, i * 50),
                    duration_ms: 50,
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
    fn lossless_roundtrip_is_exact() {
        let src = source(8);
        let mut out = Vec::new();
        let opts = WebpOptions {
            mode: WebpMode::Lossless,
            ..Default::default()
        };
        let stats = encode(&src, &opts, &mut out, &|_, _| {}).unwrap();
        let (n, end, last) = decode(&out);
        assert_eq!(n, 8);
        assert_eq!(end, 400);
        assert_eq!(stats.duration_ms, 400);
        assert_eq!(
            last,
            src.frames.last().unwrap().frame.rgba,
            "blended sub-frames rebuild the frame"
        );
    }

    #[test]
    fn lossy_and_mixed_decode_close_to_source() {
        let src = source(6);
        for mode in [
            WebpMode::Lossy { quality: 80.0 },
            WebpMode::Mixed { quality: 80.0 },
        ] {
            let mut out = Vec::new();
            encode(
                &src,
                &WebpOptions {
                    mode,
                    ..Default::default()
                },
                &mut out,
                &|_, _| {},
            )
            .unwrap();
            let (n, end, last) = decode(&out);
            assert_eq!((n, end), (6, 300), "{mode:?}");
            let want = &src.frames.last().unwrap().frame.rgba;
            let err: u64 = last
                .iter()
                .zip(want)
                .map(|(a, b)| a.abs_diff(*b) as u64)
                .sum();
            assert!(
                (err as f64 / want.len() as f64) < 8.0,
                "{mode:?} mean error too high"
            );
        }
    }

    #[test]
    fn unchanged_frames_merge() {
        let mut src = source(1);
        for i in 1..4 {
            let mut f = src.frames[0].clone();
            f.frame.timestamp_ms = i * 50;
            src.frames.push(f);
        }
        let mut out = Vec::new();
        let stats = encode(&src, &WebpOptions::default(), &mut out, &|_, _| {}).unwrap();
        assert_eq!(stats.output_frames, 1);
        assert_eq!(stats.duration_ms, 200);
        // A single frame is assembled as a still image, which shows the same thing.
        assert_eq!(decode(&out).0, 1);
    }
}
