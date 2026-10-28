//! The unified target picker: one dimmed layered window per monitor, driven by low-level hooks.
//!
//! * drag → free region · hover + click / Enter → window (or UI element in control mode)
//! * `Tab` toggles control mode, the wheel walks out to parents / back in to children
//! * `F` → monitor under the cursor · `A` → all monitors · arrows nudge the cursor (Shift ×10)
//! * `Esc` or right click cancels
//!
//! Input arrives from [`crate::hooks`] (a separate thread), so drawing here never stalls the cursor.

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, DispatchMessageW, GA_ROOT, GetAncestor, GetClassNameW, GetCursorPos,
    GetMessageW, GetWindowThreadProcessId, MSG, PM_NOREMOVE, PM_REMOVE, PeekMessageW,
    PostThreadMessageW, SW_SHOWNOACTIVATE, SetCursorPos, ShowWindow, TranslateMessage, WM_APP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONUP, WindowFromPoint,
};
use zzalcam_capture::{Monitor, ScreenRect, Target};

use crate::gfx::{Painter, Surface, measure, rect, rgba, text_format};
use crate::hooks::{InputHooks, WM_HOOK_KEY, WM_HOOK_MOUSE};
use crate::uia::{Uia, WM_UIA_RESULT};
use crate::win::overlay_window;
use crate::{Error, Strings};

const WM_CANCEL: u32 = WM_APP + 4;
/// Movement before a press becomes a drag.
const DRAG_THRESHOLD: i32 = 4;
const MAG_SRC: i32 = 15;
const MAG_SCALE: f32 = 8.0;

/// How the picker starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Hover highlights windows; dragging selects a region.
    Smart,
    /// Hover highlights UI elements (controls).
    Control,
    /// Only dragging a region; no hover highlight.
    RegionOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Region,
    Window,
    Control,
    Monitor,
    AllMonitors,
}

#[derive(Debug, Clone)]
pub struct Selection {
    pub kind: Kind,
    pub target: Target,
    /// Screen rectangle at selection time.
    pub rect: ScreenRect,
}

/// Lets another thread cancel a running [`select`].
#[derive(Debug, Clone, Copy)]
pub struct SelectHandle(u32);

impl SelectHandle {
    pub fn cancel(&self) {
        let _ = unsafe { PostThreadMessageW(self.0, WM_CANCEL, WPARAM(0), LPARAM(0)) };
    }
}

#[derive(Clone)]
enum Hover {
    None,
    Window(isize, ScreenRect),
    Desktop(usize),
    Control(ScreenRect),
}

struct Fonts {
    label: IDWriteTextFormat,
    help: IDWriteTextFormat,
    coords: IDWriteTextFormat,
}

