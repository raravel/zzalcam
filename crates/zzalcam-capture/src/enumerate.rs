//! Monitor and window discovery.

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIFactory1};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetWindowLongW, GetWindowTextLengthW, GetWindowTextW, IsIconic,
    IsWindowVisible, MONITORINFOF_PRIMARY, WS_EX_TOOLWINDOW,
};
use windows::core::BOOL;

use crate::{Error, ScreenRect};

#[derive(Debug, Clone)]
pub struct Monitor {
    pub name: String,
    pub rect: ScreenRect,
    pub primary: bool,
    /// Effective scale factor, e.g. 1.5 for 150 %.
    pub scale: f32,
    pub(crate) adapter: u32,
    pub(crate) output: u32,
}

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub title: String,
    pub rect: ScreenRect,
}

fn to_screen_rect(r: RECT) -> ScreenRect {
    ScreenRect {
        x: r.left,
        y: r.top,
        w: (r.right - r.left).max(0) as u32,
        h: (r.bottom - r.top).max(0) as u32,
    }
}

fn wide_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

/// All desktop-attached outputs, ordered left to right, then top to bottom.
pub fn monitors() -> Result<Vec<Monitor>, Error> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut out = Vec::new();
    for a in 0.. {
        let adapter = match unsafe { factory.EnumAdapters1(a) } {
            Ok(x) => x,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(e.into()),
        };
        for o in 0.. {
            let output = match unsafe { adapter.EnumOutputs(o) } {
                Ok(x) => x,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e.into()),
            };
            let desc = unsafe { output.GetDesc()? };
            if !desc.AttachedToDesktop.as_bool() {
                continue;
            }
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let primary = unsafe { GetMonitorInfoW(desc.Monitor, &mut info) }.as_bool()
                && info.dwFlags & MONITORINFOF_PRIMARY != 0;
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = unsafe { GetDpiForMonitor(desc.Monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
            out.push(Monitor {
                name: wide_to_string(&desc.DeviceName),
                rect: to_screen_rect(desc.DesktopCoordinates),
                primary,
                scale: dx as f32 / 96.0,
                adapter: a,
                output: o,
            });
        }
    }
    out.sort_by_key(|m| (m.rect.x, m.rect.y));
    Ok(out)
}

/// Bounding box of every monitor.
pub fn virtual_desktop() -> Result<ScreenRect, Error> {
    let ms = monitors()?;
    let x0 = ms
        .iter()
        .map(|m| m.rect.x)
        .min()
        .ok_or(Error::NotFound("monitor"))?;
    let y0 = ms.iter().map(|m| m.rect.y).min().unwrap();
    let x1 = ms.iter().map(|m| m.rect.right()).max().unwrap();
    let y1 = ms.iter().map(|m| m.rect.bottom()).max().unwrap();
    Ok(ScreenRect {
        x: x0,
        y: y0,
        w: (x1 - x0) as u32,
        h: (y1 - y0) as u32,
    })
}

/// Visible bounds of a top-level window (by handle) without the invisible resize border.
pub fn window_bounds(hwnd: isize) -> Option<ScreenRect> {
    window_rect(HWND(hwnd as *mut _))
}

/// Visible bounds of a window without the invisible resize border.
pub(crate) fn window_rect(hwnd: HWND) -> Option<ScreenRect> {
    let mut r = RECT::default();
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut _ as *mut _,
            size_of::<RECT>() as u32,
        )
    }
    .ok()?;
    Some(to_screen_rect(r))
}

/// Capturable top-level windows in Z order (topmost first).
pub fn windows() -> Result<Vec<WindowInfo>, Error> {
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let list = unsafe { &mut *(lparam.0 as *mut Vec<WindowInfo>) };
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
                return true.into();
            }
            if GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
                return true.into();
            }
            let mut cloaked = 0u32;
            let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, 4);
            if cloaked != 0 {
                return true.into();
            }
            let len = GetWindowTextLengthW(hwnd);
            if len == 0 {
                return true.into();
            }
            let mut buf = vec![0u16; len as usize + 1];
            let n = GetWindowTextW(hwnd, &mut buf);
            let Some(rect) = window_rect(hwnd) else {
                return true.into();
            };
            if rect.w == 0 || rect.h == 0 {
                return true.into();
            }
            list.push(WindowInfo {
                hwnd: hwnd.0 as isize,
                title: String::from_utf16_lossy(&buf[..n as usize]),
                rect,
            });
        }
        true.into()
    }
    let mut list: Vec<WindowInfo> = Vec::new();
    unsafe { EnumWindows(Some(cb), LPARAM(&mut list as *mut _ as isize))? };
    Ok(list)
}
