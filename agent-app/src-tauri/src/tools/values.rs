//! Operating controls by what they should become rather than by gestures: set_value makes a control hold a
//! value whatever kind it is (text, number, on/off, a chosen item) and checks that it does; element_action
//! runs one explicit UI Automation action without touching the mouse, the focus or the window order.

use super::locate::{Ask, Located, Miss};
use super::report::Did;
use super::*;
use crate::fx::Fx;
use serde_json::json;

/// The animation, and the work area it framed (to tell afterwards whether that area went away; null when the
/// whole window was framed). The overlay keeps one frame and one spot for the orb per window (`win`), however the
/// actions in it are framed.
fn anim(call: &str, part: &str, loc: &Located) -> (Fx, Value) {
    let win = winmgr::rect_of(loc.hwnd);
    // The work area of the last scene read, when it is in this window (it may be another window's)
    let inside = |r: &(i32, i32, i32, i32), w: &(i32, i32, i32, i32)| r.0 >= w.0 - 2 && r.1 >= w.1 - 2 && r.2 <= w.2 + 2 && r.3 <= w.3 + 2;
    let area = uia::work_area().filter(|(r, _)| win.is_some_and(|w| inside(r, &w)));
    let zone = json!(area.map(|(r, _)| fx::rect_json(r)));
    let (shown, whole) = match area {
        Some((r, w)) => (fx::rect_json(r), w),
        None => (json!(win.map(fx::rect_json)), true),
    };
    let fx = Fx::start(
        call,
        part,
        "element_action",
        json!({}),
        json!({ "targets": [fx::pt_json(loc.point)], "zone": shown, "zoneIsWindow": whole, "win": win.map(fx::rect_json) }),
    );
    (fx, zone)
}

/// Let the frame go at once when the area it was drawn round is no longer the work area (a popup closed)
fn drop_zone_if_gone(call: &str, zone: Value) {
    if !zone.is_null() && json!(uia::work_area().map(|(r, _)| fx::rect_json(r))) != zone {
        fx::zone_gone(call, zone);
    }
}

/// Fill a field the way a person would when it won't take a value directly: focus it, select everything,
/// type. Reads it back when the control says what it holds.
/// `prior` is what the control held before anything was done to it: nothing to do when that is the value
/// already, and the proof afterwards that typing changed it. It is read by the caller before a direct set is
/// tried, since a set that lands late would otherwise look like a value that was there all along.
fn type_into(loc: &Located, value: &str, call: &str, part: &str, prior: Option<String>) -> Result<String, String> {
    let same = |a: &str, b: &str| uia::newlines(a).trim() == uia::newlines(b).trim();
    if let Some(p) = &prior
        && !value.trim().is_empty()
        && same(p, value)
    {
        return Ok(format!("{} already reads \"{}\"", loc.desc, uia::clip(p, 80)));
    }
    keys::focus_target(loc, call, part)?;
    input::press_keys("ctrl+a")?;
    std::thread::sleep(Duration::from_millis(20));
    if value.is_empty() {
        input::press_keys("delete")?;
    } else {
        keys::enter_text(value, "auto")?;
    }
    std::thread::sleep(Duration::from_millis(60));
    let Some(l) = &loc.live else { return Ok(format!("Typed \"{}\" at {} (not a readable control, so not checked)", uia::clip(value, 60), loc.desc)) };
    if l.is_password() {
        return Ok(format!("Filled {}", loc.desc));
    }
    // The program takes the keys in a moment after they are sent: read again for a short while before calling it wrong
    let started = std::time::Instant::now();
    let mut got = l.value();
    while got.as_deref().is_some_and(|v| !same(v, value)) && started.elapsed() < Duration::from_millis(250) {
        std::thread::sleep(Duration::from_millis(30));
        got = l.value();
    }
    match got {
        Some(v) if same(&v, value) => {
            // It reads the value and did not before: a paste has arrived, so the clipboard can go back now
            if prior.is_some() {
                keys::clipboard_release();
            }
            Ok(format!("{} now reads \"{}\"", loc.desc, uia::clip(&v, 80)))
        }
        Some(v) => Err(format!("{} was typed into but reads \"{}\", not \"{}\" (it may reformat or reject the input)", loc.desc, uia::clip(&v, 80), uia::clip(value, 80))),
        None => Ok(format!("Typed \"{}\" into {} (it does not say what it holds, so not checked)", uia::clip(value, 60), loc.desc)),
    }
}

/// `watch` runs once the control is found and before it is touched (to note how the screen looks before)
fn set_one(ask: &Ask, value: &str, call: &str, part: &str, watch: &mut dyn FnMut(&Located)) -> Result<(String, Located), Miss> {
    let loc = locate::resolve(ask)?;
    watch(&loc);
    let (fx, _) = anim(call, part, &loc);
    let prior = loc.live.as_ref().filter(|l| !l.is_password()).and_then(|l| l.value());
    let r = match &loc.live {
        Some(l) => match l.set(value) {
            Ok(s) => Ok(s),
            Err(Some(e)) => Err(e),
            // Takes no value directly (or ignored it, as many web pages do): type it in
            Err(None) => type_into(&loc, value, call, &format!("{part}t"), prior),
        },
        None => type_into(&loc, value, call, &format!("{part}t"), prior),
    };
    fx.step(0, r.is_ok());
    fx.end(r.is_ok());
    Ok((r?, loc))
}

