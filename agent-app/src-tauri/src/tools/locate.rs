//! One way to point at things. A target is a control's reference ("e12", from observe or any reply), the text
//! something shows ("Save"), or a point on the latest screenshot. Whatever it is, it resolves to a place on
//! the screen, the window it is in, and — when it is a real control — a handle to operate it directly.
//!
//! Text is looked up among the window's controls first (exact names beat partial ones, things you can
//! operate beat labels), then read off the pixels with OCR for programs that draw their own UI. It waits
//! briefly for something still on its way, refuses to guess between equally good matches, and names the
//! nearest candidates when it finds nothing.

use super::*;
use crate::ocr;
use crate::uia::{Node, Scene, flag};
use std::time::Instant;

#[derive(Clone)]
pub enum Target {
    Ref(u32),
    Text(String),
    /// A screen point (already mapped from the screenshot)
    Point(i32, i32),
}

/// A request to find something, with everything needed to answer it off the async thread
#[derive(Clone)]
pub struct Ask {
    pub target: Target,
    /// Only controls of this kind (button, edit, link, checkbox, …)
    pub role: String,
    /// Which one, in reading order, when several match equally
    pub nth: Option<usize>,
    /// The window to look in; the one in front when None
    pub window: Option<isize>,
    /// How long to wait for text that is not there yet
    pub wait: Duration,
    pub frame: Frame,
    pub seen: Arc<Vec<winmgr::Slot>>,
    /// The action wants something to type into: text boxes win ties
    pub want_edit: bool,
    /// The control need not be on screen (it is operated through UI Automation, not clicked)
    pub anywhere: bool,
}

/// Why something could not be pointed at, as a program reading the reply tells it apart
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MissKind {
    /// Anything else: an elevated window, a disabled control, bad arguments
    Other,
    /// The screen is no longer what the AI saw: a fresh screenshot goes back with the error
    Stale,
    /// Several things answer to the text equally well (`candidates` lists the controls among them)
    Ambiguous,
    /// Nothing answers to the text (or a list has no such item)
    Absent,
}

impl MissKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MissKind::Other => "other",
            MissKind::Stale => "stale",
            MissKind::Ambiguous => "ambiguous",
            MissKind::Absent => "absent",
        }
    }

    /// Text that names something else, or nothing yet: worth waiting for or deciding between
    pub fn unresolved(self) -> bool {
        matches!(self, MissKind::Ambiguous | MissKind::Absent)
    }
}

/// A control offered instead of a target that missed
#[derive(Clone, Debug, PartialEq)]
pub struct Cand {
    pub rf: u32,
    pub role: &'static str,
    pub name: String,
    /// What it is and how it stands, without where: `Button "Save" (disabled)`
    pub label: String,
}

impl Cand {
    pub fn of(n: &Node) -> Cand {
        let mut label = n.role.to_string();
        if !n.name.is_empty() {
            label += &format!(" \"{}\"", uia::clip(&n.name, 80));
        }
        if !n.value.is_empty() && n.value != n.name {
            label += &format!(" = \"{}\"", uia::clip(&n.value, 40));
        }
        let st = n.states();
        if !st.is_empty() {
            label += &format!(" ({})", st.join(", "));
        }
        Cand { rf: n.rf, role: n.role, name: n.name.clone(), label }
    }

    pub fn json(&self) -> Value {
        serde_json::json!({ "ref": format!("e{}", self.rf), "role": self.role, "name": self.name, "label": self.label })
    }
}

pub struct Miss {
    pub msg: String,
    pub kind: MissKind,
    /// The equally good matches, when there were several among the controls
    pub candidates: Vec<Cand>,
}

impl Miss {
    pub fn new(kind: MissKind, msg: impl Into<String>) -> Miss {
        Miss { msg: msg.into(), kind, candidates: vec![] }
    }

    pub fn stale(&self) -> bool {
        self.kind == MissKind::Stale
    }
}

impl From<String> for Miss {
    fn from(msg: String) -> Self {
        // A list without the item asked for is, to whoever wrote the step, the same as text that shows nowhere
        let kind = if msg.contains(" has no item \"") { MissKind::Absent } else { MissKind::Other };
        Miss::new(kind, msg)
    }
}

