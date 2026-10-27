//! DXGI Desktop Duplication capture of an arbitrary screen rectangle, possibly spanning monitors.

use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
    IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1, IDXGIOutput5, IDXGIOutputDuplication,
};
use windows::core::Interface;

use crate::convert::bgra_to_rgba;
use crate::cursor::Composer;
use crate::{Capturer, Error, ScreenRect, monitors};

const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(2);

struct OutputDup {
    ctx: ID3D11DeviceContext,
    device: ID3D11Device,
    output: IDXGIOutput1,
    dupl: Option<IDXGIOutputDuplication>,
    staging: ID3D11Texture2D,
    /// Source box in output-local coordinates.
    src: D3D11_BOX,
    /// Destination offset in the canvas.
    dst: (usize, usize),
    /// Desktop rectangle of the output when recording started.
    monitor: ScreenRect,
    /// The output is gone or was reconfigured; the capture can't continue faithfully.
    gone: bool,
}

fn duplicate(
    output: &IDXGIOutput1,
    device: &ID3D11Device,
) -> Result<IDXGIOutputDuplication, Error> {
    // DuplicateOutput1 lets DXGI convert HDR/FP16 desktops to 8-bit BGRA for us.
    if let Ok(o5) = output.cast::<IDXGIOutput5>()
        && let Ok(d) = unsafe { o5.DuplicateOutput1(device, 0, &[DXGI_FORMAT_B8G8R8A8_UNORM]) }
    {
        return Ok(d);
    }
    Ok(unsafe { output.DuplicateOutput(device)? })
}

impl OutputDup {
    fn new(
        adapter: &IDXGIAdapter1,
        output_index: u32,
        monitor: ScreenRect,
        part: ScreenRect,
        canvas_origin: (i32, i32),
    ) -> Result<Self, Error> {
        let output: IDXGIOutput1 = unsafe { adapter.EnumOutputs(output_index)? }.cast()?;
        let rotation = unsafe { output.GetDesc()? }.Rotation;
        if rotation != DXGI_MODE_ROTATION_IDENTITY && rotation != DXGI_MODE_ROTATION_UNSPECIFIED {
            return Err(Error::Unsupported("rotated monitors are not supported yet"));
        }

        let mut device = None;
        let mut ctx = None;
        unsafe {
            D3D11CreateDevice(
                adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                Default::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            )?;
        }
        let (device, ctx) = (device.unwrap(), ctx.unwrap());

        let desc = D3D11_TEXTURE2D_DESC {
            Width: part.w,
            Height: part.h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut staging))? };

        let lx = (part.x - monitor.x) as u32;
        let ly = (part.y - monitor.y) as u32;
        let dupl = duplicate(&output, &device)?;
        Ok(Self {
            ctx,
            staging: staging.unwrap(),
            src: D3D11_BOX {
                left: lx,
                top: ly,
                front: 0,
                right: lx + part.w,
                bottom: ly + part.h,
                back: 1,
            },
            dst: (
                (part.x - canvas_origin.0) as usize,
                (part.y - canvas_origin.1) as usize,
            ),
            dupl: Some(dupl),
            device,
            output,
            monitor,
            gone: false,
        })
    }

    /// Recreate the duplication after ACCESS_LOST, unless the output's layout changed under us.
    fn reacquire(&mut self) -> Option<&IDXGIOutputDuplication> {
        let same_layout = unsafe { self.output.GetDesc() }.is_ok_and(|d| {
            let r = d.DesktopCoordinates;
            d.AttachedToDesktop.as_bool()
                && (r.left, r.top, r.right - r.left, r.bottom - r.top)
                    == (
                        self.monitor.x,
                        self.monitor.y,
                        self.monitor.w as i32,
                        self.monitor.h as i32,
                    )
        });
        if !same_layout {
            self.gone = true;
            return None;
        }
        // Same layout but duplication fails: the lock screen or a UAC prompt is up. Keep retrying
        // for as long as it lasts; the canvas simply holds the last image meanwhile.
        duplicate(&self.output, &self.device)
            .ok()
            .map(|d| &*self.dupl.insert(d))
    }

    /// Copy the newest desktop image into `canvas`. Returns false if the output had no new frame
    /// within `timeout_ms`.
    fn grab(&mut self, canvas: &mut [u8], canvas_w: usize, timeout_ms: u32) -> Result<bool, Error> {
        if self.gone {
            return Ok(false);
        }
        let dupl = match self.dupl.clone() {
            Some(d) => d,
            None => match self.reacquire() {
                Some(d) => d.clone(),
                None => return Ok(false),
            },
        };

        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        match unsafe { dupl.AcquireNextFrame(timeout_ms, &mut info, &mut resource) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(false),
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                self.dupl = None;
                return Ok(false);
            }
            Err(e) => return Err(e.into()),
        }

        // LastPresentTime == 0 means only the pointer moved; we draw the cursor ourselves.
        let copied = if info.LastPresentTime != 0 {
            resource
                .ok_or(Error::Unsupported(
                    "duplication returned no desktop texture",
                ))
                .and_then(|r| Ok(r.cast::<ID3D11Texture2D>()?))
                .map(|tex| {
                    unsafe {
                        self.ctx.CopySubresourceRegion(
                            &self.staging,
                            0,
                            0,
                            0,
                            0,
                            &tex,
                            0,
                            Some(&self.src),
                        )
                    };
                    true
                })
        } else {
            Ok(false)
        };
        // Always release, even if the copy failed, or every later AcquireNextFrame fails.
        match unsafe { dupl.ReleaseFrame() } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                self.dupl = None;
                return Ok(false);
            }
            Err(e) => return Err(e.into()),
        }
        if !copied? {
            return Ok(false);
        }

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.ctx
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?
        };
        let (w, h) = (
            (self.src.right - self.src.left) as usize,
            (self.src.bottom - self.src.top) as usize,
        );
        let pitch = mapped.RowPitch as usize;
        let src = unsafe {
            std::slice::from_raw_parts(mapped.pData as *const u8, pitch * (h - 1) + w * 4)
        };
        bgra_to_rgba(src, pitch, w, h, canvas, canvas_w, self.dst.0, self.dst.1);
        unsafe { self.ctx.Unmap(&self.staging, 0) };
        Ok(true)
    }
}

