//! 鼠标和键盘控制（Windows SendInput）。坐标是屏幕物理像素。

use std::time::Duration;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

const BLOCKED: &str = "输入被系统拦截：目标窗口可能是管理员权限运行的（可先 request_admin），或当前是锁屏/UAC 界面";

fn send(inputs: &[INPUT]) -> Result<(), String> {
    let n = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    if n as usize == inputs.len() { Ok(()) } else { Err(BLOCKED.into()) }
}

fn mouse(flags: MOUSE_EVENT_FLAGS, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn key(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

fn move_to(x: i32, y: i32) -> Result<(), String> {
    unsafe { SetCursorPos(x, y) }.map_err(|e| format!("移动鼠标失败：{e}"))
}

pub fn cursor_pos() -> Result<(i32, i32), String> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p) }.map_err(|e| format!("读取鼠标位置失败：{e}"))?;
    Ok((p.x, p.y))
}

fn buttons(button: &str) -> Result<(MOUSE_EVENT_FLAGS, MOUSE_EVENT_FLAGS), String> {
    Ok(match button {
        "" | "left" => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        "right" => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
        "middle" => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        other => return Err(format!("不认识的鼠标键：{other}")),
    })
}

/// 解析 "ctrl+shift" 这样的按键组合
fn parse_keys(combo: &str) -> Result<Vec<(VIRTUAL_KEY, bool)>, String> {
    if combo.trim().is_empty() {
        return Ok(vec![]);
    }
    combo.split('+').map(|k| vk_of(k).ok_or_else(|| format!("不认识的按键：{k}"))).collect()
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

/// 点击，可以按住修饰键（例如 ctrl 多选、shift 连选）
pub fn click(x: i32, y: i32, button: &str, count: u32, modifiers: &str) -> Result<(), String> {
    let (down, up) = buttons(button)?;
    let mods = parse_keys(modifiers)?;
    move_to(x, y)?;
    std::thread::sleep(Duration::from_millis(40));
    if !mods.is_empty() {
        send(&keys_down(&mods))?;
    }
    let mut r = Ok(());
    for _ in 0..count.clamp(1, 3) {
        r = send(&[mouse(down, 0), mouse(up, 0)]);
        if r.is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    if !mods.is_empty() {
        let _ = send(&keys_up(&mods));
    }
    r
}

pub fn mouse_move(x: i32, y: i32) -> Result<(), String> {
    move_to(x, y)
}

/// 只按下或只松开鼠标键，用于需要精细控制的拖动
pub fn mouse_button(x: Option<(i32, i32)>, button: &str, press: bool) -> Result<(), String> {
    let (down, up) = buttons(button)?;
    if let Some((x, y)) = x {
        move_to(x, y)?;
        std::thread::sleep(Duration::from_millis(40));
    }
    send(&[mouse(if press { down } else { up }, 0)])
}

/// amount > 0 向下（横向时向右）滚，< 0 向上（向左）滚，单位是"格"
pub fn scroll(x: i32, y: i32, amount: i32, horizontal: bool) -> Result<(), String> {
    move_to(x, y)?;
    std::thread::sleep(Duration::from_millis(40));
    if horizontal {
        send(&[mouse(MOUSEEVENTF_HWHEEL, amount * 120)])
    } else {
        send(&[mouse(MOUSEEVENTF_WHEEL, -amount * 120)])
    }
}

pub fn drag(x1: i32, y1: i32, x2: i32, y2: i32) -> Result<(), String> {
    move_to(x1, y1)?;
    std::thread::sleep(Duration::from_millis(40));
    send(&[mouse(MOUSEEVENTF_LEFTDOWN, 0)])?;
    for i in 1..=10 {
        std::thread::sleep(Duration::from_millis(20));
        move_to(x1 + (x2 - x1) * i / 10, y1 + (y2 - y1) * i / 10)?;
    }
    std::thread::sleep(Duration::from_millis(40));
    send(&[mouse(MOUSEEVENTF_LEFTUP, 0)])
}

/// 输入任意文字（包括中文），不依赖输入法
pub fn type_text(text: &str) -> Result<(), String> {
    for ch in text.chars() {
        if ch == '\n' {
            send(&[key(VK_RETURN, 0, KEYBD_EVENT_FLAGS(0)), key(VK_RETURN, 0, KEYEVENTF_KEYUP)])?;
        } else {
            let mut buf = [0u16; 2];
            let mut inputs = vec![];
            for unit in ch.encode_utf16(&mut buf) {
                inputs.push(key(VIRTUAL_KEY(0), *unit, KEYEVENTF_UNICODE));
                inputs.push(key(VIRTUAL_KEY(0), *unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
            }
            send(&inputs)?;
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    Ok(())
}

fn vk_of(name: &str) -> Option<(VIRTUAL_KEY, bool)> {
    let n = name.trim().to_lowercase();
    // (虚拟键, 是否扩展键)
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
            // 其他单个符号（, . / ; ' [ ] - = ` \）按当前键盘布局找对应的键
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

/// 按组合键，例如 "ctrl+c"、"win+r"、"alt+f4"、"enter"
pub fn press_keys(combo: &str) -> Result<(), String> {
    let keys = parse_keys(combo)?;
    if keys.is_empty() {
        return Err("没有指定按键".into());
    }
    let mut inputs = keys_down(&keys);
    inputs.extend(keys_up(&keys));
    send(&inputs)
}

/// 按住组合键一段时间再松开
pub fn hold_keys(combo: &str, ms: u64) -> Result<(), String> {
    let keys = parse_keys(combo)?;
    if keys.is_empty() {
        return Err("没有指定按键".into());
    }
    send(&keys_down(&keys))?;
    std::thread::sleep(Duration::from_millis(ms));
    send(&keys_up(&keys))
}
