//! Run the target picker and print the selection: `cargo run -p zzalcam-overlay --example pick [control|region]`
use zzalcam_overlay::{Mode, Strings, select};

fn main() {
    zzalcam_capture::init_dpi_awareness();
    let mode = match std::env::args().nth(1).as_deref() {
        Some("control") => Mode::Control,
        Some("region") => Mode::RegionOnly,
        _ => Mode::Smart,
    };
    // Like the app, run the picker off the main thread.
    let sel = std::thread::spawn(move || {
        select(mode, Strings::default(), |_| eprintln!("ready")).unwrap()
    })
    .join()
    .unwrap();
    println!("{sel:?}");
}
