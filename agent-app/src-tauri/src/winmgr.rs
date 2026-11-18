//! Windows on the desktop: listing, finding, switching, arranging, launching programs, and the clipboard.
//! Coordinates are physical screen pixels.

use serde_json::Value;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, HSTRING, PCWSTR, PWSTR, w};

pub type Rect = (i32, i32, i32, i32);

pub struct Win {
    pub handle: isize,
    pub title: String,
    pub process: String,
    pub rect: Rect,
    pub minimized: bool,
    pub maximized: bool,
    pub foreground: bool,
    /// A dialog or tool window that belongs to another window (its owner's handle)
    pub owner: Option<isize>,
}

pub fn hwnd(h: isize) -> HWND {
    HWND(h as *mut _)
}

pub fn process_name(pid: u32) -> String {
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

/// Every top-level window, in Z order from front to back
fn top_level() -> Vec<HWND> {
    let mut hs: Vec<HWND> = vec![];
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut hs as *mut _ as isize));
    }
    hs
}

pub fn title_of(handle: isize) -> String {
    let h = hwnd(handle);
    unsafe {
        let len = GetWindowTextLengthW(h);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(h, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

pub fn pid_of(handle: isize) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd(handle), Some(&mut pid)) };
    pid
}

/// Whether process `pid` is still running
pub fn process_alive(pid: u32) -> bool {
    use windows::Win32::System::Threading::GetExitCodeProcess;
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code).is_ok();
        let _ = CloseHandle(h);
        // STILL_ACTIVE
        ok && code == 259
    }
}

pub fn exists(handle: isize) -> bool {
    handle != 0 && unsafe { IsWindow(Some(hwnd(handle))) }.as_bool()
}

fn owner_of(h: HWND) -> Option<isize> {
    unsafe { GetWindow(h, GW_OWNER) }.ok().filter(|o| !o.is_invalid()).map(|o| o.0 as isize)
}

/// The program's main window that `handle` belongs to: its top owner, or else (a dialog shown without an owner)
/// the largest visible window of the same program under its middle. Itself when there is none. The overlay keeps
/// the orb in one place while the work goes into a program's dialogs and back.
pub fn app_of(handle: isize) -> isize {
    let mut h = handle;
    for _ in 0..8 {
        match owner_of(hwnd(h)) {
            Some(o) if rect_of(o).is_some() => h = o,
            _ => break,
        }
    }
    if h != handle {
        return h;
    }
    let Some(me) = rect_of(handle) else { return handle };
    let (cx, cy) = ((me.0 + me.2) / 2, (me.1 + me.3) / 2);
    let area = |r: &Rect| (r.2 - r.0) as i64 * (r.3 - r.1) as i64;
    let pid = pid_of(handle);
    top_level()
        .into_iter()
        .map(|w| w.0 as isize)
        .filter(|&w| w != handle && pid_of(w) == pid)
        .filter_map(|w| rect_of(w).map(|r| (w, r)))
        .filter(|(_, r)| r.0 <= cx && cx <= r.2 && r.1 <= cy && cy <= r.3 && area(r) > area(&me))
        .max_by_key(|(_, r)| area(r))
        .map(|(w, _)| w)
        .unwrap_or(handle)
}

fn win_of(h: HWND, fg: HWND) -> Win {
    let handle = h.0 as isize;
    let pid = pid_of(handle);
    let r = unsafe { visible_rect(h) }.unwrap_or_default();
    Win {
        handle,
        title: title_of(handle),
        process: process_name(pid),
        rect: (r.left, r.top, r.right, r.bottom),
        minimized: unsafe { IsIconic(h) }.as_bool(),
        maximized: unsafe { IsZoomed(h) }.as_bool(),
        foreground: h == fg,
        owner: owner_of(h),
    }
}

/// The windows a person would call windows: visible, titled, not a floating tool palette. Dialogs owned by
/// another window are included (a Save As box is a window to work in). Z order, front to back.
pub fn list() -> Vec<Win> {
    let fg = unsafe { GetForegroundWindow() };
    let me = std::process::id();
    let mut out = vec![];
    for h in top_level() {
        unsafe {
            if !IsWindowVisible(h).as_bool() || is_cloaked(h) {
                continue;
            }
            let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
            // Tool windows are palettes and helper popups; the one in front is still worth listing
            if ex & WS_EX_TOOLWINDOW.0 != 0 && h != fg {
                continue;
            }
            if GetWindowTextLengthW(h) == 0 {
                continue;
            }
        }
        // Skip our own overlay; the AI should neither see nor operate it
        if pid_of(h.0 as isize) == me {
            continue;
        }
        out.push(win_of(h, fg));
    }
    out
}

