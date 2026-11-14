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

/// Send an animation event to the overlay
pub fn emit(v: Value) {
    if let Some(app) = APP.get() {
        let _ = app.emit("fx", v);
    }
}

/// The id of the call the current task is running ("" outside a call)
pub fn call_id() -> String {
    CALL_ID.try_with(Clone::clone).unwrap_or_default()
}

/// The overlay used to report when a virtual cursor landed so the real click could wait for it.
/// Clicks no longer wait; the command stays so an overlay that still calls it gets an answer.
#[tauri::command]
pub fn fx_landed(_id: String, _index: usize) {}

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
    if call.is_empty() || zone.is_null() {
        return;
    }
    emit(json!({ "phase": "step", "id": format!("{call}.gone"), "tool": "zone", "index": 0, "ok": true, "geo": { "zone": zone } }));
}

/// Watch the window a click was framed in for a moment after the click, and the moment it closes (a popup's
/// close button, a dialog's OK) let the frame go with it. Polls on its own thread, so the action's reply never waits.
pub fn zone_gone_when_closed(call: &str, hwnd: isize, zone: Value) {
    if call.is_empty() || zone.is_null() {
        return;
    }
    let call = call.to_string();
    std::thread::spawn(move || {
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(1500) {
            // Hidden counts: a popup hides its window (after fading itself out) a moment before destroying it
            if crate::winmgr::rect_of(hwnd).is_none() {
                zone_gone(&call, zone);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
    });
}

/// A click-style animation at a screen point: the window it lands in is framed, a cursor flies there and
/// presses. Returns right away.
pub fn click(call: &str, part: &str, at: (i32, i32), count: u32, ok: bool) {
    let zone = crate::winmgr::top_window_at(at.0, at.1).and_then(crate::winmgr::rect_of).map(rect_json);
    let fx = Fx::start(call, part, "click", json!({ "count": count }), json!({ "at": pt_json(at), "zone": zone, "zoneIsWindow": true }));
    fx.step(0, ok);
    fx.end(ok);
}
