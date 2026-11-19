//! Mouse and keyboard control (Windows SendInput). Coordinates are physical screen pixels.

use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::Win32::UI::Input::Ime::ImmGetDefaultIMEWnd;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GUITHREADINFO, GetAncestor, GetCursorPos, GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, SMTO_ABORTIFHUNG,
    SendMessageTimeoutW, SetCursorPos, WM_IME_CONTROL, WindowFromPoint,
};

const BLOCKED: &str = "Input was blocked by Windows: the target window may be running as administrator (call request_admin, then reconnect), or the lock screen / UAC prompt is showing";

/// A key or mouse button Asctrl pressed and has not released yet
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Held {
    /// (virtual key, scan code, flags without KEYUP)
    Key(u16, u16, u32),
    /// The flag that releases it (MOUSEEVENTF_LEFTUP, ...)
    Button(u32),
}

/// What Asctrl holds down right now. Something left held turns the person's mouse into a drag, or their clicks
/// into ctrl- or shift-clicks, long after the AI is gone: it is let go when an input is cut off half way, when
/// the session ends, and when the program quits.
static HELD: std::sync::Mutex<Vec<Held>> = std::sync::Mutex::new(Vec::new());

const BUTTON_PAIRS: [(MOUSE_EVENT_FLAGS, MOUSE_EVENT_FLAGS); 3] =
    [(MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP), (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP), (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP)];

/// Note what the inputs Windows took pressed and released
fn track(taken: &[INPUT]) {
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    for i in taken {
        if i.r#type == INPUT_KEYBOARD {
            let k = unsafe { i.Anonymous.ki };
            let h = Held::Key(k.wVk.0, k.wScan, k.dwFlags.0 & !KEYEVENTF_KEYUP.0);
            held.retain(|x| *x != h);
            if k.dwFlags.0 & KEYEVENTF_KEYUP.0 == 0 {
                held.push(h);
            }
        } else if i.r#type == INPUT_MOUSE {
            let f = unsafe { i.Anonymous.mi.dwFlags }.0;
            for (down, up) in BUTTON_PAIRS {
                if f & (down.0 | up.0) != 0 {
                    held.retain(|x| *x != Held::Button(up.0));
                }
                if f & down.0 != 0 && f & up.0 == 0 {
                    held.push(Held::Button(up.0));
                }
            }
        }
    }
}

fn send_raw(inputs: &[INPUT]) -> usize {
    let n = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) } as usize;
    track(&inputs[..n.min(inputs.len())]);
    n
}

fn send(inputs: &[INPUT]) -> Result<(), String> {
    crate::presence::ours();
    let n = send_raw(inputs);
    // A press can bring its window to the front, above the overlay: have it checked at once
    if inputs.iter().any(|i| i.r#type == INPUT_MOUSE && unsafe { i.Anonymous.mi.dwFlags }.0 & (MOUSEEVENTF_LEFTDOWN.0 | MOUSEEVENTF_RIGHTDOWN.0 | MOUSEEVENTF_MIDDLEDOWN.0) != 0) {
        crate::overlay::poke();
    }
    if n == inputs.len() {
        Ok(())
    } else {
        // Cut off half way (Windows refused the rest): whatever went down is let go, rather than left held
        release_all();
        Err(BLOCKED.into())
    }
}

/// Let go of every key and button Asctrl still holds down. What Windows refuses to take now stays on the list,
/// for the next try.
pub fn release_all() {
    let held: Vec<Held> = HELD.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if held.is_empty() {
        return;
    }
    crate::presence::ours();
    // Keys in the reverse order they went down (modifiers last), then the buttons
    for h in held.iter().rev() {
        let up = match *h {
            Held::Key(vk, scan, flags) => key(VIRTUAL_KEY(vk), scan, KEYBD_EVENT_FLAGS(flags) | KEYEVENTF_KEYUP),
            Held::Button(up) => mouse(MOUSE_EVENT_FLAGS(up), 0),
        };
        send_raw(&[up]);
    }
}

