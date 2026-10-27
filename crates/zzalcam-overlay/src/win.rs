//! Window plumbing shared by the overlays.

use std::sync::Once;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, RegisterClassExW, SetWindowDisplayAffinity,
    WDA_EXCLUDEFROMCAPTURE, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{Result, w};
use zzalcam_capture::ScreenRect;

const CLASS: windows::core::PCWSTR = w!("ZzalcamOverlay");

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// A click-through, never-activated, topmost layered popup covering `r`, invisible to screen capture.
///
/// Input reaches the overlays through low-level hooks instead, so that hit-testing (`WindowFromPoint`,
/// UI Automation) sees the windows underneath.
pub fn overlay_window(r: ScreenRect) -> Result<HWND> {
    static REGISTER: Once = Once::new();
    unsafe {
        let instance = GetModuleHandleW(None)?;
        REGISTER.call_once(|| {
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: instance.into(),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&wc);
        });
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            CLASS,
            w!("zzalcam"),
            WS_POPUP,
            r.x,
            r.y,
            r.w as i32,
            r.h as i32,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        exclude(hwnd);
        Ok(hwnd)
    }
}

/// Hide a window (by raw handle) from screen capture, ours and everyone else's.
pub fn exclude_from_capture(hwnd: isize) {
    exclude(HWND(hwnd as *mut _));
}

/// Needs Windows 10 2004+ (our minimum).
fn exclude(hwnd: HWND) {
    // Debug builds can keep overlays visible to screenshots for visual QA.
    if cfg!(debug_assertions) && std::env::var_os("ZZALCAM_SHOW_OVERLAYS").is_some() {
        return;
    }
    let _ = unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) };
}