struct State {
    thread: u32,
    mode: Mode,
    monitors: Vec<Monitor>,
    surfaces: Vec<Surface>,
    fonts: Fonts,
    strings: Strings,
    cursor: POINT,
    hover: Hover,
    /// UI element rectangles under the cursor, innermost first; `level` picks one.
    chain: Vec<ScreenRect>,
    level: usize,
    press: Option<POINT>,
    dragging: bool,
    result: Option<Option<Selection>>,
    uia: Option<Uia>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn monitor_at(monitors: &[Monitor], p: POINT) -> Option<usize> {
    monitors.iter().position(|m| {
        p.x >= m.rect.x && p.x < m.rect.right() && p.y >= m.rect.y && p.y < m.rect.bottom()
    })
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

impl State {
    fn desktop_rect(&self) -> ScreenRect {
        let x0 = self.monitors.iter().map(|m| m.rect.x).min().unwrap_or(0);
        let y0 = self.monitors.iter().map(|m| m.rect.y).min().unwrap_or(0);
        let x1 = self
            .monitors
            .iter()
            .map(|m| m.rect.right())
            .max()
            .unwrap_or(0);
        let y1 = self
            .monitors
            .iter()
            .map(|m| m.rect.bottom())
            .max()
            .unwrap_or(0);
        ScreenRect {
            x: x0,
            y: y0,
            w: (x1 - x0) as u32,
            h: (y1 - y0) as u32,
        }
    }

    fn drag_rect(&self) -> Option<ScreenRect> {
        let p = self.press.filter(|_| self.dragging)?;
        let (x0, x1) = (p.x.min(self.cursor.x), p.x.max(self.cursor.x));
        let (y0, y1) = (p.y.min(self.cursor.y), p.y.max(self.cursor.y));
        // Inclusive of the pixel under the cursor.
        Some(ScreenRect {
            x: x0,
            y: y0,
            w: (x1 - x0 + 1) as u32,
            h: (y1 - y0 + 1) as u32,
        })
    }

    fn highlight(&self) -> Option<(ScreenRect, &str)> {
        if let Some(r) = self.drag_rect() {
            return Some((r, &self.strings.region));
        }
        match &self.hover {
            Hover::None => None,
            Hover::Window(_, r) => Some((*r, &self.strings.window)),
            Hover::Desktop(i) => Some((self.monitors[*i].rect, &self.strings.monitor)),
            Hover::Control(r) => Some((*r, &self.strings.control)),
        }
    }

    fn update_hover(&mut self) {
        match self.mode {
            Mode::RegionOnly => self.hover = Hover::None,
            Mode::Control => {
                if let Some(uia) = &self.uia {
                    uia.request(self.cursor);
                }
            }
            Mode::Smart => {
                let root = unsafe { GetAncestor(WindowFromPoint(self.cursor), GA_ROOT) };
                let mut pid = 0;
                unsafe { GetWindowThreadProcessId(root, Some(&mut pid)) };
                let desktop = root.is_invalid()
                    || pid == unsafe { GetCurrentProcessId() }
                    || matches!(class_name(root).as_str(), "Progman" | "WorkerW");
                self.hover = match (desktop, zzalcam_capture::window_bounds(root.0 as isize)) {
                    (false, Some(r)) => Hover::Window(root.0 as isize, r),
                    _ => {
                        monitor_at(&self.monitors, self.cursor).map_or(Hover::None, Hover::Desktop)
                    }
                };
            }
        }
    }

    fn finish(&mut self, sel: Option<Selection>) {
        self.result = Some(sel);
    }

    fn pick_hover(&mut self) {
        let sel = match &self.hover {
            Hover::None => return,
            Hover::Window(h, r) => Selection {
                kind: Kind::Window,
                target: Target::Window(*h),
                rect: *r,
            },
            Hover::Desktop(i) => Selection {
                kind: Kind::Monitor,
                target: Target::Monitor(*i),
                rect: self.monitors[*i].rect,
            },
            Hover::Control(r) => Selection {
                kind: Kind::Control,
                target: Target::Region(*r),
                rect: *r,
            },
        };
        self.finish(Some(sel));
    }

    fn on_mouse(&mut self, msg: u32, pt: POINT, wheel: i16) {
        self.cursor = pt;
        match msg {
            WM_MOUSEMOVE => {
                if let Some(p) = self.press
                    && ((pt.x - p.x).abs() > DRAG_THRESHOLD || (pt.y - p.y).abs() > DRAG_THRESHOLD)
                {
                    self.dragging = true;
                }
                if !self.dragging {
                    self.update_hover();
                }
            }
            WM_LBUTTONDOWN => {
                self.press = Some(pt);
                self.dragging = false;
            }
            WM_LBUTTONUP => {
                if let Some(r) = self.drag_rect() {
                    self.finish(Some(Selection {
                        kind: Kind::Region,
                        target: Target::Region(r),
                        rect: r,
                    }));
                } else {
                    self.press = None;
                    self.pick_hover();
                }
            }
            // Cancel on release: finishing on press would unhook before the release, which then
            // reaches the window below and opens its context menu.
            WM_RBUTTONUP => self.finish(None),
            WM_MOUSEWHEEL if self.mode == Mode::Control && !self.chain.is_empty() => {
                // Wheel up walks out to the parent, down back in to the child.
                self.level = if wheel > 0 {
                    (self.level + 1).min(self.chain.len() - 1)
                } else {
                    self.level.saturating_sub(1)
                };
                self.hover = Hover::Control(self.chain[self.level]);
            }
            _ => {}
        }
    }

    fn on_key(&mut self, vk: u16) {
        let shift = unsafe { GetAsyncKeyState(VK_SHIFT.0 as i32) } as u16 & 0x8000 != 0;
        let step = if shift { 10 } else { 1 };
        match vk {
            v if v == VK_ESCAPE.0 => self.finish(None),
            v if v == VK_RETURN.0 => self.pick_hover(),
            v if v == VK_TAB.0 => {
                self.mode = if self.mode == Mode::Control {
                    Mode::Smart
                } else {
                    Mode::Control
                };
                self.chain.clear();
                self.hover = Hover::None;
                self.update_hover();
            }
            v if v == b'F' as u16 => {
                if let Some(i) = monitor_at(&self.monitors, self.cursor) {
                    let rect = self.monitors[i].rect;
                    self.finish(Some(Selection {
                        kind: Kind::Monitor,
                        target: Target::Monitor(i),
                        rect,
                    }));
                }
            }
            v if v == b'A' as u16 => {
                let rect = self.desktop_rect();
                self.finish(Some(Selection {
                    kind: Kind::AllMonitors,
                    target: Target::AllMonitors,
                    rect,
                }));
            }
            v if [VK_LEFT.0, VK_RIGHT.0, VK_UP.0, VK_DOWN.0].contains(&v) => {
                let (dx, dy) = match v {
                    v if v == VK_LEFT.0 => (-step, 0),
                    v if v == VK_RIGHT.0 => (step, 0),
                    v if v == VK_UP.0 => (0, -step),
                    _ => (0, step),
                };
                // Generates a real mouse move, which flows back through the hook.
                let _ = unsafe { SetCursorPos(self.cursor.x + dx, self.cursor.y + dy) };
            }
            _ => {}
        }
    }

    fn on_uia(&mut self) {
        let Some((p, chain)) = self.uia.as_ref().and_then(Uia::take_result) else {
            return;
        };
        if self.mode != Mode::Control || (p.x, p.y) != (self.cursor.x, self.cursor.y) {
            return; // stale
        }
        self.chain = chain;
        self.level = 0;
        self.hover = self
            .chain
            .first()
            .map_or(Hover::None, |r| Hover::Control(*r));
    }

    fn paint(&self) -> Result<(), Error> {
        let mag = magnifier_pixels(self.cursor);
        let cursor_monitor = monitor_at(&self.monitors, self.cursor);
        for (i, s) in self.surfaces.iter().enumerate() {
            s.paint(|p| {
                self.paint_monitor(p, s.bounds, Some(i) == cursor_monitor, mag.as_deref())
            })?;
        }
        Ok(())
    }

    fn paint_monitor(
        &self,
        p: &Painter,
        b: ScreenRect,
        has_cursor: bool,
        mag: Option<&[u8]>,
    ) -> windows::core::Result<()> {
        let dim = rgba(0, 0, 0, 0.45);
        let accent = rgba(47, 128, 237, 1.0);
        let (bx, by, bw, bh) = (b.x as f32, b.y as f32, b.w as f32, b.h as f32);
        let hl = self.highlight();

        // Dim everything except the highlighted rectangle.
        match hl.and_then(|(r, _)| r.intersect(&b)) {
            None => p.fill(rect(bx, by, bw, bh), dim)?,
            Some(c) => {
                let (cx, cy, cw, ch) = (c.x as f32, c.y as f32, c.w as f32, c.h as f32);
                p.fill(rect(bx, by, bw, cy - by), dim)?;
                p.fill(rect(bx, cy + ch, bw, by + bh - cy - ch), dim)?;
                p.fill(rect(bx, cy, cx - bx, ch), dim)?;
                p.fill(rect(cx + cw, cy, bx + bw - cx - cw, ch), dim)?;
            }
        }

        if let Some((r, kind)) = hl {
            let rf = rect(r.x as f32, r.y as f32, r.w as f32, r.h as f32);
            p.outline(rf, 2.0, accent)?;
            let label = format!("{kind}  {} × {}", r.w, r.h);
            let (tw, th) = measure(&label, &self.fonts.label)?;
            let (lw, lh) = (tw + 16.0, th + 8.0);
            // Place the label once, on the monitor holding the rectangle's corner; every surface
            // draws it and the ones it doesn't touch clip it away.
            let corner = POINT {
                x: r.x.max(self.desktop_rect().x),
                y: r.y.max(self.desktop_rect().y),
            };
            let m = monitor_at(&self.monitors, corner).map_or(b, |i| self.monitors[i].rect);
            let (mx, my, mw) = (m.x as f32, m.y as f32, m.w as f32);
            let ly = if rf.top - lh - 6.0 >= my {
                rf.top - lh - 6.0
            } else {
                rf.top.max(my) + 6.0
            };
            let lx = rf.left.max(mx).min(mx + mw - lw);
            p.fill_rounded(rect(lx, ly, lw, lh), 4.0, rgba(20, 22, 28, 0.9))?;
            p.text(
                &label,
                &self.fonts.label,
                rect(lx + 8.0, ly, tw, lh),
                rgba(255, 255, 255, 1.0),
            )?;
        }

        if has_cursor {
            let (x, y) = (self.cursor.x as f32 + 0.5, self.cursor.y as f32 + 0.5);
            let guide = rgba(255, 255, 255, 0.35);
            p.line(bx, y, bx + bw, y, 1.0, guide)?;
            p.line(x, by, x, by + bh, 1.0, guide)?;

            if let Some(px) = mag {
                let size = MAG_SRC as f32 * MAG_SCALE;
                let mut mx = x + 24.0;
                let mut my = y + 24.0;
                if mx + size > bx + bw {
                    mx = x - 24.0 - size;
                }
                if my + size + 24.0 > by + bh {
                    my = y - 48.0 - size;
                }
                p.fill(
                    rect(mx - 2.0, my - 2.0, size + 4.0, size + 26.0),
                    rgba(20, 22, 28, 0.95),
                )?;
                p.pixels(px, MAG_SRC as u32, MAG_SRC as u32, rect(mx, my, size, size))?;
                let c = (MAG_SRC / 2) as f32 * MAG_SCALE;
                p.outline(rect(mx + c, my + c, MAG_SCALE, MAG_SCALE), 1.0, accent)?;
                let coords = format!("{}, {}", self.cursor.x, self.cursor.y);
                p.text(
                    &coords,
                    &self.fonts.coords,
                    rect(mx, my + size, size, 22.0),
                    rgba(255, 255, 255, 1.0),
                )?;
            }

            let help = match self.mode {
                Mode::Control => &self.strings.help_control,
                _ => &self.strings.help,
            };
            let (tw, th) = measure(help, &self.fonts.help)?;
            let (hw, hh) = (tw + 28.0, th + 14.0);
            let hx = bx + (bw - hw) / 2.0;
            let hy = by + bh - hh - 32.0;
            p.fill_rounded(rect(hx, hy, hw, hh), 8.0, rgba(20, 22, 28, 0.9))?;
            p.text(
                help,
                &self.fonts.help,
                rect(hx, hy, hw, hh),
                rgba(235, 238, 245, 1.0),
            )?;
        }
        Ok(())
    }
}

/// Live BGRA pixels around the cursor (overlays are excluded from capture, so this is the desktop).
fn magnifier_pixels(c: POINT) -> Option<Vec<u8>> {
    unsafe {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: MAG_SRC,
                biHeight: -MAG_SRC,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let out = CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0)
            .ok()
            .and_then(|dib| {
                let old = SelectObject(dc, dib.into());
                let half = MAG_SRC / 2;
                let ok = BitBlt(
                    dc,
                    0,
                    0,
                    MAG_SRC,
                    MAG_SRC,
                    Some(screen),
                    c.x - half,
                    c.y - half,
                    SRCCOPY,
                )
                .is_ok();
                let px = ok.then(|| {
                    let mut v = std::slice::from_raw_parts(
                        bits as *const u8,
                        (MAG_SRC * MAG_SRC * 4) as usize,
                    )
                    .to_vec();
                    // GDI leaves alpha at 0; the bitmap is premultiplied, so make it opaque.
                    v.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p[3] = 255);
                    v
                });
                SelectObject(dc, old);
                let _ = DeleteObject(dib.into());
                px
            });
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        out
    }
}

