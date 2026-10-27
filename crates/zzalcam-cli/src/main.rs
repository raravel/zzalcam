use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use zzalcam_capture::{CaptureOptions, RecordControl, ScreenRect, Target};
use zzalcam_core::{FrameSource, SessionReader, SessionWriter};
use zzalcam_encode::webp::WebpMode;
use zzalcam_encode::{EncodeStats, Format, Preset};

#[derive(Parser)]
#[command(name = "zzalcam", version, about = "Fast GIF / WebP screen recorder")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List capture targets
    List {
        #[arg(value_enum)]
        what: ListWhat,
    },
    /// Record the screen and encode to GIF / WebP (stop with Ctrl+C or --duration)
    Record(RecordArgs),
    /// Re-encode a recorded session file
    Encode {
        session: PathBuf,
        #[command(flatten)]
        enc: EncodeArgs,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum ListWhat {
    Monitors,
    Windows,
}

#[derive(Args)]
#[group(required = true, multiple = false, id = "target")]
struct TargetArgs {
    /// Monitor number as shown by `list monitors` (1-based)
    #[arg(long)]
    monitor: Option<usize>,
    /// All monitors as one canvas
    #[arg(long)]
    all: bool,
    /// Screen region x,y,width,height in physical pixels
    #[arg(long, value_parser = parse_region)]
    region: Option<ScreenRect>,
    /// Window whose title contains this text (case-insensitive), or a handle like 0x1234
    #[arg(long)]
    window: Option<String>,
}

#[derive(Args)]
struct RecordArgs {
    #[command(flatten)]
    target: TargetArgs,
    /// Capture rate
    #[arg(long, default_value_t = 15, value_parser = clap::value_parser!(u32).range(1..=60))]
    fps: u32,
    /// Stop after this long, e.g. 5s, 1500ms, 1m
    #[arg(long, value_parser = parse_duration)]
    duration: Option<Duration>,
    /// Hide the mouse cursor
    #[arg(long)]
    no_cursor: bool,
    /// Keep the raw session file next to the output for re-encoding
    #[arg(long)]
    keep_session: bool,
    #[command(flatten)]
    enc: EncodeArgs,
}

#[derive(Args)]
struct EncodeArgs {
    /// Output file (.gif or .webp)
    #[arg(short, long)]
    output: PathBuf,
    #[arg(long, value_enum, default_value_t = PresetArg::Balanced)]
    preset: PresetArg,
    /// GIF: palette size (2-256)
    #[arg(long)]
    colors: Option<u32>,
    /// GIF: dithering strength 0.0-1.0
    #[arg(long)]
    dither: Option<f32>,
    /// GIF: per-channel difference ignored as noise (0-255)
    #[arg(long)]
    tolerance: Option<u8>,
    /// GIF / lossy WebP quality 0-100
    #[arg(long)]
    quality: Option<u8>,
    /// WebP compression mode
    #[arg(long, value_enum)]
    webp_mode: Option<WebpModeArg>,
}

#[derive(Clone, Copy, ValueEnum)]
enum PresetArg {
    Best,
    Balanced,
    Smallest,
}

#[derive(Clone, Copy, ValueEnum)]
enum WebpModeArg {
    Lossless,
    Lossy,
    Mixed,
}

fn parse_region(s: &str) -> Result<ScreenRect, String> {
    let v: Vec<i64> = s
        .split(',')
        .map(|p| p.trim().parse::<i64>())
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let [x, y, w, h] = v[..] else {
        return Err("expected x,y,width,height".into());
    };
    let pos = |n: i64| i32::try_from(n).map_err(|_| format!("{n} is out of range"));
    let size = |n: i64| match u32::try_from(n) {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(format!("size {n} must be positive")),
    };
    Ok(ScreenRect {
        x: pos(x)?,
        y: pos(y)?,
        w: size(w)?,
        h: size(h)?,
    })
}

fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, unit) = s
        .find(|c: char| c.is_ascii_alphabetic())
        .map_or((s, "s"), |i| s.split_at(i));
    let n: f64 = num.parse().map_err(|_| format!("bad duration: {s}"))?;
    let secs = match unit {
        "ms" => n / 1000.0,
        "s" => n,
        "m" => n * 60.0,
        _ => return Err(format!("unknown unit in {s} (use ms, s or m)")),
    };
    Duration::try_from_secs_f64(secs).map_err(|_| format!("bad duration: {s}"))
}

