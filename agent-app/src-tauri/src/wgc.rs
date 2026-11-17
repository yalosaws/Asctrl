//! Capturing one window with Windows Graphics Capture: what the window itself draws, even where other windows
//! cover it. The screen capture in `capture` sees only what is on top; this is for reading a window that is
//! behind others (its text off the pixels) without bringing it forward.
//!
//! The picture is the window's visible frame (DWM's extended frame bounds, as `winmgr::rect_of` gives), in
//! physical pixels. A minimized window draws nothing, so there is no picture of it.

use crate::capture::Area;
use std::time::{Duration, Instant};
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::Interface;

/// How long to wait for the window's first frame
const FIRST_FRAME: Duration = Duration::from_millis(600);

/// A window's own pixels
pub struct Picture {
    /// Where the picture sits on the screen
    pub area: Area,
    /// Top-down BGRA rows, `area.width` x `area.height`
    pub px: Vec<u8>,
}

impl Picture {
    /// The part of the picture that `want` (screen pixels) covers, scaled to `w` x `h` (nearest pixel)
    pub fn cut(&self, want: Area, w: i32, h: i32) -> Result<Vec<u8>, String> {
        let (l, t) = (want.left - self.area.left, want.top - self.area.top);
        if l < 0 || t < 0 || l + want.width > self.area.width || t + want.height > self.area.height || w <= 0 || h <= 0 {
            return Err("the region is not inside the window".into());
        }
        let stride = self.area.width as usize * 4;
        let mut out = vec![0u8; w as usize * h as usize * 4];
        for y in 0..h {
            let sy = (t + (y as i64 * want.height as i64 / h as i64) as i32) as usize;
            for x in 0..w {
                let sx = (l + (x as i64 * want.width as i64 / w as i64) as i32) as usize;
                let s = sy * stride + sx * 4;
                let d = (y as usize * w as usize + x as usize) * 4;
                out[d..d + 4].copy_from_slice(&self.px[s..s + 4]);
            }
        }
        Ok(out)
    }

    /// The whole picture at `w` x `h` (not larger than it is), each pixel the average of those it covers, so
    /// small text stays readable
    pub fn shrink(&self, w: i32, h: i32) -> Vec<u8> {
        let (sw, sh) = (self.area.width as usize, self.area.height as usize);
        let (w, h) = ((w.max(1) as usize).min(sw), (h.max(1) as usize).min(sh));
        if (w, h) == (sw, sh) {
            return self.px.clone();
        }
        let mut out = vec![0u8; w * h * 4];
        for y in 0..h {
            let (y0, y1) = (y * sh / h, ((y + 1) * sh / h).max(y * sh / h + 1));
            for x in 0..w {
                let (x0, x1) = (x * sw / w, ((x + 1) * sw / w).max(x * sw / w + 1));
                let mut sum = [0u32; 4];
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        let s = (sy * sw + sx) * 4;
                        for c in 0..4 {
                            sum[c] += self.px[s + c] as u32;
                        }
                    }
                }
                let n = ((y1 - y0) * (x1 - x0)) as u32;
                let d = (y * w + x) * 4;
                for c in 0..4 {
                    out[d + c] = (sum[c] / n) as u8;
                }
            }
        }
        out
    }
}

fn err(what: &str) -> impl Fn(windows::core::Error) -> String + '_ {
    move |e| format!("window capture: {what}: {}", e.message())
}

