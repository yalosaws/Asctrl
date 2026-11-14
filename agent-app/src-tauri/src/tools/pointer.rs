//! Actions with the pointer: click, scroll, drag, and the low-level mouse steps games need.
//! Each one finds its target, makes sure the action will reach it, acts, waits just until the screen has
//! reacted, and says what changed.

use super::locate::{Ask, Located, Miss};
use super::report::Did;
use super::*;
use crate::fx::Fx;
use serde_json::json;

fn zone(hwnd: isize) -> Value {
    json!(winmgr::rect_of(hwnd).map(fx::rect_json))
}

/// Bring the window a control is in to the front when another program has the keyboard: a click does that
/// too, and the keys the AI presses next must go where it just clicked. A menu or popup of the program in
/// front is left alone (activating anything would close it).
pub(super) fn activate_for(loc: &Located) {
    let fg = winmgr::foreground();
    if fg == loc.hwnd || (fg != 0 && winmgr::pid_of(fg) == winmgr::pid_of(loc.hwnd)) {
        return;
    }
    let main = loc.scene.unwrap_or(loc.hwnd);
    let _ = winmgr::bring_to_front(winmgr::hwnd(main));
}

async fn finish(ctx: &Ctx, args: &Value, r: Result<Result<Did, Miss>, tokio::task::JoinError>) -> Output {
    match r {
        Ok(Ok(did)) => report::conclude(ctx, args, did, true).await,
        Ok(Err(m)) => report::missed(ctx, m).await,
        Err(e) => fail(e.to_string()),
    }
}

/// Click. A control of an ordinary program is pressed through UI Automation (exact, and the user's pointer
/// stays where it is); web pages, games and anything by coordinates get a real mouse click.
pub(super) async fn click(ctx: &Ctx, args: &Value) -> Output {
    let ask = match Ask::required(ctx, args) {
        Ok(a) => a,
        Err(e) => return fail(e),
    };
    let button = str_arg(args, "button").to_string();
    if !matches!(button.as_str(), "" | "left" | "right" | "middle") {
        return fail(format!("Unknown mouse button: {button}"));
    }
    let count = int_arg(args, "count", 1, 1, 3) as u32;
    let mods = str_arg(args, "modifiers").to_string();
    let mode = str_arg(args, "mode").to_string();
    if !matches!(mode.as_str(), "" | "auto" | "mouse" | "direct") {
        return fail(format!("mode: \"{mode}\" is not one of auto, mouse, direct"));
    }
    let (pace, call) = (Pace::current(), fx::call_id());
    let turn = acting().await;
    let r = tokio::task::spawn_blocking(move || -> Result<Did, Miss> {
        let loc = locate::resolve(&ask)?;
        let plain = matches!(button.as_str(), "" | "left") && count == 1 && mods.trim().is_empty();
        let forced = mode == "direct";
        if forced && loc.live.is_none() {
            return Err("mode \"direct\" presses a control without the mouse: pass its reference or name as target".to_string().into());
        }
        if forced && !plain {
            return Err("mode \"direct\" is a plain left press; right, double and modified clicks need the mouse".to_string().into());
        }
        if let Some(l) = &loc.live
            && !l.enabled()
        {
            return Err(format!("{} is disabled, so clicking it would do nothing", loc.desc).into());
        }
        let direct = loc.live.as_ref().is_some_and(|l| forced || (mode != "mouse" && plain && !l.is_web()));
        let before = pace.before(Some(loc.point));
        let framed = zone(loc.hwnd);
        let anim = Fx::start(&call, "a", "click", json!({ "count": count }), json!({ "at": fx::pt_json(loc.point), "zone": framed, "zoneIsWindow": true }));
        let mut done: Option<String> = None;
        let mut delivered = false;
        if direct && let Some(l) = &loc.live {
            activate_for(&loc);
            match l.press() {
                Ok(s) => {
                    // Keys pressed next (F2, Delete, Enter) should act on what was just picked, as after a real click
                    if s.starts_with("Selected") {
                        let _ = l.focus();
                    }
                    done = Some(format!("{s} (directly, without moving the mouse)"));
                    delivered = true;
                }
                Err(e) if forced => {
                    anim.step(0, false);
                    anim.end(false);
                    return Err(e.into());
                }
                // No press action of its own: the mouse does it
                Err(_) => {}
            }
        }
        if done.is_none() {
            let (x, y) = loc.point;
            if let Err(e) = locate::uncover(&loc).and_then(|_| input::click(x, y, &button, count, &mods)) {
                anim.step(0, false);
                anim.end(false);
                return Err(e.into());
            }
            let verb = match (count, button.as_str()) {
                (2, _) => "Double-clicked",
                (3, _) => "Triple-clicked",
                (_, "right") => "Right-clicked",
                (_, "middle") => "Middle-clicked",
                _ => "Clicked",
            };
            done = Some(format!("{verb} {}", loc.desc));
        }
        anim.step(0, true);
        anim.end(true);
        fx::zone_gone_when_closed(&call, loc.hwnd, framed);
        let change = if delivered { pace.after_direct(before) } else { pace.after(before) };
        Ok(Did { done: done.unwrap_or_default() + &loc.note, change, hwnd: loc.scene, fresh: false, delivered })
    })
    .await;
    drop(turn);
    finish(ctx, args, r).await
}