pub struct Located {
    /// Where to click, in screen pixels
    pub point: (i32, i32),
    /// The top-level window the point is in (a popup's own window, when it is in one)
    pub hwnd: isize,
    /// The window whose controls to report changes for
    pub scene: Option<isize>,
    /// The control itself, when the target is one
    pub live: Option<uia::Live>,
    /// How the target is named in replies
    pub desc: String,
    /// Said after the action: e.g. the window had moved and the point moved with it
    pub note: String,
}

impl Ask {
    /// The target in `args`: `key` (a reference or text) or the point `kx`, `ky`. None when neither is given.
    pub fn parse(ctx: &Ctx, args: &Value, key: &str, kx: &str, ky: &str) -> Result<Option<Ask>, String> {
        let frame = ctx.frame();
        let named = args.get(key).filter(|v| !v.is_null());
        let has_point = args.get(kx).is_some_and(|v| !v.is_null()) || args.get(ky).is_some_and(|v| !v.is_null());
        let target = match (named, has_point) {
            (Some(_), true) => return Err(format!("Give {key} or {kx}/{ky}, not both")),
            (None, false) => return Ok(None),
            (None, true) => {
                let (x, y) = frame.point(args, kx, ky)?;
                Target::Point(x, y)
            }
            (Some(Value::Number(n)), _) => Target::Ref(n.as_u64().filter(|n| *n > 0 && *n <= u32::MAX as u64).ok_or(format!("{key}: not a reference"))? as u32),
            (Some(Value::String(s)), _) => {
                let t = s.trim();
                if t.is_empty() {
                    return Err(format!("{key} is empty"));
                }
                match as_ref(t) {
                    Some(rf) => Target::Ref(rf),
                    None => Target::Text(t.to_string()),
                }
            }
            (Some(_), _) => return Err(format!("{key} must be a reference like \"e12\" or the text the thing shows")),
        };
        let window = winmgr::find_arg(args.get("window"))?.map(|h| h.0 as isize);
        let wait = Duration::from_secs_f64(args.get("timeoutSec").and_then(Value::as_f64).unwrap_or(1.5).clamp(0.0, 30.0));
        let nth = args.get("nth").and_then(Value::as_u64).filter(|n| *n >= 1).map(|n| n as usize);
        Ok(Some(Ask { target, role: str_arg(args, "role").trim().to_lowercase(), nth, window, wait, frame, seen: ctx.seen(), want_edit: false, anywhere: false }))
    }

    /// The usual target of an action: `target`, or `x`/`y`
    pub fn of(ctx: &Ctx, args: &Value) -> Result<Option<Ask>, String> {
        Ask::parse(ctx, args, "target", "x", "y")
    }

    pub fn required(ctx: &Ctx, args: &Value) -> Result<Ask, String> {
        Ask::of(ctx, args)?.ok_or_else(|| "Say what to act on: target (a reference like \"e12\", or the text it shows) or x and y on the latest screenshot".to_string())
    }

    pub fn editing(mut self) -> Ask {
        self.want_edit = true;
        self
    }

    pub fn anywhere(mut self) -> Ask {
        self.anywhere = true;
        self
    }
}

/// "e12" / "E12" / "[e12]" -> 12
fn as_ref(s: &str) -> Option<u32> {
    let t = s.trim_start_matches('[').trim_end_matches(']');
    let digits = t.strip_prefix('e').or_else(|| t.strip_prefix('E'))?;
    (!digits.is_empty() && digits.len() <= 9 && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits.parse().ok()).flatten().filter(|n| *n > 0)
}

/// Find what an Ask points at. Blocking: run it off the async thread.
pub fn resolve(ask: &Ask) -> Result<Located, Miss> {
    match &ask.target {
        Target::Ref(rf) => from_live(uia::live(*rf)?, ask.anywhere),
        Target::Text(q) => by_text(ask, q),
        Target::Point(x, y) => by_point(ask, *x, *y),
    }
}

fn clip_rect(r: winmgr::Rect, to: winmgr::Rect) -> Option<winmgr::Rect> {
    let c = (r.0.max(to.0), r.1.max(to.1), r.2.min(to.2), r.3.min(to.3));
    (c.2 - c.0 >= 1 && c.3 - c.1 >= 1).then_some(c)
}

fn screen_rect() -> winmgr::Rect {
    let v = capture::virtual_screen();
    (v.left, v.top, v.left + v.width, v.top + v.height)
}

fn center(r: winmgr::Rect) -> (i32, i32) {
    ((r.0 + r.2) / 2, (r.1 + r.3) / 2)
}

