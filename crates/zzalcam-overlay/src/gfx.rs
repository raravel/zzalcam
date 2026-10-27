//! Per-pixel-alpha layered windows drawn with Direct2D / DirectWrite.
//!
//! Each [`Surface`] owns a top-down 32bpp DIB bound to a Direct2D DC render target; after drawing,
//! `UpdateLayeredWindow` pushes the premultiplied pixels to the window.

use std::sync::OnceLock;

use windows::Win32::Foundation::{HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_BITMAP_PROPERTIES,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_MULTI_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_ROUNDED_RECT, D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_METRICS, DWriteCreateFactory, IDWriteFactory,
    IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, HDC,
    HGDIOBJ, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{ULW_ALPHA, UpdateLayeredWindow};
use windows::core::{HSTRING, Result};
use zzalcam_capture::ScreenRect;

pub fn rgba(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a,
    }
}

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

struct Factories {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
}

// Both factories are created thread-safe (multi-threaded D2D, shared DirectWrite), so overlay threads
// may share them.
unsafe impl Send for Factories {}
unsafe impl Sync for Factories {}

fn factories() -> Result<&'static Factories> {
    static F: OnceLock<std::result::Result<Factories, windows::core::Error>> = OnceLock::new();
    F.get_or_init(|| unsafe {
        Ok(Factories {
            d2d: D2D1CreateFactory(D2D1_FACTORY_TYPE_MULTI_THREADED, None)?,
            dwrite: DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?,
        })
    })
    .as_ref()
    .map_err(Clone::clone)
}

pub fn text_format(
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
    align: DWRITE_TEXT_ALIGNMENT,
) -> Result<IDWriteTextFormat> {
    unsafe {
        let f = factories()?.dwrite.CreateTextFormat(
            &HSTRING::from("Segoe UI"),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            &HSTRING::from(""),
        )?;
        f.SetTextAlignment(align)?;
        f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        Ok(f)
    }
}

/// Width and height a single line of text takes in `format`.
pub fn measure(text: &str, format: &IDWriteTextFormat) -> Result<(f32, f32)> {
    let wide: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        let layout = factories()?
            .dwrite
            .CreateTextLayout(&wide, format, 10_000.0, 10_000.0)?;
        let mut m = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut m)?;
        Ok((m.widthIncludingTrailingWhitespace, m.height))
    }
}

/// Memory DC with a selected top-down 32bpp DIB; freed on drop, including on error paths.
struct Dib {
    dc: HDC,
    bitmap: Option<(HBITMAP, HGDIOBJ)>,
}

impl Dib {
    fn new(w: u32, h: u32) -> Result<Self> {
        unsafe {
            let mut dib = Dib {
                dc: CreateCompatibleDC(None),
                bitmap: None,
            };
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(Some(dib.dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0)?;
            dib.bitmap = Some((bmp, SelectObject(dib.dc, bmp.into())));
            Ok(dib)
        }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            if let Some((bmp, old)) = self.bitmap {
                SelectObject(self.dc, old);
                let _ = DeleteObject(bmp.into());
            }
            let _ = DeleteDC(self.dc);
        }
    }
}

pub struct Surface {
    pub hwnd: HWND,
    /// Window position on the virtual desktop.
    pub bounds: ScreenRect,
    // Declared before `dib` so the render target is released before its DC.
    rt: ID2D1DCRenderTarget,
    dib: Dib,
}

impl Surface {
    pub fn new(hwnd: HWND, bounds: ScreenRect) -> Result<Self> {
        let dib = Dib::new(bounds.w, bounds.h)?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            // Draw in physical pixels; the process is per-monitor DPI aware.
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let r = RECT {
            left: 0,
            top: 0,
            right: bounds.w as i32,
            bottom: bounds.h as i32,
        };
        unsafe {
            let rt = factories()?.d2d.CreateDCRenderTarget(&props)?;
            rt.BindDC(dib.dc, &r)?;
            Ok(Self {
                hwnd,
                bounds,
                rt,
                dib,
            })
        }
    }