/// Where the wheel turns when no target is given: under the pointer if it is over the window in front,
/// else the middle of that window
fn default_scroll_point() -> (i32, i32) {
    let fg = winmgr::foreground();
    let cur = input::cursor_pos().ok();
    match (winmgr::rect_of(fg), cur) {
        (Some((l, t, r, b)), Some((x, y))) if x >= l && x < r && y >= t && y < b => (x, y),
        (Some((l, t, r, b)), _) => ((l + r) / 2, (t + b) / 2),
        (None, Some(p)) => p,
        (None, None) => {
            let v = capture::virtual_screen();
            (v.left + v.width / 2, v.top + v.height / 2)
        }
    }
}

pub(super) async fn scroll(ctx: &Ctx, args: &Value) -> Output {
    let ask = match Ask::of(ctx, args) {
        Ok(a) => a,
        Err(e) => return fail(e),
    };
    let dir = match str_arg(args, "direction") {
        "" => "down",
        d @ ("up" | "down" | "left" | "right") => d,
        other => return fail(format!("direction: \"{other}\" is not one of up, down, left, right")),
    }
    .to_string();
    let amount = int_arg(args, "amount", 3, 1, 100) as i32;
    let frame = ctx.frame();
    let (pace, call) = (Pace::current(), fx::call_id());
    let turn = acting().await;
    let r = tokio::task::spawn_blocking(move || -> Result<Did, Miss> {
        let (point, hwnd, desc, note) = match &ask {
            Some(a) => {
                let loc = locate::resolve(a)?;
                (loc.point, loc.scene, loc.desc.clone(), loc.note.clone())
            }
            None => {
                let p = default_scroll_point();
                (p, winmgr::top_window_at(p.0, p.1), frame.at(p.0, p.1), String::new())
            }
        };
        let horizontal = matches!(dir.as_str(), "left" | "right");
        let signed = if matches!(dir.as_str(), "down" | "right") { amount } else { -amount };
        let before = pace.before(Some(point));
        // The scroll bar is drawn on what actually scrolls (or along the window's edge), showing where it is
        let area = crate::uia::scroll_area_at(point.0, point.1, horizontal);
        let edge = area.as_ref().map(|a| a.rect).or_else(|| hwnd.and_then(winmgr::rect_of));
        let geo = json!({ "at": fx::pt_json(point), "area": edge.map(fx::rect_json), "pos": area.as_ref().and_then(|a| a.pos), "view": area.as_ref().and_then(|a| a.view) });
        let anim = Fx::start(&call, "a", "scroll", json!({ "amount": signed, "horizontal": horizontal }), geo);
        let r = input::scroll(point.0, point.1, signed, horizontal);
        if let Err(e) = r {
            anim.end(false);
            return Err(e.into());
        }
        let change = pace.after(before);
        let now = area.is_some().then(|| crate::uia::scroll_area_at(point.0, point.1, horizontal)).flatten();
        anim.end_with(true, json!({ "pos": now.and_then(|a| a.pos) }));
        let still = change.as_ref().is_some_and(|c| !c.changed);
        let hint = if still { " (if nothing moved, the view is already at its end, or the pointer is not over what scrolls)" } else { "" };
        Ok(Did { done: format!("Scrolled {dir} {amount} at {desc}{note}{hint}"), change, hwnd, fresh: false, delivered: false })
    })
    .await;
    drop(turn);
    finish(ctx, args, r).await
}

