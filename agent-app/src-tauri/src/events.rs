//! The timeline: what happens to the desktop's windows, as it happens. Windows tells us (WinEvent hooks, out of
//! context) when a top-level window is shown, hidden or destroyed, comes to the front, is minimized or restored,
//! or changes its title; each becomes an event with a sequence number. Instead of looking every so often whether
//! something has changed, a wait asks for the events after the last one it saw and sleeps until there are some.
//!
//! It is the first part of the driver's own picture of the computer (see docs/architecture-v2.md, L0): kept up
//! by the computer telling us, not by looking again. What it can't tell (the inside of a window, the pixels) is
//! still read the usual way. `ASCTRL_NO_EVENTS=1` turns it off, and every user falls back to looking every so
//! often, as before.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// What happened to a window
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Shown,
    Hidden,
    Destroyed,
    Front,
    Minimized,
    Restored,
    Renamed,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Shown => "shown",
            Kind::Hidden => "hidden",
            Kind::Destroyed => "destroyed",
            Kind::Front => "front",
            Kind::Minimized => "minimized",
            Kind::Restored => "restored",
            Kind::Renamed => "renamed",
        }
    }

    /// Something may have come up: a window shown, or brought to the front
    pub fn arrival(self) -> bool {
        matches!(self, Kind::Shown | Kind::Front | Kind::Restored)
    }
}

#[derive(Clone, Debug)]
pub struct Event {
    pub seq: u64,
    pub at: Instant,
    pub kind: Kind,
    pub hwnd: isize,
    /// Its title at the time (for a window hidden or destroyed: the last one it was seen with)
    pub title: String,
    pub pid: u32,
}

/// Events kept: older ones are dropped (whoever is that far behind looks at the windows instead)
const KEEP: usize = 4096;

struct Timeline {
    /// Sequence number of the latest event (0: none yet)
    seq: u64,
    events: VecDeque<Event>,
    /// The title each window was last seen with, to name it once it is gone
    titles: Option<std::collections::HashMap<isize, String>>,
}

impl Timeline {
    const fn new() -> Self {
        Timeline { seq: 0, events: VecDeque::new(), titles: None }
    }

    fn push(&mut self, kind: Kind, hwnd: isize, at: Instant, title: Option<String>, pid: u32) {
        self.seq += 1;
        if self.events.len() == KEEP {
            self.events.pop_front();
        }
        let titles = self.titles.get_or_insert_with(Default::default);
        let title = match title {
            Some(t) => {
                titles.insert(hwnd, t.clone());
                t
            }
            None => titles.get(&hwnd).cloned().unwrap_or_default(),
        };
        if kind == Kind::Destroyed {
            titles.remove(&hwnd);
        }
        if titles.len() > KEEP {
            titles.clear();
        }
        self.events.push_back(Event { seq: self.seq, at, kind, hwnd, title, pid });
    }

    /// The events after `seq`, oldest first; None when some of them were already dropped
    fn since(&self, seq: u64) -> Option<Vec<Event>> {
        let first = self.events.front().map_or(self.seq + 1, |e| e.seq);
        if seq + 1 < first {
            return None;
        }
        Some(self.events.iter().filter(|e| e.seq > seq).cloned().collect())
    }
}

static TIMELINE: Mutex<Timeline> = Mutex::new(Timeline::new());
static ARRIVED: Condvar = Condvar::new();
/// Set once the hooks are in: until then (or when turned off) nobody may count on being woken
static LIVE: OnceLock<bool> = OnceLock::new();
/// For async waits (the popup watcher)
static NOTIFY: tokio::sync::Notify = tokio::sync::Notify::const_new();

fn record(kind: Kind, hwnd: isize, title: Option<String>, pid: u32) {
    TIMELINE.lock().unwrap().push(kind, hwnd, Instant::now(), title, pid);
    ARRIVED.notify_all();
    NOTIFY.notify_waiters();
}

/// Whether events are coming in: when not, waits time out as polls did
pub fn live() -> bool {
    LIVE.get().copied().unwrap_or(false)
}

/// The latest event's number: pass it to `since` or `wait` later
pub fn seq() -> u64 {
    TIMELINE.lock().unwrap().seq
}

/// The events after `seq`, oldest first; None when too many have happened since to say (or events are off)
pub fn since(seq: u64) -> Option<Vec<Event>> {
    if !live() {
        return None;
    }
    TIMELINE.lock().unwrap().since(seq)
}

/// Block until there is an event after `seq`, or `max` has passed. Without events, simply sleeps `max`, which
/// is what a poll did.
pub fn wait(seq: u64, max: Duration) {
    if !live() {
        std::thread::sleep(max);
        return;
    }
    let t = TIMELINE.lock().unwrap();
    let _ = ARRIVED.wait_timeout_while(t, max, |t| t.seq <= seq);
}