    /// Draw a frame with `f` and show it. The canvas starts fully transparent.
    pub fn paint(&self, f: impl FnOnce(&Painter) -> Result<()>) -> Result<()> {
        unsafe {
            self.rt.BeginDraw();
            self.rt.Clear(Some(&rgba(0, 0, 0, 0.0)));
            let painted = f(&Painter {
                rt: &self.rt,
                origin: (self.bounds.x, self.bounds.y),
            });
            self.rt.EndDraw(None, None)?;
            painted?;
            let pos = POINT {
                x: self.bounds.x,
                y: self.bounds.y,
            };
            let size = SIZE {
                cx: self.bounds.w as i32,
                cy: self.bounds.h as i32,
            };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&pos),
                Some(&size),
                Some(self.dib.dc),
                Some(&POINT::default()),
                Default::default(),
                Some(&blend),
                ULW_ALPHA,
            )
        }
    }
}

/// Drawing helpers in **screen** coordinates; the painter translates to the surface.
pub struct Painter<'a> {
    rt: &'a ID2D1DCRenderTarget,
    origin: (i32, i32),
}

impl Painter<'_> {
    fn local(&self, r: D2D_RECT_F) -> D2D_RECT_F {
        let (ox, oy) = (self.origin.0 as f32, self.origin.1 as f32);
        D2D_RECT_F {
            left: r.left - ox,
            top: r.top - oy,
            right: r.right - ox,
            bottom: r.bottom - oy,
        }
    }

    fn brush(&self, c: D2D1_COLOR_F) -> Result<ID2D1SolidColorBrush> {
        unsafe { self.rt.CreateSolidColorBrush(&c, None) }
    }

    pub fn fill(&self, r: D2D_RECT_F, c: D2D1_COLOR_F) -> Result<()> {
        let b = self.brush(c)?;
        unsafe { self.rt.FillRectangle(&self.local(r), &b) };
        Ok(())
    }

    pub fn fill_rounded(&self, r: D2D_RECT_F, radius: f32, c: D2D1_COLOR_F) -> Result<()> {
        let b = self.brush(c)?;
        let rr = D2D1_ROUNDED_RECT {
            rect: self.local(r),
            radiusX: radius,
            radiusY: radius,
        };
        unsafe { self.rt.FillRoundedRectangle(&rr, &b) };
        Ok(())
    }

    /// Stroke drawn just outside `r` so the selected pixels themselves stay unobscured.
    pub fn outline(&self, r: D2D_RECT_F, width: f32, c: D2D1_COLOR_F) -> Result<()> {
        let b = self.brush(c)?;
        let h = width / 2.0;
        let o = D2D_RECT_F {
            left: r.left - h,
            top: r.top - h,
            right: r.right + h,
            bottom: r.bottom + h,
        };
        unsafe { self.rt.DrawRectangle(&self.local(o), &b, width, None) };
        Ok(())
    }

    pub fn line(
        &self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        width: f32,
        c: D2D1_COLOR_F,
    ) -> Result<()> {
        let b = self.brush(c)?;
        let (ox, oy) = (self.origin.0 as f32, self.origin.1 as f32);
        let p0 = windows_numerics::Vector2 {
            X: x0 - ox,
            Y: y0 - oy,
        };
        let p1 = windows_numerics::Vector2 {
            X: x1 - ox,
            Y: y1 - oy,
        };
        unsafe { self.rt.DrawLine(p0, p1, &b, width, None) };
        Ok(())
    }

    pub fn text(
        &self,
        s: &str,
        format: &IDWriteTextFormat,
        r: D2D_RECT_F,
        c: D2D1_COLOR_F,
    ) -> Result<()> {
        let b = self.brush(c)?;
        let wide: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            self.rt.DrawText(
                &wide,
                format,
                &self.local(r),
                &b,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            )
        };
        Ok(())
    }

    /// Blit BGRA pixels (`w`×`h`, opaque) scaled into `dst` with hard pixel edges.
    pub fn pixels(&self, bgra: &[u8], w: u32, h: u32, dst: D2D_RECT_F) -> Result<()> {
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        unsafe {
            let bmp = self.rt.CreateBitmap(
                D2D_SIZE_U {
                    width: w,
                    height: h,
                },
                Some(bgra.as_ptr() as *const _),
                w * 4,
                &props,
            )?;
            self.rt.DrawBitmap(
                &bmp,
                Some(&self.local(dst)),
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                None,
            );
        }
        Ok(())
    }
}
