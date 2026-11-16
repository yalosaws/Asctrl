//! Fullscreen transparent overlay: covers every monitor, lets the mouse through by default, never takes
//! focus, and is excluded from screenshots. It only catches the mouse over the "interactive regions" the
//! UI registers (the orb, the consent card).

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
    /// The whole monitor (physical pixels, screen coordinates)
    pub bounds: Rect,
    /// The area minus the taskbar
    pub work: Rect,
    pub primary: bool,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    /// Screen coordinates of the overlay's top-left (physical pixels)
    pub origin_x: i32,
    pub origin_y: i32,
    /// Physical pixels / CSS pixels
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
    /// Interactive regions (physical pixels, screen coordinates)
    regions: Mutex<Vec<Rect>>,
    /// Interaction is paused while the AI drives the mouse, so it doesn't click the orb
    suppress_until: Mutex<Option<Instant>>,
    /// Calls that move the mouse and are still running: interaction stays off for as long as any is, however
    /// long it takes (a fixed window ran out halfway through a long batch, and a click landed on the orb)
    driving: std::sync::atomic::AtomicUsize,
}

/// Interaction stays off while this lives, then for `tail` after
pub struct Driving<'a> {
    ov: &'a Overlay,
    tail: Duration,
}

impl Drop for Driving<'_> {
    fn drop(&mut self) {
        self.ov.suppress(self.tail);
        self.ov.driving.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Overlay {
    /// Turn interaction off until the returned guard is dropped (and `tail` after that)
    pub fn driving(&self, tail: Duration) -> Driving<'_> {
        self.driving.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Driving { ov: self, tail }
    }

    fn suppressed(&self) -> bool {
        self.driving.load(std::sync::atomic::Ordering::SeqCst) > 0 || self.suppress_until.lock().unwrap().is_some_and(|t| t > Instant::now())
    }

    pub fn suppress(&self, d: Duration) {
        let until = Instant::now() + d;
        let mut s = self.suppress_until.lock().unwrap();
        if s.is_none_or(|t| t < until) {
            *s = Some(until);
        }
    }
}

/// Stretch the window over every monitor and return the geometry the UI needs
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
    // 1px shorter than the screen: an exact fit makes Windows treat it as a "fullscreen app" and mute notifications
    let _ = win.set_size(PhysicalSize::new((r - l) as u32, (b - t - 1).max(1) as u32));
    Info { origin_x: l, origin_y: t, scale: win.scale_factor().unwrap_or(1.0), monitors }
}

/// Overlay window handle, for the foreground hook and the topmost poll
static HWND_RAW: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// Push the overlay straight to the top of the topmost band.
/// Not via set_always_on_top: once the window already has the topmost flag it no longer calls SetWindowPos,
/// so after another topmost window (or a program just clicked to the front) covers it, it never comes back.
fn raise() {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::*;
    let raw = HWND_RAW.load(std::sync::atomic::Ordering::Relaxed);
    if raw == 0 {
        return;
    }
    unsafe {
        let _ = SetWindowPos(
            HWND(raw as *mut _),
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_NOSENDCHANGING,
        );
    }
}

/// Re-raise immediately whenever the foreground window changes (the user clicks another app, the Start menu opens, etc.).
/// The hook callback runs on the installing thread, so it must be installed on the main thread, which has a message loop.
fn watch_foreground() {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
    use windows::Win32::UI::WindowsAndMessaging::{EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS};
    unsafe extern "system" fn on_event(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
        raise();
    }
    unsafe {
        // Foreground changes and windows restored from minimized both bring other windows forward
        SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, Some(on_event), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
        SetWinEventHook(EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZEEND, None, Some(on_event), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
    }
}

/// The overlay must never be the window in front: keys meant for programs would land on it, and an Alt there
/// puts it in system-menu mode, which stalls its event loop and freezes every animation until the user clicks
/// it. WS_EX_NOACTIVATE keeps clicks from activating it, but Tauri's show() still does; when it holds the
/// foreground, hand it to `prev`, or else to the first real window behind it.
fn give_away_foreground(prev: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};
    let ours = HWND_RAW.load(std::sync::atomic::Ordering::Relaxed);
    if ours == 0 || unsafe { GetForegroundWindow() }.0 as isize != ours {
        return;
    }
    let to = if prev != 0 && prev != ours { prev } else { crate::winmgr::list().first().map(|w| w.handle).unwrap_or(0) };
    if to != 0 {
        unsafe {
            let _ = SetForegroundWindow(HWND(to as *mut _));
        }
    }
}

