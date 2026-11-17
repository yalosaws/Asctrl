//! The tools an AI drives the computer with.
//!
//! Three ideas run through all of them:
//! - One way to point: every action takes a `target` (a control's reference such as "e12", or the text it
//!   shows) or `x`, `y` on the latest screenshot. See `locate`.
//! - Look once: `observe` reads a window's controls as text with references that stay valid; after that,
//!   every action's reply says what changed, so the AI rarely has to look again. See `report`.
//! - Never act on a stale picture: a click by coordinates is checked against the windows as they were when
//!   the AI last looked, and a control is brought into view and uncovered before it is clicked.

use crate::{capture, fx, input, screen, uia, winmgr};

mod apps;
mod batch;
mod grid;
mod keys;
mod locate;
mod look;
mod pointer;
pub mod popups;
mod recover;
mod report;
mod system;
mod values;
mod wait;
mod watch;

use base64::Engine;
use serde::Serialize;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Serialize, Default)]
pub struct Output {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<Image>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<FileData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Structured results, for a program reading the reply (a plan's runner reads a batch's steps from it)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Output {
    /// The text of the result, whichever way it went
    pub fn body(&self) -> &str {
        self.text.as_deref().or(self.error.as_deref()).unwrap_or("")
    }
}

/// The kind of miss a reply carries (see `locate::MissKind`), if it is one
pub fn miss_kind(data: Option<&Value>) -> Option<&str> {
    data?.get("miss")?.get("kind")?.as_str()
}

/// A probe (run trying a step as written before deciding another way) that missed: not a failure worth
/// flashing on the overlay, since the step goes on another way
pub fn probe_missed(args: &Value, out: &Output) -> bool {
    let unresolved = |d: Option<&Value>| matches!(miss_kind(d), Some("ambiguous" | "absent"));
    if out.ok {
        return false;
    }
    // A batch that stopped at such a probe: the plan's runner carries on from that step
    let failed = out.data.as_ref().and_then(|d| Some((d.get("failed")?.as_u64()? as usize, d.get("steps")?.as_array()?)));
    if let (Some(steps), Some((i, results))) = (args.get("steps").and_then(Value::as_array), failed) {
        let probe = steps.get(i).is_some_and(|s| bool_arg(s, "probe") == Some(true));
        return probe && unresolved(results.get(i).and_then(|r| r.get("data")));
    }
    bool_arg(args, "probe") == Some(true) && unresolved(out.data.as_ref())
}

#[derive(Serialize)]
pub struct Image {
    pub data: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
}

#[derive(Serialize)]
pub struct FileData {
    pub name: String,
    pub data: String,
}

/// The picture the AI's coordinates refer to: where the latest screenshot sits on the screen and how much it
/// was scaled. Before any screenshot it is the picture a plain screenshot would give, so the positions in
/// text replies (an observe before any screenshot) stay good once that screenshot is taken.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub left: i32,
    pub top: i32,
    pub scale: f64,
    pub width: i32,
    pub height: i32,
    /// The picture is one window as it draws itself, taken while others covered it (see `wgc`): its points
    /// belong to that window, wherever it is and whatever is in front of it now
    pub own: Option<isize>,
}

impl Frame {
    /// The picture a capture of `area` limited to `max_width` gives
    pub fn of(area: capture::Area, max_width: i32) -> Frame {
        let (scale, width, height) = capture::scaled(area, max_width);
        Frame { left: area.left, top: area.top, scale, width, height, own: None }
    }

    /// Picture coordinates -> screen coordinates
    pub fn to_screen(&self, x: f64, y: f64) -> (i32, i32) {
        (self.left + (x / self.scale).round() as i32, self.top + (y / self.scale).round() as i32)
    }

    /// Screen coordinates -> picture coordinates; None when outside the picture
    pub fn to_shot(&self, x: i32, y: i32) -> Option<(i32, i32)> {
        let px = ((x - self.left) as f64 * self.scale).round() as i32;
        let py = ((y - self.top) as f64 * self.scale).round() as i32;
        (px >= 0 && py >= 0 && px <= self.width && py <= self.height).then_some((px, py))
    }

    /// A screen point as the AI should read it: @(x,y) on the picture, else @screen(x,y)
    pub fn at(&self, x: i32, y: i32) -> String {
        match self.to_shot(x, y) {
            Some((px, py)) => format!("@({px},{py})"),
            None => format!("@screen({x},{y})"),
        }
    }

