//! Mouse and keyboard control (Windows SendInput). Coordinates are physical screen pixels.

use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, POINT};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GetAncestor, GetCursorPos, GetForegroundWindow, GetWindowThreadProcessId, SetCursorPos, WindowFromPoint,
};

const BLOCKED: &str = "Input was blocked by Windows: the target window may be running as administrator (call request_admin, then reconnect), or the lock screen / UAC prompt is showing";

fn send(inputs: &[INPUT]) -> Result<(), String> {
    let n = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    if n as usize == inputs.len() { Ok(()) } else { Err(BLOCKED.into()) }
}

fn mouse(flags: MOUSE_EVENT_FLAGS, data: i32) -> INPUT {
    mouse_at(flags, data, 0, 0)
}

fn mouse_at(flags: MOUSE_EVENT_FLAGS, data: i32, dx: i32, dy: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 },
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
             Call request_admin (the user approves), then connect again with the same code, and retry."
        )),
        _ => Ok(()),
    }
}

fn key(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

fn move_to(x: i32, y: i32) -> Result<(), String> {
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

/// Click, optionally holding modifier keys (e.g. ctrl to multi-select, shift to range-select)
pub fn click(x: i32, y: i32, button: &str, count: u32, modifiers: &str) -> Result<(), String> {
    let (down, up) = buttons(button)?;
    let mods = parse_keys(modifiers)?;
    check_target(Some((x, y)))?;
    move_to(x, y)?;
    // Long enough for the program to see the pointer arrive (hover state) before the press
    std::thread::sleep(Duration::from_millis(15));
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
    if !mods.is_empty() {
        let _ = send(&keys_up(&mods));
    }
    r
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

pub fn drag(x1: i32, y1: i32, x2: i32, y2: i32) -> Result<(), String> {
    check_target(Some((x1, y1)))?;
    move_to(x1, y1)?;
    std::thread::sleep(Duration::from_millis(40));
    send(&[mouse(MOUSEEVENTF_LEFTDOWN, 0)])?;
    let moved = (1..=10).try_for_each(|i| {
        std::thread::sleep(Duration::from_millis(20));
        move_to(x1 + (x2 - x1) * i / 10, y1 + (y2 - y1) * i / 10)
    });
    std::thread::sleep(Duration::from_millis(40));
    // Always release, even when a move failed: a button left held down would turn the user's
    // next mouse movement into a drag
    let released = send(&[mouse(MOUSEEVENTF_LEFTUP, 0)]);
    moved.and(released)
}

/// Characters sent in one go. Programs read them from their input queue at their own pace; a short pause
/// between chunks keeps slow ones (Electron, remote sessions) from dropping any.
const TYPE_CHUNK: usize = 24;

/// Type any text (including CJK) without depending on the input method
pub fn type_text(text: &str) -> Result<(), String> {
    check_target(None)?;
    let mut inputs: Vec<INPUT> = vec![];
    let mut chars = 0;
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
            send(&[key(VK_RETURN, 0, KEYBD_EVENT_FLAGS(0)), key(VK_RETURN, 0, KEYEVENTF_KEYUP)])?;
            std::thread::sleep(Duration::from_millis(15));
            chars = 0;
            continue;
        }
        let mut buf = [0u16; 2];
        for unit in ch.encode_utf16(&mut buf) {
            inputs.push(key(VIRTUAL_KEY(0), *unit, KEYEVENTF_UNICODE));
            inputs.push(key(VIRTUAL_KEY(0), *unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
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

/// Hold a key combination for a while, then release it
pub fn hold_keys(combo: &str, ms: u64) -> Result<(), String> {
    let keys = parse_keys(combo)?;
    if keys.is_empty() {
        return Err("No keys given".into());
    }
    check_target(None)?;
    send(&keys_down(&keys))?;
    std::thread::sleep(Duration::from_millis(ms));
    send(&keys_up(&keys))
}