/// Show the overlay (or make sure it is shown) without leaving it in front
pub fn show(win: &WebviewWindow) {
    let prev = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() }.0 as isize;
    let _ = win.show();
    give_away_foreground(prev);
}

fn harden(win: &WebviewWindow) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::*;
    let Ok(h) = win.hwnd() else { return };
    let h = HWND(h.0);
    HWND_RAW.store(h.0 as isize, std::sync::atomic::Ordering::Relaxed);
    unsafe {
        // Hidden from the taskbar and Alt+Tab (so the windows observe lists skip it too), and never takes focus on click
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        SetWindowLongPtrW(h, GWL_EXSTYLE, ex | (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) as isize);
        // Invisible to screen capture, so the AI always sees the real desktop (Windows 10 2004+)
        let _ = SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE);
    }
}

/// Left Alt and right Alt held down together quit the program at once, whatever state it is in: the way out
/// when it misbehaves (the overlay frozen, the AI doing something unwanted) without finding the orb's menu.
/// Polled on its own thread, so a stuck UI or event loop can't keep it from working.
fn watch_quit_keys() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LMENU, VK_RMENU};
    std::thread::spawn(|| {
        let down = |vk: u16| unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0;
        loop {
            std::thread::sleep(Duration::from_millis(30));
            if down(VK_LMENU.0) && down(VK_RMENU.0) {
                // Let both keys come up first, so the program in front doesn't see a lone Alt left held down
                let start = Instant::now();
                while (down(VK_LMENU.0) || down(VK_RMENU.0)) && start.elapsed() < Duration::from_millis(1500) {
                    std::thread::sleep(Duration::from_millis(10));
                }
                std::process::exit(0);
            }
        }
    });
}

pub fn setup(app: &AppHandle) {
    watch_quit_keys();
    let Some(win) = app.get_webview_window("main") else { return };
    let info = layout(&win);
    harden(&win);
    let _ = win.set_ignore_cursor_events(true);
    let _ = win.set_always_on_top(true);
    show(&win);
    raise();
    watch_foreground();
    *app.state::<Overlay>().info.lock().unwrap() = info;

    // Toggle mouse pass-through as the cursor enters or leaves an interactive region
    let app = app.clone();
    std::thread::spawn(move || {
        let mut passing = true;
        let mut tick = 0u32;
        loop {
            std::thread::sleep(Duration::from_millis(30));
            tick = tick.wrapping_add(1);
            let ov = app.state::<Overlay>();
            let suppressed = ov.suppressed();
            let inside = !suppressed
                && crate::input::cursor_pos().is_ok_and(|(x, y)| {
                    ov.regions.lock().unwrap().iter().any(|r| x >= r.x && y >= r.y && x < r.x + r.w && y < r.y + r.h)
                });
            if inside == passing {
                passing = !inside;
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.set_ignore_cursor_events(passing);
                }
            }
            // Fallback: another topmost window may cover it without a foreground change, so re-raise every 0.3 s
            if tick.is_multiple_of(10) {
                raise();
                // Should anything still hand the overlay the foreground, pass it on before a keystroke lands on it
                give_away_foreground(0);
            }
        }
    });
}

#[tauri::command]
pub fn overlay_info(app: AppHandle) -> Info {
    // Re-lay out every time, so a UI reload recovers after monitors or resolution change
    let info = match app.get_webview_window("main") {
        Some(w) => layout(&w),
        None => Info::default(),
    };
    *app.state::<Overlay>().info.lock().unwrap() = info.clone();
    info
}

/// The UI tells the backend which areas are clickable (CSS pixels, relative to the overlay's top-left)
#[tauri::command]
pub fn set_hit_regions(app: AppHandle, rects: Vec<HitRect>, scale: Option<f64>) {
    let ov = app.state::<Overlay>();
    let info = ov.info.lock().unwrap().clone();
    // Trust the UI's actual scale: the backend's value may be stale right after the window moves across screens
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