/// Capture `hwnd` as it draws itself. Blocking (about one frame).
pub fn capture(hwnd: isize) -> Result<Picture, String> {
    if crate::winmgr::is_minimized(hwnd) {
        return Err("window capture: the window is minimized".into());
    }
    let (l, t, _, _) = crate::winmgr::rect_of(hwnd).ok_or("window capture: no such window")?;
    unsafe {
        // WinRT needs COM on this thread; harmless when it is already there
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        let mut device = None;
        let mut context = None;
        D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut context))
            .map_err(err("no Direct3D device"))?;
        let (device, context): (ID3D11Device, ID3D11DeviceContext) = (device.ok_or("window capture: no Direct3D device")?, context.ok_or("window capture: no Direct3D context")?);
        let dxgi: IDXGIDevice = device.cast().map_err(err("no DXGI device"))?;
        let d3d: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi).map_err(err("no WinRT device"))?.cast().map_err(err("no WinRT device"))?;
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(err("not available"))?;
        let item: GraphicsCaptureItem = interop.CreateForWindow(HWND(hwnd as *mut _)).map_err(err("the window can't be captured"))?;
        let size = item.Size().map_err(err("no size"))?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&d3d, DirectXPixelFormat::B8G8R8A8UIntNormalized, 1, size).map_err(err("no frame pool"))?;
        let session = pool.CreateCaptureSession(&item).map_err(err("no session"))?;
        // Neither the pointer nor (where Windows allows) the yellow border that marks a capture
        let _ = session.SetIsCursorCaptureEnabled(false);
        let _ = session.SetIsBorderRequired(false);
        session.StartCapture().map_err(err("could not start"))?;
        let start = Instant::now();
        let frame = loop {
            if let Ok(f) = pool.TryGetNextFrame() {
                break f;
            }
            if start.elapsed() > FIRST_FRAME {
                let _ = session.Close();
                let _ = pool.Close();
                return Err("window capture: the window drew nothing".into());
            }
            std::thread::sleep(Duration::from_millis(4));
        };
        let surface = frame.Surface().map_err(err("no surface"))?;
        let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(err("no texture"))?;
        let texture: ID3D11Texture2D = access.GetInterface().map_err(err("no texture"))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut desc);
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        desc.MiscFlags = 0;
        let mut staging = None;
        device.CreateTexture2D(&desc, None, Some(&mut staging)).map_err(err("no staging texture"))?;
        let staging = staging.ok_or("window capture: no staging texture")?;
        context.CopyResource(&staging, &texture);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(err("could not read the pixels"))?;
        let (fw, fh) = (desc.Width as usize, desc.Height as usize);
        let mut px = vec![0u8; fw * fh * 4];
        for y in 0..fh {
            let src = std::slice::from_raw_parts((mapped.pData as *const u8).add(y * mapped.RowPitch as usize), fw * 4);
            px[y * fw * 4..(y + 1) * fw * 4].copy_from_slice(src);
        }
        context.Unmap(&staging, 0);
        let _ = frame.Close();
        let _ = session.Close();
        let _ = pool.Close();
        // The frame is the window's frame bounds; should they differ (the window was resized meanwhile), the
        // picture starts at the same corner and is as large as the frame
        Ok(Picture { area: Area { left: l, top: t, width: fw as i32, height: fh as i32 }, px })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_region_is_cut_out_of_the_picture_and_scaled() {
        // 4x2 picture at (100, 50): pixel value = x + 10 y in the blue channel
        let mut px = vec![0u8; 4 * 2 * 4];
        for y in 0..2 {
            for x in 0..4 {
                px[(y * 4 + x) * 4] = (x + 10 * y) as u8;
            }
        }
        let p = Picture { area: Area { left: 100, top: 50, width: 4, height: 2 }, px };
        let one = p.cut(Area { left: 102, top: 51, width: 2, height: 1 }, 2, 1).unwrap();
        assert_eq!((one[0], one[4]), (12, 13));
        // Twice as large: each pixel twice
        let big = p.cut(Area { left: 102, top: 51, width: 2, height: 1 }, 4, 2).unwrap();
        assert_eq!((big[0], big[4], big[8], big[12]), (12, 12, 13, 13));
        assert!(p.cut(Area { left: 99, top: 50, width: 2, height: 1 }, 2, 1).is_err());
        // Half the size: each pixel the average of a 2x2 block
        let half = p.shrink(2, 1);
        assert_eq!((half[0], half[4]), ((0 + 1 + 10 + 11) / 4, (2 + 3 + 12 + 13) / 4));
        assert_eq!(p.shrink(8, 8).len(), p.px.len(), "never larger than it is");
    }
}
