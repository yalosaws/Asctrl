//! Screen capture with GDI, in-process. Coordinates are physical screen pixels: this process is
//! per-monitor DPI aware, so they are the same pixels SendInput and SetCursorPos use.
//! The overlay is excluded from capture (WDA_EXCLUDEFROMCAPTURE), so the image shows the real desktop.

use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN};
use windows::core::BOOL;

const JPEG_QUALITY: u8 = 80;
/// MONITORINFO.dwFlags bit for the primary monitor
const PRIMARY: u32 = 1;

#[derive(Clone, Copy)]
pub struct Area {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

pub struct Display {
    pub area: Area,
    pub primary: bool,
}

pub struct Shot {
    /// JPEG bytes
    pub jpeg: Vec<u8>,
    pub width: i32,
    pub height: i32,
    /// Image pixels per screen pixel (<= 1)
    pub scale: f64,
}

unsafe extern "system" fn monitor_cb(h: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
    let out = unsafe { &mut *(lp.0 as *mut Vec<Display>) };
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    if unsafe { GetMonitorInfoW(h, &mut mi) }.as_bool() {
        let r = mi.rcMonitor;
        out.push(Display {
            area: Area { left: r.left, top: r.top, width: r.right - r.left, height: r.bottom - r.top },
            primary: mi.dwFlags & PRIMARY != 0,
        });
    }
    BOOL(1)
}

/// Every display, in the system's enumeration order (display #1 is the first)
pub fn displays() -> Vec<Display> {
    let mut out: Vec<Display> = vec![];
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(monitor_cb), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

/// The bounding box of all displays
pub fn virtual_screen() -> Area {
    unsafe {
        Area {
            left: GetSystemMetrics(SM_XVIRTUALSCREEN),
            top: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

/// Releases the GDI objects of one capture on every exit path
struct Gdi {
    screen: HDC,
    src_dc: HDC,
    dst_dc: HDC,
    src_bmp: HBITMAP,
    dst_bmp: HBITMAP,
}

impl Drop for Gdi {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.src_dc);
            let _ = DeleteDC(self.dst_dc);
            let _ = DeleteObject(self.src_bmp.into());
            let _ = DeleteObject(self.dst_bmp.into());
            ReleaseDC(None, self.screen);
        }
    }
}

/// Capture `area` and scale it down to at most `max_width` pixels wide (never up)
pub fn grab(area: Area, max_width: i32) -> Result<Shot, String> {
    if area.width <= 0 || area.height <= 0 {
        return Err("Nothing to capture (the area is empty)".into());
    }
    let scale = (max_width as f64 / area.width as f64).min(1.0);
    let w = ((area.width as f64 * scale) as i32).max(1);
    let h = ((area.height as f64 * scale) as i32).max(1);
    // The JPEG encoder takes 16-bit dimensions
    if w > u16::MAX as i32 || h > u16::MAX as i32 {
        return Err("The area is too large to capture".into());
    }

    let mut pixels = vec![0u8; w as usize * h as usize * 4];
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return Err("Screenshot failed: no access to the screen".into());
        }
        let mut g = Gdi { screen, src_dc: HDC::default(), dst_dc: HDC::default(), src_bmp: HBITMAP::default(), dst_bmp: HBITMAP::default() };
        g.src_dc = CreateCompatibleDC(Some(screen));
        g.src_bmp = CreateCompatibleBitmap(screen, area.width, area.height);
        if g.src_dc.is_invalid() || g.src_bmp.is_invalid() {
            return Err("Screenshot failed: out of memory".into());
        }
        let old_src = SelectObject(g.src_dc, g.src_bmp.into());
        // Fails on the lock screen and the UAC secure desktop, which no process may capture
        BitBlt(g.src_dc, 0, 0, area.width, area.height, Some(screen), area.left, area.top, SRCCOPY)
            .map_err(|e| format!("Screenshot failed: {e} (the lock screen or a Windows security prompt may be showing)"))?;

        let bmp = if (w, h) == (area.width, area.height) {
            g.src_bmp
        } else {
            g.dst_dc = CreateCompatibleDC(Some(screen));
            g.dst_bmp = CreateCompatibleBitmap(screen, w, h);
            if g.dst_dc.is_invalid() || g.dst_bmp.is_invalid() {
                return Err("Screenshot failed: out of memory".into());
            }
            let old_dst = SelectObject(g.dst_dc, g.dst_bmp.into());
            // HALFTONE averages source pixels, so small text stays readable when scaled down
            SetStretchBltMode(g.dst_dc, HALFTONE);
            let _ = SetBrushOrgEx(g.dst_dc, 0, 0, None);
            if !StretchBlt(g.dst_dc, 0, 0, w, h, Some(g.src_dc), 0, 0, area.width, area.height, SRCCOPY).as_bool() {
                return Err("Screenshot failed: could not scale the image".into());
            }
            SelectObject(g.dst_dc, old_dst);
            g.dst_bmp
        };
        // GetDIBits needs the bitmap not to be selected into any DC
        SelectObject(g.src_dc, old_src);

        // Negative height: rows come out top-down
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        if GetDIBits(screen, bmp, 0, h as u32, Some(pixels.as_mut_ptr() as *mut _), &mut bmi, DIB_RGB_COLORS) != h {
            return Err("Screenshot failed: could not read the pixels".into());
        }
    }

    let mut jpeg = Vec::with_capacity(pixels.len() / 12);
    jpeg_encoder::Encoder::new(&mut jpeg, JPEG_QUALITY)
        .encode(&pixels, w as u16, h as u16, jpeg_encoder::ColorType::Bgra)
        .map_err(|e| format!("Screenshot failed: {e}"))?;
    Ok(Shot { jpeg, width: w, height: h, scale })
}