pub(super) async fn drag(ctx: &Ctx, args: &Value) -> Output {
    let ends = (Ask::parse(ctx, args, "from", "fromX", "fromY"), Ask::parse(ctx, args, "to", "toX", "toY"));
    let (from, to) = match ends {
        (Ok(Some(a)), Ok(Some(b))) => (a, b),
        (Err(e), _) | (_, Err(e)) => return fail(e),
        _ => return fail("Say where the drag starts and ends: from / to (a reference or the text shown), or fromX, fromY / toX, toY"),
    };
    let button = str_arg(args, "button").to_string();
    let ms = (args.get("seconds").and_then(Value::as_f64).unwrap_or(0.4).clamp(0.05, 10.0) * 1000.0) as u64;
    let (pace, call) = (Pace::current(), fx::call_id());
    let turn = acting().await;
    let r = tokio::task::spawn_blocking(move || -> Result<Did, Miss> {
        // Both ends are found before the button goes down: a drag must never stop half-way looking for its end
        let a = locate::resolve(&from)?;
        let b = locate::resolve(&to)?;
        locate::uncover(&a)?;
        let before = pace.before(Some(a.point));
        let anim = Fx::start(&call, "a", "drag", json!({}), json!({ "from": fx::pt_json(a.point), "to": fx::pt_json(b.point), "zone": zone(a.hwnd), "zoneIsWindow": true }));
        let r = input::drag(a.point.0, a.point.1, b.point.0, b.point.1, &button, ms);
        anim.end(r.is_ok());
        r?;
        let change = pace.after(before);
        Ok(Did { done: format!("Dragged {} to {}{}{}", a.desc, b.desc, a.note, b.note), change, hwnd: a.scene, fresh: false, delivered: false })
    })
    .await;
    drop(turn);
    finish(ctx, args, r).await
}

/// Low-level mouse steps: move (hover), down, up, and look (relative motion for game cameras)
pub(super) async fn mouse(ctx: &Ctx, args: &Value) -> Output {
    let action = str_arg(args, "action").to_string();
    if !matches!(action.as_str(), "move" | "down" | "up" | "look") {
        return fail("action must be one of move, down, up, look");
    }
    let ask = match Ask::of(ctx, args) {
        Ok(a) => a,
        Err(e) => return fail(e),
    };
    if action == "move" && ask.is_none() {
        return fail("Say where to move: target, or x and y");
    }
    let button = str_arg(args, "button").to_string();
    let dx = int_arg(args, "dx", 0, -20000, 20000) as i32;
    let dy = int_arg(args, "dy", 0, -20000, 20000) as i32;
    if action == "look" && dx == 0 && dy == 0 {
        return fail("Give dx and/or dy");
    }
    let look_ms = (args.get("seconds").and_then(Value::as_f64).unwrap_or(0.2).clamp(0.0, 5.0) * 1000.0) as u64;
    let (pace, call) = (Pace::current(), fx::call_id());
    let turn = acting().await;
    let r = tokio::task::spawn_blocking(move || -> Result<Did, Miss> {
        if action == "look" {
            let before = pace.before(None);
            input::mouse_look(dx, dy, look_ms)?;
            return Ok(Did { done: format!("Mouse moved by ({dx}, {dy})"), change: pace.after(before), hwnd: Some(winmgr::foreground()), fresh: false, delivered: false });
        }
        let loc = match &ask {
            Some(a) => Some(locate::resolve(a)?),
            None => None,
        };
        let at = loc.as_ref().map(|l| l.point);
        let shown = at.or_else(|| input::cursor_pos().ok());
        let before = pace.before(shown);
        let (tool, done): (&'static str, String) = match action.as_str() {
            "move" => ("mouse_move", "Pointer moved to".into()),
            "down" => ("mouse_down", "Pressed and holding at".into()),
            _ => ("mouse_up", "Released at".into()),
        };
        let anim = Fx::start(&call, "a", tool, json!({ "button": button }), json!({ "at": shown.map(fx::pt_json) }));
        let r = match (action.as_str(), at) {
            ("move", Some((x, y))) => input::mouse_move(x, y),
            ("down", p) => input::mouse_button(p, &button, true),
            (_, p) => input::mouse_button(p, &button, false),
        };
        anim.end(r.is_ok());
        r?;
        let (desc, note, hwnd) = match &loc {
            Some(l) => (l.desc.clone(), l.note.clone(), l.scene),
            None => ("the pointer's position".to_string(), String::new(), shown.and_then(|(x, y)| winmgr::top_window_at(x, y))),
        };
        Ok(Did { done: format!("{done} {desc}{note}"), change: pace.after(before), hwnd, fresh: false, delivered: false })
    })
    .await;
    drop(turn);
    finish(ctx, args, r).await
}