/// The part of a control that is on screen and inside its window
fn visible_part(live: &uia::Live, win: isize) -> Option<winmgr::Rect> {
    if live.offscreen() {
        return None;
    }
    let r = clip_rect(live.rect()?, screen_rect())?;
    match winmgr::rect_of(win) {
        Some(w) => clip_rect(r, w),
        None => Some(r),
    }
}

/// Where a step's target is right now, for drawing a pointer on its way there before the step runs. Looked up
/// without waiting, scrolling or reading pixels, and without touching what replies compare against; None
/// when it can't be told that cheaply (the step then finds it as always). `cache` holds one reading of a
/// window for the whole batch, so a run of clicks by text reads the window once, not once per step.
pub fn peek(ask: &Ask, cache: &std::sync::Mutex<Option<uia::Scene>>) -> Option<(i32, i32)> {
    match &ask.target {
        Target::Ref(rf) => {
            let live = uia::live(*rf).ok()?;
            Some(uia::aim(live.role(), live.rect()?))
        }
        Target::Point(x, y) => Some((*x, *y)),
        Target::Text(q) => {
            let hwnd = ask.window.unwrap_or_else(winmgr::foreground);
            let mut held = cache.lock().ok()?;
            if held.as_ref().map(|s| s.hwnd) != Some(hwnd) {
                *held = Some(uia::snapshot(hwnd, false).ok()?);
            }
            match pick(held.as_ref()?, q, &ask.role, ask.nth, ask.want_edit) {
                Pick::One(n) => Some(n.center()),
                _ => None,
            }
        }
    }
}

fn from_live(live: uia::Live, anywhere: bool) -> Result<Located, Miss> {
    let win = live.window();
    if anywhere {
        let point = live.rect().map(|r| uia::aim(live.role(), r)).unwrap_or((0, 0));
        return Ok(Located { point, hwnd: win, scene: Some(live.hwnd), desc: live.label.clone(), live: Some(live), note: String::new() });
    }
    let mut part = visible_part(&live, win);
    if part.is_none() && live.scroll_into_view() {
        // The view scrolls, often with an animation
        part = crate::until::until(Duration::from_millis(300), || visible_part(&live, win));
    }
    let Some(rect) = part else {
        let why = if winmgr::is_minimized(live.hwnd) { "its window is minimized (focus_window restores it)" } else { "it is scrolled out of view, collapsed or hidden" };
        return Err(format!("{} is not on screen: {why}", live.label).into());
    };
    let point = uia::aim(live.role(), rect);
    Ok(Located { point, hwnd: win, scene: Some(live.hwnd), desc: live.label.clone(), live: Some(live), note: String::new() })
}

// ---- Text ----

