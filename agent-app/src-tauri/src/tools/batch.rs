//! batch: several steps in one call

use super::report::Did;
use super::*;

/// Tools a batch may run as steps: looking and acting, nothing that changes files or the system
const BATCHABLE: [&str; 16] = [
    "click", "type_text", "press_keys", "scroll", "drag", "mouse", "set_value", "element_action", "focus_window", "open_app", "wait_for",
    "write_clipboard", "read_clipboard", "find", "read_text", "observe",
];

/// Run several steps in one call, in order, stopping at the first failure. Each step still waits only until
/// the screen settles, but the AI saves a whole round trip (and a turn of thinking) per step, and the
/// outcome is reported once, at the end. With fast, input steps don't wait for the screen at all
/// (animations would hold every step up); the screen settles once at the end.
pub(super) async fn batch(ctx: &Ctx, args: &Value) -> Output {
    let Some(steps) = args.get("steps").and_then(Value::as_array) else { return fail("Missing steps") };
    if steps.is_empty() || steps.len() > 100 {
        return fail("Give 1 to 100 steps");
    }
    let fast = bool_arg(args, "fast").unwrap_or(false);
    let call = fx::call_id();
    let started = std::time::Instant::now();
    let mut lines = vec![];
    let mut failed: Option<usize> = None;
    // One reading of the window for looking ahead (locate::peek)
    let peeked = Arc::new(Mutex::new(None));
    for (i, step) in steps.iter().enumerate() {
        if failed.is_some() {
            lines.push(format!("{}. – skipped", i + 1));
            continue;
        }
        let tool = str_arg(step, "tool");
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
        let out = if !BATCHABLE.contains(&tool) {
            fail(format!("\"{tool}\" can't run in a batch; allowed: {}", BATCHABLE.join(", ")))
        } else {
            // Each step animates on its own
            let id = format!("{call}.{i}");
            let before = fx_before(ctx, tool, step);
            fx::emit(serde_json::json!({ "phase": "start", "id": id, "tool": tool, "args": step, "geo": before }));
            let out = STEP.scope(true, FAST.scope(fast, fx::CALL_ID.scope(id.clone(), run_tool(ctx, tool, step)))).await;
            let after = fx_after(ctx, tool, &before, &out);
            let t: String = out.body().chars().take(6000).collect();
            fx::emit(serde_json::json!({ "phase": "end", "id": id, "tool": tool, "ok": out.ok, "text": t, "geo": after }));
            out
        };
        // What a step read is the point of it; what a step did fits in a line
        let room = if matches!(tool, "find" | "read_text" | "observe" | "read_clipboard") { 6000 } else { 600 };
        let body: String = out.body().chars().take(room).collect();
        lines.push(format!("{}. {} {tool}: {body}", i + 1, if out.ok { "✓" } else { "✗" }));
        if !out.ok {
            failed = Some(i);
        }
    }
    // Fast steps didn't wait for the screen: let it settle once, so the report shows the outcome
    let mut settled = String::new();
    if fast {
        let t = std::time::Instant::now();
        // The batch's watch region when it framed one, else the window in front
        let area = WATCH.try_with(|w| *w).ok();
        let calm = tokio::task::spawn_blocking(move || {
            // In a framed region nothing is written off as moving on its own
            let noise = if area.is_some() { Duration::MAX } else { screen::NOISE_QUICK };
            screen::wait_stable(area.unwrap_or_else(|| screen::watch_area(None)), Duration::from_millis(250), Duration::from_secs(3), noise)
        })
        .await
        .unwrap_or(true);
        let ms = t.elapsed().as_millis();
        settled = if calm { format!(" (fast; screen settled {ms} ms after the last step)") } else { format!(" (fast; screen still changing {ms} ms after the last step)") };
    }
    let secs = started.elapsed().as_secs_f64();
    let head = match failed {
        None => format!("All {} steps done in {secs:.1} s{settled}", steps.len()),
        Some(i) => format!("Step {} failed after {secs:.1} s, so the rest were skipped", i + 1),
    };
    // One report for the whole batch: where things stand now
    let did = Did { done: format!("{head}\n{}", lines.join("\n")), change: None, hwnd: None, fresh: false, delivered: false };
    let mut out = report::conclude(ctx, args, did, true).await;
    if failed.is_some() {
        out.ok = false;
        out.error = out.text.take();
    }
    out
}