/// Run the picker on the calling thread until the user selects or cancels.
///
/// The thread must pump no other windows. `on_start` receives a handle that can cancel the picker
/// from another thread. Returns `None` when cancelled.
pub fn select(
    mode: Mode,
    strings: Strings,
    on_start: impl FnOnce(SelectHandle),
) -> Result<Option<Selection>, Error> {
    let thread = unsafe { GetCurrentThreadId() };
    // Create the message queue first, so the handle can cancel even while we set up.
    let mut msg = MSG::default();
    let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };
    on_start(SelectHandle(thread));
    let monitors = zzalcam_capture::monitors()?;
    let mut surfaces = Vec::with_capacity(monitors.len());
    for m in &monitors {
        let hwnd = overlay_window(m.rect)?;
        surfaces.push(Surface::new(hwnd, m.rect)?);
    }
    let mut cursor = POINT::default();
    let _ = unsafe { GetCursorPos(&mut cursor) };
    let fonts = Fonts {
        label: text_format(
            14.0,
            DWRITE_FONT_WEIGHT_SEMI_BOLD,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        )?,
        help: text_format(
            15.0,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        )?,
        coords: text_format(
            12.0,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        )?,
    };

    let mut state = State {
        thread,
        mode,
        monitors,
        surfaces,
        fonts,
        strings,
        cursor,
        hover: Hover::None,
        chain: Vec::new(),
        level: 0,
        press: None,
        dragging: false,
        result: None,
        uia: Some(Uia::start(thread)),
    };
    state.update_hover();
    state.paint()?;
    for s in &state.surfaces {
        let _ = unsafe { ShowWindow(s.hwnd, SW_SHOWNOACTIVATE) };
    }
    STATE.with(|s| *s.borrow_mut() = Some(state));

    let result = (|| -> Result<Option<Selection>, Error> {
        let _hooks = InputHooks::start(thread)?;

        loop {
            if !unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                return Ok(None);
            }
            // Handle everything queued, then draw once.
            let mut dirty = false;
            loop {
                dirty |= STATE.with(|s| {
                    let mut s = s.borrow_mut();
                    let st = s.as_mut().unwrap();
                    match msg.message {
                        WM_HOOK_MOUSE => {
                            let pt = POINT {
                                x: msg.lParam.0 as u64 as u32 as i32,
                                y: (msg.lParam.0 as u64 >> 32) as u32 as i32,
                            };
                            st.on_mouse(
                                msg.wParam.0 as u32 & 0xffff,
                                pt,
                                (msg.wParam.0 >> 16) as u16 as i16,
                            );
                            true
                        }
                        WM_HOOK_KEY => {
                            st.on_key(msg.wParam.0 as u16);
                            true
                        }
                        WM_UIA_RESULT => {
                            st.on_uia();
                            true
                        }
                        WM_CANCEL => {
                            st.finish(None);
                            false
                        }
                        _ => {
                            unsafe {
                                let _ = TranslateMessage(&msg);
                                DispatchMessageW(&msg);
                            }
                            false
                        }
                    }
                });
                if !unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
                    break;
                }
            }
            let done = STATE.with(|s| -> Result<Option<Option<Selection>>, Error> {
                let s = s.borrow();
                let st = s.as_ref().unwrap();
                if st.result.is_none() && dirty {
                    st.paint()?;
                }
                Ok(st.result.clone())
            })?;
            if let Some(sel) = done {
                return Ok(sel);
            }
        }
    })();

    if let Some(st) = STATE.with(|s| s.borrow_mut().take()) {
        debug_assert_eq!(st.thread, thread);
        let hwnds: Vec<HWND> = st.surfaces.iter().map(|s| s.hwnd).collect();
        drop(st);
        for h in hwnds {
            let _ = unsafe { DestroyWindow(h) };
        }
    }
    result
}
