//! Keeps real clicks in step with the overlay. Before a step acts it waits for its virtual cursor to land;
//! right after it acts the overlay is told, so the cursor presses at the moment the click really happens.

use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;

/// How long a step waits for its cursor. If the overlay never answers (no window, nothing to fly to),
/// the first timeout turns pacing off for the rest of that call, so a long sequence is not held up
const LAND_TIMEOUT: Duration = Duration::from_millis(2500);

static APP: OnceLock<AppHandle> = OnceLock::new();
static CALLS: OnceLock<Mutex<HashMap<String, Arc<Call>>>> = OnceLock::new();

tokio::task_local! {
    /// The id of the tool call this task is running
    pub static CALL_ID: String;
}

fn calls() -> &'static Mutex<HashMap<String, Arc<Call>>> {
    CALLS.get_or_init(Default::default)
}

pub fn init(app: AppHandle) {
    let _ = APP.set(app);
}

/// Register a call before its start event goes out, so no landing can be missed
pub fn begin(id: &str) {
    calls().lock().unwrap().insert(id.to_string(), Arc::new(Call { id: id.to_string(), ..Default::default() }));
}

pub fn end(id: &str) {
    calls().lock().unwrap().remove(id);
}

/// The call the current task is running, if it is paced
pub fn current() -> Option<Arc<Call>> {
    let id = CALL_ID.try_with(Clone::clone).ok()?;
    calls().lock().unwrap().get(&id).cloned()
}

/// The overlay says the cursor for step `index` of call `id` has landed
#[tauri::command]
pub fn fx_landed(id: String, index: usize) {
    if let Some(c) = calls().lock().unwrap().get(&id) {
        c.landed.lock().unwrap().insert(index);
        c.notify.notify_waiters();
    }
}

#[derive(Default)]
pub struct Call {
    id: String,
    landed: Mutex<HashSet<usize>>,
    notify: Notify,
    dead: AtomicBool,
}

impl Call {
    /// Wait until the cursor for step `i` is over its target
    pub async fn wait(&self, i: usize) {
        if self.dead.load(Ordering::Relaxed) {
            return;
        }
        let deadline = tokio::time::Instant::now() + LAND_TIMEOUT;
        loop {
            let n = self.notify.notified();
            tokio::pin!(n);
            n.as_mut().enable();
            if self.landed.lock().unwrap().contains(&i) {
                return;
            }
            if tokio::time::timeout_at(deadline, n).await.is_err() {
                self.dead.store(true, Ordering::Relaxed);
                return;
            }
        }
    }

    /// Step `i` has just acted: the overlay presses its cursor now
    pub fn step(&self, i: usize, ok: bool) {
        if let Some(app) = APP.get() {
            let _ = app.emit("fx", json!({ "phase": "step", "id": self.id, "index": i, "ok": ok }));
        }
    }
}
