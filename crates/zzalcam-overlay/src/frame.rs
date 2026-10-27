//! Border around the area being recorded (plus the start countdown), excluded from capture.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, DispatchMessageW, HWND_TOPMOST, MSG, PM_REMOVE, PeekMessageW, SW_SHOWNOACTIVATE,
    SWP_NOACTIVATE, SetWindowPos, ShowWindow, TranslateMessage,
};
use zzalcam_capture::ScreenRect;

use crate::Error;
use crate::gfx::{Surface, rect, rgba, text_format};
use crate::win::overlay_window;

/// Border thickness drawn outside the recorded rectangle.
const BORDER: i32 = 3;
const POLL: Duration = Duration::from_millis(50);

enum Cmd {
    Countdown(Option<u32>),
    Paused(bool),
}

/// Handle to the border thread; dropping it closes the border.
pub struct RecordingFrame {
    tx: Option<Sender<Cmd>>,
    thread: Option<JoinHandle<()>>,
}

impl RecordingFrame {
    /// Show a border around `rect`. With `follow`, the border tracks that window as it moves.
    pub fn show(rect: ScreenRect, follow: Option<isize>) -> Result<Self, Error> {
        let (tx, rx) = channel();
        let (ready_tx, ready_rx) = channel();
        let thread = std::thread::spawn(move || {
            let res = run(rect, follow, rx, &ready_tx);
            if let Err(e) = res {
                let _ = ready_tx.send(Err(e));
            }
        });
        ready_rx.recv().map_err(|_| Error::Thread)??;
        Ok(Self {
            tx: Some(tx),
            thread: Some(thread),
        })
    }

    /// Show a big number in the middle of the area, or clear it with `None`.
    pub fn countdown(&self, n: Option<u32>) {
        self.send(Cmd::Countdown(n));
    }

    pub fn set_paused(&self, paused: bool) {
        self.send(Cmd::Paused(paused));
    }

    fn send(&self, cmd: Cmd) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(cmd);
        }
    }
}

impl Drop for RecordingFrame {
    fn drop(&mut self) {
        drop(self.tx.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn outer(r: ScreenRect) -> ScreenRect {
    ScreenRect {
        x: r.x - BORDER,
        y: r.y - BORDER,
        w: r.w + 2 * BORDER as u32,
        h: r.h + 2 * BORDER as u32,
    }
}

fn run(
    mut area: ScreenRect,
    follow: Option<isize>,
    rx: Receiver<Cmd>,
    ready: &Sender<Result<(), Error>>,
) -> Result<(), Error> {
    let hwnd = overlay_window(outer(area))?;
    let big = text_format(140.0, DWRITE_FONT_WEIGHT_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER)?;
    let mut surface = Surface::new(hwnd, outer(area))?;
    let mut countdown: Option<u32> = None;
    let mut paused = false;

    let draw = |s: &Surface, area: ScreenRect, countdown: Option<u32>, paused: bool| {
        s.paint(|p| {
            let color = if paused {
                rgba(245, 158, 11, 1.0)
            } else {
                rgba(239, 68, 68, 1.0)
            };
            let r = rect(area.x as f32, area.y as f32, area.w as f32, area.h as f32);
            p.outline(r, BORDER as f32, color)?;
            if let Some(n) = countdown {
                p.fill(r, rgba(0, 0, 0, 0.35))?;
                p.text(&n.to_string(), &big, r, rgba(255, 255, 255, 1.0))?;
            }
            Ok(())
        })
    };
    draw(&surface, area, countdown, paused)?;
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    let _ = ready.send(Ok(()));

    let result = loop {
        let mut msg = MSG::default();
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let mut redraw = false;
        match rx.recv_timeout(POLL) {
            Ok(Cmd::Countdown(n)) => (countdown, redraw) = (n, true),
            Ok(Cmd::Paused(p)) => (paused, redraw) = (p, true),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break Ok(()),
        }
        if let Some(r) = follow.and_then(zzalcam_capture::window_bounds) {
            // Window recordings keep their start size; only the position follows.
            let moved = ScreenRect {
                x: r.x,
                y: r.y,
                ..area
            };
            if moved != area {
                area = moved;
                let o = outer(area);
                unsafe {
                    let _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOPMOST),
                        o.x,
                        o.y,
                        o.w as i32,
                        o.h as i32,
                        SWP_NOACTIVATE,
                    );
                }
                surface = match Surface::new(hwnd, o) {
                    Ok(s) => s,
                    Err(e) => break Err(e.into()),
                };
                redraw = true;
            }
        }
        // A display change or GPU reset invalidates the render target: rebuild it once and retry.
        if redraw && draw(&surface, area, countdown, paused).is_err() {
            let retried = Surface::new(hwnd, outer(area)).and_then(|s| {
                draw(&s, area, countdown, paused)?;
                Ok(s)
            });
            match retried {
                Ok(s) => surface = s,
                Err(e) => break Err(e.into()),
            }
        }
    };
    drop(surface);
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    result
}
