//! Write a synthetic recording (moving box over a gradient) for testing the editor:
//! `cargo run -p zzalcam-core --example synth_session -- <out.zzcs> [frames]`
use zzalcam_core::SessionWriter;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("output path");
    let n: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(45);
    let (w, h) = (640u32, 360u32);
    let mut wr = SessionWriter::create(&path, w, h).unwrap();
    for i in 0..n {
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let in_box = (x as i64 - (40 + i as i64 * 12)).unsigned_abs() < 40
                    && (y as i64 - 180).unsigned_abs() < 40;
                let px = if in_box {
                    [230, 60, 60, 255]
                } else {
                    [(x * 255 / w) as u8, 90, (y * 255 / h) as u8, 255]
                };
                rgba.extend_from_slice(&px);
            }
        }
        wr.push(&rgba, i * 66).unwrap();
    }
    wr.finish(n * 66).unwrap();
    println!("wrote {n} frames to {path}");
}
