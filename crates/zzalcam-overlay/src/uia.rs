//! UI Automation lookups on a worker thread.
//!
//! UIA calls are cross-process and can take tens of milliseconds; doing them on the overlay thread
//! would stall the low-level mouse hook and with it the whole system cursor. The worker always
//! answers the most recent point only.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use windows::Win32::Foundation::{LPARAM, POINT, WPARAM};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation};
use windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW;
use zzalcam_capture::ScreenRect;

/// Posted to the requesting thread when a lookup finishes; fetch it with [`Uia::take_result`].
pub const WM_UIA_RESULT: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;
const MAX_DEPTH: usize = 32;

#[derive(Default)]
struct Shared {
    request: Option<POINT>,
    result: Option<(POINT, Vec<ScreenRect>)>,
    quit: bool,
}

pub struct Uia {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    worker: Option<JoinHandle<()>>,
}

impl Uia {
    /// Start the worker; results are announced to `notify_thread` with [`WM_UIA_RESULT`].
    pub fn start(notify_thread: u32) -> Self {
        let shared = Arc::new((Mutex::new(Shared::default()), Condvar::new()));
        let s = shared.clone();
        let worker = std::thread::spawn(move || worker(s, notify_thread));
        Self {
            shared,
            worker: Some(worker),
        }
    }

    /// Ask for the element chain under `p`, replacing any request not yet started.
    pub fn request(&self, p: POINT) {
        let (lock, cv) = &*self.shared;
        lock.lock().unwrap().request = Some(p);
        cv.notify_one();
    }

    /// Element rectangles under the requested point, innermost first, up to the top-level window.
    pub fn take_result(&self) -> Option<(POINT, Vec<ScreenRect>)> {
        self.shared.0.lock().unwrap().result.take()
    }
}

impl Drop for Uia {
    fn drop(&mut self) {
        let (lock, cv) = &*self.shared;
        lock.lock().unwrap().quit = true;
        cv.notify_one();
        // Don't block the overlay on a hung cross-process UIA call; the worker exits on its own.
        drop(self.worker.take());
    }
}

fn worker(shared: Arc<(Mutex<Shared>, Condvar)>, notify_thread: u32) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if let Ok(uia) =
            CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
        {
            let (lock, cv) = &*shared;
            loop {
                let p = {
                    let mut s = lock.lock().unwrap();
                    while s.request.is_none() && !s.quit {
                        s = cv.wait(s).unwrap();
                    }
                    if s.quit {
                        break;
                    }
                    s.request.take().unwrap()
                };
                let chain = element_chain(&uia, p).unwrap_or_default();
                lock.lock().unwrap().result = Some((p, chain));
                let _ = PostThreadMessageW(notify_thread, WM_UIA_RESULT, WPARAM(0), LPARAM(0));
            }
        }
        CoUninitialize();
    }
}

fn element_chain(uia: &IUIAutomation, p: POINT) -> windows::core::Result<Vec<ScreenRect>> {
    unsafe {
        let walker = uia.ControlViewWalker()?;
        let root = uia.GetRootElement()?;
        let me = GetCurrentProcessId() as i32;
        let mut el = uia.ElementFromPoint(p)?;
        let mut out: Vec<ScreenRect> = Vec::new();
        for _ in 0..MAX_DEPTH {
            if uia.CompareElements(&el, &root)?.as_bool() {
                break;
            }
            // Never offer our own overlays or control bar as a target.
            if el.CurrentProcessId()? != me {
                let r = el.CurrentBoundingRectangle()?;
                let sr = ScreenRect {
                    x: r.left,
                    y: r.top,
                    w: (r.right - r.left).max(0) as u32,
                    h: (r.bottom - r.top).max(0) as u32,
                };
                if sr.w > 0 && sr.h > 0 && out.last() != Some(&sr) {
                    out.push(sr);
                }
            }
            el = match walker.GetParentElement(&el) {
                Ok(parent) => parent,
                Err(_) => break,
            };
        }
        Ok(out)
    }
}
