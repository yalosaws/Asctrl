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

/// The visible windows above the overlay now, front to back. Only topmost windows can be (the overlay is one), so
/// the walk from the top stops at the overlay, which is normally first, or at the first window that is not topmost.
fn above_now() -> Vec<isize> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::*;
    let ours = HWND_RAW.load(std::sync::atomic::Ordering::Relaxed);
    let mut out = Vec::new();
    if ours == 0 {
        return out;
    }
    unsafe {
        let mut h = GetTopWindow(None).unwrap_or_default();
        while !h.is_invalid() && h.0 as isize != ours {
            if GetWindowLongW(h, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0 == 0 {
                // Below the topmost band without meeting the overlay: it is not showing at all
                return Vec::new();
            }
            if IsWindowVisible(h).as_bool() {
                out.push(h.0 as isize);
            }
            h = GetWindow(h, GW_HWNDNEXT).unwrap_or(HWND::default());
        }
    }
    out
}

/// Windows that stay above the overlay however it is raised (the shell's own, in a band above every program's
/// topmost windows: the Start menu, the input switcher...). They are not covering it in a way it can undo, so
/// they don't count; without this the keeper would raise it forever.
static UNBEATEN: Mutex<Vec<isize>> = Mutex::new(Vec::new());

/// Whether a window the overlay can go above is above it now
fn covered_now() -> bool {
    let skip = UNBEATEN.lock().unwrap();
    above_now().iter().any(|h| !skip.contains(h))
}

/// The keeper thread, for pokes from the threads that act
static KEEPER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
const WM_POKE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 7;

/// Something may just have put a window above the overlay (the driver brought a window to the front, or pressed a
/// mouse button, which does): the keeper checks at once. Returns at once; never waits for the overlay.
pub fn poke() {
    let t = KEEPER.load(std::sync::atomic::Ordering::Relaxed);
    if t != 0 {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(t, WM_POKE, Default::default(), Default::default());
        }
    }
}

/// Raises in the last second, so two programs that each insist on being on top don't trade places forever
static RAISES: Mutex<(Option<Instant>, u32)> = Mutex::new((None, 0));

/// Raise the overlay if anything is above it
fn keep_up() {
    if !covered_now() {
        return;
    }
    let mut r = RAISES.lock().unwrap();
    if r.0.is_none_or(|t| t.elapsed() > Duration::from_secs(1)) {
        *r = (Some(Instant::now()), 0);
    }
    r.1 += 1;
    if r.1 <= 30 {
        drop(r);
        let t = Instant::now();
        raise();
        let still = above_now();
        let ms = t.elapsed().as_millis();
        // For the animation log (ASCTRL_FX_LOG): each raise, how long it took, and what it could not go above
        crate::fx::note(&serde_json::json!({ "phase": "keeper", "raiseMs": ms, "unbeaten": still.len() }));
        *UNBEATEN.lock().unwrap() = still;
    }
}

/// Keep the overlay on top, on a thread of its own: anything drawn on it (the pointers, a frame round a window)
/// must not blink out while the window it is drawn over comes to the front. A program clicked to the front, a
/// window restored, another topmost window (the test bench is one) all go above the overlay; this thread sees
/// that the moment it happens (window events, pokes from the driver's own actions) and puts the overlay back
/// above within a millisecond or so, and checks every 40 ms besides for what sends no event. On the main thread,
/// which also runs the overlay's page, the same hooks came late: a window stayed above it for up to 280 ms.
fn keep_on_top() {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
    use windows::Win32::UI::WindowsAndMessaging::*;
    unsafe extern "system" fn on_event(_: HWINEVENTHOOK, event: u32, hwnd: HWND, obj: i32, _: i32, _: u32, _: u32) {
        // A window shown counts only when it is a whole top-level window (not a caret, a tooltip's text...)
        if event == EVENT_OBJECT_SHOW && (obj != OBJID_WINDOW.0 || unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd) {
            return;
        }
        keep_up();
    }
    let _ = std::thread::Builder::new().name("overlay-keeper".into()).spawn(|| unsafe {
        let mut msg = MSG::default();
        // The thread's message queue exists from here, so pokes can be posted to it
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        KEEPER.store(GetCurrentThreadId(), std::sync::atomic::Ordering::Relaxed);
        let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
        // Foreground changes, windows restored from minimized, windows reordered or shown all can cover it
        for (from, to) in [(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND), (EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZEEND), (EVENT_OBJECT_SHOW, EVENT_OBJECT_SHOW), (EVENT_OBJECT_REORDER, EVENT_OBJECT_REORDER)] {
            SetWinEventHook(from, to, None, Some(on_event), 0, 0, flags);
        }
        let _ = SetTimer(None, 0, 40, None);
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_POKE || msg.message == WM_TIMER {
                keep_up();
            }
            DispatchMessageW(&msg);
        }
    });
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

/// Hidden from the taskbar and Alt+Tab (so the windows observe lists skip it too), and never takes focus on a
/// click. Again after each change of click-through: the window library rewrites the whole extended style then,
/// dropping these (the overlay was found in Alt+Tab, able to take the focus).
fn restyle() {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::*;
    let raw = HWND_RAW.load(std::sync::atomic::Ordering::Relaxed);
    if raw == 0 {
        return;
    }
    let h = HWND(raw as *mut _);
    unsafe {
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        let want = (ex | (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) as isize) & !(WS_EX_APPWINDOW.0 as isize);
        if want != ex {
            SetWindowLongPtrW(h, GWL_EXSTYLE, want);
        }
    }
}

fn harden(win: &WebviewWindow) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::*;
    let Ok(h) = win.hwnd() else { return };
    let h = HWND(h.0);
    HWND_RAW.store(h.0 as isize, std::sync::atomic::Ordering::Relaxed);
    restyle();
    unsafe {
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
    restyle();
    raise();
    keep_on_top();
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
                restyle();
            }
            // Should anything still hand the overlay the foreground, pass it on before a keystroke lands on it
            // (keeping it on top is the keeper thread's job)
            if tick.is_multiple_of(10) {
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
    // The screens the overlay draws on, for replaying the animation log (test/fxcheck.ts)
    crate::fx::note(&serde_json::json!({ "phase": "info", "info": info }));
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
