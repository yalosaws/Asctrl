//! 窗口列表、切换窗口、打开程序。坐标是屏幕物理像素。

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
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

/// 用户能看到的顶层窗口（任务栏上那些），按 Z 顺序从前到后
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
            // 跳过自己的悬浮层，AI 不该看到也不该操作它
            if pid == std::process::id() {
                continue;
            }
            let mut r = RECT::default();
            let _ = GetWindowRect(h, &mut r);
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

/// 按句柄或标题/进程名关键字找窗口
pub fn find(handle: Option<isize>, query: &str) -> Result<HWND, String> {
    let wins = list();
    if let Some(h) = handle {
        return wins.iter().find(|w| w.handle == h).map(|w| HWND(w.handle as *mut _)).ok_or(format!("找不到句柄为 {h} 的窗口"));
    }
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Err("需要 handle 或 title".into());
    }
    let hits: Vec<&Win> =
        wins.iter().filter(|w| w.title.to_lowercase().contains(&q) || w.process.to_lowercase().contains(&q)).collect();
    match hits.len() {
        0 => Err(format!("没有标题或程序名包含「{query}」的窗口，可以先 list_windows 看看")),
        _ => Ok(HWND(hits[0].handle as *mut _)),
    }
}

pub fn act(h: HWND, action: &str) -> Result<String, String> {
    unsafe {
        match action {
            "" | "focus" => {
                if IsIconic(h).as_bool() {
                    let _ = ShowWindow(h, SW_RESTORE);
                }
                // 后台程序不能直接抢焦点：先模拟一次 Alt，系统才允许 SetForegroundWindow
                let alt = |flags| INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VK_MENU, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
                };
                SendInput(&[alt(KEYBD_EVENT_FLAGS(0)), alt(KEYEVENTF_KEYUP)], size_of::<INPUT>() as i32);
                let _ = BringWindowToTop(h);
                if !SetForegroundWindow(h).as_bool() {
                    return Err("系统拒绝切换窗口（目标可能是管理员权限的窗口）".into());
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
            other => return Err(format!("不支持的操作：{other}")),
        }
    }
    Ok(match action {
        "minimize" => "已最小化",
        "maximize" => "已最大化",
        "restore" => "已还原",
        "close" => "已发送关闭请求（程序可能会弹出保存提示）",
        _ => "已切换到该窗口",
    }
    .into())
}

/// 用资源管理器的方式打开：程序名、完整路径、文件、网址、ms-settings: 等都可以
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
        2 | 3 => Err(format!("找不到「{target}」")),
        5 => Err("拒绝访问".into()),
        n => Err(format!("打开失败（错误码 {n}）")),
    }
}

/// 窗口当前位置（屏幕物理像素）；最小化或已关闭时返回 None
pub fn rect_of(handle: isize) -> Option<(i32, i32, i32, i32)> {
    let h = HWND(handle as *mut _);
    unsafe {
        if !IsWindow(Some(h)).as_bool() || !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
            return None;
        }
        let mut r = RECT::default();
        GetWindowRect(h, &mut r).ok()?;
        Some((r.left, r.top, r.right, r.bottom))
    }
}
