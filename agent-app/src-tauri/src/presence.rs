//! Someone else at the keyboard: the person at the computer, or anyone reaching it through another program (a
//! remote desktop tool). Their mouse and keyboard come first: an action that needs the real mouse, keyboard
//! or focus waits until they have stopped for a moment (see `tools::acting`), instead of fighting them over
//! the pointer. Actions that need none of these (a page in Asctrl's own browser) go ahead meanwhile.
//!
//! The input Asctrl sends carries a mark of its own (`TAG`, in each input's extra info); raw input (a copy of
//! every mouse and keyboard event, delivered to a hidden window of ours) notes the time of the last one without
//! it. Not low-level hooks: Windows makes every mouse move on the computer wait for those to return, so a driver
//! that was busy, paged out on a computer short of memory, or hung made the person's mouse stutter or freeze.
//! Raw input is only a copy; nothing waits for it. Moving the pointer with SetCursorPos sends no input at all,
//! so a move right after Asctrl's own input is still taken as Asctrl's. `ASCTRL_NO_YIELD=1` turns this off.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The mark on Asctrl's own input ("ASCT")
pub const TAG: usize = 0x4153_4354;

/// A move this soon after Asctrl's own input is the pointer it moved
const OWN_MOVE: Duration = Duration::from_millis(120);

static EPOCH: OnceLock<Instant> = OnceLock::new();
/// Milliseconds since EPOCH (+1, so 0 means never) of the last input by someone else, and of Asctrl's own
static OTHERS: AtomicU64 = AtomicU64::new(0);
static OURS: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicBool = AtomicBool::new(false);

fn now() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

/// Asctrl is about to send input (or move the pointer)
pub fn ours() {
    OURS.store(now(), Ordering::SeqCst);
}

/// An input event seen by the hooks: someone else's unless it carries the mark, or is a move right after ours
fn seen(tagged: bool, is_move: bool) {
    if tagged {
        return;
    }
    let t = now();
    if is_move && t.saturating_sub(OURS.load(Ordering::SeqCst)) < OWN_MOVE.as_millis() as u64 {
        return;
    }
    OTHERS.store(t, Ordering::SeqCst);
}

/// How long ago someone else last used the mouse or keyboard; None when never (or not watched)
pub fn others_ago() -> Option<Duration> {
    if !LIVE.load(Ordering::SeqCst) {
        return None;
    }
    match OTHERS.load(Ordering::SeqCst) {
        0 => None,
        t => Some(Duration::from_millis(now().saturating_sub(t))),
    }
}

/// Put the hooks in, on a thread of their own that pumps their messages
pub fn start() {
    if std::env::var("ASCTRL_NO_YIELD").is_ok_and(|v| v == "1") || LIVE.load(Ordering::SeqCst) {
        return;
    }
    let _ = now();
    let _ = std::thread::Builder::new().name("asctrl-presence".into()).spawn(hook_thread);
}

/// The extra info SendInput attaches arrives in raw input cut to 32 bits
const TAG32: u32 = TAG as u32;

#[cfg(windows)]
fn hook_thread() {
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;

    unsafe extern "system" fn on_message(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        if msg == WM_INPUT {
            let mut raw = RAWINPUT::default();
            let mut size = size_of::<RAWINPUT>() as u32;
            let got = unsafe { GetRawInputData(HRAWINPUT(l.0 as *mut _), RID_INPUT, Some(&mut raw as *mut _ as *mut _), &mut size, size_of::<RAWINPUTHEADER>() as u32) };
            if got != u32::MAX && got as usize >= size_of::<RAWINPUTHEADER>() {
                if raw.header.dwType == RIM_TYPEMOUSE.0 {
                    let m = unsafe { raw.data.mouse };
                    let buttons = unsafe { m.Anonymous.Anonymous.usButtonFlags };
                    seen(m.ulExtraInformation == TAG32, buttons == 0);
                } else if raw.header.dwType == RIM_TYPEKEYBOARD.0 {
                    let k = unsafe { raw.data.keyboard };
                    seen(k.ExtraInformation == TAG32, false);
                }
            }
        }
        unsafe { DefWindowProcW(h, msg, w, l) }
    }

    unsafe {
        let module = GetModuleHandleW(None).ok().map(|m| m.into()).unwrap_or_default();
        let class = WNDCLASSW { lpfnWndProc: Some(on_message), hInstance: module, lpszClassName: w!("AsctrlPresence"), ..Default::default() };
        RegisterClassW(&class);
        // A message-only window: never shown, only there to be sent the copies
        let Ok(win) = CreateWindowExW(Default::default(), w!("AsctrlPresence"), w!(""), Default::default(), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(module), None) else { return };
        // INPUTSINK: the copies come whichever window is in front
        let devices = [
            RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x02, dwFlags: RIDEV_INPUTSINK, hwndTarget: win },
            RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x06, dwFlags: RIDEV_INPUTSINK, hwndTarget: win },
        ];
        if RegisterRawInputDevices(&devices, size_of::<RAWINPUTDEVICE>() as u32).is_err() {
            return;
        }
        LIVE.store(true, Ordering::SeqCst);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(not(windows))]
fn hook_thread() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_input_without_the_mark_counts_and_not_the_pointer_we_just_moved() {
        LIVE.store(true, Ordering::SeqCst);
        OTHERS.store(0, Ordering::SeqCst);
        seen(true, false);
        assert_eq!(others_ago(), None, "our own input");
        ours();
        seen(false, true);
        assert_eq!(others_ago(), None, "a move right after ours is the pointer we moved");
        seen(false, false);
        assert!(others_ago().is_some_and(|a| a < Duration::from_millis(50)), "a key or click without the mark is someone else's");
        LIVE.store(false, Ordering::SeqCst);
        assert_eq!(others_ago(), None, "not watched: nobody to wait for");
    }
}
