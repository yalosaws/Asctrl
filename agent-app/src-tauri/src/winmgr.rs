//! Window listing, window switching, launching programs, clipboard text. Coordinates are physical screen pixels.

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, HSTRING, PCWSTR, PWSTR, w};

pub struct Win {
    pub handle: isize,
    pub title: String,
    pub process: String,
    pub pid: u32,
    pub rect: (i32, i32, i32, i32),
    pub minimized: bool,
    pub maximized: bool,
    pub foreground: bool,
}

fn process_name(pid: u32) -> String {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return String::new();
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        full.rsplit('\\').next().unwrap_or(&full).to_string()
    }
}

fn is_cloaked(h: HWND) -> bool {
    let mut v: u32 = 0;
    unsafe { DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut v as *mut u32 as *mut _, 4) }.is_ok() && v != 0
}

unsafe extern "system" fn enum_cb(h: HWND, lp: LPARAM) -> BOOL {
    let out = unsafe { &mut *(lp.0 as *mut Vec<HWND>) };
    out.push(h);
    BOOL(1)
}

/// Top-level windows the user can see (the ones on the taskbar), in Z order from front to back
pub fn list() -> Vec<Win> {
    let mut hs: Vec<HWND> = vec![];
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut hs as *mut _ as isize));
    }
    let fg = unsafe { GetForegroundWindow() };
    let mut out = vec![];
    for h in hs {
        unsafe {
            if !IsWindowVisible(h).as_bool() || is_cloaked(h) {
                continue;
            }
            let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
            if ex & WS_EX_TOOLWINDOW.0 != 0 {
                continue;
            }
            if GetWindow(h, GW_OWNER).map(|o| !o.is_invalid()).unwrap_or(false) && ex & WS_EX_APPWINDOW.0 == 0 {
                continue;
            }
            let len = GetWindowTextLengthW(h);
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; len as usize + 1];
            let n = GetWindowTextW(h, &mut buf);
            let title = String::from_utf16_lossy(&buf[..n as usize]);
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            // Skip our own overlay; the AI should neither see nor operate it
            if pid == std::process::id() {
                continue;
            }
            let r = visible_rect(h).unwrap_or_default();
            out.push(Win {
                handle: h.0 as isize,
                title,
                process: process_name(pid),
                pid,
                rect: (r.left, r.top, r.right, r.bottom),
                minimized: IsIconic(h).as_bool(),
                maximized: IsZoomed(h).as_bool(),
                foreground: h == fg,
            });
        }
    }
    out
}

/// Find a window by handle or by a title / process-name keyword
pub fn find(handle: Option<isize>, query: &str) -> Result<HWND, String> {
    let wins = list();
    if let Some(h) = handle {
        return wins.iter().find(|w| w.handle == h).map(|w| HWND(w.handle as *mut _)).ok_or(format!("No window with handle {h}"));
    }
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Err("handle or title is required".into());
    }
    // Front-most match wins
    wins.iter()
        .find(|w| w.title.to_lowercase().contains(&q) || w.process.to_lowercase().contains(&q))
        .map(|w| HWND(w.handle as *mut _))
        .ok_or(format!("No window whose title or program name contains \"{query}\"; try list_windows first"))
}

pub fn act(h: HWND, action: &str) -> Result<String, String> {
    unsafe {
        match action {
            "" | "focus" => {
                if IsIconic(h).as_bool() {
                    let _ = ShowWindow(h, SW_RESTORE);
                }
                // A background process can't just take focus: simulate an Alt press first so Windows allows SetForegroundWindow
                let alt = |flags| INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VK_MENU, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
                };
                SendInput(&[alt(KEYBD_EVENT_FLAGS(0)), alt(KEYEVENTF_KEYUP)], size_of::<INPUT>() as i32);
                let _ = BringWindowToTop(h);
                if !SetForegroundWindow(h).as_bool() {
                    return Err("Windows refused to switch windows (the target may be running as administrator)".into());
                }
            }
            "minimize" => {
                let _ = ShowWindow(h, SW_MINIMIZE);
            }
            "maximize" => {
                let _ = ShowWindow(h, SW_MAXIMIZE);
                let _ = SetForegroundWindow(h);
            }
            "restore" => {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            "close" => {
                PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0)).map_err(|e| e.to_string())?;
            }
            other => return Err(format!("Unsupported action: {other}")),
        }
    }
    Ok(match action {
        "minimize" => "Minimized",
        "maximize" => "Maximized",
        "restore" => "Restored",
        "close" => "Close requested (the program may ask to save)",
        _ => "Switched to the window",
    }
    .into())
}

/// Open the way Explorer does: program names, full paths, files, URLs, ms-settings: links, etc.
pub fn open(target: &str, args: &str, cwd: &str) -> Result<(), String> {
    let t = HSTRING::from(target);
    let a = HSTRING::from(args);
    let d = HSTRING::from(cwd);
    let r = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &t,
            if args.is_empty() { PCWSTR::null() } else { PCWSTR(a.as_ptr()) },
            if cwd.is_empty() { PCWSTR::null() } else { PCWSTR(d.as_ptr()) },
            SW_SHOWNORMAL,
        )
    };
    match r.0 as isize {
        n if n > 32 => Ok(()),
        2 | 3 => Err(format!("Can't find \"{target}\"")),
        5 => Err("Access denied".into()),
        n => Err(format!("Failed to open (error code {n})")),
    }
}

/// Current window position (physical screen pixels); None when minimized or closed
pub fn rect_of(handle: isize) -> Option<(i32, i32, i32, i32)> {
    let h = HWND(handle as *mut _);
    unsafe {
        if !IsWindow(Some(h)).as_bool() || !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
            return None;
        }
        let r = visible_rect(h)?;
        Some((r.left, r.top, r.right, r.bottom))
    }
}

/// Bounds of the visible part of the window. On Windows 10/11 GetWindowRect also includes an invisible
/// resize border (about 7px left, right and bottom), so an outline drawn from it is too big; DWM gives the real visible bounds.
unsafe fn visible_rect(h: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    let size = std::mem::size_of::<RECT>() as u32;
    if unsafe { DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut RECT as *mut _, size) }.is_ok() && r.right > r.left && r.bottom > r.top {
        return Some(r);
    }
    unsafe { GetWindowRect(h, &mut r) }.ok()?;
    Some(r)
}

/// Put text on the clipboard
pub fn set_clipboard_text(text: &str) -> Result<(), String> {
    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    const CF_UNICODETEXT: u32 = 13;
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    unsafe {
        // Another program (a clipboard manager, a remote-desktop client) may hold the clipboard for a moment
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        if !opened {
            return Err("The clipboard is in use by another program; try again".into());
        }
        let result = (|| {
            EmptyClipboard().map_err(|e| e.to_string())?;
            let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).map_err(|e| e.to_string())?;
            let p = GlobalLock(mem) as *mut u16;
            if p.is_null() {
                let _ = GlobalFree(Some(mem));
                return Err("Out of memory".to_string());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
            let _ = GlobalUnlock(mem);
            // On success the clipboard owns the memory; on failure it is still ours to free
            if let Err(e) = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0))) {
                let _ = GlobalFree(Some(mem));
                return Err(e.to_string());
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        result.map_err(|e| format!("Failed to write the clipboard: {e}"))
    }
}