pub enum Pick<'a> {
    One(&'a Node),
    Many(Vec<&'a Node>),
    None,
}

fn role_matches(role: &str, want: &str) -> bool {
    if want.is_empty() {
        return true;
    }
    let r = role.to_lowercase();
    if r == want {
        return true;
    }
    match want {
        "button" => matches!(role, "Button" | "SplitButton"),
        "link" | "hyperlink" => role == "Link",
        "edit" | "textbox" | "input" | "field" | "text box" => matches!(role, "Edit" | "Document" | "ComboBox" | "Spinner"),
        "checkbox" | "check box" | "switch" | "toggle" => role == "CheckBox",
        "radio" | "radiobutton" | "radio button" => role == "RadioButton",
        "tab" => role == "TabItem",
        "menu" | "menuitem" | "menu item" => role == "MenuItem",
        "item" | "listitem" | "list item" | "row" => matches!(role, "ListItem" | "TreeItem" | "DataItem"),
        "combobox" | "combo box" | "dropdown" | "select" => role == "ComboBox",
        "text" | "label" => role == "Text",
        "image" | "icon" => role == "Image",
        _ => false,
    }
}

/// How well a control answers to `want` (already normalised); 0 = not at all.
/// An exact name beats any partial one; among partial ones, the name the text makes up most of wins.
fn score(n: &Node, want: &str, want_edit: bool) -> u32 {
    let wl = want.chars().count() as f64;
    let part = |s: &str, exact: f64, base: f64| -> f64 {
        if s.is_empty() || !s.contains(want) {
            0.0
        } else if s == want {
            exact
        } else {
            base + 30.0 * wl / s.chars().count() as f64 + if s.starts_with(want) { 5.0 } else { 0.0 }
        }
    };
    let mut s = part(&uia::norm(&n.name), 100.0, 45.0).max(part(&uia::norm(&n.value), 70.0, 25.0));
    if s == 0.0 {
        return 0;
    }
    if n.operable() {
        s += 8.0;
    }
    if want_edit && n.editable() {
        s += 15.0;
    }
    if n.has(flag::DISABLED) {
        s -= 4.0;
    }
    if n.has(flag::OFFSCREEN) {
        s -= 10.0;
    }
    s.max(1.0) as u32
}

/// Is `j` nested inside `i` in the scene's listing?
fn inside(scene: &Scene, i: usize, j: usize) -> bool {
    j > i && scene.nodes[i + 1..=j].iter().all(|n| n.depth > scene.nodes[i].depth)
}

/// The control `q` names in a scene: the best match when it stands alone, every equally good one otherwise
pub fn pick<'a>(scene: &'a Scene, q: &str, role: &str, nth: Option<usize>, want_edit: bool) -> Pick<'a> {
    let want = uia::norm(q);
    if want.is_empty() {
        return Pick::None;
    }
    let scored: Vec<(usize, u32)> = scene.nodes.iter().enumerate().filter(|(_, n)| role_matches(n.role, role)).map(|(i, n)| (i, score(n, &want, want_edit))).filter(|(_, s)| *s > 0).collect();
    let Some(top) = scored.iter().map(|(_, s)| *s).max() else { return Pick::None };
    let mut best: Vec<usize> = scored.iter().filter(|(_, s)| *s + 2 >= top).map(|(i, _)| *i).collect();
    // A control and something inside it with the same name are one thing: keep the one a click does something to
    let snapshot = best.clone();
    best.retain(|&j| {
        !snapshot.iter().any(|&i| {
            if i == j {
                return false;
            }
            let (a, b) = (&scene.nodes[i], &scene.nodes[j]);
            if inside(scene, i, j) {
                // j is inside i: j goes unless it is the operable one
                !(b.operable() && !a.operable())
            } else if inside(scene, j, i) {
                // i is inside j: j goes only when i is operable and j is not
                a.operable() && !b.operable()
            } else {
                // The same spot listed twice
                i < j && a.rect == b.rect
            }
        })
    });
    best.sort_by_key(|&i| (scene.nodes[i].rect.1 / 8, scene.nodes[i].rect.0));
    match (best.len(), nth) {
        (0, _) => Pick::None,
        (1, None | Some(1)) => Pick::One(&scene.nodes[best[0]]),
        (n, Some(k)) if k <= n => Pick::One(&scene.nodes[best[k - 1]]),
        _ => Pick::Many(best.into_iter().map(|i| &scene.nodes[i]).collect()),
    }
}

/// Every control that answers to `q`, best first
pub fn matches<'a>(scene: &'a Scene, q: &str, role: &str) -> Vec<&'a Node> {
    let want = uia::norm(q);
    if want.is_empty() {
        return vec![];
    }
    let mut scored: Vec<(u32, &Node)> = scene.nodes.iter().filter(|n| role_matches(n.role, role)).map(|n| (score(n, &want, false), n)).filter(|(s, _)| *s > 0).collect();
    scored.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
    scored.into_iter().map(|(_, n)| n).collect()
}

fn bigrams(s: &str) -> Vec<(char, char)> {
    let c: Vec<char> = s.chars().collect();
    c.windows(2).map(|w| (w[0], w[1])).collect()
}

/// The names in a scene most like `want`, for "did you mean"
fn closest(scene: &Scene, want: &str) -> Vec<String> {
    let wb = bigrams(want);
    let mut scored: Vec<(u32, &Node)> = scene
        .nodes
        .iter()
        .filter(|n| !n.name.is_empty())
        .filter_map(|n| {
            let nn = uia::norm(&n.name);
            let nb = bigrams(&nn);
            let sim = if wb.is_empty() || nb.is_empty() {
                // One-character names: only an exact start counts
                if nn.starts_with(want) || want.starts_with(&nn) { 0.5 } else { 0.0 }
            } else {
                let common = wb.iter().filter(|b| nb.contains(b)).count() as f64;
                2.0 * common / (wb.len() + nb.len()) as f64
            };
            (sim >= 0.3).then_some(((sim * 1000.0) as u32 + if n.operable() { 50 } else { 0 }, n))
        })
        .collect();
    scored.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
    let mut out: Vec<String> = vec![];
    for (_, n) in scored {
        let l = n.label();
        if !out.contains(&l) {
            out.push(l);
        }
        if out.len() >= 5 {
            break;
        }
    }
    out
}

