//! Actions with the pointer: click, scroll, drag, and the low-level mouse steps games need.
//! Each one finds its target, makes sure the action will reach it, acts, waits just until the screen has
//! reacted, and says what changed.

use super::locate::{Ask, Located, Miss};
use super::report::Did;
use super::*;
use crate::until;
use crate::fx::Fx;
use serde_json::json;

/// The frame for an action in `hwnd` (the window), and that it is the whole window
fn zone(hwnd: isize) -> (Value, bool) {
    (json!(winmgr::rect_of(hwnd).map(fx::rect_json)), true)
}

/// Where the last scrolled area stood after its scroll: (its rect, horizontal, percent). The next scroll of the
/// same area starts its thumb there, since reading the position before scrolling would hold the scroll up.
/// A scrolled area's rect, whether it scrolls sideways, and where it stood (percent)
type ScrollSpot = (winmgr::Rect, bool, f64);
static LAST_SCROLL: std::sync::Mutex<Option<ScrollSpot>> = std::sync::Mutex::new(None);

/// The scroll bar on the overlay, worked out on its own thread while the scroll goes ahead: which area scrolls
/// at `point` (UI Automation, which can take a while in a browser), sent as a step; then, once the scroll is
/// done (`true` on the returned sender) and the screen has settled, where it went, sent with the end.
fn scroll_bar_follows(anim: Fx, point: (i32, i32), horizontal: bool, hwnd: Option<isize>) -> std::sync::mpsc::Sender<bool> {
    let (tx, rx) = std::sync::mpsc::channel::<bool>();
    std::thread::spawn(move || {
        let area = crate::uia::scroll_area_at(point.0, point.1, horizontal);
        let edge = area.as_ref().map(|a| a.rect).or_else(|| hwnd.and_then(winmgr::rect_of));
        let from = area.as_ref().and_then(|a| {
            let last = *LAST_SCROLL.lock().unwrap();
            last.filter(|(r, h, _)| *r == a.rect && *h == horizontal).map(|(_, _, p)| p)
        });
        anim.step_with(0, true, json!({ "area": edge.map(fx::rect_json), "view": area.as_ref().and_then(|a| a.view), "pos": from }));
        let ok = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap_or(false);
        if !ok {
            return anim.end(false);
        }
        let now = area.as_ref().and_then(|a| crate::uia::scroll_area_at(point.0, point.1, horizontal).filter(|n| n.rect == a.rect));
        let pos = now.as_ref().and_then(|n| n.pos);
        if let (Some(n), Some(p)) = (&now, pos) {
            *LAST_SCROLL.lock().unwrap() = Some((n.rect, horizontal, p));
        }
        anim.end_with(true, json!({ "pos": pos }));
    });
    tx
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
    // A press that needs neither the mouse nor the focus: asked for (mode direct), or while the person at the
    // computer is using them (they come first). Pressed through UI Automation where the window is, without
    // bringing it forward; one at a time in that window, not in the queue for the mouse and keyboard.
    let plain = matches!(button.as_str(), "" | "left") && count == 1 && mods.trim().is_empty();
    if plain && (mode == "direct" || (matches!(mode.as_str(), "" | "auto") && person_busy())) {
        // Which window, if it can be pressed so (the control can't leave this thread: it is found again below)
        let a2 = ask.clone();
        let win = tokio::task::spawn_blocking(move || locate::resolve(&a2).ok().filter(|loc| loc.live.as_ref().is_some_and(|l| !l.is_web() && l.enabled())).map(|loc| loc.scene.unwrap_or(loc.hwnd)))
            .await
            .ok()
            .flatten();
        if let Some(w) = win {
            let _lane = super::web::lane(&format!("window:{w}")).await;
            let (a3, c2) = (ask.clone(), call.clone());
            let r = tokio::task::spawn_blocking(move || -> Result<Did, Miss> {
                let loc = locate::resolve(&a3)?;
                let l = loc.live.as_ref().ok_or_else(|| Miss::from(format!("{} can't be pressed without the mouse", loc.desc)))?;
                let before = pace.before(None);
                let (framed, whole) = zone(loc.hwnd);
                let anim = Fx::start(&c2, "a", "click", json!({ "count": 1 }), json!({ "at": fx::pt_json(loc.point), "zone": framed, "zoneIsWindow": whole, "win": framed, "app": fx::app_json(loc.hwnd) }));
                let theirs = winmgr::foreground();
                let s = l.press_quietly();
                // Should the press still have brought its window forward, the person's window goes back in front
                let gave_back = theirs != 0 && theirs != loc.hwnd && winmgr::foreground() == loc.hwnd && winmgr::bring_to_front(winmgr::hwnd(theirs)).is_ok();
                anim.step(0, s.is_ok());
                anim.end(s.is_ok());
                let s = s.map_err(Miss::from)?;
                let change = pace.after_direct(before);
                let back = if gave_back { "; it brought its window forward, so the person's window was put back in front" } else { "" };
                Ok(Did { done: format!("{s} (directly, in the background: the mouse, keyboard and window in front were left to the person{back}){}", loc.note), change, hwnd: loc.scene, fresh: false, delivered: true, verified: None })
            })
            .await;
            return finish(ctx, args, r).await;
        }
    }
    let turn = match acting().await {
        Ok(t) => t,
        Err(o) => return o,
    };
    let r = tokio::task::spawn_blocking(move || -> Result<Did, Miss> {
        let loc = locate::resolve(&ask)?;
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
        // What a plain click on it is for, where the control can say whether that happened
        let expect = loc.live.as_ref().filter(|_| plain).and_then(|l| match l.role() {
            "CheckBox" => l.toggle_state().filter(|s| *s != "mixed").map(Expect::Toggle),
            "RadioButton" | "TabItem" => l.is_selected().map(|_| Expect::Select),
            _ => None,
        });
        let before = pace.before(Some(loc.point));
        let (framed, whole) = zone(loc.hwnd);
        let anim = Fx::start(&call, "a", "click", json!({ "count": count }), json!({ "at": fx::pt_json(loc.point), "zone": framed, "zoneIsWindow": whole, "win": framed, "app": fx::app_json(loc.hwnd) }));
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
        fx::zone_follows_window(&call, loc.hwnd, framed);
        let mut done = done.unwrap_or_default();
        let verified = match (expect, &loc.live) {
            (Some(e), Some(l)) => {
                let ok = until::holds(CLICK_TAKES, || e.met(l));
                if !ok {
                    done += &format!(", but it is still {}", l.toggle_state().unwrap_or("not selected"));
                }
                Some(ok)
            }
            _ => None,
        };
        let change = if delivered { pace.after_direct(before) } else { pace.after(before) };
        Ok(Did { done: done + &loc.note, change, hwnd: loc.scene, fresh: false, delivered, verified })
    })
    .await;
    drop(turn);
    finish(ctx, args, r).await
}

