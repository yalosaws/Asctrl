//! Programs and windows: opening, switching, arranging

use super::report::Did;
use super::*;

pub(super) async fn open_app(ctx: &Ctx, args: &Value) -> Output {
    // Cleared first, so a failed launch never animates toward the previous program's window
    *ctx.opened.lock().unwrap() = None;
    let target = str_arg(args, "target").trim().to_string();
    if target.is_empty() {
        return fail("target is empty");
    }
    let a = str_arg(args, "args").to_string();
    let cwd = match str_arg(args, "cwd") {
        "" => String::new(),
        c => match resolve_path(c) {
            Ok(p) => p.to_string_lossy().into_owned(),
            Err(e) => return fail(e),
        },
    };
    let wait = Duration::from_secs_f64(args.get("timeoutSec").and_then(Value::as_f64).unwrap_or(5.0).clamp(0.0, 60.0));
    let turn = acting().await;
    let r = tokio::task::spawn_blocking(move || winmgr::open(&target, &a, &cwd, wait).map(|h| (target, h))).await;
    drop(turn);
    match r {
        Ok(Ok((target, h))) => {
            *ctx.opened.lock().unwrap() = h;
            let done = match h.and_then(winmgr::info) {
                Some(w) => format!("Opened {target}: \"{}\" ({}, handle {}) is in front", uia::clip(&w.title, 60), w.process, w.handle),
                None => format!("Opened {target}. No window came up within {:.0} s: a console program has none, and a slow one may still be loading (wait_for its window)", wait.as_secs_f64()),
            };
            report::conclude(ctx, args, Did { done, change: None, hwnd: h, fresh: h.is_some(), delivered: false }, true).await
        }
        Ok(Err(e)) => fail(e),
        Err(e) => fail(e.to_string()),
    }
}

/// Switch to a window, or minimize / maximize / restore / close / move it
pub(super) async fn focus_window(ctx: &Ctx, args: &Value) -> Output {
    let h = match winmgr::find_arg(args.get("window")) {
        Ok(Some(h)) => h.0 as isize,
        Ok(None) => return fail("Say which window: a handle from list_windows, or part of its title or program name"),
        Err(e) => return fail(e),
    };
    let action = str_arg(args, "action").to_string();
    let f = ctx.frame();
    // A new place and size, given like everything else: in screenshot pixels
    let num = |k: &str| args.get(k).and_then(Value::as_f64);
    let place = (num("x"), num("y"), num("width"), num("height"));
    let moving = action == "move" || (action.is_empty() && (place.0.is_some() || place.2.is_some()));
    if moving && place.0.is_some() != place.1.is_some() {
        return fail("Give both x and y to move a window");
    }
    let fx_id = fx::call_id();
    let turn = acting().await;
    let r = tokio::task::spawn_blocking(move || -> Result<Did, String> {
        let hw = winmgr::hwnd(h);
        let at = winmgr::rect_of(h).map(|(l, t, r, b)| ((l + r) / 2, (t + b) / 2));
        let before = screen::before(at);
        let done = if moving {
            let pos = match (place.0, place.1) {
                (Some(x), Some(y)) => Some(f.to_screen(x, y)),
                _ => None,
            };
            let size = |v: Option<f64>| v.map(|v| (v / f.scale).round() as i32);
            let r = winmgr::set_bounds(hw, pos.map(|p| p.0), pos.map(|p| p.1), size(place.2), size(place.3))?;
            let _ = winmgr::bring_to_front(hw);
            format!("Moved the window to {}", f.rect(r))
        } else {
            winmgr::act(hw, &action)?
        };
        // The overlay hears where the window went right away (not after the screen settles), so its outline moves with it
        if !fx_id.is_empty() {
            let rect = winmgr::rect_of(h).map(fx::rect_json);
            fx::emit(serde_json::json!({ "phase": "step", "id": fx_id, "tool": "focus_window", "index": 0, "ok": true, "geo": { "rect": rect } }));
        }
        let change = screen::settle(&before, Duration::from_millis(150), Duration::from_millis(800));
        let there = winmgr::rect_of(h).is_some();
        let title = uia::clip(&winmgr::title_of(h), 60);
        let done = if title.is_empty() { done } else { format!("{done}: \"{title}\"") };
        Ok(Did { done, change: Some(change), hwnd: there.then_some(h), fresh: false, delivered: false })
    })
    .await;
    drop(turn);
    match r {
        Ok(Ok(did)) => report::conclude(ctx, args, did, true).await,
        Ok(Err(e)) => fail(e),
        Err(e) => fail(e.to_string()),
    }
}