    /// A screen rectangle as the AI should read it
    pub fn rect(&self, (l, t, r, b): winmgr::Rect) -> String {
        match (self.to_shot(l, t), self.to_shot(r, b)) {
            (Some((x0, y0)), Some((x1, y1))) => format!("({x0},{y0})-({x1},{y1})"),
            _ => format!("screen ({l},{t})-({r},{b})"),
        }
    }

    /// A point given in arguments `kx`, `ky`, checked to be on the picture
    pub fn point(&self, args: &Value, kx: &str, ky: &str) -> Result<(i32, i32), String> {
        let x = args.get(kx).and_then(Value::as_f64).ok_or(format!("Missing {kx}"))?;
        let y = args.get(ky).and_then(Value::as_f64).ok_or(format!("Missing {ky}"))?;
        if x < 0.0 || y < 0.0 || x > self.width as f64 || y > self.height as f64 {
            return Err(format!("({x},{y}) is outside the picture ({}x{}); coordinates refer to the latest screenshot", self.width, self.height));
        }
        Ok(self.to_screen(x, y))
    }

    /// The rectangle given as x0, y0, x1, y1 in `args`, in screen pixels
    pub fn area(&self, args: &Value, min: i32) -> Result<capture::Area, String> {
        let (a, b) = (self.point(args, "x0", "y0")?, self.point(args, "x1", "y1")?);
        let (l, t, r, btm) = (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
        if r - l < min || btm - t < min {
            return Err("Region is too small".into());
        }
        Ok(capture::Area { left: l, top: t, width: r - l, height: btm - t })
    }
}

/// What a screenshot shows: everything, one display, or one window
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Scope {
    Screen,
    Display(usize),
    Window(isize),
}

/// The view automatic screenshots use: the one the AI last asked for
#[derive(Clone, Copy)]
pub struct View {
    pub scope: Scope,
    pub max_width: i32,
}

pub struct Ctx {
    frame: Mutex<Option<Frame>>,
    view: Mutex<View>,
    /// The windows as they were stacked when the AI last looked (a screenshot, a listing, a reply that
    /// described the screen). A click by coordinates is checked against it.
    seen: Mutex<Arc<Vec<winmgr::Slot>>>,
    shell: tokio::sync::Mutex<Option<crate::shell::Shell>>,
    /// Pid of the session's PowerShell, kept outside `shell` so a reset can kill it while a command holds that lock
    shell_pid: Mutex<Option<u32>>,
    jobs: Mutex<crate::jobs::Jobs>,
    /// Window the last open_app brought up, for the overlay to land on
    opened: Mutex<Option<isize>>,
}

impl Default for Ctx {
    fn default() -> Self {
        Ctx {
            frame: Mutex::new(None),
            view: Mutex::new(View { scope: Scope::Screen, max_width: 1600 }),
            seen: Mutex::new(Arc::new(vec![])),
            shell: Default::default(),
            shell_pid: Mutex::new(None),
            jobs: Default::default(),
            opened: Mutex::new(None),
        }
    }
}

impl Ctx {
    /// Called when the controller disconnects: ends the PowerShell session and all background jobs, and
    /// forgets what the last controller saw
    pub async fn reset(&self) {
        self.reset_shell().await;
        *self.frame.lock().unwrap() = None;
        *self.view.lock().unwrap() = View { scope: Scope::Screen, max_width: 1600 };
        *self.seen.lock().unwrap() = Arc::new(vec![]);
        popups::stop("");
        uia::reset();
    }

    pub async fn reset_shell(&self) {
        // Kill by pid first: a command still running holds the `shell` lock until it ends, and the whole
        // point of a reset (the controller left, the user ended the session) is to stop it now.
        let pid = self.shell_pid.lock().unwrap().take();
        let mut jobs = std::mem::take(&mut *self.jobs.lock().unwrap());
        let _ = tokio::task::spawn_blocking(move || {
            if let Some(pid) = pid {
                crate::jobs::kill_tree(pid);
            }
            jobs.stop_all();
        })
        .await;
        if let Some(s) = self.shell.lock().await.take() {
            s.kill();
        }
    }

    pub fn frame(&self) -> Frame {
        let taken = *self.frame.lock().unwrap();
        taken.unwrap_or_else(|| Frame::of(capture::virtual_screen(), self.view().max_width))
    }

    fn view(&self) -> View {
        *self.view.lock().unwrap()
    }

    fn seen(&self) -> Arc<Vec<winmgr::Slot>> {
        self.seen.lock().unwrap().clone()
    }

