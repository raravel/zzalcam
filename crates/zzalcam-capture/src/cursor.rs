//! Mouse cursor compositing for DXGI capture (Desktop Duplication frames do not include the cursor).
//!
//! The capturer keeps a clean canvas; [`Composer`] maintains a separate output canvas with the cursor
//! drawn on top, so cursor pixels never leak into change detection of the screen content.

use std::collections::HashMap;

use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
    GetObjectW, HBITMAP, HDC, ReleaseDC,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, GetCursorInfo, GetIconInfo, HCURSOR, ICONINFO,
};

use zzalcam_core::Rect;

#[derive(Clone, Copy)]
enum Px {
    Clear,
    /// Straight-alpha RGBA.
    Color([u8; 4]),
    /// Monochrome cursors' XOR pixels invert the screen.
    Invert,
}

struct Shape {
    w: u32,
    h: u32,
    hot_x: i32,
    hot_y: i32,
    px: Vec<Px>,
}

fn read_bitmap(dc: HDC, bmp: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    let mut info = BITMAP::default();
    if unsafe {
        GetObjectW(
            bmp.into(),
            size_of::<BITMAP>() as i32,
            Some(&mut info as *mut _ as *mut _),
        )
    } == 0
    {
        return None;
    }
    let (w, h) = (info.bmWidth as u32, info.bmHeight as u32);
    let mut bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32), // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let lines = unsafe {
        GetDIBits(
            dc,
            bmp,
            0,
            h,
            Some(buf.as_mut_ptr() as *mut _),
            &mut bi,
            DIB_RGB_COLORS,
        )
    };
    (lines == h as i32).then_some((w, h, buf))
}

fn load_shape(cursor: HCURSOR) -> Option<Shape> {
    let mut ii = ICONINFO::default();
    unsafe { GetIconInfo(cursor.into(), &mut ii) }.ok()?;
    let dc = unsafe { GetDC(None) };
    let mask = read_bitmap(dc, ii.hbmMask);
    let color = (!ii.hbmColor.is_invalid())
        .then(|| read_bitmap(dc, ii.hbmColor))
        .flatten();
    unsafe {
        ReleaseDC(None, dc);
        let _ = DeleteObject(ii.hbmMask.into());
        if !ii.hbmColor.is_invalid() {
            let _ = DeleteObject(ii.hbmColor.into());
        }
    }
    let (mw, mh, mask) = mask?;
    let and_bit = |i: usize| mask[i * 4] != 0; // DIB of a 1bpp mask expands to 0x00 / 0xff

    let (w, h, px) = match color {
        Some((w, h, c)) => {
            let has_alpha = c.chunks_exact(4).any(|p| p[3] != 0);
            let px = (0..(w * h) as usize)
                .map(|i| {
                    let (b, g, r, a) = (c[i * 4], c[i * 4 + 1], c[i * 4 + 2], c[i * 4 + 3]);
                    if has_alpha {
                        if a == 0 {
                            Px::Clear
                        } else {
                            Px::Color([r, g, b, a])
                        }
                    } else if !and_bit(i) {
                        Px::Color([r, g, b, 255])
                    } else if r | g | b != 0 {
                        Px::Invert
                    } else {
                        Px::Clear
                    }
                })
                .collect();
            (w, h, px)
        }
        None => {
            // Monochrome: top half AND mask, bottom half XOR mask.
            let h = mh / 2;
            let px = (0..(mw * h) as usize)
                .map(
                    |i| match (and_bit(i), mask[(i + (mw * h) as usize) * 4] != 0) {
                        (false, false) => Px::Color([0, 0, 0, 255]),
                        (false, true) => Px::Color([255, 255, 255, 255]),
                        (true, false) => Px::Clear,
                        (true, true) => Px::Invert,
                    },
                )
                .collect();
            (mw, h, px)
        }
    };
    Some(Shape {
        w,
        h,
        hot_x: ii.xHotspot as i32,
        hot_y: ii.yHotspot as i32,
        px,
    })
}

const MAX_CACHED_SHAPES: usize = 64;

