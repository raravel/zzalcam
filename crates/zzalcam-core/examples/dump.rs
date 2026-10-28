//! Print basic stats of a session file: `cargo run --example dump -- <file.zzcs> [out.ppm]`
use zzalcam_core::{FrameSource, SessionReader};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("session path");
    let rd = SessionReader::open(&path).unwrap();
    println!(
        "{}x{} frames={} end={}ms",
        rd.width(),
        rd.height(),
        rd.frame_count(),
        rd.end_ms()
    );
    let idx: usize = std::env::var("FRAME")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let f = rd.frame(idx).unwrap();
    let px = f.frame.rgba.as_chunks::<4>().0.iter();
    let n = px.len() as f64;
    let mut sum = [0f64; 4];
    for p in px {
        for c in 0..4 {
            sum[c] += p[c] as f64;
        }
    }
    println!("frame mean rgba = {:?}", sum.map(|s| (s / n).round()));
    if let Some(out) = args.next() {
        let mut ppm = format!("P6 {} {} 255\n", rd.width(), rd.height()).into_bytes();
        ppm.extend(
            f.frame
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]]),
        );
        std::fs::write(out, ppm).unwrap();
    }
}