/// The visible part of a window, to read text off
fn window_area(hwnd: isize) -> Option<capture::Area> {
    let r = clip_rect(winmgr::rect_of(hwnd)?, screen_rect())?;
    (r.2 - r.0 >= 16 && r.3 - r.1 >= 16).then_some(capture::Area { left: r.0, top: r.1, width: r.2 - r.0, height: r.3 - r.1 })
}

fn by_text(ask: &Ask, q: &str) -> Result<Located, Miss> {
    let start = Instant::now();
    let f = ask.frame;
    let at = |x: i32, y: i32| f.at(x, y);
    loop {
        let seen = crate::events::seq();
        // The window in front is looked up again each time round: a dialog may be on its way up
        let hwnd = ask.window.unwrap_or_else(winmgr::foreground);
        let mut title = winmgr::title_of(hwnd);
        let mut did_you_mean: Vec<String> = vec![];
        let mut controls = 0;
        let mut problem = String::new();
        match uia::snapshot(hwnd, false) {
            Ok(scene) => {
                controls = scene.nodes.len();
                if !scene.title.is_empty() {
                    title = scene.title.clone();
                }
                match pick(&scene, q, &ask.role, ask.nth, ask.want_edit) {
                    Pick::One(n) => {
                        let rf = n.rf;
                        // The reply will say what the action changed: against what the AI last saw of this
                        // window, or against how it is right now when it never looked
                        if uia::known_cost(hwnd).is_none() {
                            uia::remember(&scene);
                        }
                        return from_live(uia::live(rf)?, ask.anywhere);
                    }
                    Pick::Many(c) => {
                        let lines: Vec<String> = c.iter().take(8).enumerate().map(|(i, n)| format!("{}. {}", i + 1, n.line(&at))).collect();
                        let more = if c.len() > 8 { format!("\n… {} more", c.len() - 8) } else { String::new() };
                        uia::remember(&scene);
                        return Err(format!("\"{q}\" matches {} controls in \"{}\" equally well. Pass one's reference as target, or nth:\n{}{more}", c.len(), uia::clip(&title, 60), lines.join("\n")).into());
                    }
                    Pick::None => did_you_mean = closest(&scene, &uia::norm(q)),
                }
            }
            // An elevated window can't be read or clicked at all: say so now
            Err(e) if e.contains("administrator") => return Err(e.into()),
            Err(e) => problem = e,
        }
        // Not among the controls: read the window's pixels. A role asks for a real control, which text on a canvas is not.
        let mut ocr_lines = 0;
        if ask.role.is_empty()
            && let Some(area) = window_area(hwnd)
        {
            match ocr::read_in(hwnd, area) {
                Ok(lines) => {
                    ocr_lines = lines.len();
                    let hits = ocr::find(&lines, q);
                    // A label that is the whole line (a button, a menu item) beats the same words inside a sentence
                    let exact: Vec<&ocr::Hit> = hits.iter().filter(|h| h.exact).collect();
                    let pool: Vec<&ocr::Hit> = if exact.is_empty() { hits.iter().collect() } else { exact };
                    let chosen = match (pool.len(), ask.nth) {
                        (0, _) => None,
                        (1, None | Some(1)) => Some(pool[0]),
                        (n, Some(k)) if k <= n => Some(pool[k - 1]),
                        (n, Some(k)) => return Err(Miss::new(MissKind::Absent, format!("\"{q}\" is on screen only {n} time(s), so there is no match {k}"))),
                        (n, None) => {
                            let lines: Vec<String> = pool.iter().take(8).enumerate().map(|(i, h)| format!("{}. {} in \"{}\"", i + 1, f.at(center(h.rect).0, center(h.rect).1), uia::clip(&h.line, 60))).collect();
                            return Err(format!("\"{q}\" is on screen in {n} places. Pass nth, or click the one you mean by x and y:\n{}", lines.join("\n")).into());
                        }
                    };
                    if let Some(h) = chosen {
                        let point = center(h.rect);
                        // The window it was read in (off its own picture when something covers it), or that
                        // program's menu or popup over it; never another program's window in front
                        let top = winmgr::top_window_at(point.0, point.1).filter(|t| winmgr::pid_of(*t) == winmgr::pid_of(hwnd));
                        return Ok(Located {
                            point,
                            hwnd: top.unwrap_or(hwnd),
                            scene: Some(hwnd),
                            live: None,
                            desc: format!("the text \"{q}\" {}", f.at(point.0, point.1)),
                            note: String::new(),
                        });
                    }
                }
                Err(e) => {
                    if problem.is_empty() {
                        problem = e;
                    }
                }
            }
        }
        if start.elapsed() >= ask.wait {
            let kind = if ask.role.is_empty() { String::new() } else { format!(" {}", ask.role) };
            let mut msg = format!("Nothing in \"{}\" shows \"{q}\"{}", uia::clip(&title, 60), if kind.is_empty() { String::new() } else { format!(" as a{kind}") });
            msg += &format!(" (looked through {controls} controls");
            if ask.role.is_empty() {
                msg += &format!(" and {ocr_lines} lines of text read off the pixels");
            }
            msg += ").";
            if !did_you_mean.is_empty() {
                msg += &format!(" Closest: {}.", did_you_mean.join(", "));
            }
            if ask.window.is_none() {
                msg += " It may be in another window: pass window.";
            }
            if !problem.is_empty() {
                msg += &format!(" ({problem})");
            }
            return Err(Miss::new(MissKind::Absent, msg));
        }
        // Look again when a window comes, goes or changes title, or after a while for what changes inside one
        crate::events::wait(seen, Duration::from_millis(120).min(ask.wait.saturating_sub(start.elapsed())));
    }
}