fn mouse(flags: MOUSE_EVENT_FLAGS, data: i32) -> INPUT {
    mouse_at(flags, data, 0, 0)
}

fn mouse_at(flags: MOUSE_EVENT_FLAGS, data: i32, dx: i32, dy: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: crate::presence::TAG },
        },
    }
}

/// Whether a process token is elevated. None when it can't be told.
fn token_elevated(process: HANDLE) -> Option<bool> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.ok()?;
    let mut e = TOKEN_ELEVATION::default();
    let mut len = 0u32;
    let r = unsafe {
        GetTokenInformation(token, TokenElevation, Some(&mut e as *mut _ as *mut _), size_of::<TOKEN_ELEVATION>() as u32, &mut len)
    };
    unsafe {
        let _ = CloseHandle(token);
    }
    r.ok().map(|_| e.TokenIsElevated != 0)
}

/// The program behind a window, if it runs as administrator: (elevated, exe name)
fn window_elevated(hwnd: HWND) -> Option<(bool, String)> {
    if hwnd.is_invalid() {
        return None;
    }
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    let hwnd = if root.is_invalid() { hwnd } else { root };
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 || pid == std::process::id() {
        return None;
    }
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 512];
    let mut n = buf.len() as u32;
    let name = match unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut n) } {
        Ok(()) => {
            let path = String::from_utf16_lossy(&buf[..n as usize]);
            path.rsplit('\\').next().unwrap_or(&path).to_string()
        }
        Err(_) => format!("process {pid}"),
    };
    // A non-elevated process of the same user can always open the token of another non-elevated one;
    // being refused means the target is elevated (or otherwise above us)
    let elevated = token_elevated(process).unwrap_or(true);
    unsafe {
        let _ = CloseHandle(process);
    }
    Some((elevated, name))
}

/// Windows (UIPI) drops input that a non-elevated program sends to an elevated one, often without
/// any error. Check the foreground window, and the window under `at` for mouse actions, before acting.
fn check_target(at: Option<(i32, i32)>) -> Result<(), String> {
    if token_elevated(unsafe { GetCurrentProcess() }) == Some(true) {
        return Ok(());
    }
    let mut targets = vec![unsafe { GetForegroundWindow() }];
    if let Some((x, y)) = at {
        targets.push(unsafe { WindowFromPoint(POINT { x, y }) });
    }
    targets.into_iter().try_for_each(|h| elevation_block(h, "mouse and keyboard input to it"))
}

/// Err when `hwnd` belongs to an elevated program and this one is not: Windows (UIPI) blocks input and
/// UI Automation from a non-elevated program to it. `what` names what is blocked.
pub fn elevation_block(hwnd: HWND, what: &str) -> Result<(), String> {
    if token_elevated(unsafe { GetCurrentProcess() }) == Some(true) {
        return Ok(());
    }
    match window_elevated(hwnd) {
        Some((true, name)) => Err(format!(
            "{name} is running as administrator, so Windows blocks {what} from the assist app. \
             Call request_admin (Windows shows its own confirmation), then connect again with the same code, and retry."
        )),
        _ => Ok(()),
    }
}

fn key(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: crate::presence::TAG } },
    }
}

fn move_to(x: i32, y: i32) -> Result<(), String> {
    crate::presence::ours();
    // SetCursorPos refused by UIPI reports "The operation completed successfully"; the code is useless
    unsafe { SetCursorPos(x, y) }.map_err(|_| BLOCKED.to_string())
}

pub fn cursor_pos() -> Result<(i32, i32), String> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p) }.map_err(|e| format!("Failed to read the mouse position: {e}"))?;
    Ok((p.x, p.y))
}

fn buttons(button: &str) -> Result<(MOUSE_EVENT_FLAGS, MOUSE_EVENT_FLAGS), String> {
    Ok(match button {
        "" | "left" => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        "right" => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
        "middle" => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        other => return Err(format!("Unknown mouse button: {other}")),
    })
}