pub struct DesktopCapturer {
    width: u32,
    height: u32,
    outputs: Vec<OutputDup>,
    clean: Vec<u8>,
    composer: Option<Composer>,
}

impl DesktopCapturer {
    pub fn new(rect: ScreenRect, cursor: bool) -> Result<Self, Error> {
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
        let mut outputs = Vec::new();
        for m in monitors()? {
            let Some(part) = m.rect.intersect(&rect) else {
                continue;
            };
            let adapter = unsafe { factory.EnumAdapters1(m.adapter)? };
            outputs.push(OutputDup::new(
                &adapter,
                m.output,
                m.rect,
                part,
                (rect.x, rect.y),
            )?);
        }
        if outputs.is_empty() || rect.w == 0 || rect.h == 0 {
            return Err(Error::EmptyRegion);
        }
        let pixels = crate::checked_area(rect.w, rect.h)?;
        // Areas outside every monitor (non-rectangular layouts) stay opaque black.
        let mut clean = [0u8, 0, 0, 255].repeat(pixels);

        // A fresh duplication delivers the full desktop on its first frame; wait for it on every
        // output so the recording never starts with blank areas.
        let deadline = Instant::now() + FIRST_FRAME_TIMEOUT;
        for o in &mut outputs {
            while !o.grab(&mut clean, rect.w as usize, 50)? {
                if Instant::now() > deadline {
                    return Err(Error::Unsupported("desktop duplication produced no frames"));
                }
            }
        }
        Ok(Self {
            width: rect.w,
            height: rect.h,
            outputs,
            clean,
            composer: cursor.then(|| Composer::new(rect.w, rect.h, (rect.x, rect.y))),
        })
    }
}

impl Capturer for DesktopCapturer {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn grab(&mut self) -> Result<bool, Error> {
        let mut changed = false;
        for o in &mut self.outputs {
            changed |= o.grab(&mut self.clean, self.width as usize, 0)?;
        }
        Ok(match &mut self.composer {
            Some(c) => c.compose(&self.clean, changed),
            None => changed,
        })
    }

    fn canvas(&self) -> &[u8] {
        match &self.composer {
            Some(c) => c.canvas(),
            None => &self.clean,
        }
    }

    fn ended(&self) -> bool {
        self.outputs.iter().any(|o| o.gone)
    }
}