    /// The AI is being shown the screen as it is now: later clicks by coordinates are checked against this
    fn saw(&self, layout: Vec<winmgr::Slot>) {
        *self.seen.lock().unwrap() = Arc::new(layout);
    }
}

const MAX_TEXT: usize = 60_000;

fn fail(msg: impl Into<String>) -> Output {
    Output { ok: false, error: Some(msg.into()), ..Default::default() }
}

fn text(mut s: String) -> Output {
    let total = s.chars().count();
    if total > MAX_TEXT {
        s = s.chars().take(MAX_TEXT).collect::<String>() + &format!("\n… (truncated, {total} characters total)");
    }
    Output { ok: true, text: Some(s), ..Default::default() }
}

fn int_arg(args: &Value, key: &str, def: i64, min: i64, max: i64) -> i64 {
    args.get(key).and_then(Value::as_f64).map(|f| (f.round() as i64).clamp(min, max)).unwrap_or(def)
}

fn str_arg<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("")
}

fn bool_arg(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

async fn blocking(f: impl FnOnce() -> Result<String, String> + Send + 'static) -> Output {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(s)) => text(s),
        Ok(Err(e)) => fail(e),
        Err(e) => fail(e.to_string()),
    }
}

/// There is one real mouse, keyboard and focus: actions run one at a time, so two sent in parallel can't
/// interleave (one moving the pointer while the other presses, or typing into the other's window)
static ACTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Take the turn to act. A paste puts the clipboard back a moment after it returns; whatever acts next
/// waits for that first, so it never sees (or overwrites) the borrowed clipboard.
async fn acting() -> tokio::sync::MutexGuard<'static, ()> {
    let turn = ACTING.lock().await;
    let _ = tokio::task::spawn_blocking(keys::clipboard_settled).await;
    turn
}

/// No change within this long after a mouse or keyboard action: nothing happened (or not yet)
const QUIET: Duration = Duration::from_millis(300);
/// The same after an action the program itself confirmed (a control pressed or set through UI Automation):
/// it was delivered for certain, and its effect shows within a frame or two
const QUIET_DIRECT: Duration = Duration::from_millis(120);
/// Never wait longer than this for the screen to stop changing after an action
const SETTLE_MAX: Duration = Duration::from_millis(1500);
/// In fast mode, the pause after each input so the program has taken it before the next one arrives
const FAST_GAP: Duration = Duration::from_millis(25);

tokio::task_local! {
    /// Set by batch fast: input steps don't wait for the screen to settle, only for the program to take the input
    static FAST: bool;
    /// Set by a watch argument: the region (screen pixels) whose changes count after an input step
    static WATCH: capture::Area;
    /// Set inside a batch: steps say what they did in a line; the batch reports the outcome once at the end
    static STEP: bool;
}

/// How an action waits for the screen afterwards, decided before it runs (task-locals don't cross threads)
#[derive(Clone, Copy)]
struct Pace {
    fast: bool,
    watch: Option<capture::Area>,
}

impl Pace {
    fn current() -> Pace {
        Pace { fast: FAST.try_with(|f| *f).unwrap_or(false), watch: WATCH.try_with(|w| *w).ok() }
    }

    /// The screen before an action at `at` (None: a keystroke), to compare with afterwards
    fn before(&self, at: Option<(i32, i32)>) -> Option<screen::Before> {
        if self.fast {
            return None;
        }
        Some(match self.watch {
            Some(area) => screen::before_in(area, at),
            None => screen::before(at),
        })
    }

    /// Wait just until the screen has reacted and stopped changing; in fast mode, only for the input to be taken
    fn after(&self, before: Option<screen::Before>) -> Option<screen::Change> {
        self.after_within(before, QUIET)
    }

    /// The same after an action the program confirmed, which needs less patience
    fn after_direct(&self, before: Option<screen::Before>) -> Option<screen::Change> {
        self.after_within(before, QUIET_DIRECT)
    }

    fn after_within(&self, before: Option<screen::Before>, quiet: Duration) -> Option<screen::Change> {
        match before {
            Some(b) => Some(screen::settle(&b, quiet, SETTLE_MAX)),
            None => {
                std::thread::sleep(FAST_GAP);
                None
            }
        }
    }
}

/// The region a tool's or batch's `watch` argument frames ({x0, y0, x1, y1} on the latest screenshot), if any
fn watch_arg(ctx: &Ctx, args: &Value) -> Result<Option<capture::Area>, String> {
    match args.get("watch").filter(|w| !w.is_null()) {
        None => Ok(None),
        Some(w) => ctx.frame().area(w, 8).map(Some).map_err(|e| format!("watch: {e}")),
    }
}

