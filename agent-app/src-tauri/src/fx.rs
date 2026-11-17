//! Tells the overlay what the AI is doing, so the person at the screen can follow along: where a click lands,
//! which window is being worked in, what a command printed. Nothing here ever holds an action back: the
//! drawing follows the action, never the other way round.

use serde_json::{Value, json};
use std::sync::OnceLock;
use tauri::{AppHandle, Emitter};

static APP: OnceLock<AppHandle> = OnceLock::new();

tokio::task_local! {
    /// The id of the tool call this task is running
    pub static CALL_ID: String;
}

pub fn init(app: AppHandle) {
    let _ = APP.set(app);
}

/// ASCTRL_FX_LOG=<file>: every animation event is also appended there, one JSON line each with the ms since
/// the driver started, to see what the overlay was told when its drawing looks wrong
static FX_LOG: OnceLock<Option<(std::sync::Mutex<std::fs::File>, std::time::Instant)>> = OnceLock::new();

/// Write `v` to the animation log, if there is one
pub fn note(v: &Value) {
    if let Some((f, t0)) = FX_LOG.get_or_init(|| {
        let p = std::env::var_os("ASCTRL_FX_LOG")?;
        let f = std::fs::OpenOptions::new().create(true).append(true).open(p).ok()?;
        Some((std::sync::Mutex::new(f), std::time::Instant::now()))
    }) {
        use std::io::Write;
        let _ = writeln!(f.lock().unwrap(), "{} {}", t0.elapsed().as_millis(), v);
    }
}

/// Send an animation event to the overlay
pub fn emit(v: Value) {
    note(&v);
    if let Some(app) = APP.get() {
        let _ = app.emit("fx", v);
    }
}

/// The id of the call the current task is running ("" outside a call)
pub fn call_id() -> String {
    CALL_ID.try_with(Clone::clone).unwrap_or_default()
}

pub fn rect_json(r: (i32, i32, i32, i32)) -> Value {
    json!([r.0, r.1, r.2 - r.0, r.3 - r.1])
}

pub fn pt_json(p: (i32, i32)) -> Value {
    json!([p.0, p.1])
}

/// One animation on the overlay: started when the action is about to happen, stepped the moment it does,
/// ended with its outcome. Usable from any thread.
pub struct Fx {
    id: String,
    tool: &'static str,
}

impl Fx {
    /// `call` is the tool call's id; `part` tells this animation from others of the same call
    pub fn start(call: &str, part: &str, tool: &'static str, args: Value, geo: Value) -> Fx {
        let id = format!("{call}.{part}");
        emit(json!({ "phase": "start", "id": id, "tool": tool, "args": args, "geo": geo }));
        Fx { id, tool }
    }

    /// Step `i` has just acted: its virtual cursor presses as soon as it gets there
    pub fn step(&self, i: usize, ok: bool) {
        emit(json!({ "phase": "step", "id": self.id, "tool": self.tool, "index": i, "ok": ok }));
    }

    /// A step that also brings what the overlay needs to draw it, found out after the start went out
    pub fn step_with(&self, i: usize, ok: bool, geo: Value) {
        emit(json!({ "phase": "step", "id": self.id, "tool": self.tool, "index": i, "ok": ok, "geo": geo }));
    }

    pub fn end(self, ok: bool) {
        self.end_with(ok, json!({}));
    }

    /// Ended, with where things stand now (e.g. a scroll's new position)
    pub fn end_with(self, ok: bool, geo: Value) {
        emit(json!({ "phase": "end", "id": self.id, "tool": self.tool, "ok": ok, "text": "", "geo": geo }));
    }
}

/// The area a click framed is gone (it was a popup the click closed, like a browser's translate bubble): the
/// overlay lets the frame go now rather than after its usual linger. `zone` is the one sent at the start.
pub fn zone_gone(call: &str, zone: Value) {
    zone_left(call, zone, false);
}

/// The frame no longer surrounds its area: the window it was drawn round was closed or hidden (`moved` false:
/// it has already faded itself out, so the frame goes at once), or it moved or changed size (`moved` true: the
/// frame fades, since the window is still there to look at)
fn zone_left(call: &str, zone: Value, moved: bool) {
    if call.is_empty() || zone.is_null() {
        return;
    }
    emit(json!({ "phase": "step", "id": format!("{call}.gone"), "tool": "zone", "index": 0, "ok": true, "geo": { "zone": zone, "moved": moved } }));
}

/// How long the overlay can keep a work-area frame up after the last action in it (fx.ts unframeLater), and a
/// little more: as long as the frame can be on screen, it must not outlive or lag behind its window
const ZONE_LINGER: std::time::Duration = std::time::Duration::from_millis(8500);

/// Only the frame drawn last is watched: a newer one replaces it on the overlay
static ZONE_WATCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Watch the window an action was framed in for as long as the frame can stay up, and let the frame go the
/// moment the window closes, hides or minimizes (a popup's close button, a dialog's OK) or moves or changes size
/// (maximize, restore, a drag): a frame left round where a window used to be is worse than none. Polls on its own
/// thread, so the action's reply never waits.
pub fn zone_follows_window(call: &str, hwnd: isize, zone: Value) {
    if call.is_empty() || zone.is_null() {
        return;
    }
    let call = call.to_string();
    let me = ZONE_WATCH.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    let start_rect = crate::winmgr::rect_of(hwnd);
    std::thread::spawn(move || {
        let start = std::time::Instant::now();
        while start.elapsed() < ZONE_LINGER {
            if ZONE_WATCH.load(std::sync::atomic::Ordering::SeqCst) != me {
                return;
            }
            match crate::winmgr::rect_of(hwnd) {
                // Hidden counts: a popup hides its window (after fading itself out) a moment before destroying it
                None => return zone_left(&call, zone, false),
                Some(r) if Some(r) != start_rect => return zone_left(&call, zone, true),
                _ => {}
            }
            // Popups close right after the click: watch closely at first, then at a gentler pace
            let pace = if start.elapsed() < std::time::Duration::from_millis(1500) { 15 } else { 60 };
            std::thread::sleep(std::time::Duration::from_millis(pace));
        }
    });
}

/// A click-style animation at a screen point: the window it lands in is framed, a cursor flies there and
/// presses. Returns right away.
pub fn click(call: &str, part: &str, at: (i32, i32), count: u32, ok: bool) {
    let win = crate::winmgr::top_window_at(at.0, at.1);
    let zone = json!(win.and_then(crate::winmgr::rect_of).map(rect_json));
    let fx = Fx::start(call, part, "click", json!({ "count": count }), json!({ "at": pt_json(at), "zone": zone, "zoneIsWindow": true, "win": zone }));
    fx.step(0, ok);
    fx.end(ok);
    if let Some(w) = win {
        zone_follows_window(call, w, zone);
    }
}