/// One window by handle, even one `list` leaves out (untitled, a popup)
pub fn info(handle: isize) -> Option<Win> {
    exists(handle).then(|| win_of(hwnd(handle), unsafe { GetForegroundWindow() }))
}

/// Find a window by handle or by a title / process-name keyword. "taskbar" and "desktop" name the shell's own.
pub fn find(handle: Option<isize>, query: &str) -> Result<HWND, String> {
    if let Some(h) = handle {
        return if exists(h) && pid_of(h) != std::process::id() { Ok(hwnd(h)) } else { Err(format!("No window with handle {h}; observe lists the windows with their handles")) };
    }
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Err("Say which window: a handle (observe lists them), or part of its title or program name".into());
    }
    let wins = list();
    // Front-most match wins; a title match beats a program-name match further forward only when exact
    if let Some(w) = wins.iter().find(|w| w.title.to_lowercase() == q) {
        return Ok(hwnd(w.handle));
    }
    if let Some(w) = wins.iter().find(|w| w.title.to_lowercase().contains(&q) || w.process.to_lowercase().contains(&q)) {
        return Ok(hwnd(w.handle));
    }
    let shell = match q.as_str() {
        "taskbar" => unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) }.ok(),
        "desktop" => unsafe { FindWindowW(w!("Progman"), PCWSTR::null()) }.ok(),
        _ => None,
    };
    if let Some(h) = shell.filter(|h| !h.is_invalid()) {
        return Ok(h);
    }
    let open: Vec<String> = wins.iter().take(12).map(|w| format!("\"{}\" ({})", clip(&w.title, 40), w.process)).collect();
    Err(format!("No window whose title or program name contains \"{query}\". Open windows: {}", if open.is_empty() { "none".into() } else { open.join(", ") }))
}

/// A `window` argument: a handle (number, or a string of digits) or a title / program-name keyword.
/// None when the argument is absent or empty.
pub fn find_arg(v: Option<&Value>) -> Result<Option<HWND>, String> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => find(Some(n.as_i64().unwrap_or(0) as isize), "").map(Some),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => match s.trim().parse::<i64>() {
            Ok(h) if h > 0xFFFF && exists(h as isize) => Ok(Some(hwnd(h as isize))),
            _ => find(None, s).map(Some),
        },
        Some(_) => Err("window must be a handle or part of a title".into()),
    }
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() > n { s.chars().take(n).collect::<String>() + "…" } else { s.to_string() }
}

/// One window's place in the stack, for telling whether what is under a point changed
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Slot {
    pub handle: isize,
    pub pid: u32,
    pub rect: Rect,
}

/// Whether another program's window lies over part of `area` (screen pixels) of `handle`'s window, so a capture
/// of the screen there would show it rather than this one. True too when the window is not on screen at all.
pub fn covered(handle: isize, area: crate::capture::Area) -> bool {
    let slots = layout();
    let Some(i) = slots.iter().position(|s| s.handle == handle) else { return true };
    let pid = slots[i].pid;
    let (al, at, ar, ab) = (area.left, area.top, area.left + area.width, area.top + area.height);
    slots[..i].iter().any(|s| s.pid != pid && s.rect.0 < ar && s.rect.2 > al && s.rect.1 < ab && s.rect.3 > at)
}

/// Every window that can be under the mouse, front to back: visible, not cloaked, not minimized, not ours,
/// and not click-through. Unlike `list`, untitled windows count (menus, popups, the taskbar, the desktop).
pub fn layout() -> Vec<Slot> {
    let me = std::process::id();
    let mut out = vec![];
    for h in top_level() {
        unsafe {
            if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() || is_cloaked(h) {
                continue;
            }
            let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
            if ex & WS_EX_TRANSPARENT.0 != 0 && ex & WS_EX_LAYERED.0 != 0 {
                continue;
            }
            let Some(r) = visible_rect(h) else { continue };
            if r.right - r.left < 2 || r.bottom - r.top < 2 {
                continue;
            }
            let pid = pid_of(h.0 as isize);
            if pid == me {
                continue;
            }
            out.push(Slot { handle: h.0 as isize, pid, rect: (r.left, r.top, r.right, r.bottom) });
        }
    }
    out
}