/// Remote paths: supports ~ and %ENV_VARS%; relative paths are resolved against the user's home
fn resolve_path(p: &str) -> Result<std::path::PathBuf, String> {
    let p = p.trim();
    if p.is_empty() {
        return Err("path is empty".into());
    }
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let mut s = p.replacen('~', &home, if p.starts_with('~') { 1 } else { 0 });
    // Expand left to right and continue after each value, so a value that itself contains % is never expanded again
    let mut from = 0;
    while let Some(a) = s[from..].find('%').map(|i| from + i) {
        let Some(b) = s[a + 1..].find('%').map(|i| a + 1 + i) else { break };
        let val = std::env::var(&s[a + 1..b]).map_err(|_| format!("Environment variable {} does not exist", &s[a + 1..b]))?;
        s.replace_range(a..=b, &val);
        from = a + val.len();
    }
    let path = std::path::PathBuf::from(s);
    Ok(if path.is_absolute() { path } else { std::path::PathBuf::from(home).join(path) })
}

fn path_arg(args: &Value, key: &str) -> Result<std::path::PathBuf, String> {
    resolve_path(str_arg(args, key))
}

/// Capture on a blocking thread and wrap the result as a tool image
async fn shoot(area: capture::Area, max_width: i32) -> Result<(capture::Shot, Image), String> {
    let shot = tokio::task::spawn_blocking(move || capture::grab(area, max_width)).await.map_err(|e| e.to_string())??;
    let image = Image { data: base64::engine::general_purpose::STANDARD.encode(&shot.jpeg), mime_type: "image/jpeg".into() };
    Ok((shot, image))
}

type BoxedRun<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Output> + Send + 'a>>;

/// Run a tool
pub async fn run(ctx: &Ctx, tool: &str, args: &Value) -> Output {
    run_tool(ctx, tool, args).await
}

/// Boxed so batch can run tools from inside a tool. A watch argument frames the region input steps wait on.
fn run_tool<'a>(ctx: &'a Ctx, tool: &'a str, args: &'a Value) -> BoxedRun<'a> {
    Box::pin(async move {
        match watch_arg(ctx, args) {
            Err(e) => fail(e),
            Ok(Some(area)) => WATCH.scope(area, dispatch(ctx, tool, args)).await,
            Ok(None) => dispatch(ctx, tool, args).await,
        }
    })
}

async fn dispatch(ctx: &Ctx, tool: &str, args: &Value) -> Output {
    match tool {
        // Looking
        "observe" => look::observe(ctx, args).await,
        "screenshot" => look::screenshot(ctx, args).await,
        "read_text" => look::read_text(ctx, args).await,
        "wait_for" => wait::wait_for(ctx, args).await,
        "read_grid" => grid::read_grid(ctx, args).await,
        "watch" => watch::watch(ctx, args).await,
        // Acting
        "click" => pointer::click(ctx, args).await,
        "scroll" => pointer::scroll(ctx, args).await,
        "drag" => pointer::drag(ctx, args).await,
        "mouse" => pointer::mouse(ctx, args).await,
        "type_text" => keys::type_text(ctx, args).await,
        "press_keys" => keys::press_keys(ctx, args).await,
        "set_value" => values::set_value(ctx, args).await,
        "element_action" => values::element_action(ctx, args).await,
        "batch" => batch::batch(ctx, args).await,
        // Programs, windows, clipboard
        "open_app" => apps::open_app(ctx, args).await,
        "focus_window" => apps::focus_window(ctx, args).await,
        // With text: put it on the clipboard; without: say what is on it
        "clipboard" => {
            let _turn = acting().await;
            match args.get("text").and_then(Value::as_str).map(str::to_string) {
                Some(t) => blocking(move || winmgr::set_clipboard_text(&t).map(|_| format!("Copied to clipboard ({} chars)", t.chars().count()))).await,
                None => blocking(winmgr::clipboard_describe).await,
            }
        }
        // Shell, files, system
        other => system::run(ctx, other, args).await,
    }
}

// ---- Geometry for the overlay's animations (physical screen pixels) ----

/// Before a tool runs: what the overlay can be told from the arguments alone. Actions that point at
/// something send their own animation once they know where it is.
pub fn fx_before(ctx: &Ctx, tool: &str, args: &Value) -> Value {
    use serde_json::json;
    let f = ctx.frame();
    let pt = |kx: &str, ky: &str| f.point(args, kx, ky).ok().map(fx::pt_json);
    match tool {
        "screenshot" if args.get("x0").is_some() => json!({ "from": pt("x0", "y0"), "to": pt("x1", "y1") }),
        "focus_window" => match winmgr::find_arg(args.get("window")) {
            Ok(Some(h)) => {
                let h = h.0 as isize;
                json!({ "hwnd": h, "rect": winmgr::rect_of(h).map(fx::rect_json) })
            }
            _ => json!({}),
        },
        _ => json!({}),
    }
}

