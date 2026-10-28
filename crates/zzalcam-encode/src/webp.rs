//! Animated WebP via libwebp's WebPAnimEncoder (does its own sub-rectangle and blend optimisation).

use std::io::Write;
use std::ptr;

use libwebp_sys as sys;
use zzalcam_core::FrameSource;

use crate::{EncodeError, EncodeStats, Progress};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WebpMode {
    Lossless,
    Lossy {
        quality: f32,
    },
    /// Per-frame choice between lossy and lossless, whichever is smaller.
    Mixed {
        quality: f32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct WebpOptions {
    pub mode: WebpMode,
    /// libwebp effort 0 (fast) – 6 (slow, small).
    pub method: i32,
    /// Max distance between key frames; 0 lets libwebp decide.
    pub kmax: i32,
    pub kmin: i32,
    /// Try harder to minimise output size (much slower).
    pub minimize_size: bool,
    /// Frames shorter than this are merged into their successor.
    pub min_delay_ms: u64,
    /// 0 loops forever.
    pub loop_count: i32,
}

impl Default for WebpOptions {
    fn default() -> Self {
        Self {
            mode: WebpMode::Mixed { quality: 85.0 },
            method: 4,
            kmax: 0,
            kmin: 0,
            minimize_size: false,
            min_delay_ms: 10,
            loop_count: 0,
        }
    }
}

/// Owns a WebPAnimEncoder and frees it on drop.
struct AnimEncoder(*mut sys::WebPAnimEncoder);

impl AnimEncoder {
    fn error(&self) -> EncodeError {
        let msg = unsafe {
            let p = sys::WebPAnimEncoderGetError(self.0);
            if p.is_null() {
                "unknown error".into()
            } else {
                std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        EncodeError::WebP(msg)
    }
}

impl Drop for AnimEncoder {
    fn drop(&mut self) {
        unsafe { sys::WebPAnimEncoderDelete(self.0) }
    }
}

/// Owns an imported WebPPicture and frees it on drop.
struct Picture(sys::WebPPicture);

impl Drop for Picture {
    fn drop(&mut self) {
        unsafe { sys::WebPPictureFree(&mut self.0) }
    }
}

fn config(opts: &WebpOptions) -> Result<sys::WebPConfig, EncodeError> {
    let mut cfg =
        sys::WebPConfig::new().map_err(|_| EncodeError::WebP("config init failed".into()))?;
    match opts.mode {
        WebpMode::Lossless => {
            cfg.lossless = 1;
            cfg.quality = 75.0;
            cfg.exact = 0;
        }
        WebpMode::Lossy { quality } | WebpMode::Mixed { quality } => {
            cfg.lossless = 0;
            cfg.quality = quality.clamp(0.0, 100.0);
        }
    }
    cfg.method = opts.method.clamp(0, 6);
    cfg.thread_level = 1;
    if unsafe { sys::WebPValidateConfig(&cfg) } == 0 {
        return Err(EncodeError::WebP("invalid config".into()));
    }
    Ok(cfg)
}

/// Encode one RGBA8 image as a lossy still WebP (for previews).
pub fn encode_still(
    rgba: &[u8],
    width: u32,
    height: u32,
    quality: f32,
) -> Result<Vec<u8>, EncodeError> {
    if rgba.len() != width as usize * height as usize * 4 {
        return Err(EncodeError::Invalid("buffer does not match image size"));
    }
    let mut out = ptr::null_mut();
    let n = unsafe {
        sys::WebPEncodeRGBA(
            rgba.as_ptr(),
            width as i32,
            height as i32,
            width as i32 * 4,
            quality,
            &mut out,
        )
    };
    if n == 0 || out.is_null() {
        return Err(EncodeError::WebP("still encode failed".into()));
    }
    let bytes = unsafe { std::slice::from_raw_parts(out, n) }.to_vec();
    unsafe { sys::WebPFree(out as *mut _) };
    Ok(bytes)
}

/// Encode `src` as an animated WebP into `out`.
pub fn encode<W: Write>(
    src: &dyn FrameSource,
    opts: &WebpOptions,
    mut out: W,
    progress: Progress,
) -> Result<EncodeStats, EncodeError> {
    let (w, h) = (src.width() as i32, src.height() as i32);
    if w <= 0 || h <= 0 || w > 16383 || h > 16383 {
        return Err(EncodeError::Invalid("WebP dimensions must be 1..=16383"));
    }
    let cfg = config(opts)?;

    let mut enc_opts: sys::WebPAnimEncoderOptions = unsafe { std::mem::zeroed() };
    if unsafe {
        sys::WebPAnimEncoderOptionsInitInternal(&mut enc_opts, sys::WEBP_MUX_ABI_VERSION as i32)
    } == 0
    {
        return Err(EncodeError::WebP("encoder options init failed".into()));
    }
    enc_opts.anim_params.loop_count = opts.loop_count;
    enc_opts.allow_mixed = matches!(opts.mode, WebpMode::Mixed { .. }) as i32;
    enc_opts.minimize_size = opts.minimize_size as i32;
    if opts.kmax > 0 {
        enc_opts.kmax = opts.kmax;
        enc_opts.kmin = opts.kmin.clamp(0, opts.kmax - 1);
    }
    let raw = unsafe {
        sys::WebPAnimEncoderNewInternal(w, h, &enc_opts, sys::WEBP_MUX_ABI_VERSION as i32)
    };
    if raw.is_null() {
        return Err(EncodeError::WebP("encoder allocation failed".into()));
    }
    let enc = AnimEncoder(raw);

    let total = src.frame_count();
    let mut stats = EncodeStats {
        input_frames: total,
        ..Default::default()
    };
    let mut timestamp = 0u64;
    let mut pacer = crate::pacing::Pacer::new(src.frames(), 1, opts.min_delay_ms);
    while let Some(paced) = pacer.next() {
        let paced = paced?;
        let mut pic = Picture(
            sys::WebPPicture::new().map_err(|_| EncodeError::WebP("picture init failed".into()))?,
        );
        pic.0.use_argb = 1;
        pic.0.width = w;
        pic.0.height = h;
        if unsafe { sys::WebPPictureImportRGBA(&mut pic.0, paced.frame.rgba.as_ptr(), w * 4) } == 0
        {
            return Err(EncodeError::WebP("picture import failed".into()));
        }
        if unsafe { sys::WebPAnimEncoderAdd(enc.0, &mut pic.0, timestamp as i32, &cfg) } == 0 {
            return Err(enc.error());
        }
        timestamp += paced.delay;
        stats.output_frames += 1;
        progress(pacer.consumed(), total);
    }
    stats.duration_ms = timestamp;

    // A final NULL frame tells libwebp when the last frame ends.
    if unsafe { sys::WebPAnimEncoderAdd(enc.0, ptr::null_mut(), timestamp as i32, ptr::null()) }
        == 0
    {
        return Err(enc.error());
    }
    let mut data: sys::WebPData = unsafe { std::mem::zeroed() };
    if unsafe { sys::WebPAnimEncoderAssemble(enc.0, &mut data) } == 0 {
        return Err(enc.error());
    }
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
    use zzalcam_core::{Frame, TimedFrame, VecSource};

    fn source(n: u64) -> VecSource {
        let (w, h) = (48u32, 32u32);
        let frames = (0..n)
            .map(|i| {
                let rgba = (0..w * h)
                    .flat_map(|p| [(p % w) as u8 * 5, (i * 20) as u8, (p / w) as u8 * 7, 255])
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

    fn decoded_frame_count(bytes: &[u8]) -> (i32, u32) {
        unsafe {
            let data = sys::WebPData {
                bytes: bytes.as_ptr(),
                size: bytes.len(),
            };
            let mut dopts: sys::WebPAnimDecoderOptions = std::mem::zeroed();
            sys::WebPAnimDecoderOptionsInit(&mut dopts);
            let dec = sys::WebPAnimDecoderNew(&data, &dopts);
            assert!(!dec.is_null());
            let mut info: sys::WebPAnimInfo = std::mem::zeroed();
            sys::WebPAnimDecoderGetInfo(dec, &mut info);
            let (mut n, mut last_ts) = (0, 0);
            let mut buf = ptr::null_mut();
            while sys::WebPAnimDecoderHasMoreFrames(dec) != 0 {
                sys::WebPAnimDecoderGetNext(dec, &mut buf, &mut last_ts);
                n += 1;
            }
            sys::WebPAnimDecoderDelete(dec);
            (last_ts, n)
        }
    }

    #[test]
    fn encodes_all_modes() {
        let src = source(6);
        for mode in [
            WebpMode::Lossless,
            WebpMode::Lossy { quality: 70.0 },
            WebpMode::Mixed { quality: 70.0 },
        ] {
            let mut out = Vec::new();
            let stats = encode(
                &src,
                &WebpOptions {
                    mode,
                    ..Default::default()
                },
                &mut out,
                &|_, _| {},
            )
            .unwrap();
            assert_eq!(&out[0..4], b"RIFF");
            assert_eq!(&out[8..12], b"WEBP");
            assert_eq!(stats.duration_ms, 300);
            let (end_ts, frames) = decoded_frame_count(&out);
            assert_eq!(frames, 6, "{mode:?}");
            assert_eq!(end_ts, 300, "{mode:?}");
        }
    }
}
