//! Tools the agent can run. The system_info / list_processes / event_log scripts match the Node version in src/agent/tools.ts.

use crate::{capture, files, input, jobs, jobs::Jobs, ocr, pace, powershell, screen, shell::Shell, uia, winmgr};

mod batch;
mod text_tools;
mod wait;
use base64::Engine;
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Mutex;
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

/// Position and scale of the latest screenshot, used to map screenshot coordinates to screen coordinates
#[derive(Clone, Copy)]
struct Geom {
    left: i32,
    top: i32,
    scale: f64,
    width: i32,
    height: i32,
}

impl Geom {
    /// Screenshot coordinates -> screen coordinates
    fn shot_to_screen(&self, x: f64, y: f64) -> (i32, i32) {
        (self.left + (x / self.scale).round() as i32, self.top + (y / self.scale).round() as i32)
    }

    /// Screen coordinates -> screenshot coordinates; None when outside this screenshot
    fn screen_to_shot(&self, x: i32, y: i32) -> Option<(i32, i32)> {
        let px = ((x - self.left) as f64 * self.scale).round() as i32;
        let py = ((y - self.top) as f64 * self.scale).round() as i32;
        (px >= 0 && py >= 0 && px <= self.width && py <= self.height).then_some((px, py))
    }
}

#[derive(Default)]
pub struct Ctx {
    geom: Mutex<Option<Geom>>,
    shell: tokio::sync::Mutex<Option<Shell>>,
    /// Pid of the session's PowerShell, kept outside `shell` so a reset can kill it while a command holds that lock
    shell_pid: Mutex<Option<u32>>,
    jobs: Mutex<Jobs>,
    /// Window the last open_app brought up, for the overlay to land on
    opened: Mutex<Option<isize>>,
}

impl Ctx {
    /// Called when the technician disconnects: ends the PowerShell session and all background jobs
    pub async fn reset(&self) {
        // Kill by pid first: a command still running holds the `shell` lock until it ends, and the whole
        // point of a reset (the technician left, the user ended the session) is to stop it now.
        let pid = self.shell_pid.lock().unwrap().take();
        let mut jobs = std::mem::take(&mut *self.jobs.lock().unwrap());
        let _ = tokio::task::spawn_blocking(move || {
            if let Some(pid) = pid {
                jobs::kill_tree(pid);
            }
            jobs.stop_all();
        })
        .await;
        if let Some(s) = self.shell.lock().await.take() {
            s.kill();
        }
    }
}

const MAX_FILE: usize = 20 * 1024 * 1024;

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

async fn ps_text(script: &str, timeout_secs: u64) -> Output {
    let r = powershell::run(script, Duration::from_secs(timeout_secs)).await;
    if r.timed_out {
        return fail(format!("Timed out ({timeout_secs} s)"));
    }
    let mut parts = vec![r.stdout.trim().to_string()];
    if !r.stderr.trim().is_empty() {
        parts.push(format!("[stderr]\n{}", r.stderr.trim()));
    }
    if r.exit_code != Some(0) {
        parts.push(format!("[exit code {:?}]", r.exit_code));
    }
    let joined = parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("\n\n");
    text(if joined.is_empty() { "(no output)".into() } else { joined })
}

fn int_arg(args: &Value, key: &str, def: i64, min: i64, max: i64) -> i64 {
    args.get(key).and_then(Value::as_f64).map(|f| (f.round() as i64).clamp(min, max)).unwrap_or(def)
}

const SYSTEM_INFO_PS: &str = r#"
$os = Get-CimInstance Win32_OperatingSystem
$cs = Get-CimInstance Win32_ComputerSystem
$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
$disks = Get-CimInstance Win32_LogicalDisk -Filter "DriveType=3" | ForEach-Object {
  [pscustomobject]@{ Drive=$_.DeviceID; SizeGB=[math]::Round($_.Size/1GB,1); FreeGB=[math]::Round($_.FreeSpace/1GB,1) }
}
[pscustomobject]@{
  ComputerName = $env:COMPUTERNAME
  User         = $env:USERNAME
  OS           = "$($os.Caption) $($os.Version) build $($os.BuildNumber)"
  LastBoot     = $os.LastBootUpTime.ToString('yyyy-MM-dd HH:mm')
  UptimeHours  = [math]::Round(((Get-Date) - $os.LastBootUpTime).TotalHours, 1)
  Model        = "$($cs.Manufacturer) $($cs.Model)"
  CPU          = $cpu.Name.Trim()
  CpuLoadPct   = $cpu.LoadPercentage
  RamGB        = [math]::Round($cs.TotalPhysicalMemory/1GB,1)
  RamFreeGB    = [math]::Round($os.FreePhysicalMemory*1KB/1GB,1)
  Disks        = $disks
} | ConvertTo-Json -Depth 4
"#;

const PROCESSES_PS: &str = r#"
Get-Process | Sort-Object WorkingSet64 -Descending | Select-Object -First __TOP__ @{n='Name';e={$_.ProcessName}}, Id,
  @{n='MemMB';e={[math]::Round($_.WorkingSet64/1MB)}}, @{n='CpuSec';e={[math]::Round($_.CPU,1)}},
  @{n='Title';e={$_.MainWindowTitle}} | Format-Table -AutoSize | Out-String -Width 200
"#;

const EVENT_LOG_PS: &str = r#"
$f = @{ LogName='__LOG__'; Level=1,2,3; StartTime=(Get-Date).AddHours(-__HOURS__) }
Get-WinEvent -FilterHashtable $f -MaxEvents __MAX__ -ErrorAction SilentlyContinue |
  Select-Object @{n='Time';e={$_.TimeCreated.ToString('MM-dd HH:mm:ss')}}, LevelDisplayName, ProviderName, Id,
  @{n='Message';e={($_.Message -split "`n")[0]}} | Format-List | Out-String -Width 300
