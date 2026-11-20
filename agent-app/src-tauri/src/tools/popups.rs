//! Popups that turn up while a plan runs. While a plan is marked as running, a watcher looks every
//! `POLL` for windows that were not there before. A new window that shows none of the text the plan works
//! with, and has a button the plan said to press (`on`) or a button that only dismisses (`Not now`, `关闭`),
//! is dismissed: the press waits its turn among the plan's actions (the same lock every action takes), so
//! it goes in between two steps, never in the middle of one, and the plan's next step sees the screen
//! without it. On the overlay it is a pointer of its own.
//!
//! Nothing is pressed that could do more than dismiss: no OK, Cancel or Yes (they answer a question the
//! plan may have asked), nothing destructive, and nothing in a window that shows what the plan works with
//! (that window is the plan's own, e.g. a Save As box it opened).

use super::*;
use crate::uia::flag;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const POLL: Duration = Duration::from_millis(60);
/// With the timeline: a look this often anyway, for what it didn't tell
const IDLE: Duration = Duration::from_millis(250);
/// A window that just came up is given this long to put its buttons in before it is judged (once)
const GRACE: Duration = Duration::from_millis(40);

/// Buttons that do nothing but make a popup go away
const DISMISS: [&str; 24] = [
    "not now", "no thanks", "no, thanks", "later", "maybe later", "remind me later", "dismiss", "skip", "got it", "close", "×", "✕",
    "以后再说", "稍后", "稍后再说", "暂不", "暂不需要", "不用了", "跳过", "知道了", "我知道了", "关闭", "下次再说", "残忍拒绝",
];

/// Never pressed on the watcher's own judgment, whatever the rules say
const DESTRUCTIVE: [&str; 22] = [
    "delete", "remove", "erase", "discard", "send", "submit", "purchase", "buy", "pay", "replace", "overwrite", "format", "uninstall",
    "删除", "移除", "清空", "丢弃", "发送", "提交", "购买", "支付", "卸载",
];

struct Watch {
    revision: u64,
    /// The plan's own: {text a popup shows: the button to press}
    on: Vec<(String, String)>,
    /// Text the plan works with: a window showing any of it belongs to the plan
    keep: Vec<String>,
    /// Windows already looked at (there at the start, or judged since)
    known: HashSet<isize>,
    /// What was done, for the plan's report
    log: Vec<String>,
}

static WATCHES: Mutex<std::collections::BTreeMap<String, Watch>> = Mutex::new(std::collections::BTreeMap::new());
static WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();
/// A popup is being judged or dismissed: the plan's next step waits for it (see `clear`)
static BUSY: AtomicBool = AtomicBool::new(false);
/// Distinguishes a current rule set from a stale decision awaiting the input lock.
static GEN: AtomicU64 = AtomicU64::new(0);

fn lower(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(Value::as_str).map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
}

/// A plan (run) starts or ends. To the overlay it is one piece of work, so the pauses between its steps (a
/// decision being made) are not taken for the AI thinking; while it runs, popups that turn up are dismissed in
/// between its steps. `args`: { plan, on, keep } to start, { plan, end: true } to end. At its end: what the
/// watcher did, one line each.
pub fn plan_mark(args: &Value) -> String {
    let plan = args.get("plan").and_then(Value::as_str).unwrap_or("plan").to_string();
    let end = args.get("end").and_then(Value::as_bool).unwrap_or(false);
    fx::emit(serde_json::json!({ "phase": if end { "end" } else { "start" }, "id": format!("plan-{plan}"), "tool": "plan", "ok": true, "args": {}, "geo": {} }));
    if end {
        stop(&plan)
    } else {
        start(args);
        String::new()
    }
}

/// A plan starts: watch for popups until `stop`. `args`: { plan, on: {text: press}, keep: [text] }
pub fn start(args: &Value) {
    let on = args
        .get("on")
        .and_then(Value::as_object)
        .map(|m| m.iter().filter_map(|(k, v)| Some((k.trim().to_lowercase(), v.as_str()?.trim().to_string()))).collect())
        .unwrap_or_default();
    let known = winmgr::list().into_iter().map(|w| w.handle).collect();
    let plan = str_arg(args, "plan").to_string();
    let revision = GEN.fetch_add(1, Ordering::SeqCst);
    let mut watches = WATCHES.lock().unwrap();
    let watch = watches.entry(plan).or_insert_with(|| Watch { revision, on: vec![], keep: vec![], known, log: vec![] });
    watch.revision = revision;
    watch.on = on;
    watch.keep = lower(args.get("keep").unwrap_or(&Value::Null));
    drop(watches);
    static START: std::sync::Once = std::sync::Once::new();
    START.call_once(|| { tauri::async_runtime::spawn(watch_popups()); });
    WAKE.notify_one();
}

