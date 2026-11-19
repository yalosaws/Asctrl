//! What an action's reply says. The aim is that the AI never has to look again just to learn what its last
//! action did: the reply carries the cheapest evidence that is enough.
//!
//! - Always: what was done, and whether the screen reacted (and where).
//! - When the window's controls are readable and the AI has a reading of them: what changed among them, as
//!   text with references ready to use. A window that just came to the front is listed whole.
//! - A screenshot only when text can't tell: the window is unknown or draws its own UI, or pixels changed
//!   while no control did.
//!
//! `after` overrides it: "none", "ui", "screenshot", "both".

use super::*;

/// What an action did, for the reply
pub struct Did {
    pub done: String,
    /// How the screen reacted (None in fast mode, or for things that don't touch the screen)
    pub change: Option<screen::Change>,
    /// The window acted in
    pub hwnd: Option<isize>,
    /// The window is new to the AI (it just opened, or was just waited for): list its controls whole
    pub fresh: bool,
    /// The program itself confirmed the action (UI Automation): no visible change does not mean it missed
    pub delivered: bool,
    /// Whether what the action was for was seen to happen: Some(true) checked and so, Some(false) checked and
    /// not so (the action then fails, whatever was sent), None when it can't be told
    pub verified: Option<bool>,
}

impl Did {
    pub fn new(done: impl Into<String>) -> Did {
        Did { done: done.into(), change: None, hwnd: None, fresh: false, delivered: false, verified: None }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum After {
    Auto,
    None,
    Ui,
    Shot,
    Both,
}

fn after_arg(args: &Value) -> Result<After, String> {
    Ok(match str_arg(args, "after") {
        "" | "auto" => After::Auto,
        "none" => After::None,
        "ui" | "changes" => After::Ui,
        "screenshot" | "image" => After::Shot,
        "both" => After::Both,
        other => return Err(format!("after: \"{other}\" is not one of auto, none, ui, screenshot, both")),
    })
}

/// What the screen did after an action, for the AI to catch a click that missed without taking a screenshot
pub fn describe_change(f: &Frame, c: Option<&screen::Change>, delivered: bool) -> String {
    let Some(c) = c else { return String::new() };
    let ms = c.waited.as_millis();
    let front = c.new_front.as_ref().map(|t| format!(". \"{}\" is now in front", uia::clip(t, 60))).unwrap_or_default();
    match (c.changed, c.rect) {
        (true, Some(r)) if c.busy => format!(". Screen changed around {} and was still changing after {ms} ms (loading or animating){front}", f.rect(r)),
        (true, Some(r)) => format!(". Screen changed around {} (settled in {ms} ms){front}", f.rect(r)),
        (true, None) => front,
        _ if delivered => format!(". Nothing visibly changed within {ms} ms"),
        _ => format!(". No visible change within {ms} ms: the action may have missed, or the program is still working"),
    }
}

/// What the screen did, for a program reading the reply: "changed", "unchanged", or None when it wasn't
/// watched (fast mode, or an action that doesn't touch the screen). Says the same as `describe_change`.
pub fn effect(c: Option<&screen::Change>) -> Option<&'static str> {
    let c = c?;
    match (c.changed, c.rect.is_some() || c.new_front.is_some()) {
        (true, true) => Some("changed"),
        (true, false) => None,
        (false, _) => Some("unchanged"),
    }
}

/// The structured part of an action's reply
fn did_data(did: &Did) -> Option<Value> {
    let mut data = serde_json::json!({});
    if let Some(e) = effect(did.change.as_ref()) {
        data["effect"] = serde_json::json!(e);
    }
    if let Some(v) = did.verified {
        data["verified"] = serde_json::json!(v);
    }
    data.as_object().is_some_and(|o| !o.is_empty()).then_some(data)
}

/// Most lines of control changes in a reply; observe shows everything
const MAX_CHANGES: usize = 40;
/// A window whose controls take longer than this to read is not re-read after every action
const SLOW_SCENE: Duration = Duration::from_millis(500);

/// The controls part of a reply: what changed in `hwnd` since the AI last read it, or — for a window it has
/// not read — the window as it is. None when its controls say nothing useful (a canvas, an unreadable window).
/// `fresh` lists an unread window whole; otherwise an unread window gets no controls part.
fn controls_part(f: Frame, hwnd: isize, fresh: bool, force: bool) -> Option<(String, bool)> {
    let known = uia::known_cost(hwnd);
    if !force {
        if known.is_none() && !fresh {
            return None;
        }
        if known.is_some_and(|c| c > SLOW_SCENE) {
            return Some(("(This window's controls are slow to read, so they were not re-read; use observe when you need them.)".into(), true));
        }
    }
    let scene = uia::snapshot(hwnd, false).ok()?;
    let at = |x: i32, y: i32| f.at(x, y);
    let title = uia::clip(&scene.title, 60);
    let out = match uia::changes(&scene, MAX_CHANGES, &at) {
        Some(d) if d.is_empty() => (format!("No control changed in \"{title}\"."), false),
        Some(d) if d.starts_with("The window now shows") => (d.replacen("The window now shows", &format!("\"{title}\" now shows"), 1), true),
        Some(d) => (format!("Changes in \"{title}\":\n{d}"), true),
        None if scene.nodes.is_empty() => return None,
        None => (format!("\"{title}\" — {} controls:\n{}", scene.nodes.len(), uia::render(&scene, &uia::Render { max: MAX_CHANGES + 20, at: &at })), true),
    };
    uia::remember(&scene);
    Some(out)
}

/// Build the reply to an action. `shot_default`: whether, with after: auto, a screenshot goes back when
/// the controls can't tell the story (true for actions; false for things like wait_for).
pub async fn conclude(ctx: &Ctx, args: &Value, did: Did, shot_default: bool) -> Output {
    let after = match after_arg(args) {
        Ok(a) => a,
        Err(e) => return fail(e),
    };
    let f = ctx.frame();
    let mut body = did.done.clone() + &describe_change(&f, did.change.as_ref(), did.delivered);
    let data = did_data(&did);
    let with = |mut o: Output| {
        o.data = data.clone();
        o
    };
    // Sent, and seen not to have done what it was for: a failure, not a success with a warning
    if did.verified == Some(false) {
        let mut o = fail(body);
        o.data = data;
        return o;
    }
    // Inside a batch every step says what it did in a line; the batch reports once at the end
    if STEP.try_with(|s| *s).unwrap_or(false) || after == After::None {
        return with(text(body));
    }
    let came_up = did.change.as_ref().and_then(|c| c.front);
    let hwnd = came_up.or(did.hwnd).filter(|h| winmgr::exists(*h)).unwrap_or_else(winmgr::foreground);
    let fresh = came_up.is_some() || did.fresh;
    let force = matches!(after, After::Ui | After::Both);

    let mut told = false;
    let mut controls_changed = false;
    if after != After::Shot && hwnd != 0 {
        let part = tokio::task::spawn_blocking(move || controls_part(f, hwnd, fresh, force)).await.ok().flatten();
        if let Some((s, changed)) = part {
            body += "\n";
            body += &s;
            told = true;
            controls_changed = changed;
        }
    }
    let pixels_moved = did.change.as_ref().is_some_and(|c| c.changed && c.rect.is_some());
    let want_shot = match after {
        After::Shot | After::Both => true,
        After::Auto => shot_default && (!told || (!controls_changed && pixels_moved)),
        _ => false,
    };
    if told {
        ctx.saw(tokio::task::spawn_blocking(winmgr::layout).await.unwrap_or_default());
    }
    if !want_shot {
        return with(text(body));
    }
    match look::capture_view(ctx).await {
        Ok((image, note)) => with(Output { ok: true, text: Some(format!("{body}\n{note}")), image: Some(image), ..Default::default() }),
        Err(e) => with(text(format!("{body}\n(the screenshot after it failed: {e})"))),
    }
}

/// An action that was refused because the screen is not what the AI saw: the error goes back with a fresh
/// screenshot, so the AI can aim again at once
pub async fn refuse_stale(ctx: &Ctx, msg: String) -> Output {
    if STEP.try_with(|s| *s).unwrap_or(false) {
        return fail(msg);
    }
    match look::capture_view(ctx).await {
        Ok((image, note)) => Output { ok: false, error: Some(format!("{msg}\n{note}")), image: Some(image), ..Default::default() },
        Err(_) => fail(msg),
    }
}

/// The structured part of a failed lookup's reply: `{miss: {kind, candidates}}`
pub fn miss_data(kind: locate::MissKind, candidates: &[locate::Cand]) -> Value {
    serde_json::json!({ "miss": { "kind": kind.as_str(), "candidates": candidates.iter().map(locate::Cand::json).collect::<Vec<_>>() } })
}

/// Turn a failed lookup into a reply
pub async fn missed(ctx: &Ctx, miss: locate::Miss) -> Output {
    let data = Some(miss_data(miss.kind, &miss.candidates));
    let mut o = if miss.stale() { refuse_stale(ctx, miss.msg).await } else { fail(miss.msg) };
    o.data = data;
    o
}
