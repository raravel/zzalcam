//! Reference benchmark from docs/design.md: 1080p, 15 fps, 10 s of synthetic UI interaction
//! (typing, scrolling a list, a moving cursor, idle stretches).
//!
//! `cargo run --release -p zzalcam-encode --example bench [-- [--json <file>] [<outdir>]]`
//! Prints one line per encoder; sizes are deterministic, timings are not. `--json` writes the
//! results for `bench/compare.py`; `<outdir>` keeps the encoded files.

use std::io::Write;
use std::time::Instant;

use zzalcam_core::{Error, Frame, FrameSource, TimedFrame};
use zzalcam_encode::{Preset, gif, webp};

const W: u32 = 1920;
const H: u32 = 1080;
const FRAMES: u64 = 150;
const STEP_MS: u64 = 1000 / 15;

struct SyntheticUi;

fn hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^ (x >> 16)
}

fn fill(c: &mut [u8], x0: u32, y0: u32, w: u32, h: u32, rgb: [u8; 3]) {
    for y in y0..(y0 + h).min(H) {
        for x in x0..(x0 + w).min(W) {
            let i = ((y * W + x) * 4) as usize;
            c[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
}

/// A "word" of fake glyphs: dark strokes of varying width.
fn text(c: &mut [u8], x0: u32, y0: u32, chars: u32, seed: u32) {
    for k in 0..chars {
        let g = hash(seed.wrapping_add(k));
        if g.is_multiple_of(7) {
            continue; // space
        }
        let gw = 5 + g % 4;
        fill(
            c,
            x0 + k * 10,
            y0 + (g >> 8) % 3,
            gw,
            12 - (g >> 12) % 4,
            [40, 40, 48],
        );
    }
}

fn render(i: u64) -> Vec<u8> {
    let mut c = vec![0u8; (W * H * 4) as usize];
    // Window chrome: gradient sidebar, title bar, content area.
    for y in 0..H {
        for x in 0..W {
            let p = ((y * W + x) * 4) as usize;
            let rgb = if x < 280 {
                [30 + (y * 30 / H) as u8, 34, 52]
            } else if y < 48 {
                [236, 238, 242]
            } else {
                [252, 252, 252]
            };
            c[p..p + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    // Sidebar items with icons.
    for k in 0..14 {
        fill(
            &mut c,
            24,
            80 + k * 56,
            28,
            28,
            [(hash(k) % 200) as u8 + 40, 140, 220],
        );
        text(&mut c, 64, 88 + k * 56, 14, k);
    }
    // Scrolling list: frames 20..60 scroll 8 px per frame.
    let scroll = (i.clamp(20, 60) - 20) as u32 * 8;
    for row in 0..40u32 {
        let y = 70 + row * 40;
        let Some(y) = y.checked_sub(scroll) else {
            continue;
        };
        if !(60..700).contains(&y) {
            continue;
        }
        if row % 2 == 0 {
            fill(&mut c, 300, y, 1100, 36, [244, 246, 250]);
        }
        text(&mut c, 320, y + 12, 60 + hash(row) % 40, row * 977);
    }
    // Typing into an editor box from frame 70 on, one character per frame.
    fill(&mut c, 300, 740, 1560, 300, [255, 255, 255]);
    let typed = i.saturating_sub(70) as u32;
    for line in 0..=(typed / 140) {
        let n = if line == typed / 140 {
            typed % 140
        } else {
            140
        };
        text(&mut c, 320, 760 + line * 22, n, 31337 + line);
    }
    // Cursor: moves during frames 0..20 and 110..150, idle otherwise.
    let t = if i < 20 {
        i
    } else if i < 110 {
        20
    } else {
        20 + i - 110
    };
    let (cx, cy) = (600 + t as u32 * 21, 300 + t as u32 * 9);
    fill(&mut c, cx, cy, 12, 18, [0, 0, 0]);
    fill(&mut c, cx + 2, cy + 2, 8, 14, [255, 255, 255]);
    c
}

impl FrameSource for SyntheticUi {
    fn width(&self) -> u32 {
        W
    }
    fn height(&self) -> u32 {
        H
    }
    fn frame_count(&self) -> usize {
        FRAMES as usize
    }
    fn frames(&self) -> Box<dyn Iterator<Item = Result<TimedFrame, Error>> + '_> {
        Box::new((0..FRAMES).map(|i| {
            Ok(TimedFrame {
                frame: Frame::new(W, H, render(i), i * STEP_MS),
                duration_ms: STEP_MS,
            })
        }))
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut json, mut outdir) = (None, None);
    while let Some(a) = args.next() {
        if a == "--json" {
            json = args.next().map(std::path::PathBuf::from);
        } else {
            outdir = Some(std::path::PathBuf::from(a));
        }
    }
    let results = std::cell::RefCell::new(Vec::<(String, f64, u64)>::new());
    let src = SyntheticUi;
    let t = Instant::now();
    let _ = src.frames().count();
    let gen_s = t.elapsed().as_secs_f64();
    println!("source: {W}x{H}, {FRAMES} frames @ 15 fps (generation {gen_s:.2}s, included below)");

    let sink = |name: &str| -> Box<dyn Write> {
        match &outdir {
            Some(d) => Box::new(std::io::BufWriter::new(
                std::fs::File::create(d.join(name)).unwrap(),
            )),
            None => Box::new(std::io::sink()),
        }
    };
    let counting = |name: &str, f: &dyn Fn(&mut dyn Write) -> zzalcam_encode::EncodeStats| {
        let mut out = CountingWriter {
            inner: sink(name),
            bytes: 0,
        };
        let t = Instant::now();
        let s = f(&mut out);
        results
            .borrow_mut()
            .push((name.to_string(), t.elapsed().as_secs_f64(), out.bytes));
        println!(
            "{name:22} {:7.2}s {:9.1} KB  {} frames",
            t.elapsed().as_secs_f64(),
            out.bytes as f64 / 1024.0,
            s.output_frames
        );
    };
    for (preset, label) in [
        (Preset::Balanced, "balanced"),
        (Preset::Smallest, "smallest"),
        (Preset::Best, "best"),
    ] {
        counting(&format!("gif_{label}.gif"), &|o| {
            gif::encode(&src, &preset.gif(), o, &|_, _| {}).unwrap()
        });
    }
    for (preset, label) in [
        (Preset::Balanced, "balanced"),
        (Preset::Smallest, "smallest"),
    ] {
        counting(&format!("webp_{label}.webp"), &|o| {
            webp::encode(&src, &preset.webp(), o, &|_, _| {}).unwrap()
        });
    }
    if let Some(path) = json {
        let entries: Vec<String> = results
            .borrow()
            .iter()
            .map(|(n, secs, bytes)| {
                format!("  \"{n}\": {{ \"secs\": {secs:.3}, \"bytes\": {bytes} }}")
            })
            .collect();
        std::fs::write(
            &path,
            format!(
                "{{
{}
}}
",
                entries.join(
                    ",
"
                )
            ),
        )
        .unwrap();
    }
}

struct CountingWriter {
    inner: Box<dyn Write>,
    bytes: u64,
}

impl Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