/// The same for async code
pub async fn wait_async(seq: u64, max: Duration) {
    if !live() {
        tokio::time::sleep(max).await;
        return;
    }
    let n = NOTIFY.notified();
    tokio::pin!(n);
    n.as_mut().enable();
    if self::seq() > seq {
        return;
    }
    let _ = tokio::time::timeout(max, n).await;
}

/// Put the hooks in, on a thread of their own that pumps their messages. Once; harmless to call again.
pub fn start() {
    if LIVE.get().is_some() {
        return;
    }
    if std::env::var("ASCTRL_NO_EVENTS").is_ok_and(|v| v == "1") {
        let _ = LIVE.set(false);
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("asctrl-events".into())
        .spawn(move || hook_thread(tx))
        .ok();
    let ok = rx.recv_timeout(Duration::from_secs(2)).unwrap_or(false);
    let _ = LIVE.set(ok);
}

#[cfg(windows)]
fn hook_thread(ready: std::sync::mpsc::Sender<bool>) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
    use windows::Win32::UI::WindowsAndMessaging::*;

    unsafe extern "system" fn on_event(_: HWINEVENTHOOK, event: u32, hwnd: HWND, object: i32, child: i32, _: u32, _: u32) {
        // The window itself, not something inside it (a caret, a scroll bar, a list item)
        if hwnd.is_invalid() || object != OBJID_WINDOW.0 || child != CHILDID_SELF as i32 {
            return;
        }
        // Top-level windows only: what is inside a window is read from the window
        if unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd {
            return;
        }
        let kind = match event {
            EVENT_OBJECT_SHOW => Kind::Shown,
            EVENT_OBJECT_HIDE => Kind::Hidden,
            EVENT_OBJECT_DESTROY => Kind::Destroyed,
            EVENT_SYSTEM_FOREGROUND => Kind::Front,
            EVENT_SYSTEM_MINIMIZESTART => Kind::Minimized,
            EVENT_SYSTEM_MINIMIZEEND => Kind::Restored,
            EVENT_OBJECT_NAMECHANGE => Kind::Renamed,
            _ => return,
        };
        // Asked without sending the window a message, so a program that hangs can't hold the hook up
        let title = match kind {
            Kind::Hidden | Kind::Destroyed => None,
            _ => {
                let mut buf = [0u16; 256];
                let n = unsafe { InternalGetWindowText(hwnd, &mut buf) }.max(0) as usize;
                Some(String::from_utf16_lossy(&buf[..n]))
            }
        };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        record(kind, hwnd.0 as isize, title, pid);
    }

    let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
    let hooks = unsafe {
        [
            SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, Some(on_event), 0, 0, flags),
            SetWinEventHook(EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND, None, Some(on_event), 0, 0, flags),
            // DESTROY, SHOW, HIDE are 0x8001-0x8003
            SetWinEventHook(EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, None, Some(on_event), 0, 0, flags),
            SetWinEventHook(EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE, None, Some(on_event), 0, 0, flags),
        ]
    };
    let ok = hooks.iter().all(|h| !h.is_invalid());
    let _ = ready.send(ok);
    if !ok {
        return;
    }
    // Out-of-context hooks are delivered through this thread's message queue
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(not(windows))]
fn hook_thread(ready: std::sync::mpsc::Sender<bool>) {
    let _ = ready.send(false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_after_a_number_come_oldest_first_until_too_old() {
        let mut t = Timeline::new();
        let now = Instant::now();
        for i in 0..5 {
            t.push(Kind::Shown, i, now, Some(format!("w{i}")), 1);
        }
        // Gone: named as it was last seen
        t.push(Kind::Destroyed, 3, now, None, 1);
        assert_eq!(t.events.back().unwrap().title, "w3");
        t.push(Kind::Hidden, 3, now, None, 1);
        assert_eq!(t.events.back().unwrap().title, "", "forgotten once destroyed");
        let got = t.since(3).unwrap();
        assert_eq!(got.iter().map(|e| e.hwnd).collect::<Vec<_>>(), vec![3, 4, 3, 3]);
        assert!(t.since(7).unwrap().is_empty());
        for i in 0..KEEP as isize {
            t.push(Kind::Front, i, now, None, 1);
        }
        // The first few were dropped: from before them nothing can be said
        assert!(t.since(2).is_none());
        assert_eq!(t.since(t.seq - 1).unwrap().len(), 1);
    }

    #[test]
    fn arrivals_are_what_brings_something_up() {
        assert!(Kind::Shown.arrival() && Kind::Front.arrival() && Kind::Restored.arrival());
        assert!(!Kind::Hidden.arrival() && !Kind::Renamed.arrival() && !Kind::Destroyed.arrival());
    }
}