/// Parse a key combination such as "ctrl+shift"
fn parse_keys(combo: &str) -> Result<Vec<(VIRTUAL_KEY, bool)>, String> {
    if combo.trim().is_empty() {
        return Ok(vec![]);
    }
    combo.split('+').map(|k| vk_of(k).ok_or_else(|| format!("Unknown key: {k}"))).collect()
}

fn ext(e: bool) -> KEYBD_EVENT_FLAGS {
    if e { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) }
}

fn keys_down(keys: &[(VIRTUAL_KEY, bool)]) -> Vec<INPUT> {
    keys.iter().map(|&(vk, e)| key(vk, 0, ext(e))).collect()
}

fn keys_up(keys: &[(VIRTUAL_KEY, bool)]) -> Vec<INPUT> {
    keys.iter().rev().map(|&(vk, e)| key(vk, 0, ext(e) | KEYEVENTF_KEYUP)).collect()
}

/// The last click made: when, where, with which button
static LAST_CLICK: std::sync::Mutex<Option<(std::time::Instant, i32, i32, String)>> = std::sync::Mutex::new(None);

/// Windows turns two clicks on the same spot within the double-click time into a double-click, whoever
/// meant what. Two separate click calls mean two clicks: when the last one was here a moment ago, wait
/// until the pair can no longer be taken for a double-click.
fn keep_clicks_apart(x: i32, y: i32, button: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXDOUBLECLK, SM_CYDOUBLECLK};
    let Some((when, lx, ly, lb)) = LAST_CLICK.lock().unwrap().clone() else { return };
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXDOUBLECLK).max(4), GetSystemMetrics(SM_CYDOUBLECLK).max(4)) };
    if lb != button || (x - lx).abs() > w || (y - ly).abs() > h {
        return;
    }
    let limit = Duration::from_millis(unsafe { GetDoubleClickTime() } as u64 + 30);
    if let Some(left) = limit.checked_sub(when.elapsed()) {
        std::thread::sleep(left);
    }
}

