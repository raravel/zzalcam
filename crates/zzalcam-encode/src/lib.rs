//! GIF and animated WebP encoders tuned for screen recordings.

pub mod export;
pub mod gif;
pub mod pacing;
pub mod transform;
pub mod webp;
pub mod webp_mux;

use std::path::Path;

/// Output container, usually picked from the file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Gif,
    WebP,
}

impl Format {
    pub fn from_path(path: &Path) -> Option<Format> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "gif" => Some(Format::Gif),
            "webp" => Some(Format::WebP),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Gif => "gif",
            Format::WebP => "webp",
        }
    }

    /// Largest width or height the container can store.
    pub fn max_dimension(self) -> u32 {
        match self {
            Format::Gif => u16::MAX as u32,
            Format::WebP => 16383,
        }
    }
}

/// Per-capture optimisation preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preset {
    Best,
    #[default]
    Balanced,
    Smallest,
}

impl Preset {
    pub fn gif(self) -> gif::GifOptions {
        let base = gif::GifOptions::default();
        match self {
            Preset::Best => gif::GifOptions {
                quality: 100,
                dithering: 1.0,
                tolerance: 0,
                speed: 3,
                ..base
            },
            Preset::Balanced => base,
            // Dithering noise is expensive to compress; local palettes stay on because forcing scene
            // changes onto the global palette costs far more quality than it saves bytes.
            Preset::Smallest => gif::GifOptions {
                quality: 85,
                max_colors: 128,
                dithering: 0.0,
                tolerance: 6,
                speed: 5,
                ..base
            },
        }
    }

    pub fn webp(self) -> webp::WebpOptions {
        let base = webp::WebpOptions::default();
        match self {
            // Lossless effort above 2 roughly triples encode time for ~3 % smaller output.
            Preset::Best => webp::WebpOptions {
                mode: webp::WebpMode::Lossless,
                method: 2,
                ..base
            },
            Preset::Balanced => base,
            // minimize_size costs >10x encode time for ~1 % on screen recordings, so it stays off.
            Preset::Smallest => webp::WebpOptions {
                mode: webp::WebpMode::Mixed { quality: 60.0 },
                ..base
            },
        }
    }
}

/// Encode `src` into `out` in `format` with a preset's settings.
pub fn encode_with_preset<W: std::io::Write>(
    src: &dyn zzalcam_core::FrameSource,
    format: Format,
    preset: Preset,
    out: W,
    progress: Progress,
) -> Result<EncodeStats, EncodeError> {
    match format {
        Format::Gif => gif::encode(src, &preset.gif(), out, progress),
        Format::WebP => webp::encode(src, &preset.webp(), out, progress),
    }
}

/// Summary returned by every encoder.
#[derive(Debug, Clone, Default)]
pub struct EncodeStats {
    pub input_frames: usize,
    pub output_frames: usize,
    pub local_palettes: usize,
    pub bytes: u64,
    pub duration_ms: u64,
}

/// Called with `(frames_done, frames_total)`.
pub type Progress<'a> = &'a (dyn Fn(usize, usize) + Sync);

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error(transparent)]
    Source(#[from] zzalcam_core::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("gif: {0}")]
    Gif(#[from] ::gif::EncodingError),
    #[error("quantization: {0}")]
    Quant(#[from] imagequant::Error),
    #[error("webp: {0}")]
    WebP(String),
    #[error("{0}")]
    Invalid(&'static str),
}