"#;

const CLIPBOARD_READ_PS: &str = r#"
Add-Type -AssemblyName System.Windows.Forms
$f = Get-Clipboard -Format FileDropList
if ($f) { "[copied files]"; $f | ForEach-Object { $_.FullName }; return }
$img = Get-Clipboard -Format Image
if ($img) { "[clipboard holds an image $($img.Width)x$($img.Height)]"; return }
$t = Get-Clipboard -Format Text -Raw
if ($null -eq $t -or $t -eq '') { '(clipboard is empty)' } else { $t }
"#;

fn str_arg<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("")
}

fn geom(ctx: &Ctx) -> Result<Geom, String> {
    ctx.geom.lock().unwrap().ok_or_else(|| "Call screenshot first; coordinates are relative to the screenshot".to_string())
}

/// Screenshot coordinates -> screen coordinates
fn to_screen(ctx: &Ctx, args: &Value, kx: &str, ky: &str) -> Result<(i32, i32), String> {
    shot_to_screen(*ctx.geom.lock().unwrap(), args, kx, ky)
}

/// to_screen with the screenshot geometry already in hand (for work on a blocking thread)
fn shot_to_screen(g: Option<Geom>, args: &Value, kx: &str, ky: &str) -> Result<(i32, i32), String> {
    let g = g.ok_or_else(|| "Call screenshot first; coordinates are relative to the screenshot".to_string())?;
    let x = args.get(kx).and_then(Value::as_f64).ok_or(format!("Missing {kx}"))?;
    let y = args.get(ky).and_then(Value::as_f64).ok_or(format!("Missing {ky}"))?;
    if x < 0.0 || y < 0.0 || x > g.width as f64 || y > g.height as f64 {
        return Err(format!("Coordinates are outside the screenshot ({}x{})", g.width, g.height));
    }
    Ok(g.shot_to_screen(x, y))
}

/// Optional coordinates: None when neither is given
fn opt_screen(ctx: &Ctx, args: &Value) -> Result<Option<(i32, i32)>, String> {
    if args.get("x").is_none() && args.get("y").is_none() {
        return Ok(None);
    }
    to_screen(ctx, args, "x", "y").map(Some)
}

/// Capture on a blocking thread and wrap the result as a tool image
async fn shoot(area: capture::Area, max_width: i64) -> Result<(capture::Shot, Image), String> {
    let shot = tokio::task::spawn_blocking(move || capture::grab(area, max_width as i32)).await.map_err(|e| e.to_string())??;
    let image = Image { data: base64::engine::general_purpose::STANDARD.encode(&shot.jpeg), mime_type: "image/jpeg".into() };
    Ok((shot, image))
}

/// Tell the AI about the other displays when there are several
fn describe_displays(ds: &[capture::Display]) -> String {
    if ds.len() <= 1 {
        return String::new();
    }
    let list = ds
        .iter()
        .enumerate()
        .map(|(i, d)| format!("#{} {}x{}{}", i + 1, d.area.width, d.area.height, if d.primary { " (primary)" } else { "" }))
        .collect::<Vec<_>>()
        .join(", ");
    format!("\n{} displays: {list}. Pass `display` to capture just one, for a clearer image.", ds.len())
}

fn win_line(g: Option<Geom>, w: &winmgr::Win) -> String {
    let mut flags = vec![];
    if w.foreground {
        flags.push("foreground");
    }
    if w.minimized {
        flags.push("minimized");
    }
    if w.maximized {
        flags.push("maximized");
    }
    let (l, t, r, b) = w.rect;
    let pos = if w.minimized {
        String::new()
    } else {
        match g.and_then(|g| g.screen_to_shot((l + r) / 2, (t + b) / 2)) {
            Some((cx, cy)) => format!(" centre at screenshot ({cx},{cy})"),
            None => " outside the current screenshot".into(),
        }
    };
    let flags = if flags.is_empty() { String::new() } else { format!(" [{}]", flags.join(", ")) };
    format!("[{}] {} (pid {}) \"{}\"{flags}{pos}", w.handle, w.process, w.pid, w.title)
}

fn path_arg(args: &Value, key: &str) -> Result<PathBuf, String> {
    resolve_path(str_arg(args, key))
}

async fn blocking(f: impl FnOnce() -> Result<String, String> + Send + 'static) -> Output {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(s)) => text(s),
        Ok(Err(e)) => fail(e),
        Err(e) => fail(e.to_string()),
    }
}

/// There is one real mouse and keyboard: calls that drive them run one at a time, so two clicks sent in
/// parallel can't interleave (one moving the pointer while the other presses). Element actions don't take it.
static REAL_INPUT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// type_text pastes text longer than this (method auto)
const PASTE_OVER: usize = 200;

/// Paste `t` with ctrl+v, then put the clipboard back as it was
fn paste(t: &str, saved: Option<String>) -> Result<(), String> {
    winmgr::set_clipboard_text(t)?;
    let r = input::press_keys("ctrl+v");
    // The program reads the clipboard when it handles the keystroke, a moment later: restoring too soon would paste the old text
    std::thread::sleep(Duration::from_millis(350));
    match saved {
        Some(s) => {
            let _ = winmgr::set_clipboard_text(&s);
        }
        None => winmgr::clear_clipboard(),
    }
    r
}