fn main() -> Result<()> {
    zzalcam_capture::init_dpi_awareness();
    match Cli::parse().cmd {
        Cmd::List {
            what: ListWhat::Monitors,
        } => {
            for (i, m) in zzalcam_capture::monitors()?.iter().enumerate() {
                let r = m.rect;
                println!(
                    "{}: {} {}x{} at ({}, {}) scale {:.0}%{}",
                    i + 1,
                    m.name,
                    r.w,
                    r.h,
                    r.x,
                    r.y,
                    m.scale * 100.0,
                    if m.primary { " [primary]" } else { "" }
                );
            }
        }
        Cmd::List {
            what: ListWhat::Windows,
        } => {
            for w in zzalcam_capture::windows()? {
                println!("{:#x}: {} ({}x{})", w.hwnd, w.title, w.rect.w, w.rect.h);
            }
        }
        Cmd::Record(args) => record(args)?,
        Cmd::Encode { session, enc } => {
            let src = SessionReader::open(&session)
                .with_context(|| format!("opening {}", session.display()))?;
            encode(&src, &enc)?;
        }
    }
    Ok(())
}

fn resolve_target(t: &TargetArgs) -> Result<Target> {
    if let Some(n) = t.monitor {
        let count = zzalcam_capture::monitors()?.len();
        if n == 0 || n > count {
            bail!("monitor {n} does not exist (found {count})");
        }
        return Ok(Target::Monitor(n - 1));
    }
    if t.all {
        return Ok(Target::AllMonitors);
    }
    if let Some(r) = t.region {
        return Ok(Target::Region(r));
    }
    let query = t.window.as_deref().expect("clap enforces one target");
    if let Some(hex) = query.strip_prefix("0x") {
        return Ok(Target::Window(
            isize::from_str_radix(hex, 16).context("bad window handle")?,
        ));
    }
    let needle = query.to_lowercase();
    let w = zzalcam_capture::windows()?
        .into_iter()
        .find(|w| w.title.to_lowercase().contains(&needle))
        .ok_or_else(|| anyhow!("no window title contains \"{query}\""))?;
    eprintln!("Window: {} ({}x{})", w.title, w.rect.w, w.rect.h);
    Ok(Target::Window(w.hwnd))
}

