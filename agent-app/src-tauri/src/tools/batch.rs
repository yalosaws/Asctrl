//! batch: the steps of a plan, run here in one go so no step waits on a round trip.
//!
//! Pace. An input normally waits until the screen has reacted and stopped changing, so the next step sees
//! what it did; on a screen that animates, that wait is most of the time spent. `pace`:
//! - "normal": every input waits.
//! - "fast": no input waits, only for the program to take it; the screen settles once before anything that
//!   reads it (a wait, a look) and at the end.
//! - "auto" (default): a step marked with a template number `k` (the same step of a plan repeated over rows
//!   of data) runs at the normal pace the first time; if that brought no window up and nothing else to the
//!   front, its later runs go fast. Anything unexpected in a fast run (a window, a popup, a failure) puts
//!   the rest of the batch back to the normal pace.
//!
//! A step that names text fitting several controls or none is recovered here first where that needs no
//! judgment (see `recover`): the plan's `on` rules, a moment's wait, and the candidates for the runner to
//! choose among. A step recovered here lets the batch go on.
//!
//! Each step's result comes back in `data` (`steps`: ok, text, fast, and the step's own data such as
//! `effect` or `miss`; `notes` when it was recovered), so the plan's runner can carry on from a step that failed
//! without the rest of the batch being sent again. A batch that failed also says where things stand
//! (`where`: the failed step's window as observe reads it), so the runner can report without looking again.

use super::*;
use std::collections::{HashMap, HashSet};

/// Tools a batch may run as steps: looking and acting, nothing that changes files or the system
const BATCHABLE: [&str; 16] = [
    "watch", "click", "type_text", "press_keys", "scroll", "drag", "mouse", "set_value", "element_action", "focus_window", "open_app", "wait_for",
    "clipboard", "read_text", "observe", "read_grid",
];
/// Steps that put input in, the ones the pace applies to
const INPUTS: [&str; 8] = ["click", "type_text", "press_keys", "scroll", "drag", "mouse", "set_value", "element_action"];
/// Steps whose reply is the point of them: kept long
const READS: [&str; 5] = ["read_text", "observe", "clipboard", "read_grid", "watch"];
const MAX_STEPS: usize = 2000;

#[derive(Clone, Copy, PartialEq)]
enum Pace {
    Auto,
    Fast,
    Normal,
}

/// What is in front and which windows are up: an input that changed either brought something up
fn windows_now() -> (isize, HashSet<isize>) {
    (winmgr::foreground(), winmgr::list().into_iter().map(|w| w.handle).collect())
}

/// Let the screen come to rest after inputs that didn't wait for it (in `area`, else the window in front)
async fn settle(area: Option<capture::Area>) -> (bool, u128) {
    let t = std::time::Instant::now();
    let calm = tokio::task::spawn_blocking(move || {
        // In a framed region nothing is written off as moving on its own
        let noise = if area.is_some() { Duration::MAX } else { screen::NOISE_QUICK };
        screen::wait_stable(area.unwrap_or_else(|| screen::watch_area(None)), Duration::from_millis(150), Duration::from_millis(1500), noise)
    })
    .await
    .unwrap_or(true);
    (calm, t.elapsed().as_millis())
}

/// Run one step the way the batch does, with its own animation on the overlay
async fn run_step(ctx: &Ctx, tool: &str, step: &Value, id: String, fast: bool) -> Output {
    let geo = fx_before(ctx, tool, step);
    fx::emit(serde_json::json!({ "phase": "start", "id": id, "tool": tool, "args": step, "geo": geo }));
    let out = STEP.scope(true, FAST.scope(fast, fx::CALL_ID.scope(id.clone(), run_tool(ctx, tool, step)))).await;
    let after = fx_after(ctx, tool, &geo, &out);
    let t: String = out.body().chars().take(6000).collect();
    fx::emit(serde_json::json!({ "phase": "end", "id": id, "tool": tool, "ok": out.ok, "quiet": probe_missed(step, &out), "text": t, "geo": after }));
    out
}