/// The front-most window of a layout that covers a point
pub fn slot_at(layout: &[Slot], x: i32, y: i32) -> Option<Slot> {
    layout.iter().find(|s| x >= s.rect.0 && x < s.rect.2 && y >= s.rect.1 && y < s.rect.3).copied()
}

/// Popups that belong to a window but are windows of their own: an open menu, a drop-down list, a context
/// menu, an autocomplete box. They are untitled or tool windows of the same program, so `list` leaves them out.
pub fn satellites(main: isize) -> Vec<isize> {
    let pid = pid_of(main);
    let me = std::process::id();
    if pid == 0 || pid == me {
        return vec![];
    }
    let mut out = vec![];
    for h in top_level() {
        let handle = h.0 as isize;
        if handle == main {
            continue;
        }
        unsafe {
            if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() || is_cloaked(h) || pid_of(handle) != pid {
                continue;
            }
            let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
            let style = GetWindowLongW(h, GWL_STYLE) as u32;
            let listed = GetWindowTextLengthW(h) > 0 && ex & WS_EX_TOOLWINDOW.0 == 0;
            // A titled window of its own (another document, a dialog) is addressed by itself
            if listed {
                continue;
            }
            if style & WS_POPUP.0 == 0 && ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0 | WS_EX_TOPMOST.0) == 0 {
                continue;
            }
            let Some(r) = visible_rect(h) else { continue };
            if r.right - r.left < 8 || r.bottom - r.top < 8 {
                continue;
            }
        }
        out.push(handle);
        if out.len() >= 6 {
            break;
        }
    }
    out
}

/// We run in the background, and Windows keeps a background process from bringing windows to the front.
/// A simulated Alt press counts as the user's input, which lifts that lock for us.
/// A bare Alt tap is also a keystroke for the window in front, though: it opens that program's menu (Claude's
/// top-left menu), and in a window with no menu bar, such as our own overlay, it starts system-menu mode,
/// which stalls the overlay's event loop until the next click. An unassigned key between Alt down and Alt up
/// makes it an Alt combination instead of an Alt tap, which nothing acts on.
fn unlock_foreground() {
    const MASK: VIRTUAL_KEY = VIRTUAL_KEY(0xE8);
    let key = |vk, flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: crate::presence::TAG } },
    };
    let down = KEYBD_EVENT_FLAGS(0);
    let keys = [key(VK_MENU, down), key(MASK, down), key(MASK, KEYEVENTF_KEYUP), key(VK_MENU, KEYEVENTF_KEYUP)];
    crate::presence::ours();
    unsafe { SendInput(&keys, size_of::<INPUT>() as i32) };
}

/// Whether this top-level window is really the one in front
pub fn is_front(h: isize) -> bool {
    let fg = unsafe { GetForegroundWindow() };
    !fg.is_invalid() && root_of(fg.0 as isize) == h
}