/// No change within this long after an action: nothing happened (or not yet)
const QUIET: Duration = Duration::from_millis(350);
/// Never wait longer than this for the screen to stop changing after an action
const SETTLE_MAX: Duration = Duration::from_millis(1500);

tokio::task_local! {
    /// Set by batch fast: input steps don't wait for the screen to settle, only for the program to take the input
    static FAST: bool;
    /// Set by a watch argument: the region (screen pixels) whose changes count after an input step
    static WATCH: capture::Area;
}

/// The region a tool's or batch's `watch` argument frames ({x0, y0, x1, y1} on the last screenshot), if any
fn watch_arg(ctx: &Ctx, args: &Value) -> Result<Option<capture::Area>, String> {
    let Some(w) = args.get("watch").filter(|w| !w.is_null()) else { return Ok(None) };
    let (a, b) = (to_screen(ctx, w, "x0", "y0")?, to_screen(ctx, w, "x1", "y1")?);
    let (l, t, r, btm) = (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
    if r - l < 8 || btm - t < 8 {
        return Err("watch region is too small".into());
    }
    Ok(Some(capture::Area { left: l, top: t, width: r - l, height: btm - t }))
}

/// In fast mode, the pause after each input so the program has taken it before the next one arrives
const FAST_GAP: Duration = Duration::from_millis(25);

/// Run a blocking input operation on a background thread, then wait just until the screen has reacted
/// and stopped changing (instead of a fixed pause), and say what changed
async fn input_op(ctx: &Ctx, at: Option<(i32, i32)>, f: impl FnOnce() -> Result<(), String> + Send + 'static, done: String) -> Output {
    let _turn = REAL_INPUT.lock().await;
    if FAST.try_with(|f| *f).unwrap_or(false) {
        let r = tokio::task::spawn_blocking(move || {
            f()?;
            std::thread::sleep(FAST_GAP);
            Ok::<_, String>(())
        })
        .await;
        return match r {
            Ok(Ok(())) => text(done),
            Ok(Err(e)) => fail(e),
            Err(e) => fail(e.to_string()),
        };
    }
    let g = *ctx.geom.lock().unwrap();
    let watch = WATCH.try_with(|w| *w).ok();
    let r = tokio::task::spawn_blocking(move || {
        let before = match watch {
            Some(area) => screen::before_in(area, at),
            None => screen::before(at),
        };
        f()?;
        Ok::<_, String>(Some(screen::settle(&before, QUIET, SETTLE_MAX)))
    })
    .await;
    match r {
        Ok(Ok(change)) => text(format!("{done}{}", describe_change(g, change.as_ref()))),
        Ok(Err(e)) => fail(e),
        Err(e) => fail(e.to_string()),
    }
}

/// A screen rectangle as the AI should read it: on the last screenshot when it is there, else in screen pixels
fn rect_text(g: Option<Geom>, (l, t, r, b): (i32, i32, i32, i32)) -> String {
    match g.and_then(|g| Some((g.screen_to_shot(l, t)?, g.screen_to_shot(r, b)?))) {
        Some(((x0, y0), (x1, y1))) => format!("({x0},{y0})-({x1},{y1})"),
        None => format!("screen ({l},{t})-({r},{b})"),
    }
}

/// A screen point as the AI should read it, like list_elements: @(x,y) on the last screenshot, else @screen(x,y)
fn at_text(g: Option<Geom>, x: i32, y: i32) -> String {
    match g.and_then(|g| g.screen_to_shot(x, y)) {
        Some((px, py)) => format!("@({px},{py})"),
        None => format!("@screen({x},{y})"),
    }
}

/// What the screen did after an action, for the AI to catch a click that missed without taking a screenshot
fn describe_change(g: Option<Geom>, c: Option<&screen::Change>) -> String {
    let Some(c) = c else { return String::new() };
    let ms = c.waited.as_millis();
    let front = c.new_front.as_ref().map(|t| format!(". \"{t}\" is now in front")).unwrap_or_default();
    match (c.changed, c.rect) {
        (true, Some(r)) if c.busy => format!(". Screen changed around {} and was still changing after {ms} ms (loading or animating){front}", rect_text(g, r)),
        (true, Some(r)) => format!(". Screen changed around {} (settled in {ms} ms){front}", rect_text(g, r)),
        (true, None) => front,
        _ => format!(". No visible change in the window within {ms} ms: the action may have missed, or the program is still working"),
    }
}

/// Remote paths: supports ~ and %ENV_VARS%; relative paths are resolved against the user's home
fn resolve_path(p: &str) -> Result<PathBuf, String> {
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
    let path = PathBuf::from(s);
    Ok(if path.is_absolute() { path } else { PathBuf::from(home).join(path) })
}

async fn shell_run(ctx: &Ctx, script: &str, timeout_secs: u64) -> Output {
    let mut guard = ctx.shell.lock().await;
    if guard.is_none() {
        match Shell::start().await {
            Ok(s) => {
                *ctx.shell_pid.lock().unwrap() = s.pid();
                *guard = Some(s);
            }
            Err(e) => return fail(e),
        }
    }
    let result = guard.as_mut().unwrap().run(script, Duration::from_secs(timeout_secs)).await;
    match result {
        Ok(out) => {
            let mut s = if out.text.is_empty() { "(no output)".to_string() } else { out.text };
            let bad_exit = out.exit_code.filter(|c| *c != 0);
            if let Some(code) = bad_exit {
                s.push_str(&format!("\n\n[exit code {code}]"));
            }
            // An error record, an exception or a non-zero exit code means the command failed;
            // the full output (including what did succeed) is still returned
            let mut o = text(s);
            if out.had_error || bad_exit.is_some() {
                o.ok = false;
                o.error = o.text.take();
            }
            o
        }
        Err(e) => {
            // Timed out or the session died: drop it (and whatever it started) so the next command gets a fresh one
            *ctx.shell_pid.lock().unwrap() = None;
            if let Some(s) = guard.take() {
                s.kill();
            }
            fail(e)
        }
    }
}

/// Run a tool. With `_shot` ({maxWidth, display}) a screenshot comes back in the same reply, once the
/// screen has settled: one round trip instead of two after every action.
pub async fn run(ctx: &Ctx, tool: &str, args: &Value) -> Output {
    let mut out = run_tool(ctx, tool, args).await;
    if let Some(s) = args.get("_shot").filter(|s| s.is_object())
        && out.image.is_none() && tool != "screenshot" {
            let shot = screenshot(ctx, int_arg(s, "maxWidth", 1600, 320, 3840), int_arg(s, "display", 0, 0, 16) as usize).await;
            out.image = shot.image;
            let note = match (shot.ok, shot.text, shot.error) {
                (true, Some(t), _) => t,
                (_, _, e) => format!("(the screenshot after it failed: {})", e.unwrap_or_default()),
            };
            match (&mut out.text, &mut out.error) {
                (_, Some(e)) => *e = format!("{e}\n{note}"),
                (Some(t), None) => *t = format!("{t}\n{note}"),
                (None, None) => out.text = Some(note),
            }
        }
    out
}

async fn screenshot(ctx: &Ctx, maxw: i64, display: usize) -> Output {
    let displays = capture::displays();
    let area = match display {
        0 => capture::virtual_screen(),
        n => match displays.get(n - 1) {
            Some(d) => d.area,
            None => return fail(format!("There is no display #{display}")),
        },
    };
    match shoot(area, maxw).await {
        Ok((shot, image)) => {
            *ctx.geom.lock().unwrap() =
                Some(Geom { left: area.left, top: area.top, scale: shot.scale, width: shot.width, height: shot.height });
            let which = if display == 0 { String::new() } else { format!("display #{display}, ") };
            let others = if display == 0 { describe_displays(&displays) } else { String::new() };
            Output {
                ok: true,
                image: Some(image),
                text: Some(format!(
                    "Screenshot {which}{}x{}. Use pixel coordinates on this image for click/scroll/drag etc.{others}",
                    shot.width, shot.height
                )),
                ..Default::default()
            }
        }
        Err(e) => fail(e),
    }
}

type BoxedRun<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Output> + Send + 'a>>;

/// Boxed so batch can run tools from inside a tool. A watch argument frames the region input steps wait on.
fn run_tool<'a>(ctx: &'a Ctx, tool: &'a str, args: &'a Value) -> BoxedRun<'a> {
    Box::pin(async move {
        match watch_arg(ctx, args) {
            Err(e) => fail(e),
            Ok(Some(area)) => WATCH.scope(area, run_tool_inner(ctx, tool, args)).await,
            Ok(None) => run_tool_inner(ctx, tool, args).await,
        }
    })
}