async fn watch_popups() {
    loop {
        if WATCHES.lock().unwrap().is_empty() {
            WAKE.notified().await;
            continue;
        }
        if crate::events::live() {
            let before = crate::events::seq();
            crate::events::wait_async(before, IDLE).await;
            if crate::events::seq() != before {
                if crate::events::since(before).is_some_and(|events| !events.iter().any(|e| e.kind.arrival())) { continue; }
                tokio::time::sleep(GRACE).await;
            }
        } else {
            tokio::time::sleep(POLL).await;
        }
        if let Ok(Some((owner, revision, found))) = tokio::task::spawn_blocking(look).await
            && let Ok(_turn) = acting().await
        {
            let _ = tokio::task::spawn_blocking(move || {
                let mut watches = WATCHES.lock().unwrap();
                if let Some(w) = watches.get_mut(&owner)
                    && w.revision == revision && winmgr::exists(found.hwnd)
                {
                    w.log.push(dismiss(found));
                }
            }).await;
        }
        BUSY.store(false, Ordering::SeqCst);
    }
}

/// Stop only this plan. An empty name clears every watch during session teardown.
pub fn stop(plan: &str) -> String {
    let mut watches = WATCHES.lock().unwrap();
    if plan.is_empty() {
        watches.clear();
        return String::new();
    }
    watches.remove(plan).map(|w| w.log.join("\n")).unwrap_or_default()
}

/// Wait while a popup is being dealt with (at most `max`), so a step does not act on a screen it covers
pub async fn clear(max: Duration) {
    let t = std::time::Instant::now();
    while BUSY.load(Ordering::SeqCst) && t.elapsed() < max {
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

struct Found {
    hwnd: isize,
    title: String,
    point: (i32, i32),
    /// Pressed directly (UI Automation) unless it is part of a web page
    rf: Option<u32>,
    button: String,
    why: &'static str,
}

/// A new window to dismiss, if there is one. Every new window is judged once.
fn look() -> Option<(String, u64, Found)> {
    let mut all: HashSet<_> = winmgr::list().into_iter().map(|w| w.handle).collect();
    let fg = winmgr::foreground();
    if fg != 0 && winmgr::pid_of(fg) != std::process::id() { all.insert(fg); }
    let mut watches = WATCHES.lock().unwrap();
    let single = watches.len() == 1;
    for hwnd in all {
        let mut chosen = None;
        let mut conflict = false;
        for (owner, watch) in watches.iter_mut() {
            if !watch.known.insert(hwnd) { continue; }
            BUSY.store(true, Ordering::SeqCst);
            if let Some(found) = judge(hwnd, &watch.on, &watch.keep, single) {
                if chosen.is_some() { conflict = true; }
                chosen = Some((owner.clone(), watch.revision, found));
            }
        }
        if chosen.is_some() && !conflict { return chosen; }
    }
    None
}

fn judge(h: isize, on: &[(String, String)], keep: &[String], allow_default: bool) -> Option<Found> {
    let scene = uia::snapshot(h, false).ok()?;
    let title = scene.title.to_lowercase();
    let shows = |t: &str| title.contains(t) || scene.nodes.iter().any(|n| n.name.to_lowercase().contains(t));
    let press_of = |name: &str| {
        let name = name.to_lowercase();
        scene
            .nodes
            .iter()
            .filter(|n| n.has(flag::ACTIONABLE) && n.rect.2 > n.rect.0)
            .find(|n| n.name.trim().to_lowercase() == name)
            .map(|n| Found {
                hwnd: h,
                title: scene.title.clone(),
                point: ((n.rect.0 + n.rect.2) / 2, (n.rect.1 + n.rect.3) / 2),
                rf: (!n.has(flag::WEB)).then_some(n.rf),
                button: n.name.trim().to_string(),
                why: "",
            })
    };
    // The plan's own rules first: it named this popup
    if let Some(f) = on.iter().filter(|(text, _)| shows(text)).find_map(|(_, press)| press_of(press)) {
        return Some(Found { why: "the plan's rule", ..f });
    }
    // A window with what the plan works with is the plan's, not a popup
    if !allow_default || keep.iter().any(|t| shows(t)) {
        return None;
    }
    let f = DISMISS.iter().find_map(|b| press_of(b))?;
    let l = f.button.to_lowercase();
    if DESTRUCTIVE.iter().any(|d| l.contains(d)) {
        return None;
    }
    Some(Found { why: "a popup the plan didn't expect", ..f })
}

fn dismiss(f: Found) -> String {
    let call = format!("popup-{}", GEN.load(Ordering::SeqCst));
    let pressed = match f.rf.map(uia::live) {
        Some(Ok(l)) => l.press().map(|_| ()),
        _ if winmgr::foreground() == f.hwnd => input::click(f.point.0, f.point.1, "", 1, ""),
        _ => Err("Popup is no longer in front".into()),
    };
    let ok = pressed.is_ok();
    fx::click(&call, "a", f.point, 1, ok);
    let title = uia::clip(&f.title, 50);
    match pressed {
        Ok(()) => format!("↷ \"{title}\" popped up ({}): pressed \"{}\"", f.why, f.button),
        Err(e) => format!("↷ \"{title}\" popped up ({}): pressing \"{}\" failed: {e}", f.why, f.button),
    }
}