/// Bring a window to the front (restoring it when minimized). No-op when it already is.
pub fn bring_to_front(h: HWND) -> Result<(), String> {
    unsafe {
        if GetForegroundWindow() == h && !IsIconic(h).as_bool() {
            return Ok(());
        }
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        // Without the Alt tap Windows only flashes the taskbar button; try plainly first, since a stray Alt
        // can open the menu bar of whatever is in front
        if !SetForegroundWindow(h).as_bool() || GetForegroundWindow() != h {
            unlock_foreground();
            let _ = BringWindowToTop(h);
            if !SetForegroundWindow(h).as_bool() {
                // A window of a program running as administrator in front: one that isn't can't switch away from it
                // by any means (its keys are blocked too), so say whose it is
                if let Err(e) = crate::input::elevation_block(GetForegroundWindow(), "switching away from its window") {
                    return Err(format!("Windows refused to switch windows: {e}"));
                }
                return Err("Windows refused to switch windows (the target may be running as administrator)".into());
            }
        }
    }
    // The switch is asynchronous: give it a moment to land so the next keystroke goes to the right window
    for _ in 0..20 {
        if unsafe { GetForegroundWindow() } == h {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // It went above the overlay (it does when it is a topmost window): the overlay is put back on top at once
    crate::overlay::poke();
    Ok(())
}

pub fn act(h: HWND, action: &str) -> Result<String, String> {
    unsafe {
        match action {
            "" | "focus" => bring_to_front(h)?,
            "minimize" => {
                let _ = ShowWindow(h, SW_MINIMIZE);
            }
            "maximize" => {
                let _ = ShowWindow(h, SW_MAXIMIZE);
                let _ = bring_to_front(h);
            }
            "restore" => {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            "close" => {
                PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0)).map_err(|e| e.to_string())?;
            }
            other => return Err(format!("Unsupported action: {other} (use focus, minimize, maximize, restore, close or move)")),
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

/// Move and/or resize a window so its visible edge lands on the given rectangle (screen pixels); a missing
/// number keeps the current value. A maximized or minimized window is restored first.
pub fn set_bounds(h: HWND, x: Option<i32>, y: Option<i32>, w: Option<i32>, hgt: Option<i32>) -> Result<Rect, String> {
    unsafe {
        if IsIconic(h).as_bool() || IsZoomed(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
            std::thread::sleep(std::time::Duration::from_millis(60));
        }
        let vis = visible_rect(h).ok_or("That window has no position")?;
        let mut raw = RECT::default();
        GetWindowRect(h, &mut raw).map_err(|e| e.to_string())?;
        // The window rectangle includes an invisible resize border; keep that margin as it is
        let (ml, mt, mr, mb) = (vis.left - raw.left, vis.top - raw.top, raw.right - vis.right, raw.bottom - vis.bottom);
        let (nx, ny) = (x.unwrap_or(vis.left), y.unwrap_or(vis.top));
        let (nw, nh) = (w.unwrap_or(vis.right - vis.left).max(120), hgt.unwrap_or(vis.bottom - vis.top).max(80));
        SetWindowPos(h, None, nx - ml, ny - mt, nw + ml + mr, nh + mt + mb, SWP_NOZORDER | SWP_NOACTIVATE)
            .map_err(|_| "Windows refused to move the window (it may be running as administrator)".to_string())?;
        std::thread::sleep(std::time::Duration::from_millis(30));
        let r = visible_rect(h).unwrap_or(vis);
        Ok((r.left, r.top, r.right, r.bottom))
    }
}

/// What came up for something opened
pub struct Opened {
    pub hwnd: isize,
    /// It is the window that was in front already, which only changed its title (a URL opened as a new tab of
    /// the browser in front): that is all that was seen, not proof the page is the one asked for
    pub in_place: bool,
}

/// Open the way Explorer does: program names, full paths, files, URLs, ms-settings: links, etc.
/// Returns the window that came up for it, brought to the front if Windows left it behind (None for a console
/// program, or anything that shows no window within `wait`).
pub fn open(target: &str, args: &str, cwd: &str, wait: std::time::Duration) -> Result<Option<Opened>, String> {
    let before: std::collections::HashSet<isize> = list().iter().map(|w| w.handle).collect();
    let fg0 = foreground();
    // A URL opened in the browser that is already in front shows up only as a new tab: its title changes.
    // Only a URL is looked for that way; for anything else a title changing in front is the page's own doing
    // (a clock, an unread count) and says nothing about what was opened.
    let lower = target.to_ascii_lowercase();
    let url = lower.contains("://") || lower.starts_with("www.");
    let title0 = title_of(fg0);
    // Otherwise the new program's window would open behind the current one: take the right to set the
    // foreground, and pass it on to the program being launched
    unlock_foreground();
    let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
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
        n if n > 32 => {}
        2 | 3 => return Err(format!("Can't find \"{target}\"")),
        5 => return Err("Access denied".into()),
        n => return Err(format!("Failed to open (error code {n})")),
    }
    // Wait for a new window, or for an existing one to come forward (a URL in a browser already open, a single-instance app)
    let came_up = || -> Option<isize> {
        if let Some(w) = list().into_iter().find(|w| !before.contains(&w.handle) && !w.minimized) {
            return Some(w.handle);
        }
        let fg = foreground();
        if url && fg != 0 && fg == fg0 && title_of(fg) != title0 {
            return Some(fg);
        }
        (fg != 0 && fg != fg0 && before.contains(&fg)).then_some(fg)
    };
    let start = std::time::Instant::now();
    let mut found = None;
    while start.elapsed() < wait {
        std::thread::sleep(std::time::Duration::from_millis(40));
        if let Some(h) = came_up() {
            found = Some(h);
            break;
        }
    }
    let Some(first) = found else { return Ok(None) };
    // Windows often resize or replace a splash screen right after appearing: wait until the window is the
    // same one in the same place twice in a row (a splash that lingers is cut off after 600 ms)
    let mut last = (first, rect_of(first));
    let mut same = 0;
    let settle = std::time::Instant::now();
    while same < 3 && settle.elapsed() < std::time::Duration::from_millis(600) {
        std::thread::sleep(std::time::Duration::from_millis(40));
        let h = came_up().unwrap_or(last.0);
        let now = (h, rect_of(h));
        same = if now == last { same + 1 } else { 0 };
        last = now;
    }
    let h = Some(last.0).filter(|&h| rect_of(h).is_some()).or(Some(first).filter(|&h| rect_of(h).is_some()));
    if let Some(h) = h
        && foreground() != h
    {
        let _ = bring_to_front(hwnd(h));
    }
    Ok(h.map(|h| Opened { hwnd: h, in_place: h == fg0 && title_of(h) != title0 }))
}

/// The window in front right now. Our own overlay never counts: should Windows hand it the foreground (it
/// can, right after the app starts), the window in front is the first real one behind it.
pub fn foreground() -> isize {
    let h = unsafe { GetForegroundWindow() }.0 as isize;
    if h != 0 && pid_of(h) == std::process::id() {
        return list().first().map(|w| w.handle).unwrap_or(0);
    }
    // Its top-level window: a press through UI Automation can leave a child window (a button) as the foreground
    if h != 0 { root_of(h) } else { 0 }
}

/// Make sure keystrokes have somewhere to go: when our own overlay holds the foreground, hand it to the
/// first real window behind it. Returns the window that is in front afterwards.
pub fn real_foreground() -> isize {
    let raw = unsafe { GetForegroundWindow() }.0 as isize;
    let h = foreground();
    if h != 0 && h != raw {
        let _ = bring_to_front(hwnd(h));
    }
    h
}

/// The top-level window under a screen point (the overlay lets the mouse through, so it is never this)
pub fn top_window_at(x: i32, y: i32) -> Option<isize> {
    let h = unsafe { WindowFromPoint(POINT { x, y }) };
    if h.is_invalid() {
        return None;
    }
    Some(root_of(h.0 as isize))
}

/// Physical pixels per CSS pixel of a window (its monitor's scale; 1.0 when unknown)
pub fn scale_of(handle: isize) -> f64 {
    let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd(handle)) };
    if dpi == 0 { 1.0 } else { dpi as f64 / 96.0 }
}

/// The top-level window a (possibly child) window belongs to
pub fn root_of(handle: isize) -> isize {
    let root = unsafe { GetAncestor(hwnd(handle), GA_ROOT) };
    if root.is_invalid() { handle } else { root.0 as isize }
}

/// Current window position (physical screen pixels); None when minimized or closed
pub fn rect_of(handle: isize) -> Option<Rect> {
    let h = hwnd(handle);
    unsafe {
        if !IsWindow(Some(h)).as_bool() || !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
            return None;
        }
        let r = visible_rect(h)?;
        Some((r.left, r.top, r.right, r.bottom))
    }
}

