//! batch: several steps in one call

use super::*;

/// Tools a batch may run as steps: actions and checks, nothing that changes files or the system
const BATCHABLE: [&str; 19] = [
    "click", "click_text", "scroll", "drag", "mouse_move", "mouse_down", "mouse_up", "mouse_look", "type_text", "press_keys", "hold_key",
    "element_action", "list_elements", "focus_window", "open_app", "wait_for", "write_clipboard", "find_text", "wait",
];

/// Run several steps in one call, in order, stopping at the first failure. Each step still waits only until
/// the screen settles, but the AI saves a whole round trip (and a turn of thinking) per step. With fast, input
/// steps don't wait for the screen at all (animations would hold every step up); the screen settles once at the end.
pub(super) async fn batch(ctx: &Ctx, args: &Value) -> Output {
    let Some(steps) = args.get("steps").and_then(Value::as_array) else { return fail("Missing steps") };
    if steps.is_empty() || steps.len() > 50 {
        return fail("Give 1 to 50 steps");
    }
    let fast = args.get("fast").and_then(Value::as_bool).unwrap_or(false);
    let call = pace::CALL_ID.try_with(Clone::clone).unwrap_or_default();
    let started = std::time::Instant::now();
    let mut lines = vec![];
    let mut failed: Option<usize> = None;
    for (i, step) in steps.iter().enumerate() {
        if failed.is_some() {
            lines.push(format!("{}. – skipped", i + 1));
            continue;
        }
        let tool = str_arg(step, "tool");
        let out = if !BATCHABLE.contains(&tool) {
            fail(format!("\"{tool}\" can't run in a batch; allowed: {}", BATCHABLE.join(", ")))
        } else if tool == "wait" {
            let ms = (step.get("seconds").and_then(Value::as_f64).unwrap_or(0.5).clamp(0.0, 30.0) * 1000.0) as u64;
            tokio::time::sleep(Duration::from_millis(ms)).await;
            text(format!("Waited {ms} ms"))
        } else {
            // Each step animates on its own; its real input doesn't wait for the animation (an id the pacer doesn't know)
            let id = format!("{call}.{i}");
            let before = fx_before(ctx, tool, step);
            pace::emit(serde_json::json!({ "phase": "start", "id": id, "tool": tool, "args": step, "geo": before }));
            let out = FAST.scope(fast, pace::CALL_ID.scope(id.clone(), run_tool(ctx, tool, step))).await;
            let after = fx_after(ctx, tool, &before);
            let t: String = out.text.as_deref().or(out.error.as_deref()).unwrap_or("").chars().take(6000).collect();
            pace::emit(serde_json::json!({ "phase": "end", "id": id, "tool": tool, "ok": out.ok, "text": t, "geo": after }));
            out
        };
        let body: String = out.text.as_deref().or(out.error.as_deref()).unwrap_or("").chars().take(3000).collect();
        lines.push(format!("{}. {} {tool}: {body}", i + 1, if out.ok { "✓" } else { "✗" }));
        if !out.ok {
            failed = Some(i);
        }
    }
    // Fast steps didn't wait for the screen: let it settle once, so the final screenshot shows the outcome
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
    match failed {
        None => text(format!("All {} steps done in {secs:.1} s{settled}\n{}", steps.len(), lines.join("\n"))),
        Some(i) => Output {
            ok: false,
            error: Some(format!("Step {} failed after {secs:.1} s, so the rest were skipped:\n{}", i + 1, lines.join("\n"))),
            ..Default::default()
        },
    }
}