// ---- A point on the screenshot ----

/// Check a click at a screen point against the windows as the AI last saw them (`seen`) and as they are now.
/// The window that was under the point must still be there and the same size; when it has moved, the point
/// moves with it (the second result says so); and nothing from another program may have come in front.
/// Both stacks are read the same way, so a see-through overlay that is always there never trips it.
pub fn guard(seen: &[winmgr::Slot], now: &[winmgr::Slot], x: i32, y: i32, title: &dyn Fn(isize) -> String) -> Result<(i32, i32, bool), String> {
    if seen.is_empty() {
        return Ok((x, y, false));
    }
    let name = |h: isize| match title(h) {
        t if t.trim().is_empty() => "A window".to_string(),
        t => format!("\"{}\"", uia::clip(&t, 50)),
    };
    let Some(then) = winmgr::slot_at(seen, x, y) else {
        return match winmgr::slot_at(now, x, y) {
            None => Ok((x, y, false)),
            Some(top) => Err(format!("{} has appeared at that point", name(top.handle))),
        };
    };
    let Some(cur) = now.iter().find(|s| s.handle == then.handle) else {
        return Err("the window that was at that point has closed or been minimized".into());
    };
    let size = |r: winmgr::Rect| (r.2 - r.0, r.3 - r.1);
    if size(cur.rect) != size(then.rect) {
        return Err(format!("{} has been resized, so what was at that point has moved", name(cur.handle)));
    }
    let (nx, ny) = (x + cur.rect.0 - then.rect.0, y + cur.rect.1 - then.rect.1);
    match winmgr::slot_at(now, nx, ny) {
        // The program's own menu or popup over its window is part of working in it
        Some(top) if top.handle == then.handle || top.pid == then.pid => Ok((nx, ny, (nx, ny) != (x, y))),
        Some(top) => Err(format!("{} is now in front of that point", name(top.handle))),
        None => Err("the window that was at that point has moved off the screen".into()),
    }
}

fn by_point(ask: &Ask, x: i32, y: i32) -> Result<Located, Miss> {
    let (px, py) = ask.frame.to_shot(x, y).unwrap_or((x, y));
    if let Some(h) = ask.frame.own {
        return in_own_picture(&ask.frame, h, x, y, (px, py));
    }
    let (nx, ny, moved) = guard(&ask.seen, &winmgr::layout(), x, y, &winmgr::title_of)
        .map_err(|why| Miss::new(MissKind::Stale, format!("Not done: since you last looked, {why}. Nothing was clicked or typed. Aim again on the fresh screenshot.")))?;
    let hwnd = winmgr::top_window_at(nx, ny).unwrap_or(0);
    let note = if moved { " (the window had moved since the screenshot; the point moved with it)".to_string() } else { String::new() };
    Ok(Located { point: (nx, ny), hwnd, scene: (hwnd != 0).then_some(hwnd), live: None, desc: format!("({px},{py})"), note })
}