fn session_dir() -> Result<PathBuf> {
    let base =
        std::env::var_os("LOCALAPPDATA").ok_or_else(|| anyhow!("LOCALAPPDATA is not set"))?;
    let dir = PathBuf::from(base).join("zzalcam").join("sessions");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn record(args: RecordArgs) -> Result<()> {
    // Fail on a bad output path before recording, not after.
    let format = Format::from_path(&args.enc.output)
        .ok_or_else(|| anyhow!("output must end in .gif or .webp"))?;

    let target = resolve_target(&args.target)?;
    let mut cap = zzalcam_capture::open(
        &target,
        &CaptureOptions {
            fps: args.fps,
            cursor: !args.no_cursor,
        },
    )?;
    let (w, h) = cap.size();
    let max = format.max_dimension();
    if w > max || h > max {
        bail!("{w}x{h} exceeds the {max}px limit of this format; pick a smaller target");
    }

    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let session_path = session_dir()?.join(format!("{stamp}.zzcs"));
    let writer = SessionWriter::create(&session_path, w, h)?;

    let ctl = Arc::new(RecordControl::default());
    let encoding = Arc::new(AtomicBool::new(false));
    let (c, e) = (ctl.clone(), encoding.clone());
    let (part, session) = (partial_path(&args.enc.output), session_path.clone());
    // Ctrl+C while recording stops and encodes; Ctrl+C while encoding aborts, keeping the session.
    ctrlc::set_handler(move || {
        if e.load(Ordering::Relaxed) {
            std::fs::remove_file(&part).ok();
            eprintln!("\nAborted. Session kept at {}", session.display());
            std::process::exit(130);
        }
        c.stop.store(true, Ordering::Relaxed);
    })?;

    eprintln!(
        "Recording {w}x{h} @ {} fps — press Ctrl+C to stop",
        args.fps
    );
    let recorded = zzalcam_capture::record(cap.as_mut(), writer, args.fps, args.duration, &ctl);
    drop(cap);
    match &recorded {
        Ok(stats) => eprintln!(
            "Captured {:.1}s: {} distinct frames out of {} samples",
            stats.duration_ms as f64 / 1000.0,
            stats.frames_written,
            stats.ticks
        ),
        // Whatever reached the session file is still worth saving.
        Err(e) => eprintln!("Recording stopped early: {e}"),
    }

    let src = SessionReader::open(&session_path)?;
    if src.frame_count() == 0 {
        eprintln!("Session kept at {}", session_path.display());
        recorded?;
        bail!("nothing was recorded");
    }
    encoding.store(true, Ordering::Relaxed);
    let result = encode(&src, &args.enc);
    if args.keep_session || result.is_err() || recorded.is_err() {
        eprintln!("Session kept at {}", session_path.display());
    } else {
        std::fs::remove_file(&session_path).ok();
    }
    result?;
    // The partial recording was saved, but still report that the capture failed.
    recorded.map(|_| ()).context("recording ended early")
}

/// Where an encode writes before it is renamed to `output` on success.
fn partial_path(output: &Path) -> PathBuf {
    let mut p = output.as_os_str().to_owned();
    p.push(".part");
    PathBuf::from(p)
}

fn encode(src: &dyn FrameSource, args: &EncodeArgs) -> Result<()> {
    let format = Format::from_path(&args.output)
        .ok_or_else(|| anyhow!("output must end in .gif or .webp"))?;
    if src.frame_count() == 0 {
        bail!("nothing was recorded");
    }
    let preset = match args.preset {
        PresetArg::Best => Preset::Best,
        PresetArg::Balanced => Preset::Balanced,
        PresetArg::Smallest => Preset::Smallest,
    };

    let started = Instant::now();
    let total = src.frame_count();
    let progress = move |done: usize, total: usize| {
        if done == total || done.is_multiple_of(10) {
            eprint!("\rEncoding {done}/{total}");
        }
    };
    // Write next to the target and rename on success, so a failed encode never leaves a
    // truncated file that looks valid.
    let partial = partial_path(&args.output);
    let file = BufWriter::new(
        File::create(&partial).with_context(|| format!("creating {}", partial.display()))?,
    );
    let stats = encode_to(src, args, format, preset, file, &progress);
    let stats = match stats {
        Ok(s) => {
            std::fs::rename(&partial, &args.output)
                .with_context(|| format!("writing {}", args.output.display()))?;
            s
        }
        Err(e) => {
            std::fs::remove_file(&partial).ok();
            return Err(e);
        }
    };

    eprintln!();
    let _ = std::io::stderr().flush();

    let bytes = std::fs::metadata(&args.output)?.len();
    eprintln!(
        "Saved {} — {} ({} frames from {}, {:.1}s{}) in {:.2}s",
        args.output.display(),
        human_size(bytes),
        stats.output_frames,
        total,
        stats.duration_ms as f64 / 1000.0,
        if stats.local_palettes > 0 {
            format!(", {} local palettes", stats.local_palettes)
        } else {
            String::new()
        },
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn encode_to(
    src: &dyn FrameSource,
    args: &EncodeArgs,
    format: Format,
    preset: Preset,
    file: impl Write,
    progress: zzalcam_encode::Progress,
) -> Result<EncodeStats> {
    Ok(match format {
        Format::Gif => {
            let mut o = preset.gif();
            if let Some(v) = args.colors {
                o.max_colors = v;
            }
            if let Some(v) = args.dither {
                o.dithering = v;
            }
            if let Some(v) = args.tolerance {
                o.tolerance = v;
            }
            if let Some(v) = args.quality {
                o.quality = v;
            }
            zzalcam_encode::gif::encode(src, &o, file, progress)?
        }
        Format::WebP => {
            let mut o = preset.webp();
            let quality = args.quality.map(f32::from);
            o.mode = match (args.webp_mode, o.mode) {
                (Some(WebpModeArg::Lossless), _) => WebpMode::Lossless,
                (Some(WebpModeArg::Lossy), m) => WebpMode::Lossy {
                    quality: quality.unwrap_or(quality_of(m)),
                },
                (Some(WebpModeArg::Mixed), m) => WebpMode::Mixed {
                    quality: quality.unwrap_or(quality_of(m)),
                },
                (None, WebpMode::Lossy { quality: q }) => WebpMode::Lossy {
                    quality: quality.unwrap_or(q),
                },
                (None, WebpMode::Mixed { quality: q }) => WebpMode::Mixed {
                    quality: quality.unwrap_or(q),
                },
                (None, m) => m,
            };
            zzalcam_encode::webp::encode(src, &o, file, progress)?
        }
    })
}

fn quality_of(m: WebpMode) -> f32 {
    match m {
        WebpMode::Lossy { quality } | WebpMode::Mixed { quality } => quality,
        WebpMode::Lossless => 85.0,
    }
}

fn human_size(b: u64) -> String {
    match b {
        b if b >= 1 << 20 => format!("{:.2} MB", b as f64 / (1 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}