/// Click, optionally holding modifier keys (e.g. ctrl to multi-select, shift to range-select)
pub fn click(x: i32, y: i32, button: &str, count: u32, modifiers: &str) -> Result<(), String> {
    let (down, up) = buttons(button)?;
    let mods = parse_keys(modifiers)?;
    check_target(Some((x, y)))?;
    keep_clicks_apart(x, y, button);
    aim(x, y)?;
    if !mods.is_empty() {
        send(&keys_down(&mods))?;
    }
    let mut r = Ok(());
    for _ in 0..count.clamp(1, 3) {
        r = send(&[mouse(down, 0), mouse(up, 0)]);
        if r.is_err() || count == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if !mods.is_empty() && send(&keys_up(&mods)).is_err() {
        release_all();
    }
    *LAST_CLICK.lock().unwrap() = Some((std::time::Instant::now(), x, y, button.to_string()));
    r
}

/// Put the pointer on (x, y) for a press, and make sure it is still there when the press goes: the person at the
/// computer may take the mouse in the moment between the move and the press, and a press then lands wherever
/// they put it. When that happens it waits for them to let go (up to 3 s), aims again, and gives up rather than
/// press elsewhere. Only while someone has used the mouse or keyboard lately: a game that keeps re-centring the
/// pointer is left alone.
fn aim(x: i32, y: i32) -> Result<(), String> {
    let lately = || crate::presence::others_ago().is_some_and(|d| d < Duration::from_secs(2));
    for attempt in 0..2 {
        move_to(x, y)?;
        let placed = cursor_pos().unwrap_or((x, y));
        // Long enough for the program to see the pointer arrive (hover state) before the press
        std::thread::sleep(Duration::from_millis(15));
        let moved = cursor_pos().is_ok_and(|p| (p.0 - placed.0).abs() > 1 || (p.1 - placed.1).abs() > 1);
        if !(moved && lately()) {
            return Ok(());
        }
        if attempt == 0 {
            let t = std::time::Instant::now();
            while crate::presence::others_ago().is_some_and(|d| d < Duration::from_millis(600)) && t.elapsed() < Duration::from_secs(3) {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    Err("Not done: the person at the computer moved the mouse just as the press was about to happen, so nothing was pressed (they come first). Try again in a moment.".into())
}

pub fn mouse_move(x: i32, y: i32) -> Result<(), String> {
    check_target(Some((x, y)))?;
    move_to(x, y)
}

/// Move the mouse by (dx, dy) counts, the way a physical mouse does, spread over `ms` milliseconds.
/// Games that read raw mouse input (first-person cameras) ignore SetCursorPos; they only see this.
pub fn mouse_look(dx: i32, dy: i32, ms: u64) -> Result<(), String> {
    check_target(None)?;
    // At most ~20 counts per step so the motion reads as a sweep rather than a jump
    let steps = ((dx.abs().max(dy.abs()) + 19) / 20).clamp(1, 200);
    let pause = Duration::from_millis(ms / steps as u64);
    let (mut sx, mut sy) = (0, 0);
    for i in 1..=steps {
        let (tx, ty) = (dx * i / steps, dy * i / steps);
        send(&[mouse_at(MOUSEEVENTF_MOVE, 0, tx - sx, ty - sy)])?;
        (sx, sy) = (tx, ty);
        if i < steps {
            std::thread::sleep(pause);
        }
    }
    Ok(())
}

/// Press or release a mouse button on its own, for drags that need fine control
pub fn mouse_button(x: Option<(i32, i32)>, button: &str, press: bool) -> Result<(), String> {
    let (down, up) = buttons(button)?;
    check_target(x.or_else(|| cursor_pos().ok()))?;
    if let Some((x, y)) = x {
        move_to(x, y)?;
        std::thread::sleep(Duration::from_millis(40));
    }
    send(&[mouse(if press { down } else { up }, 0)])
}

/// amount > 0 scrolls down (right when horizontal), < 0 up (left), in wheel notches
pub fn scroll(x: i32, y: i32, amount: i32, horizontal: bool) -> Result<(), String> {
    check_target(Some((x, y)))?;
    move_to(x, y)?;
    std::thread::sleep(Duration::from_millis(15));
    if horizontal {
        send(&[mouse(MOUSEEVENTF_HWHEEL, amount * 120)])
    } else {
        send(&[mouse(MOUSEEVENTF_WHEEL, -amount * 120)])
    }
}

/// Press `button` at the first point, move to the second in small steps (programs that track drags need to
/// see the pointer travel), pause, and release. `ms` is how long the travel takes.
pub fn drag(x1: i32, y1: i32, x2: i32, y2: i32, button: &str, ms: u64) -> Result<(), String> {
    let (down, up) = buttons(button)?;
    check_target(Some((x1, y1)))?;
    move_to(x1, y1)?;
    std::thread::sleep(Duration::from_millis(40));
    send(&[mouse(down, 0)])?;
    // A first small nudge: many programs only start a drag once the pointer leaves a few-pixel dead zone
    let dist = (((x2 - x1) as f64).hypot((y2 - y1) as f64)) as i32;
    let steps = (dist / 24).clamp(6, 40);
    let pause = Duration::from_millis((ms / steps as u64).clamp(4, 40));
    let moved = (1..=steps).try_for_each(|i| {
        std::thread::sleep(pause);
        move_to(x1 + (x2 - x1) * i / steps, y1 + (y2 - y1) * i / steps)
    });
    std::thread::sleep(Duration::from_millis(40));
    // Always release, even when a move failed: a button left held down would turn the user's
    // next mouse movement into a drag
    let released = send(&[mouse(up, 0)]);
    moved.and(released)
}

/// Characters sent in one go. Programs read them from their input queue at their own pace; a short pause
/// between chunks keeps slow ones (Electron, remote sessions) from dropping any.
const TYPE_CHUNK: usize = 24;

/// The keyboard layout of the window in front, and whether its input method may turn real keystrokes for
/// letters into candidates instead of text: it says it is in native mode (Chinese, Japanese, Korean), or the
/// layout is a CJK one, whose input methods (Sogou, Microsoft Pinyin and other TSF ones) mostly don't answer
/// the query, so their mode can't be told
fn target_keyboard() -> (HKL, bool) {
    // WM_IME_CONTROL queries
    const IMC_GETCONVERSIONMODE: usize = 1;
    const IMC_GETOPENSTATUS: usize = 5;
    // IME_CMODE_NATIVE: typing native characters rather than letters
    const CMODE_NATIVE: usize = 1;
    unsafe {
        let fg = GetForegroundWindow();
        let tid = GetWindowThreadProcessId(fg, None);
        let mut gi = GUITHREADINFO { cbSize: size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        let focus = if GetGUIThreadInfo(tid, &mut gi).is_ok() && !gi.hwndFocus.is_invalid() { gi.hwndFocus } else { fg };
        let ime = ImmGetDefaultIMEWnd(focus);
        let query = |cmd: usize| {
            let mut r = 0usize;
            let ok = SendMessageTimeoutW(ime, WM_IME_CONTROL, WPARAM(cmd), LPARAM(0), SMTO_ABORTIFHUNG, 100, Some(&mut r));
            (ok.0 != 0).then_some(r)
        };
        let native = !ime.is_invalid() && query(IMC_GETOPENSTATUS).unwrap_or(0) != 0 && query(IMC_GETCONVERSIONMODE).unwrap_or(0) & CMODE_NATIVE != 0;
        let hkl = GetKeyboardLayout(tid);
        // Primary language of the layout: Chinese, Japanese, Korean
        let cjk = matches!(hkl.0 as usize & 0x3ff, 0x04 | 0x11 | 0x12);
        (hkl, native || cjk)
    }
}

/// The real key presses for `ch` on keyboard layout `hkl` (with shift when it needs it), or None when the
/// character has no key of its own there (CJK, emoji, AltGr symbols). Real key codes matter for games and
/// web pages that read which key was pressed: a character sent as unicode reaches them with no key code.
fn real_keys(ch: char, hkl: HKL, caps: bool) -> Option<Vec<INPUT>> {
    if !(ch == ' ' || ch.is_ascii_graphic()) {
        return None;
    }
    let r = unsafe { VkKeyScanExW(ch as u16, hkl) };
    if r == -1 {
        return None;
    }
    let (vk, mods) = (r as u16 & 0xff, (r as u16 >> 8) & 0xff);
    // Only plain and shifted keys: ctrl / alt combinations would trigger shortcuts
    if mods & !1 != 0 {
        return None;
    }
    // Caps lock swaps the case of letters, so shift is needed the other way round
    let shift = (mods & 1 != 0) ^ (caps && ch.is_ascii_alphabetic());
    let scan = unsafe { MapVirtualKeyExW(vk as u32, MAPVK_VK_TO_VSC, Some(hkl)) } as u16;
    let vk = VIRTUAL_KEY(vk);
    let mut v = vec![];
    if shift {
        v.push(key(VK_LSHIFT, 0x2a, KEYBD_EVENT_FLAGS(0)));
    }
    v.push(key(vk, scan, KEYBD_EVENT_FLAGS(0)));
    v.push(key(vk, scan, KEYEVENTF_KEYUP));
    if shift {
        v.push(key(VK_LSHIFT, 0x2a, KEYEVENTF_KEYUP));
    }
    Some(v)
}

/// Type any text (including CJK) without depending on the input method. Characters that have a key on the
/// keyboard are pressed as real keys, so games and pages that read key codes see them; the rest is sent as
/// unicode. Where a Chinese / Japanese / Korean input method could catch keys, everything is sent as unicode,
/// digits too: such an input method handles real keys on a path of its own, which the unicode characters
/// overtake, so "第6行" came out as "第行6" and characters went missing.
/// `real_keys_only`: press real keys whatever the input method (games, which read keys and use none).
/// `caught_up`: with such an input method, called before each Enter with the line just typed, to wait until
/// the program has taken it in: Enter is not held up by the input method and would otherwise overtake the
/// last characters of the line, or drop them.
pub fn type_text(text: &str, real_keys_only: bool, caught_up: &mut dyn FnMut(&str)) -> Result<(), String> {
    check_target(None)?;
    let (hkl, ime) = target_keyboard();
    let native = ime && !real_keys_only;
    let caps = unsafe { GetKeyState(VK_CAPITAL.0 as i32) } & 1 != 0;
    let mut inputs: Vec<INPUT> = vec![];
    let mut chars = 0;
    // What has been typed of the current line
    let mut line = String::new();
    let flush = |inputs: &mut Vec<INPUT>| -> Result<(), String> {
        if !inputs.is_empty() {
            send(inputs)?;
            inputs.clear();
            std::thread::sleep(Duration::from_millis(4));
        }
        Ok(())
    };
    for ch in text.chars() {
        // "\r\n" is one Enter, not two
        if ch == '\r' {
            continue;
        }
        if ch == '\n' {
            // Enter goes on its own: it may open a dialog or submit a form, which needs a moment
            flush(&mut inputs)?;
            if native && !line.is_empty() {
                caught_up(&line);
            }
            send(&[key(VK_RETURN, 0, KEYBD_EVENT_FLAGS(0)), key(VK_RETURN, 0, KEYEVENTF_KEYUP)])?;
            std::thread::sleep(Duration::from_millis(15));
            chars = 0;
            line.clear();
            continue;
        }
        line.push(ch);
        // In native mode letters would become pinyin / kana, punctuation full-width, and digits race the rest
        match real_keys(ch, hkl, caps).filter(|_| !native) {
            Some(keys) => inputs.extend(keys),
            None => {
                let mut buf = [0u16; 2];
                for unit in ch.encode_utf16(&mut buf) {
                    inputs.push(key(VIRTUAL_KEY(0), *unit, KEYEVENTF_UNICODE));
                    inputs.push(key(VIRTUAL_KEY(0), *unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
                }
            }
        }
        chars += 1;
        if chars >= TYPE_CHUNK {
            flush(&mut inputs)?;
            chars = 0;
        }
    }
    flush(&mut inputs)
}

fn vk_of(name: &str) -> Option<(VIRTUAL_KEY, bool)> {
    let n = name.trim().to_lowercase();
    // (virtual key, is extended key)
    let k = match n.as_str() {
        "ctrl" | "control" => (VK_CONTROL, false),
        "shift" => (VK_SHIFT, false),
        "alt" => (VK_MENU, false),
        "win" | "windows" | "meta" | "super" | "cmd" => (VK_LWIN, true),
        "enter" | "return" => (VK_RETURN, false),
        "esc" | "escape" => (VK_ESCAPE, false),
        "tab" => (VK_TAB, false),
        "space" => (VK_SPACE, false),
        "backspace" => (VK_BACK, false),
        "delete" | "del" => (VK_DELETE, true),
        "insert" | "ins" => (VK_INSERT, true),
        "home" => (VK_HOME, true),
        "end" => (VK_END, true),
        "pageup" | "pgup" => (VK_PRIOR, true),
        "pagedown" | "pgdn" => (VK_NEXT, true),
        "up" => (VK_UP, true),
        "down" => (VK_DOWN, true),
        "left" => (VK_LEFT, true),
        "right" => (VK_RIGHT, true),
        "capslock" => (VK_CAPITAL, false),
        "printscreen" | "prtsc" => (VK_SNAPSHOT, true),
        "apps" | "menu" => (VK_APPS, true),
        "pause" | "break" => (VK_PAUSE, false),
        "numlock" => (VK_NUMLOCK, true),
        "scrolllock" => (VK_SCROLL, false),
        "volumeup" => (VK_VOLUME_UP, true),
        "volumedown" => (VK_VOLUME_DOWN, true),
        "volumemute" | "mute" => (VK_VOLUME_MUTE, true),
        "plus" => (VK_OEM_PLUS, false),
        _ => {
            let b = n.as_bytes();
            if b.len() == 1 && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit()) {
                return Some((VIRTUAL_KEY(b[0].to_ascii_uppercase() as u16), false));
            }
            // Other single symbols (, . / ; ' [ ] - = ` \) are looked up in the current keyboard layout
            if b.len() == 1 && b[0].is_ascii_punctuation() {
                let r = unsafe { VkKeyScanW(b[0] as u16) };
                if r != -1 && (r as u16 >> 8) & 0xff == 0 {
                    return Some((VIRTUAL_KEY(r as u16 & 0xff), false));
                }
            }
            if let Some(f) = n.strip_prefix('f').and_then(|s| s.parse::<u16>().ok()).filter(|f| (1..=24).contains(f)) {
                return Some((VIRTUAL_KEY(VK_F1.0 + f - 1), false));
            }
            return None;
        }
    };
    Some(k)
}

/// Press a key combination, e.g. "ctrl+c", "win+r", "alt+f4", "enter"
pub fn press_keys(combo: &str) -> Result<(), String> {
    let keys = parse_keys(combo)?;
    if keys.is_empty() {
        return Err("No keys given".into());
    }
    check_target(None)?;
    let mut inputs = keys_down(&keys);
    inputs.extend(keys_up(&keys));
    send(&inputs)
}

/// Press several combinations one after another: "ctrl+a delete", "tab tab enter", "alt+f s"
pub fn press_sequence(seq: &str) -> Result<usize, String> {
    let combos: Vec<&str> = seq.split_whitespace().collect();
    if combos.is_empty() {
        return Err("No keys given".into());
    }
    // Reject the whole sequence before pressing anything when one key is unknown
    for c in &combos {
        if parse_keys(c)?.is_empty() {
            return Err("No keys given".into());
        }
    }
    for (i, c) in combos.iter().enumerate() {
        if i > 0 {
            std::thread::sleep(Duration::from_millis(30));
        }
        press_keys(c)?;
    }
    Ok(combos.len())
}

/// Hold a key combination for a while, then release it
pub fn hold_keys(combo: &str, ms: u64) -> Result<(), String> {
    let keys = parse_keys(combo)?;
    if keys.is_empty() {
        return Err("No keys given".into());
    }
    check_target(None)?;
    send(&keys_down(&keys))?;
    std::thread::sleep(Duration::from_millis(ms));
    let r = send(&keys_up(&keys));
    if r.is_err() {
        release_all();
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_goes_down_is_kept_until_it_comes_up() {
        HELD.lock().unwrap().clear();
        let shift = key(VK_LSHIFT, 0x2a, KEYBD_EVENT_FLAGS(0));
        let a = key(VIRTUAL_KEY(0x41), 0x1e, KEYBD_EVENT_FLAGS(0));
        // Cut off after shift and A went down, before either came up
        track(&[shift, a]);
        assert_eq!(HELD.lock().unwrap().len(), 2);
        track(&[key(VIRTUAL_KEY(0x41), 0x1e, KEYEVENTF_KEYUP)]);
        assert_eq!(*HELD.lock().unwrap(), vec![Held::Key(VK_LSHIFT.0, 0x2a, 0)]);
        track(&[mouse(MOUSEEVENTF_LEFTDOWN, 0)]);
        assert!(HELD.lock().unwrap().contains(&Held::Button(MOUSEEVENTF_LEFTUP.0)));
        track(&[mouse(MOUSEEVENTF_LEFTUP, 0), key(VK_LSHIFT, 0x2a, KEYEVENTF_KEYUP)]);
        assert!(HELD.lock().unwrap().is_empty());
    }
}