/// A point on a picture of one window taken while others covered it: the point belongs to that window, which
/// must still be there and the same size; it moves with the window, and `uncover` brings it forward
fn in_own_picture(f: &Frame, h: isize, x: i32, y: i32, (px, py): (i32, i32)) -> Result<Located, Miss> {
    let stale = |why: &str| Miss::new(MissKind::Stale, format!("Not done: since you last looked, {why}. Nothing was clicked or typed. Take a fresh screenshot."));
    if winmgr::is_minimized(h) {
        return Err(stale("the window was minimized"));
    }
    let Some((l, t, r, b)) = winmgr::rect_of(h) else { return Err(stale("the window has closed")) };
    let size = ((f.width as f64 / f.scale).round() as i32, (f.height as f64 / f.scale).round() as i32);
    if ((r - l) - size.0).abs() > 3 || ((b - t) - size.1).abs() > 3 {
        return Err(stale("the window has been resized, so what was at that point has moved"));
    }
    let (nx, ny) = (x + l - f.left, y + t - f.top);
    let note = if (nx, ny) != (x, y) { " (the window had moved since the screenshot; the point moved with it)".to_string() } else { String::new() };
    Ok(Located { point: (nx, ny), hwnd: h, scene: Some(h), live: None, desc: format!("({px},{py})"), note })
}

/// How long a window brought forward may take to be drawn in front
const FRONT_TAKES: Duration = Duration::from_millis(300);

