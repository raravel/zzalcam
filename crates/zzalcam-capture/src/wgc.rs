//! Window capture with Windows.Graphics.Capture.
//!
//! The output keeps the window's size at the start of recording: if the window grows the extra area
//! is cropped, if it shrinks the uncovered area keeps its last content.

use std::time::{Duration, Instant};

use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::Win32::UI::WindowsAndMessaging::IsWindow;
use windows::core::{IInspectable, Interface};

use crate::convert::bgra_to_rgba;
use crate::{Capturer, Error};

const PIXEL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(2);

pub struct WindowCapturer {
    hwnd: HWND,
    ctx: ID3D11DeviceContext,
    d3d: IDirect3DDevice,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    content_size: SizeInt32,
    staging: ID3D11Texture2D,
    width: u32,
    height: u32,
    canvas: Vec<u8>,
    closed: bool,
}

impl WindowCapturer {
    pub fn new(hwnd: isize, cursor: bool) -> Result<Self, Error> {
        let hwnd = HWND(hwnd as *mut _);
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            return Err(Error::NotFound("window"));
        }
        // Already-initialised apartments are fine.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        if !GraphicsCaptureSession::IsSupported()? {
            return Err(Error::Unsupported(
                "Windows.Graphics.Capture requires Windows 10 1903 or later",
            ));
        }

        let mut device: Option<ID3D11Device> = None;
        let mut ctx = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
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
        let dxgi: IDXGIDevice = device.cast()?;
        let d3d: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }
            .cast::<IInspectable>()?
            .cast()?;

        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(hwnd)? };
        let size = item.Size()?;
        let (width, height) = (size.Width.max(1) as u32, size.Height.max(1) as u32);

        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&d3d, PIXEL_FORMAT, 2, size)?;
        let session = pool.CreateCaptureSession(&item)?;
        session.SetIsCursorCaptureEnabled(cursor)?;
        // Windows 11+: hide the yellow capture border. Older builds lack the property.
        let _ = session.SetIsBorderRequired(false);

        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
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

        session.StartCapture()?;
        let mut cap = Self {
            hwnd,
            ctx,
            d3d,
            pool,
            session,
            content_size: size,
            staging: staging.unwrap(),
            width,
            height,
            canvas: [0u8, 0, 0, 255].repeat(crate::checked_area(width, height)?),
            closed: false,
        };

        // Block until the first frame so the recording never starts with an empty canvas.
        let deadline = Instant::now() + FIRST_FRAME_TIMEOUT;
        while !cap.grab()? {
            if Instant::now() > deadline {
                return Err(Error::Unsupported(
                    "window produced no frames (minimised or protected?)",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(cap)
    }

    fn latest_frame(&self) -> Option<Direct3D11CaptureFrame> {
        let mut latest = None;
        // A null frame (nothing pending) surfaces as an error; drain to the newest one.
        while let Ok(f) = self.pool.TryGetNextFrame() {
            latest = Some(f);
        }
        latest
    }
}

impl Capturer for WindowCapturer {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn grab(&mut self) -> Result<bool, Error> {
        if !unsafe { IsWindow(Some(self.hwnd)) }.as_bool() {
            self.closed = true;
            return Ok(false);
        }
        let Some(frame) = self.latest_frame() else {
            return Ok(false);
        };

        let content = frame.ContentSize()?;
        let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        let tex: ID3D11Texture2D = unsafe { access.GetInterface()? };
        // After the window grows, frames from the old (smaller) pool report the new content size;
        // clamp to the surface as well or D3D11 silently drops the copy.
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { tex.GetDesc(&mut desc) };
        let w = (content.Width.max(0) as u32)
            .min(self.width)
            .min(desc.Width);
        let h = (content.Height.max(0) as u32)
            .min(self.height)
            .min(desc.Height);
        let src = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: w,
            bottom: h,
            back: 1,
        };
        if w > 0 && h > 0 {
            unsafe {
                self.ctx
                    .CopySubresourceRegion(&self.staging, 0, 0, 0, 0, &tex, 0, Some(&src))
            };
        }
        frame.Close()?;
        // Resize the pool only after the frame is done with, as Microsoft's samples do.
        if content != self.content_size {
            self.content_size = content;
            self.pool.Recreate(&self.d3d, PIXEL_FORMAT, 2, content)?;
        }
        if w == 0 || h == 0 {
            return Ok(false);
        }

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.ctx
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?
        };
        let pitch = mapped.RowPitch as usize;
        let data = unsafe {
            std::slice::from_raw_parts(
                mapped.pData as *const u8,
                pitch * (h as usize - 1) + w as usize * 4,
            )
        };
        bgra_to_rgba(
            data,
            pitch,
            w as usize,
            h as usize,
            &mut self.canvas,
            self.width as usize,
            0,
            0,
        );
        unsafe { self.ctx.Unmap(&self.staging, 0) };
        Ok(true)
    }

    fn canvas(&self) -> &[u8] {
        &self.canvas
    }

    fn ended(&self) -> bool {
        self.closed
    }
}

impl Drop for WindowCapturer {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