/// Where a browser window shows its page, on the screen: the largest visible child window of class `class`
/// (Chrome_RenderWidgetHostHWND), in physical pixels
pub fn page_rect(handle: isize, class: &str) -> Option<Rect> {
    struct Find<'a> {
        class: &'a str,
        best: Option<RECT>,
    }
    unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> BOOL {
        let f = unsafe { &mut *(lp.0 as *mut Find) };
        let mut buf = [0u16; 64];
        let n = unsafe { GetClassNameW(h, &mut buf) } as usize;
        if String::from_utf16_lossy(&buf[..n]) == f.class && unsafe { IsWindowVisible(h) }.as_bool() {
            let mut r = RECT::default();
            if unsafe { GetWindowRect(h, &mut r) }.is_ok() {
                let area = |r: &RECT| (r.right - r.left) as i64 * (r.bottom - r.top) as i64;
                if f.best.as_ref().is_none_or(|b| area(&r) > area(b)) {
                    f.best = Some(r);
                }
            }
        }
        BOOL(1)
    }
    let mut f = Find { class, best: None };
    unsafe {
        let _ = EnumChildWindows(Some(hwnd(handle)), Some(each), LPARAM(&mut f as *mut Find as isize));
    }
    f.best.map(|r| (r.left, r.top, r.right, r.bottom))
}