async fn run_tool_inner(ctx: &Ctx, tool: &str, args: &Value) -> Output {
    match tool {
        "screenshot" => screenshot(ctx, int_arg(args, "maxWidth", 1600, 320, 3840), int_arg(args, "display", 0, 0, 16) as usize).await,
        "find_text" => text_tools::find_text(ctx, args).await,
        "click_text" => text_tools::click_text(ctx, args).await,
        "wait_for" => wait::wait_for(ctx, args).await,
        "batch" => batch::batch(ctx, args).await,
        "zoom" => {
            let g = match geom(ctx) {
                Ok(g) => g,
                Err(e) => return fail(e),
            };
            let (a, b) = match (to_screen(ctx, args, "x0", "y0"), to_screen(ctx, args, "x1", "y1")) {
                (Ok(a), Ok(b)) => (a, b),
                (Err(e), _) | (_, Err(e)) => return fail(e),
            };
            let (l, t, r, btm) = (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
            if r - l < 4 || btm - t < 4 {
                return fail("Region is too small");
            }
            let maxw = int_arg(args, "maxWidth", 1600, 320, 3840);
            match shoot(capture::Area { left: l, top: t, width: r - l, height: btm - t }, maxw).await {
                Ok((shot, image)) => Output {
                    ok: true,
                    image: Some(image),
                    text: Some(format!(
                        "Zoomed image {}x{} ({:.1}x the last screenshot). This image is for viewing only; click coordinates still refer to the last screenshot.",
                        shot.width,
                        shot.height,
                        shot.scale / g.scale
                    )),
                    ..Default::default()
                },
                Err(e) => fail(e),
            }
        }
        "cursor_position" => {
            let (x, y) = match input::cursor_pos() {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let g = *ctx.geom.lock().unwrap();
            match g.map(|g| g.screen_to_shot(x, y)) {
                Some(Some((px, py))) => text(format!("Mouse is at screenshot ({px}, {py})")),
                Some(None) => text(format!("Mouse is at screen ({x}, {y}), outside the last screenshot")),
                None => text(format!("Mouse is at screen ({x}, {y}). Take a screenshot first to get screenshot coordinates")),
            }
        }
        "mouse_move" => {
            let (x, y) = match to_screen(ctx, args, "x", "y") {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            input_op(ctx, Some((x, y)), move || input::mouse_move(x, y), "Mouse moved".into()).await
        }
        "mouse_look" => {
            let dx = int_arg(args, "dx", 0, -20000, 20000) as i32;
            let dy = int_arg(args, "dy", 0, -20000, 20000) as i32;
            if dx == 0 && dy == 0 {
                return fail("Give dx and/or dy");
            }
            let ms = (args.get("seconds").and_then(Value::as_f64).unwrap_or(0.2).clamp(0.0, 5.0) * 1000.0) as u64;
            input_op(ctx, None, move || input::mouse_look(dx, dy, ms), format!("Mouse moved by ({dx}, {dy})")).await
        }
        "mouse_down" | "mouse_up" => {
            let at = match opt_screen(ctx, args) {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let button = str_arg(args, "button").to_string();
            let press = tool == "mouse_down";
            input_op(ctx, at.or_else(|| input::cursor_pos().ok()), move || input::mouse_button(at, &button, press), if press { "Pressed" } else { "Released" }.into()).await
        }
        "hold_key" => {
            let k = str_arg(args, "keys").to_string();
            let ms = (args.get("seconds").and_then(Value::as_f64).unwrap_or(1.0).clamp(0.05, 10.0) * 1000.0) as u64;
            input_op(ctx, None, move || input::hold_keys(&k, ms), "Keys released".into()).await
        }
        "read_clipboard" => ps_text(CLIPBOARD_READ_PS, 15).await,
        "write_clipboard" => {
            // Native rather than PowerShell: the text would travel on the command line, which Windows caps at 32K characters
            let t = str_arg(args, "text").to_string();
            blocking(move || winmgr::set_clipboard_text(&t).map(|_| format!("Copied to clipboard ({} chars)", t.chars().count()))).await
        }
        "list_windows" => {
            let g = *ctx.geom.lock().unwrap();
            blocking(move || {
                let ws = winmgr::list();
                if ws.is_empty() {
                    return Ok("No visible windows".into());
                }
                let lines: Vec<String> = ws.iter().map(|w| win_line(g, w)).collect();
                Ok(format!("{} windows (front to back, handle in brackets):\n{}", ws.len(), lines.join("\n")))
            })
            .await
        }
        "list_elements" => {
            let handle = args.get("handle").and_then(Value::as_i64).map(|h| h as isize);
            let title = str_arg(args, "title").to_string();
            let query = str_arg(args, "query").to_string();
            let max = int_arg(args, "maxResults", 300, 1, 2000) as usize;
            let offscreen = args.get("includeOffscreen").and_then(Value::as_bool).unwrap_or(false);
            let g = *ctx.geom.lock().unwrap();
            blocking(move || {
                let hwnd = if handle.is_some() || !title.trim().is_empty() { winmgr::find(handle, &title)? } else { uia::foreground() };
                uia::list(hwnd, &query, max, offscreen, &|x, y| g.and_then(|g| g.screen_to_shot(x, y)))
            })
            .await
        }
        "element_action" => {
            let batch = args.get("actions").and_then(Value::as_array);
            let items: Vec<(u64, String, String)> = match batch {
                Some(list) => list.iter().map(element_item).collect(),
                None => vec![element_item(args)],
            };
            let sequence = batch.is_some() && args.get("sequence").and_then(Value::as_bool).unwrap_or(false);
            let max = if sequence { 100 } else { 20 };
            if items.is_empty() || items.len() > max {
                return fail(format!("Give 1 to {max} actions"));
            }
            let changes = items.iter().any(|i| i.1 != "text");
            let g = *ctx.geom.lock().unwrap();
            let at = items.iter().find_map(|(id, _, _)| uia::mark_of(*id as usize)).map(|((l, t, r, b), _)| ((l + r) / 2, (t + b) / 2));
            let before = if changes { tokio::task::spawn_blocking(move || screen::before(at)).await.ok() } else { None };
            // Each step acts when its virtual cursor lands, so what is drawn and what happens line up
            let pace = pace::current();
            let pace = pace.as_deref();
            // None = skipped: a sequence stops at its first failure
            let results: Vec<Option<Result<String, String>>> = if sequence {
                // One after another, in the order given, as fast as the cursors arrive (or intervalMs apart)
                let gap = Duration::from_millis(int_arg(args, "intervalMs", 0, 0, 5000) as u64);
                let mut out = Vec::with_capacity(items.len());
                for (i, (id, action, value)) in items.into_iter().enumerate() {
                    if matches!(out.last(), Some(Some(Err(_))) | Some(None)) {
                        out.push(None);
                        continue;
                    }
                    if i > 0 && !gap.is_zero() {
                        tokio::time::sleep(gap).await;
                    }
                    out.push(Some(paced_act(pace, i, id, action, value).await));
                }
                out
            } else {
                // All at once, each as soon as its own cursor lands: UI Automation calls are independent
                let runs = items.into_iter().enumerate().map(|(i, (id, action, value))| paced_act(pace, i, id, action, value));
                futures_util::future::join_all(runs).await.into_iter().map(Some).collect()
            };
            // Let the program react before anything looks at the screen again, and say whether it did
            let mut change = String::new();
            if let Some(b) = before.filter(|_| results.iter().any(|r| matches!(r, Some(Ok(_))))) {
                let c = tokio::task::spawn_blocking(move || screen::settle(&b, QUIET, SETTLE_MAX)).await.ok();
                change = describe_change(g, c.as_ref());
            }
            if batch.is_none() {
                return match results.into_iter().next().flatten() {
                    Some(Ok(s)) => text(s + &change),
                    Some(Err(e)) => fail(e),
                    None => fail("No result"),
                };
            }
            let failed = results.iter().filter(|r| matches!(r, Some(Err(_)))).count();
            let skipped = results.iter().filter(|r| r.is_none()).count();
            // One line per action, in the order given; fx.ts reads the ✓ / ✗ / – marks
            let lines: Vec<String> = results
                .iter()
                .enumerate()
                .map(|(i, r)| match r {
                    Some(Ok(s)) => format!("{}. ✓ {}", i + 1, s),
                    Some(Err(e)) => format!("{}. ✗ {}", i + 1, e),
                    None => format!("{}. – skipped", i + 1),
                })
                .collect();
            let mut o = text(lines.join("\n") + &change);
            if failed > 0 {
                o.ok = false;
                let note = if skipped > 0 { format!(", so the sequence stopped and {skipped} were skipped") } else { String::new() };
                o.error = Some(format!("{failed} of {} actions failed{note}:\n{}", results.len(), o.text.take().unwrap_or_default()));
            }
            o
        }
        "focus_window" => {
            let handle = args.get("handle").and_then(Value::as_i64).map(|h| h as isize);
            let title = str_arg(args, "title").to_string();
            let action = str_arg(args, "action").to_string();
            let g = *ctx.geom.lock().unwrap();
            // The outline goes round the window first, then the window changes, and the overlay hears where it went
            // right away (not after the screen settles and the screenshot is taken), so the outline moves with it
            if let Some(p) = pace::current() {
                p.wait(0).await;
            }
            let fx_id = pace::CALL_ID.try_with(Clone::clone).ok();
            let r = tokio::task::spawn_blocking(move || {
                let h = winmgr::find(handle, &title)?;
                let at = winmgr::rect_of(h.0 as isize).map(|(l, t, r, b)| ((l + r) / 2, (t + b) / 2));
                let before = screen::before(at);
                let done = winmgr::act(h, &action)?;
                if let Some(id) = fx_id {
                    // The new position is set as soon as the call returns; Windows only animates the picture
                    let rect = winmgr::rect_of(h.0 as isize).map(rect_json);
                    pace::emit(serde_json::json!({ "phase": "step", "id": id, "tool": "focus_window", "index": 0, "ok": true, "geo": { "rect": rect } }));
                }
                Ok::<_, String>((done, Some(screen::settle(&before, Duration::from_millis(150), Duration::from_millis(800)))))
            })
            .await;
            match r {
                Ok(Ok((done, c))) => text(done + &describe_change(g, c.as_ref())),
                Ok(Err(e)) => fail(e),
                Err(e) => fail(e.to_string()),
            }
        }
        "open_app" => {
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
            // Waits (up to ~4.5 s) for the program's window and brings it to the front
            let r = tokio::task::spawn_blocking(move || winmgr::open(&target, &a, &cwd).map(|h| (target, h))).await;
            match r {
                Ok(Ok((target, h))) => {
                    *ctx.opened.lock().unwrap() = h;
                    let title = h.and_then(|h| winmgr::list().into_iter().find(|w| w.handle == h)).map(|w| format!(" — window \"{}\" (handle {}) is in front", w.title, w.handle));
                    text(format!("Opened {target}{}", title.unwrap_or_default()))
                }
                Ok(Err(e)) => fail(e),
                Err(e) => fail(e.to_string()),
            }
        }
        "read_file" => {
            let path = match path_arg(args, "path") {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let offset = int_arg(args, "offset", 1, 1, i64::MAX) as usize;
            let limit = int_arg(args, "limit", 2000, 1, 20000) as usize;
            blocking(move || files::read(&path, offset, limit)).await
        }
        "write_file" => {
            let path = match path_arg(args, "path") {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let enc = match files::Enc::parse(str_arg(args, "encoding")) {
                Ok(e) => e,
                Err(e) => return fail(e),
            };
            let content = str_arg(args, "content").to_string();
            blocking(move || files::write(&path, &content, enc)).await
        }
        "edit_file" => {
            let path = match path_arg(args, "path") {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let (old, new) = (str_arg(args, "oldText").to_string(), str_arg(args, "newText").to_string());
            let all = args.get("replaceAll").and_then(Value::as_bool).unwrap_or(false);
            blocking(move || files::edit(&path, &old, &new, all)).await
        }
        "list_dir" => {
            let p = match str_arg(args, "path") {
                "" => "~",
                p => p,
            };
            let path = match resolve_path(p) {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            blocking(move || files::list_dir(&path)).await
        }
        "find_files" => {
            let path = match path_arg(args, "path") {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let pattern = str_arg(args, "pattern").to_string();
            let max = int_arg(args, "maxResults", 200, 1, 5000) as usize;
            blocking(move || files::find(&path, &pattern, max)).await
        }
        "disk_usage" => {
            let p = match str_arg(args, "path") {
                "" => "C:\\",
                p => p,
            };
            let path = match resolve_path(p) {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let depth = int_arg(args, "depth", 2, 0, 6) as usize;
            let per_level = int_arg(args, "perFolder", 8, 1, 50) as usize;
            let files = int_arg(args, "topFiles", 20, 0, 500) as usize;
            blocking(move || files::disk_usage(&path, depth, per_level, files)).await
        }
        "search_files" => {
            let path = match path_arg(args, "path") {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let pattern = str_arg(args, "pattern").to_string();
            if pattern.is_empty() {
                return fail("pattern is empty");
            }
            let glob = str_arg(args, "glob").to_string();
            let ic = args.get("ignoreCase").and_then(Value::as_bool).unwrap_or(true);
            let max = int_arg(args, "maxResults", 100, 1, 2000) as usize;
            let context = int_arg(args, "context", 0, 0, 10) as usize;
            blocking(move || files::search(&path, &pattern, &glob, ic, max, context)).await
        }
        "job_output" => {
            let mut jobs = ctx.jobs.lock().unwrap();
            match args.get("jobId").and_then(Value::as_u64) {
                None => text(jobs.list()),
                Some(id) => match jobs.output(id as u32, args.get("all").and_then(Value::as_bool).unwrap_or(false)) {
                    Ok(s) => text(s),
                    Err(e) => fail(e),
                },
            }
        }
        "stop_job" => {
            let Some(id) = args.get("jobId").and_then(Value::as_u64) else { return fail("Missing jobId") };
            let r = ctx.jobs.lock().unwrap().stop(id as u32);
            match r {
                Ok(s) => text(s),
                Err(e) => fail(e),
            }
        }
        "click" => {
            let (x, y) = match to_screen(ctx, args, "x", "y") {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let button = str_arg(args, "button").to_string();
            let count = int_arg(args, "count", 1, 1, 3) as u32;
            let mods = str_arg(args, "modifiers").to_string();
            // Click when the virtual cursor gets there, and have it press right as the real click happens
            let pace = pace::current();
            if let Some(p) = &pace {
                p.wait(0).await;
            }
            let out = input_op(ctx, Some((x, y)), move || input::click(x, y, &button, count, &mods), "Clicked".into()).await;
            if let Some(p) = &pace {
                p.step(0, out.ok);
            }
            out
        }
        "scroll" => {
            let (x, y) = match to_screen(ctx, args, "x", "y") {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let amount = int_arg(args, "amount", 3, -50, 50) as i32;
            let horizontal = args.get("horizontal").and_then(Value::as_bool).unwrap_or(false);
            input_op(ctx, Some((x, y)), move || input::scroll(x, y, amount, horizontal), "Scrolled".into()).await
        }
        "drag" => {
            let from = to_screen(ctx, args, "fromX", "fromY");
            let to = to_screen(ctx, args, "toX", "toY");
            let ((x1, y1), (x2, y2)) = match (from, to) {
                (Ok(a), Ok(b)) => (a, b),
                (Err(e), _) | (_, Err(e)) => return fail(e),
            };
            input_op(ctx, Some((x1, y1)), move || input::drag(x1, y1, x2, y2), "Dragged".into()).await
        }
        "type_text" => {
            let t = str_arg(args, "text").to_string();
            if t.is_empty() {
                return fail("text is empty");
            }
            // Programs take keystrokes at their own pace (a few hundred a second); a paste lands at once.
            // auto pastes long text, unless the clipboard holds something that couldn't be put back.
            let method = str_arg(args, "method").to_string();
            let long = t.chars().count() > PASTE_OVER;
            let saved = if method == "paste" || (method != "keys" && long) { Some(tokio::task::spawn_blocking(winmgr::clipboard_text).await) } else { None };
            match saved {
                Some(Ok(Ok(saved))) => {
                    input_op(ctx, None, move || paste(&t, saved), "Typed (pasted through the clipboard, which was put back afterwards)".into()).await
                }
                Some(_) if method == "paste" => fail("The clipboard holds an image or files, which pasting would lose; use method keys"),
                _ => input_op(ctx, None, move || input::type_text(&t), "Typed".into()).await,
            }
        }
        "press_keys" => {
            let k = str_arg(args, "keys").to_string();
            input_op(ctx, None, move || input::press_keys(&k), "Keys pressed".into()).await
        }
        "upload_file" => {
            let path = match path_arg(args, "path") {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let bytes = match base64::engine::general_purpose::STANDARD.decode(str_arg(args, "data")) {
                Ok(b) => b,
                Err(e) => return fail(format!("data is not valid base64: {e}")),
            };
            if bytes.len() > MAX_FILE {
                return fail("File is too large; the limit is 20 MB");
            }
            blocking(move || {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                std::fs::write(&path, &bytes).map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
                Ok(format!("Saved to {} ({} bytes)", path.display(), bytes.len()))
            })
            .await
        }
        "download_file" => {
            let path = match path_arg(args, "path") {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let read = tokio::task::spawn_blocking(move || {
                let m = std::fs::metadata(&path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
                if m.is_dir() {
                    return Err("This is a folder; only single files can be downloaded".to_string());
                }
                if m.len() > MAX_FILE as u64 {
                    return Err(format!("File is too large ({} MB); the limit is 20 MB", m.len() / 1024 / 1024));
                }
                let bytes = std::fs::read(&path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                Ok((format!("{} ({} bytes)", path.display(), bytes.len()), name, base64::engine::general_purpose::STANDARD.encode(bytes)))
            })
            .await;
            match read {
                Ok(Ok((text, name, data))) => Output { ok: true, text: Some(text), file: Some(FileData { name, data }), ..Default::default() },
                Ok(Err(e)) => fail(e),
                Err(e) => fail(e.to_string()),
            }
        }
        "reset_shell" => {
            ctx.reset().await;
            text("Reset. The next command runs in a fresh PowerShell (back in the home directory)".into())
        }
        "system_info" => ps_text(SYSTEM_INFO_PS, 30).await,
        "list_processes" => {
            let top = int_arg(args, "top", 30, 1, 200);
            ps_text(&PROCESSES_PS.replace("__TOP__", &top.to_string()), 30).await
        }
        "event_log" => {
            let log = match args.get("log").and_then(Value::as_str) {
                Some("Application") => "Application",
                _ => "System",
            };
            let script = EVENT_LOG_PS
                .replace("__LOG__", log)
                .replace("__HOURS__", &int_arg(args, "hours", 24, 1, 720).to_string())
                .replace("__MAX__", &int_arg(args, "max", 30, 1, 200).to_string());
            ps_text(&script, 60).await
        }
        "run_powershell" => {
            let script = str_arg(args, "script");
            if script.trim().is_empty() {
                return fail("script is empty");
            }
            if args.get("background").and_then(Value::as_bool) == Some(true) {
                let r = ctx.jobs.lock().unwrap().start(script);
                return match r {
                    Ok(id) => text(format!(
                        "Started in the background as job {id}. Use job_output to see progress and output, stop_job to stop it. \
                         Background jobs run in a separate PowerShell and do not share the session's directory or variables."
                    )),
                    Err(e) => fail(e),
                };
            }
            shell_run(ctx, script, int_arg(args, "timeoutSec", 60, 1, 600) as u64).await
        }
        other => fail(format!("Unknown tool: {other}")),
    }
}

// ---- Geometry for the animations (physical screen pixels) ----

fn rect_json(r: (i32, i32, i32, i32)) -> Value {
    serde_json::json!([r.0, r.1, r.2 - r.0, r.3 - r.1])
}

/// One element action, once its cursor has landed; the overlay presses that cursor as soon as it is done
async fn paced_act(pace: Option<&pace::Call>, i: usize, id: u64, action: String, value: String) -> Result<String, String> {
    if let Some(p) = pace {
        p.wait(i).await;
    }
    let r = tokio::task::spawn_blocking(move || uia::act(id as usize, &action, &value)).await.unwrap_or_else(|e| Err(e.to_string()));
    if let Some(p) = pace {
        p.step(i, r.is_ok());
    }
    r
}

/// (id, action, value) of one element action, from the call itself or one entry of its `actions`
fn element_item(v: &Value) -> (u64, String, String) {
    (v.get("id").and_then(Value::as_u64).unwrap_or(0), str_arg(v, "action").to_string(), str_arg(v, "value").to_string())
}

/// Before a tool runs: where it will click, where it will drag to, which window it acts on
pub fn fx_before(ctx: &Ctx, tool: &str, args: &Value) -> Value {
    use serde_json::json;
    let pt = |kx: &str, ky: &str| to_screen(ctx, args, kx, ky).ok().map(|(x, y)| json!([x, y]));
    let cur = || input::cursor_pos().ok().map(|(x, y)| json!([x, y]));
    match tool {
        // A click has no controls to go by (often a game drawing its own board): frame the window it lands in
        "click" => {
            let at = to_screen(ctx, args, "x", "y").ok();
            let zone = at.and_then(|(x, y)| winmgr::top_window_at(x, y)).and_then(winmgr::rect_of).map(rect_json);
            json!({ "at": at.map(|(x, y)| json!([x, y])), "zone": zone, "zoneIsWindow": true })
        }
        "scroll" | "mouse_move" => json!({ "at": pt("x", "y") }),
        "mouse_down" | "mouse_up" => json!({ "at": pt("x", "y").or_else(cur) }),
        "drag" => {
            // Framed like a click: the window the drag starts in, so the orb stays put between clicks and drags there
            let from = to_screen(ctx, args, "fromX", "fromY").ok();
            let zone = from.and_then(|(x, y)| winmgr::top_window_at(x, y)).and_then(winmgr::rect_of).map(rect_json);
            json!({ "from": from.map(|(x, y)| json!([x, y])), "to": pt("toX", "toY"), "zone": zone, "zoneIsWindow": true })
        }
        "zoom" => json!({ "from": pt("x0", "y0"), "to": pt("x1", "y1") }),
        "type_text" | "press_keys" | "hold_key" => json!({ "at": cur() }),
        // One virtual cursor per control, flying out of the orb; the user's pointer stays where it is.
        // null keeps the index of an action that has nothing to point at (text, or an unknown id)
        "element_action" => {
            let items: Vec<(u64, String, String)> = match args.get("actions").and_then(Value::as_array) {
                Some(list) => list.iter().map(element_item).collect(),
                None => vec![element_item(args)],
            };
            let targets: Vec<Value> = items
                .iter()
                .map(|(id, action, _)| {
                    let mark = uia::mark_of(*id as usize).filter(|_| action != "text");
                    mark.map_or(Value::Null, |((l, t, r, b), _)| json!([(l + r) / 2, (t + b) / 2]))
                })
                .collect();
            // One fixed frame around everything operable in that window, drawn once for all its actions
            let area = uia::work_area();
            json!({ "targets": targets, "zone": area.map(|(r, _)| rect_json(r)), "zoneIsWindow": area.is_some_and(|(_, w)| w) })
        }
        "focus_window" => {
            let handle = args.get("handle").and_then(Value::as_i64).map(|h| h as isize);
            match winmgr::find(handle, str_arg(args, "title")) {
                Ok(h) => {
                    let h = h.0 as isize;
                    json!({ "hwnd": h, "rect": winmgr::rect_of(h).map(rect_json) })
                }
                Err(_) => json!({}),
            }
        }
        _ => json!({}),
    }
}

/// After a tool runs: which part of the screen was captured, what the window became
pub fn fx_after(ctx: &Ctx, tool: &str, before: &Value) -> Value {
    use serde_json::json;
    match tool {
        "screenshot" => match *ctx.geom.lock().unwrap() {
            Some(g) => json!({ "shot": [g.left, g.top, (g.width as f64 / g.scale).round(), (g.height as f64 / g.scale).round()] }),
            None => json!({}),
        },
        "focus_window" => match before.get("hwnd").and_then(Value::as_i64) {
            Some(h) => json!({ "rect": winmgr::rect_of(h as isize).map(rect_json) }),
            None => json!({}),
        },
        // The program's window, wherever it is (a console program has none)
        "open_app" => {
            let h = ctx.opened.lock().unwrap().take();
            json!({ "rect": h.and_then(winmgr::rect_of).map(rect_json) })
        }
        _ => json!({}),
    }
}