pub(super) async fn set_value(ctx: &Ctx, args: &Value) -> Output {
    // One control, or several as fields: [{ target, value }, …]
    let many = args.get("fields").is_some_and(|f| !f.is_null());
    let items: Vec<Value> = match args.get("fields").and_then(Value::as_array) {
        Some(list) if !list.is_empty() => list
            .iter()
            .map(|f| {
                let mut f = f.clone();
                // A field without its own window is looked for in the call's
                if let (Some(o), Some(w)) = (f.as_object_mut(), args.get("window"))
                    && !o.contains_key("window")
                {
                    o.insert("window".into(), w.clone());
                }
                f
            })
            .collect(),
        _ if many => return fail("fields must be a list of { target, value }"),
        _ => vec![args.clone()],
    };
    if items.len() > 50 {
        return fail("Up to 50 fields in one call");
    }
    let mut asks = vec![];
    for (i, f) in items.iter().enumerate() {
        let which = if many { format!("Field {}: ", i + 1) } else { String::new() };
        let Some(value) = f.get("value").filter(|v| !v.is_null()) else { return fail(format!("{which}value is missing")) };
        // true / false and numbers are as welcome as their spellings
        let value = match value {
            Value::String(s) => s.clone(),
            Value::Bool(b) => if *b { "on" } else { "off" }.to_string(),
            other => other.to_string(),
        };
        match Ask::required(ctx, f) {
            Ok(a) => asks.push((a.editing(), value)),
            Err(e) => return fail(format!("{which}{e}")),
        }
    }
    let (pace, call) = (Pace::current(), fx::call_id());
    let turn = match acting().await {
        Ok(t) => t,
        Err(o) => return o,
    };
    let r = tokio::task::spawn_blocking(move || -> Result<(Did, bool), Miss> {
        // The screen is compared from just before the first control is touched, in the window it is in
        let mut before: Option<Option<screen::Before>> = None;
        let mut lines = vec![];
        let mut failed = 0;
        let mut hwnd = None;
        for (i, (ask, value)) in asks.iter().enumerate() {
            let mut watch = |loc: &Located| {
                if before.is_none() {
                    before = Some(pace.before(Some(loc.point)));
                }
            };
            match set_one(ask, value, &call, &format!("v{i}"), &mut watch) {
                Ok((s, loc)) => {
                    hwnd = hwnd.or(loc.scene);
                    lines.push((true, s));
                }
                // The screen is not what the AI saw: stop here rather than fill the rest blind
                Err(m) if m.stale() => return Err(m),
                // One control that wasn't found: said as a miss, so a plan's runner can decide what was meant
                Err(m) if !many && m.kind.unresolved() => return Err(m),
                Err(m) => {
                    failed += 1;
                    lines.push((false, m.msg));
                }
            }
        }
        let change = match before {
            Some(b) => pace.after_direct(b),
            None => None,
        };
        let done = if many {
            let head = if failed == 0 { format!("All {} fields set", lines.len()) } else { format!("{failed} of {} fields failed", lines.len()) };
            format!("{head}:\n{}", lines.iter().enumerate().map(|(i, (ok, l))| format!("{}. {} {l}", i + 1, if *ok { "✓" } else { "✗" })).collect::<Vec<_>>().join("\n"))
        } else {
            lines.remove(0).1
        };
        Ok((Did { done, change, hwnd, fresh: false, delivered: true }, failed == 0))
    })
    .await;
    drop(turn);
    match r {
        Ok(Ok((did, true))) => report::conclude(ctx, args, did, true).await,
        Ok(Ok((did, false))) => {
            // What did get set still changed the window: say so, but as a failure
            let mut o = report::conclude(ctx, args, did, false).await;
            o.ok = false;
            o.error = o.text.take();
            o
        }
        Ok(Err(m)) => report::missed(ctx, m).await,
        Err(e) => fail(e.to_string()),
    }
}

pub(super) async fn element_action(ctx: &Ctx, args: &Value) -> Output {
    let ask = match Ask::parse(ctx, args, "target", "", "") {
        Ok(Some(a)) => a.anywhere(),
        Ok(None) => return fail("Say which control: target (a reference like \"e12\", or its name)"),
        Err(e) => return fail(e),
    };
    let action = str_arg(args, "action").to_string();
    let value = str_arg(args, "value").to_string();
    let (pace, call) = (Pace::current(), fx::call_id());
    let turn = match acting().await {
        Ok(t) => t,
        Err(o) => return o,
    };
    let r = tokio::task::spawn_blocking(move || -> Result<Did, Miss> {
        let loc = locate::resolve(&ask)?;
        let Some(live) = &loc.live else {
            return Err(format!("{} was read off the pixels, so it is not a control that can be operated this way; use click", loc.desc).into());
        };
        let before = pace.before(Some(loc.point));
        let (fx, zone) = anim(&call, "a", &loc);
        let r = live.act(&action, &value);
        fx.step(0, r.is_ok());
        fx.end(r.is_ok());
        let done = r?;
        let change = pace.after_direct(before);
        drop_zone_if_gone(&call, zone);
        Ok(Did { done, change, hwnd: loc.scene, fresh: false, delivered: true })
    })
    .await;
    drop(turn);
    match r {
        Ok(Ok(did)) => report::conclude(ctx, args, did, true).await,
        Ok(Err(m)) => report::missed(ctx, m).await,
        Err(e) => fail(e.to_string()),
    }
}
