//! 全屏透明悬浮层：盖住所有显示器、鼠标默认穿透、不抢焦点、不进截图。
//! 只有光标落在界面登记的"可交互区域"（小精灵、确认卡片）上时才接收鼠标。

use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewWindow};

#[derive(Serialize, Clone, Copy, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Serialize, Clone)]
pub struct MonitorInfo {
    /// 整个显示器（物理像素，屏幕坐标）
    pub bounds: Rect,
    /// 去掉任务栏后的区域
    pub work: Rect,
    pub primary: bool,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    /// 悬浮层左上角的屏幕坐标（物理像素）
    pub origin_x: i32,
    pub origin_y: i32,
    /// 物理像素 / CSS 像素
    pub scale: f64,
    pub monitors: Vec<MonitorInfo>,
}

#[derive(Deserialize)]
pub struct HitRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[derive(Default)]
pub struct Overlay {
    info: Mutex<Info>,
    /// 可交互区域（物理像素，屏幕坐标）
    regions: Mutex<Vec<Rect>>,
    /// AI 正在操作鼠标时暂停交互，免得点到小精灵身上
    suppress_until: Mutex<Option<Instant>>,
}

impl Overlay {
    pub fn suppress(&self, d: Duration) {
        let until = Instant::now() + d;
        let mut s = self.suppress_until.lock().unwrap();
        if s.is_none_or(|t| t < until) {
            *s = Some(until);
        }
    }
}

/// 把窗口铺满所有显示器，返回界面需要的坐标信息
fn layout(win: &WebviewWindow) -> Info {
    let mons = win.available_monitors().unwrap_or_default();
    let primary = win.primary_monitor().ok().flatten();
    let rect = |p: PhysicalPosition<i32>, s: PhysicalSize<u32>| Rect { x: p.x, y: p.y, w: s.width as i32, h: s.height as i32 };
    let monitors: Vec<MonitorInfo> = mons
        .iter()
        .map(|m| MonitorInfo {
            bounds: rect(*m.position(), *m.size()),
            work: rect(m.work_area().position, m.work_area().size),
            primary: primary.as_ref().is_some_and(|p| p.position() == m.position()),
        })
        .collect();
    let (mut l, mut t, mut r, mut b) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for m in &monitors {
        l = l.min(m.bounds.x);
        t = t.min(m.bounds.y);
        r = r.max(m.bounds.x + m.bounds.w);
        b = b.max(m.bounds.y + m.bounds.h);
    }
    if monitors.is_empty() {
        (l, t, r, b) = (0, 0, 1920, 1080);
    }
    let _ = win.set_position(PhysicalPosition::new(l, t));
    // 比屏幕矮 1 像素：正好盖满时 Windows 会当成"全屏程序"，把通知静音
    let _ = win.set_size(PhysicalSize::new((r - l) as u32, (b - t - 1).max(1) as u32));
    Info { origin_x: l, origin_y: t, scale: win.scale_factor().unwrap_or(1.0), monitors }
}

#[cfg(windows)]
fn harden(win: &WebviewWindow) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::*;
    let Ok(h) = win.hwnd() else { return };
    let h = HWND(h.0);
    unsafe {
        // 不在任务栏/Alt+Tab 出现（也让 list_windows 自动忽略它），点击时不抢焦点
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        SetWindowLongPtrW(h, GWL_EXSTYLE, ex | (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) as isize);
        // 截图时看不到这一层，AI 看到的永远是真实桌面（Win10 2004 起支持）
        let _ = SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE);
    }
}

#[cfg(not(windows))]
fn harden(_: &WebviewWindow) {}

#[cfg(windows)]
fn cursor() -> Option<(i32, i32)> {
    crate::input::cursor_pos().ok()
}

#[cfg(not(windows))]
fn cursor() -> Option<(i32, i32)> {
    None
}

pub fn setup(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else { return };
    let info = layout(&win);
    harden(&win);
    let _ = win.set_ignore_cursor_events(true);
    let _ = win.set_always_on_top(true);
    let _ = win.show();
    *app.state::<Overlay>().info.lock().unwrap() = info;

    // 光标进出可交互区域时切换"鼠标穿透"
    let app = app.clone();
    std::thread::spawn(move || {
        let mut passing = true;
        let mut tick = 0u32;
        loop {
            std::thread::sleep(Duration::from_millis(30));
            tick = tick.wrapping_add(1);
            let ov = app.state::<Overlay>();
            let suppressed = ov.suppress_until.lock().unwrap().is_some_and(|t| t > Instant::now());
            let inside = !suppressed
                && cursor().is_some_and(|(x, y)| {
                    ov.regions.lock().unwrap().iter().any(|r| x >= r.x && y >= r.y && x < r.x + r.w && y < r.y + r.h)
                });
            if inside == passing {
                passing = !inside;
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.set_ignore_cursor_events(passing);
                }
            }
            // 别的置顶窗口可能盖过来，每隔几秒重新置顶一次
            if tick % 100 == 0 {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.set_always_on_top(true);
                }
            }
        }
    });
}

#[tauri::command]
pub fn overlay_info(app: AppHandle) -> Info {
    // 每次都重新铺一遍，显示器插拔、改分辨率后界面刷新即可恢复
    let info = match app.get_webview_window("main") {
        Some(w) => layout(&w),
        None => Info::default(),
    };
    *app.state::<Overlay>().info.lock().unwrap() = info.clone();
    info
}

/// 界面告诉后台哪些地方可以点（CSS 像素，相对于悬浮层左上角）
#[tauri::command]
pub fn set_hit_regions(app: AppHandle, rects: Vec<HitRect>, scale: Option<f64>) {
    let ov = app.state::<Overlay>();
    let info = ov.info.lock().unwrap().clone();
    // 以界面实际的缩放为准：窗口刚跨屏移动后，后台记的值可能还是旧的
    let s = scale.filter(|v| v.is_finite() && *v > 0.0).unwrap_or(info.scale);
    *ov.regions.lock().unwrap() = rects
        .iter()
        .map(|r| Rect {
            x: info.origin_x + (r.x * s).floor() as i32,
            y: info.origin_y + (r.y * s).floor() as i32,
            w: (r.w * s).ceil() as i32,
            h: (r.h * s).ceil() as i32,
        })
        .collect();
}