pub fn is_minimized(handle: isize) -> bool {
    unsafe { IsIconic(hwnd(handle)) }.as_bool()
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

// ---- Clipboard ----

const CF_BITMAP: u32 = 2;
const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;

/// Another program (a clipboard manager, a remote-desktop client) may hold the clipboard for a moment
fn open_clipboard() -> Result<(), String> {
    use windows::Win32::System::DataExchange::OpenClipboard;
    for _ in 0..10 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    Err("The clipboard is in use by another program; try again".into())
}

unsafe fn clipboard_unicode() -> Result<String, String> {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::GetClipboardData;
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    unsafe {
        let h = GetClipboardData(CF_UNICODETEXT).map_err(|e| e.to_string())?;
        let p = GlobalLock(HGLOBAL(h.0)) as *const u16;
        if p.is_null() {
            return Ok(String::new());
        }
        // Never read past the block, even when the text is not terminated
        let max = GlobalSize(HGLOBAL(h.0)) / 2;
        let mut n = 0;
        while n < max && *p.add(n) != 0 {
            n += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
        let _ = GlobalUnlock(HGLOBAL(h.0));
        Ok(s)
    }
}

/// What the clipboard holds, so it can be put back: Ok(Some(text)), Ok(None) when empty,
/// Err when it holds something other than text (an image, copied files), which can't be restored
pub fn clipboard_text() -> Result<Option<String>, String> {
    use windows::Win32::System::DataExchange::{CloseClipboard, CountClipboardFormats, IsClipboardFormatAvailable};
    open_clipboard()?;
    let r = unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
            if CountClipboardFormats() == 0 { Ok(None) } else { Err("The clipboard holds something other than text".to_string()) }
        } else {
            clipboard_unicode().map(Some)
        }
    };
    unsafe {
        let _ = CloseClipboard();
    }
    r
}

/// The clipboard as the AI should read it: its text, the list of copied files, or a note that it holds an image
pub fn clipboard_describe() -> Result<String, String> {
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, IsClipboardFormatAvailable};
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    open_clipboard()?;
    let r = unsafe {
        if IsClipboardFormatAvailable(CF_HDROP).is_ok() {
            match GetClipboardData(CF_HDROP) {
                Ok(h) => {
                    let drop = HDROP(h.0);
                    let n = DragQueryFileW(drop, u32::MAX, None);
                    let mut lines = vec!["[copied files]".to_string()];
                    for i in 0..n {
                        let len = DragQueryFileW(drop, i, None) as usize;
                        let mut buf = vec![0u16; len + 1];
                        let got = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
                        lines.push(String::from_utf16_lossy(&buf[..got.min(len)]));
                    }
                    Ok(lines.join("\n"))
                }
                Err(e) => Err(e.to_string()),
            }
        } else if IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok() {
            clipboard_unicode().map(|t| if t.is_empty() { "(clipboard is empty)".into() } else { t })
        } else if IsClipboardFormatAvailable(CF_BITMAP).is_ok() || IsClipboardFormatAvailable(CF_DIB).is_ok() {
            Ok("[clipboard holds an image]".into())
        } else {
            Ok("(clipboard is empty)".into())
        }
    };
    unsafe {
        let _ = CloseClipboard();
    }
    r.map_err(|e| format!("Failed to read the clipboard: {e}"))
}

/// A number that goes up every time anything changes the clipboard
pub fn clipboard_serial() -> u32 {
    unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

/// Empty the clipboard
pub fn clear_clipboard() {
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard};
    if open_clipboard().is_ok() {
        unsafe {
            let _ = EmptyClipboard();
            let _ = CloseClipboard();
        }
    }
}

/// Put text on the clipboard
pub fn set_clipboard_text(text: &str) -> Result<(), String> {
    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    open_clipboard()?;
    unsafe {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_front_most_window_covering_a_point_wins() {
        let layout = [
            Slot { handle: 1, pid: 10, rect: (100, 100, 300, 300) },
            Slot { handle: 2, pid: 20, rect: (0, 0, 1000, 1000) },
        ];
        assert_eq!(slot_at(&layout, 150, 150).map(|s| s.handle), Some(1));
        assert_eq!(slot_at(&layout, 50, 50).map(|s| s.handle), Some(2));
        assert_eq!(slot_at(&layout, 300, 300).map(|s| s.handle), Some(2), "right and bottom edges are outside");
        assert_eq!(slot_at(&layout, 2000, 50), None);
    }
}
