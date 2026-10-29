//! Pack raw RGBA frames read from stdin (width × height × 4 bytes each, back to back) into a
//! session file, as if they had been recorded at a fixed frame rate. Used to build the README
//! banner (docs/media/render.py):
//! `cargo run -p zzalcam-core --example frames_session -- <width> <height> <fps> <out.zzcs> < frames`
use std::io::Read;

use zzalcam_core::SessionWriter;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [w, h, fps, out] = &args[..] else {
        panic!("usage: frames_session <width> <height> <fps> <out.zzcs> < frames");
    };
    let w: u32 = w.parse().expect("width");
    let h: u32 = h.parse().expect("height");
    let fps: u64 = fps.parse().expect("fps");
    assert!(fps > 0, "fps must be > 0");
    let mut stdin = std::io::stdin().lock();
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let mut wr = SessionWriter::create(out, w, h).unwrap();
    let ts = |i: usize| i as u64 * 1000 / fps;
    let mut n = 0;
    loop {
        match stdin.read_exact(&mut rgba) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => panic!("reading stdin: {e}"),
        }
        wr.push(&rgba, ts(n)).unwrap();
        n += 1;
    }
    assert!(n > 0, "no frames on stdin");
    let kept = wr.frame_count();
    wr.finish(ts(n)).unwrap();
    println!("{n} frames in, {kept} kept (unchanged ones merged), wrote {out}");
}