/// Make sure a real click on a located control reaches it: when a window of another program covers the
/// spot, the control's own window is brought forward first. A plain point is clicked as it is.
pub fn uncover(loc: &Located) -> Result<(), String> {
    // A point on the screenshot is clicked where it is: it was checked against what the AI saw
    if loc.live.is_none() && loc.scene.is_none() {
        return Ok(());
    }
    let (x, y) = loc.point;
    let pid = winmgr::pid_of(loc.hwnd);
    let reaches = || winmgr::top_window_at(x, y).filter(|t| *t != loc.hwnd && winmgr::pid_of(*t) != pid);
    if reaches().is_none() {
        return Ok(());
    }
    let main = loc.scene.unwrap_or(loc.hwnd);
    winmgr::bring_to_front(winmgr::hwnd(main))?;
    crate::until::holds(FRONT_TAKES, || reaches().is_none());
    match reaches() {
        None => Ok(()),
        Some(cover) => Err(format!(
            "{} is covered by \"{}\", which stays in front; move or close that window (or use mode: \"direct\" to press it without the mouse)",
            loc.desc,
            uia::clip(&winmgr::title_of(cover), 50)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::winmgr::Slot;

    fn node(rf: u32, depth: usize, role: &'static str, name: &str, flags: u32, x: i32, y: i32) -> Node {
        Node { rf, depth, role, name: name.into(), value: String::new(), rect: (x, y, x + 80, y + 20), flags }
    }

    fn scene(nodes: Vec<Node>) -> Scene {
        Scene { hwnd: 1, title: "T".into(), nodes, ..Default::default() }
    }

    fn one(p: Pick) -> u32 {
        match p {
            Pick::One(n) => n.rf,
            Pick::Many(c) => panic!("ambiguous: {:?}", c.iter().map(|n| n.rf).collect::<Vec<_>>()),
            Pick::None => panic!("nothing found"),
        }
    }

    #[test]
    fn references_are_told_from_text() {
        assert_eq!(as_ref("e12"), Some(12));
        assert_eq!(as_ref("[E7]"), Some(7));
        assert_eq!(as_ref("e"), None);
        assert_eq!(as_ref("edit"), None);
        assert_eq!(as_ref("12"), None, "bare digits are text: a calculator key, a day in a calendar");
        assert_eq!(as_ref("e0"), None);
    }

    #[test]
    fn an_exact_name_beats_a_longer_one_and_a_button_beats_its_label() {
        let s = scene(vec![
            node(1, 0, "Button", "Save As...", flag::INVOKE, 0, 0),
            node(2, 0, "Text", "Save", 0, 0, 30),
            node(3, 0, "Button", "&Save", flag::INVOKE, 0, 60),
        ]);
        assert_eq!(one(pick(&s, "save", "", None, false)), 3);
        assert_eq!(one(pick(&s, "Save As", "", None, false)), 1);
        assert_eq!(one(pick(&s, "save", "text", None, false)), 2, "a role narrows it");
    }

    #[test]
    fn equal_matches_are_not_guessed_between() {
        let s = scene(vec![node(1, 0, "Button", "Delete", flag::INVOKE, 0, 100), node(2, 0, "Button", "Delete", flag::INVOKE, 0, 20)]);
        assert!(matches!(pick(&s, "Delete", "", None, false), Pick::Many(c) if c.len() == 2));
        assert_eq!(one(pick(&s, "Delete", "", Some(1), false)), 2, "nth counts in reading order, top first");
        assert_eq!(one(pick(&s, "Delete", "", Some(2), false)), 1);
        assert!(matches!(pick(&s, "Delete", "", Some(3), false), Pick::Many(_)));
    }

    #[test]
    fn a_control_and_its_inner_text_are_one_match() {
        let s = scene(vec![node(1, 0, "ListItem", "report.docx", flag::SELECT, 0, 0), node(2, 1, "Text", "report.docx", 0, 0, 0), node(3, 0, "Text", "other", 0, 0, 40)]);
        assert_eq!(one(pick(&s, "report.docx", "", None, false)), 1);
        // A named pane holding the real link: the link is the thing to click
        let s = scene(vec![node(1, 0, "Group", "Sign in", 0, 0, 0), node(2, 1, "Link", "Sign in", flag::INVOKE, 0, 0)]);
        assert_eq!(one(pick(&s, "sign in", "", None, false)), 2);
    }

    #[test]
    fn typing_prefers_the_box_over_its_label() {
        let mut edit = node(2, 0, "Edit", "Name", flag::VALUE, 100, 0);
        edit.value = "old".into();
        let s = scene(vec![node(1, 0, "Text", "Name", 0, 0, 0), edit]);
        assert_eq!(one(pick(&s, "Name", "", None, true)), 2);
        assert_eq!(one(pick(&s, "old", "", None, true)), 2, "a box can be named by what it holds");
    }

    #[test]
    fn nothing_found_offers_the_nearest_names() {
        let s = scene(vec![node(1, 0, "Button", "Submit order", flag::INVOKE, 0, 0), node(2, 0, "Button", "Cancel", flag::INVOKE, 0, 30)]);
        assert!(matches!(pick(&s, "Sumbit", "", None, false), Pick::None));
        let near = closest(&s, &uia::norm("Submit"));
        assert_eq!(near, vec!["Button \"Submit order\" [e1]".to_string()]);
    }

    fn slot(handle: isize, pid: u32, rect: winmgr::Rect) -> Slot {
        Slot { handle, pid, rect }
    }

    #[test]
    fn a_click_is_checked_against_what_was_seen() {
        let title = |h: isize| format!("w{h}");
        let app = slot(1, 10, (100, 100, 500, 400));
        let desk = slot(9, 1, (0, 0, 1920, 1080));
        let seen = vec![app, desk];
        // Nothing changed
        assert_eq!(guard(&seen, &seen, 200, 200, &title), Ok((200, 200, false)));
        // Nothing was ever seen: nothing to check against
        assert_eq!(guard(&[], &seen, 200, 200, &title), Ok((200, 200, false)));
        // The window moved: the point moves with it
        let moved = vec![slot(1, 10, (150, 120, 550, 420)), desk];
        assert_eq!(guard(&seen, &moved, 200, 200, &title), Ok((250, 220, true)));
        // Another program's window came in front
        let covered = vec![slot(2, 20, (150, 150, 300, 300)), app, desk];
        assert!(guard(&seen, &covered, 200, 200, &title).unwrap_err().contains("\"w2\" is now in front"));
        // ... but not over the point being clicked
        assert_eq!(guard(&seen, &covered, 450, 350, &title), Ok((450, 350, false)));
        // The program's own menu over it is fine
        let menu = vec![slot(3, 10, (150, 150, 300, 300)), app, desk];
        assert_eq!(guard(&seen, &menu, 200, 200, &title), Ok((200, 200, false)));
        // Closed, resized
        assert!(guard(&seen, &[desk], 200, 200, &title).unwrap_err().contains("closed"));
        let resized = vec![slot(1, 10, (100, 100, 900, 700)), desk];
        assert!(guard(&seen, &resized, 200, 200, &title).unwrap_err().contains("resized"));
        // A window opened where there was only desktop
        assert!(guard(&seen, &covered, 160, 160, &title).is_err());
        assert!(guard(&[desk], &seen, 200, 200, &title).unwrap_err().contains("in front"));
    }
}