/// Windows that came up, went away or came to the front, as `{ms, kind, handle, program, title}` (ms from `t0`);
/// a title changing is left out, and at most the last 30
fn window_events(evs: &[crate::events::Event], t0: std::time::Instant) -> Value {
    use crate::events::Kind;
    let keep: Vec<&crate::events::Event> = evs.iter().filter(|e| e.kind != Kind::Renamed && !e.title.trim().is_empty()).collect();
    let from = keep.len().saturating_sub(30);
    Value::Array(
        keep[from..]
            .iter()
            .map(|e| {
                serde_json::json!({
                    "ms": e.at.saturating_duration_since(t0).as_millis() as u64,
                    "kind": e.kind.as_str(),
                    "handle": e.hwnd,
                    "program": winmgr::process_name(e.pid),
                    "title": e.title,
                })
            })
            .collect(),
    )
}

/// The plan's popup rules: { "text a popup shows": "what to press" }
fn on_rules(args: &Value) -> Vec<(String, String)> {
    args.get("on")
        .and_then(Value::as_object)
        .map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).filter(|(k, v)| !k.trim().is_empty() && !v.trim().is_empty()).collect())
        .unwrap_or_default()
}

pub(super) async fn batch(ctx: &Ctx, args: &Value) -> Output {
    let Some(steps) = args.get("steps").and_then(Value::as_array) else { return fail("Missing steps") };
    if steps.is_empty() || steps.len() > MAX_STEPS {
        return fail(format!("Give 1 to {MAX_STEPS} steps"));
    }
    let pace = match str_arg(args, "pace") {
        "" | "auto" => Pace::Auto,
        "fast" => Pace::Fast,
        "normal" => Pace::Normal,
        other => return fail(format!("pace: \"{other}\" is not one of auto, fast, normal")),
    };
    for (i, s) in steps.iter().enumerate() {
        let tool = str_arg(s, "tool");
        if !BATCHABLE.contains(&tool) {
            return fail(format!("Step {}: \"{tool}\" can't run in a batch; allowed: {}", i + 1, BATCHABLE.join(", ")));
        }
    }
    let on = on_rules(args);
    let seq0 = crate::events::seq();
    // The plan starts with this batch: as plan_mark would, without a call of its own
    if let Some(m) = args.get("mark").filter(|m| m.is_object()) {
        popups::plan_mark(m);
    }
    let area = WATCH.try_with(|w| *w).ok();
    let call = fx::call_id();
    let started = std::time::Instant::now();
    let mut results = vec![];
    let mut failed: Option<usize> = None;
    // Template step -> whether it can go fast (it brought nothing up when it ran at the normal pace)
    let mut learned: HashMap<u64, bool> = HashMap::new();
    // Something unexpected happened in a fast run: the normal pace from here on
    let mut careful = pace == Pace::Normal;
    // Inputs went in without the screen being let to settle
    let mut unsettled = false;
    let mut fast_count = 0usize;
    // The step before went in fast: the screen may not show what it did yet
    let mut prev_fast = false;
    // One reading of the window for looking ahead (locate::peek)
    let peeked = Arc::new(Mutex::new(None));
    for (i, step) in steps.iter().enumerate() {
        let tool = str_arg(step, "tool");
        let k = step.get("k").and_then(Value::as_u64);
        // A popup being dismissed goes first: this step must not act on the screen it covers
        popups::clear(Duration::from_secs(2)).await;
        let input = INPUTS.contains(&tool);
        let fast = input
            && match pace {
                Pace::Fast => !careful,
                Pace::Normal => false,
                Pace::Auto => !careful && k.is_some_and(|k| learned.get(&k) == Some(&true)),
            };
        // A step that waits or reads sees the screen at rest
        if !fast && unsettled {
            settle(area).await;
            unsettled = false;
        }
        // While this step runs, find where the next click will land, so its pointer can be on its way before
        // the click happens rather than setting off once it already has. On the side: nothing waits for it.
        if let Some(next) = steps.get(i + 1).filter(|s| str_arg(s, "tool") == "click")
            && let Ok(Some(ask)) = locate::Ask::of(ctx, next)
        {
            let (id, cache) = (format!("{call}.{}.a", i + 1), peeked.clone());
            tokio::task::spawn_blocking(move || {
                if let Some(p) = locate::peek(&ask, &cache) {
                    fx::emit(serde_json::json!({ "phase": "plan", "id": id, "tool": "click", "geo": { "at": fx::pt_json(p) } }));
                }
            });
        }
        let before = if input { Some(windows_now()) } else { None };
        // Each step animates on its own
        let id = format!("{call}.{i}");
        let mut out = run_step(ctx, tool, step, id.clone(), fast).await;
        let mut notes: Vec<String> = vec![];
        if !out.ok
            && let Some(wish) = recover::Wish::of(step)
        {
            let settled = i == 0 || !prev_fast || str_arg(&steps[i - 1], "tool") == "wait_for";
            let mut n = 0;
            let again = |s: Value| {
                n += 1;
                let rid = format!("{id}.r{n}");
                async move { run_step(ctx, tool, &s, rid, false).await }
            };
            let r = recover::recover(ctx, step, &wish, &on, settled, out, again).await;
            out = r.out;
            notes = r.notes;
        }
        // Did it bring anything up? (A popup the watcher is on counts: it was not the step's doing, but the
        // screen is not what the next step expects until it is gone.)
        if let Some((fg0, wins0)) = before {
            let (fg, wins) = windows_now();
            let quiet = out.ok && fg == fg0 && wins.is_subset(&wins0) && !popups::busy();
            if fast {
                fast_count += 1;
                unsettled = true;
                if !quiet {
                    careful = true;
                    if let Some(k) = k {
                        learned.insert(k, false);
                    }
                }
            } else if let Some(k) = k {
                // Learnt the first time; anything unquiet later makes it careful for good
                let e = learned.entry(k).or_insert(quiet);
                *e &= quiet;
            }
        }
        prev_fast = fast;
        let room = if READS.contains(&tool) { 6000 } else { 600 };
        let body: String = out.body().chars().take(room).collect();
        let mut r = serde_json::json!({ "ok": out.ok, "text": body, "fast": fast });
        if let Some(d) = out.data.take() {
            r["data"] = d;
        }
        if !notes.is_empty() {
            r["notes"] = serde_json::json!(notes);
        }
        results.push(r);
        if !out.ok {
            failed = Some(i);
            break;
        }
    }
    let mut tail = String::new();
    if unsettled {
        let (calm, ms) = settle(area).await;
        tail = if calm { format!("; screen settled {ms} ms after the last step") } else { format!("; screen still changing {ms} ms after the last step") };
    }
    let secs = started.elapsed().as_secs_f64();
    let fast_note = if fast_count > 0 { format!(", {fast_count} fast") } else { String::new() };
    let head = match failed {
        None => format!("All {} steps done in {secs:.1} s{fast_note}{tail}", steps.len()),
        Some(i) => format!("Step {} failed after {secs:.1} s{fast_note}; the rest were not run", i + 1),
    };
    let lines: Vec<String> = results
        .iter()
        .enumerate()
        .map(|(i, r)| format!("{}. {} {}: {}", i + 1, if r["ok"] == true { "✓" } else { "✗" }, str_arg(&steps[i], "tool"), r["text"].as_str().unwrap_or("")))
        .collect();
    let body = format!("{head}\n{}", lines.join("\n"));
    let mut data = serde_json::json!({ "steps": results, "failed": failed });
    // What happened to the windows meanwhile, from the timeline
    if let Some(evs) = crate::events::since(seq0) {
        data["windows"] = tokio::task::spawn_blocking(move || window_events(&evs, started)).await.unwrap_or_default();
    }
    // The plan ends with this batch, if it went through (one that stopped is carried on by the runner)
    if failed.is_none()
        && let Some(plan) = args.get("end").and_then(Value::as_str)
    {
        data["ended"] = serde_json::json!(popups::plan_mark(&serde_json::json!({ "plan": plan, "end": true })));
    }
    // Where things stand, so whoever re-plans needn't look again: the failed step's window
    if let Some(i) = failed.filter(|_| bool_arg(args, "where") != Some(false)) {
        let mut o = serde_json::json!({ "max": 40 });
        if let Some(w) = steps[i].get("window") {
            o["window"] = w.clone();
        }
        let seen = look::observe(ctx, &o).await;
        if seen.ok {
            data["where"] = serde_json::json!(seen.body().lines().take(45).collect::<Vec<_>>().join("\n"));
        }
    }
    let data = Some(data);
    match failed {
        None => Output { ok: true, text: Some(body), data, ..Default::default() },
        Some(_) => Output { ok: false, error: Some(body), data, ..Default::default() },
    }
}