/// After a tool runs: which part of the screen was captured, what the window became
pub fn fx_after(ctx: &Ctx, tool: &str, before: &Value, out: &Output) -> Value {
    use serde_json::json;
    match tool {
        "screenshot" | "observe" if out.image.is_some() => {
            let g = ctx.frame();
            json!({ "shot": [g.left, g.top, (g.width as f64 / g.scale).round(), (g.height as f64 / g.scale).round()] })
        }
        "focus_window" => match before.get("hwnd").and_then(Value::as_i64) {
            Some(h) => json!({ "rect": winmgr::rect_of(h as isize).map(fx::rect_json) }),
            None => json!({}),
        },
        // The program's window, wherever it is (a console program has none)
        "open_app" => {
            let h = ctx.opened.lock().unwrap().take();
            json!({ "rect": h.and_then(winmgr::rect_of).map(fx::rect_json) })
        }
        _ => json!({}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_maps_both_ways_and_rejects_points_off_the_picture() {
        let f = Frame { left: 100, top: 50, scale: 0.5, width: 800, height: 450, own: None };
        assert_eq!(f.to_screen(400.0, 100.0), (900, 250));
        assert_eq!(f.to_shot(900, 250), Some((400, 100)));
        assert_eq!(f.to_shot(99, 250), None);
        assert_eq!(f.at(900, 250), "@(400,100)");
        assert_eq!(f.at(5000, 250), "@screen(5000,250)");
        let args = serde_json::json!({ "x": 801, "y": 10 });
        assert!(f.point(&args, "x", "y").is_err());
        let args = serde_json::json!({ "x": 800, "y": 450 });
        assert_eq!(f.point(&args, "x", "y"), Ok((1700, 950)));
    }

    #[test]
    fn the_frame_before_a_screenshot_is_the_one_the_screenshot_gives() {
        let area = capture::Area { left: -640, top: 0, width: 3200, height: 1440 };
        let f = Frame::of(area, 1600);
        assert_eq!((f.left, f.top, f.scale, f.width, f.height), (-640, 0, 0.5, 1600, 720));
        assert_eq!(f.to_screen(1600.0, 0.0), (2560, 0));
        // A screen no wider than the limit is not scaled
        let small = Frame::of(capture::Area { left: 0, top: 0, width: 1366, height: 768 }, 1600);
        assert_eq!((small.scale, small.width, small.height), (1.0, 1366, 768));
    }

    #[test]
    fn a_batch_stopped_by_a_missed_probe_is_no_failure_to_flash() {
        let step = |kind: Option<&str>| match kind {
            Some(k) => serde_json::json!({ "ok": false, "text": "", "data": { "miss": { "kind": k, "candidates": [] } } }),
            None => serde_json::json!({ "ok": false, "text": "Access denied" }),
        };
        let out = |kind: Option<&str>| Output { ok: false, data: Some(serde_json::json!({ "failed": 1, "steps": [{ "ok": true, "text": "" }, step(kind)] })), ..Default::default() };
        let args = |probe: bool| serde_json::json!({ "steps": [{ "tool": "keys" }, { "tool": "click", "target": "send", "probe": probe }] });
        assert!(probe_missed(&args(true), &out(Some("absent"))));
        assert!(probe_missed(&args(true), &out(Some("ambiguous"))));
        // Not a probe, or a probe that failed some other way: a real failure
        assert!(!probe_missed(&args(false), &out(Some("absent"))));
        assert!(!probe_missed(&args(true), &out(Some("stale"))));
        assert!(!probe_missed(&args(true), &out(None)));
        // A single call
        let one = Output { ok: false, data: Some(report::miss_data(locate::MissKind::Absent, &[])), ..Default::default() };
        assert!(probe_missed(&serde_json::json!({ "target": "send", "probe": true }), &one));
    }

    #[test]
    fn paths_expand_home_and_environment() {
        unsafe { std::env::set_var("ASCTRL_T", "C:\\x") };
        assert_eq!(resolve_path("%ASCTRL_T%\\a.txt").unwrap(), std::path::PathBuf::from("C:\\x\\a.txt"));
        assert!(resolve_path("%NOPE_NOT_SET_ASCTRL%\\a").is_err());
        assert!(resolve_path("  ").is_err());
    }
}