pub struct Composer {
    shapes: HashMap<isize, Option<Shape>>,
    out: Vec<u8>,
    width: u32,
    height: u32,
    origin: (i32, i32),
    /// Canvas area covered by the cursor in `out`.
    drawn: Option<Rect>,
    last: Option<(isize, POINT)>,
    primed: bool,
}

impl Composer {
    pub fn new(width: u32, height: u32, origin: (i32, i32)) -> Self {
        Self {
            shapes: HashMap::new(),
            out: vec![0; (width * height * 4) as usize],
            width,
            height,
            origin,
            drawn: None,
            last: None,
            primed: false,
        }
    }

    pub fn canvas(&self) -> &[u8] {
        &self.out
    }

    /// Rebuild the output from `clean`. Returns true if the output changed.
    pub fn compose(&mut self, clean: &[u8], clean_changed: bool) -> bool {
        let mut ci = CURSORINFO {
            cbSize: size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        let state = (unsafe { GetCursorInfo(&mut ci) }.is_ok()
            && ci.flags.0 & CURSOR_SHOWING.0 != 0)
            .then_some((ci.hCursor.0 as isize, ci.ptScreenPos));
        let cursor_changed =
            state.map(|(c, p)| (c, p.x, p.y)) != self.last.map(|(c, p)| (c, p.x, p.y));
        if self.primed && !clean_changed && !cursor_changed {
            return false;
        }

        if clean_changed || !self.primed {
            self.primed = true;
            self.out.copy_from_slice(clean);
        } else if let Some(r) = self.drawn {
            restore(&mut self.out, clean, self.width, r);
        }
        self.drawn = None;
        self.last = state;

        let Some((handle, pos)) = state else {
            return true;
        };
        // Apps can create cursors dynamically; don't let the cache grow without bound.
        if self.shapes.len() >= MAX_CACHED_SHAPES && !self.shapes.contains_key(&handle) {
            self.shapes.clear();
        }
        let shape = self
            .shapes
            .entry(handle)
            .or_insert_with(|| load_shape(HCURSOR(handle as *mut _)));
        if let Some(shape) = shape {
            self.drawn = draw(
                &mut self.out,
                self.width,
                self.height,
                shape,
                pos.x - shape.hot_x - self.origin.0,
                pos.y - shape.hot_y - self.origin.1,
            );
        }
        true
    }
}

fn restore(out: &mut [u8], clean: &[u8], width: u32, r: Rect) {
    let stride = width as usize * 4;
    for y in r.y as usize..(r.y + r.h) as usize {
        let s = y * stride + r.x as usize * 4;
        let e = s + r.w as usize * 4;
        out[s..e].copy_from_slice(&clean[s..e]);
    }
}

fn draw(out: &mut [u8], cw: u32, ch: u32, shape: &Shape, x0: i32, y0: i32) -> Option<Rect> {
    let (cx0, cy0) = (x0.max(0), y0.max(0));
    let cx1 = (x0 + shape.w as i32).min(cw as i32);
    let cy1 = (y0 + shape.h as i32).min(ch as i32);
    if cx1 <= cx0 || cy1 <= cy0 {
        return None;
    }
    for y in cy0..cy1 {
        for x in cx0..cx1 {
            let p = shape.px[((y - y0) as u32 * shape.w + (x - x0) as u32) as usize];
            let d = &mut out[((y as u32 * cw + x as u32) * 4) as usize..][..3];
            match p {
                Px::Clear => {}
                Px::Invert => d.iter_mut().for_each(|c| *c = 255 - *c),
                Px::Color([r, g, b, a]) => {
                    let a = a as u32;
                    for (dc, sc) in d.iter_mut().zip([r, g, b]) {
                        *dc = ((sc as u32 * a + *dc as u32 * (255 - a) + 127) / 255) as u8;
                    }
                }
            }
        }
    }
    Some(Rect::new(
        cx0 as u32,
        cy0 as u32,
        (cx1 - cx0) as u32,
        (cy1 - cy0) as u32,
    ))
}