/// How long a clicked box or option may take to show it was ticked or picked
const CLICK_TAKES: Duration = Duration::from_millis(800);

/// What a plain click on a control with a state is for
#[derive(Clone, Copy)]
enum Expect {
    /// A box that was in this state flips
    Toggle(&'static str),
    /// An option or tab becomes the picked one
    Select,
}

impl Expect {
    fn met(self, l: &uia::Live) -> bool {
        match self {
            Expect::Toggle(was) => l.toggle_state().is_some_and(|s| s != was),
            Expect::Select => l.is_selected() == Some(true),
        }
    }
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
    let turn = match acting().await {
        Ok(t) => t,
        Err(o) => return o,
    };
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
        // The animation finds out on its own thread what scrolls there; the scroll never waits for it
        let anim = Fx::start(&call, "a", "scroll", json!({ "amount": signed, "horizontal": horizontal }), json!({ "at": fx::pt_json(point) }));
        let done = scroll_bar_follows(anim, point, horizontal, hwnd);
        let r = input::scroll(point.0, point.1, signed, horizontal);
        if let Err(e) = r {
            let _ = done.send(false);
            return Err(e.into());
        }
        let change = pace.after(before);
        let _ = done.send(true);
        let still = change.as_ref().is_some_and(|c| !c.changed);
        let hint = if still { " (if nothing moved, the view is already at its end, or the pointer is not over what scrolls)" } else { "" };
        Ok(Did { done: format!("Scrolled {dir} {amount} at {desc}{note}{hint}"), change, hwnd, fresh: false, delivered: false, verified: None })
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
    let turn = match acting().await {
        Ok(t) => t,
        Err(o) => return o,
    };
    let r = tokio::task::spawn_blocking(move || -> Result<Did, Miss> {
        // Both ends are found before the button goes down: a drag must never stop half-way looking for its end
        let a = locate::resolve(&from)?;
        let b = locate::resolve(&to)?;
        locate::uncover(&a)?;
        let before = pace.before(Some(a.point));
        let (framed, whole) = zone(a.hwnd);
        let anim = Fx::start(&call, "a", "drag", json!({}), json!({ "from": fx::pt_json(a.point), "to": fx::pt_json(b.point), "zone": framed, "zoneIsWindow": whole, "win": framed, "app": fx::app_json(a.hwnd) }));
        let r = input::drag(a.point.0, a.point.1, b.point.0, b.point.1, &button, ms);
        anim.end(r.is_ok());
        // A drag often moves or resizes the very window it was framed in (a title bar, a splitter)
        fx::zone_follows_window(&call, a.hwnd, framed);
        r?;
        let change = pace.after(before);
        Ok(Did { done: format!("Dragged {} to {}{}{}", a.desc, b.desc, a.note, b.note), change, hwnd: a.scene, fresh: false, delivered: false, verified: None })
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
    let turn = match acting().await {
        Ok(t) => t,
        Err(o) => return o,
    };
    let r = tokio::task::spawn_blocking(move || -> Result<Did, Miss> {
        if action == "look" {
            let before = pace.before(None);
            input::mouse_look(dx, dy, look_ms)?;
            return Ok(Did { done: format!("Mouse moved by ({dx}, {dy})"), change: pace.after(before), hwnd: Some(winmgr::foreground()), fresh: false, delivered: false, verified: None });
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
        Ok(Did { done: format!("{done} {desc}{note}"), change: pace.after(before), hwnd, fresh: false, delivered: false, verified: None })
    })
    .await;
    drop(turn);
    finish(ctx, args, r).await
}
