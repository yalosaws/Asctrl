//! A step of a program: one tool, with its own animation on the overlay, at the pace the program runs at, and —
//! when the text it names fits several controls or none — recovered as far as needs no judgment (`recover`)
//! before anyone is asked. A plan runs as a program too, so this is how every multi-step piece of work goes.

use super::*;

/// Steps that put input in, the ones a pace applies to
pub const INPUTS: [&str; 7] = ["click", "type_text", "press_keys", "scroll", "drag", "mouse", "set_value"];
/// Steps whose reply is the point of them: kept long in a plan's step results
pub const READS: [&str; 5] = ["read_text", "observe", "clipboard", "read_grid", "wait_for"];

/// Run one step with its own animation on the overlay. `fast`: an input that waits only for the program to take it.
pub(super) async fn run_step(ctx: &Ctx, tool: &str, step: &Value, id: String, fast: bool) -> Output {
    let geo = fx_before(ctx, tool, step);
    fx::emit(serde_json::json!({ "phase": "start", "id": id, "tool": tool, "args": step, "geo": geo }));
    let out = STEP.scope(true, FAST.scope(fast, fx::CALL_ID.scope(id.clone(), run_tool(ctx, tool, step)))).await;
    let after = fx_after(ctx, tool, &geo, &out);
    let t: String = out.body().chars().take(6000).collect();
    fx::emit(serde_json::json!({ "phase": "end", "id": id, "tool": tool, "ok": out.ok, "quiet": probe_missed(step, &out), "text": t, "geo": after }));
    out
}

/// A step, and when it named text that fits several controls or none, that miss recovered as far as needs no
/// judgment: the popup rules `on`, a moment's wait when the screen was not `settled`, and the candidates to choose
/// among. What was done on the way goes in the reply's data as `notes`, one line each.
pub(super) async fn run_recovering(ctx: &Ctx, tool: &str, step: &Value, id: String, fast: bool, on: &[(String, String)], settled: bool) -> Output {
    let out = run_step(ctx, tool, step, id.clone(), fast).await;
    if out.ok {
        return out;
    }
    let Some(wish) = recover::Wish::of(tool, step) else { return out };
    let mut n = 0;
    let again = |s: Value| {
        n += 1;
        let rid = format!("{id}.r{n}");
        async move { run_step(ctx, tool, &s, rid, false).await }
    };
    let r = recover::recover(ctx, step, &wish, on, settled, out, again).await;
    let mut out = r.out;
    if !r.notes.is_empty() {
        let mut d = out.data.take().unwrap_or_else(|| serde_json::json!({}));
        d["notes"] = serde_json::json!(r.notes);
        out.data = Some(d);
    }
    out
}

/// Popup rules as a step or a plan gives them: { "text a popup shows": "what to press" }
pub(super) fn on_rules(v: Option<&Value>) -> Vec<(String, String)> {
    v.and_then(Value::as_object)
        .map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).filter(|(k, v)| !k.trim().is_empty() && !v.trim().is_empty()).collect())
        .unwrap_or_default()
}

/// Windows that came up, went away or came to the front, as `{ms, kind, handle, program, title}` (ms from `t0`);
/// a title changing is left out, and at most the last 30
pub(super) fn window_events(evs: &[crate::events::Event], t0: std::time::Instant) -> Value {
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
