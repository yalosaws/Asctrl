//! Tools the agent can run. The system_info / list_processes / event_log scripts match the Node version in src/agent/tools.ts.

use crate::{capture, files, input, jobs, jobs::Jobs, powershell, shell::Shell, winmgr};
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
    let g = geom(ctx)?;
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

/// Run a blocking input operation on a background thread, then pause briefly so the UI can refresh
async fn input_op(f: impl FnOnce() -> Result<(), String> + Send + 'static, done: String) -> Output {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(())) => {
            tokio::time::sleep(Duration::from_millis(300)).await;
            text(done)
        }
        Ok(Err(e)) => fail(e),
        Err(e) => fail(e.to_string()),
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

pub async fn run(ctx: &Ctx, tool: &str, args: &Value) -> Output {
    match tool {
        "screenshot" => {
            let maxw = int_arg(args, "maxWidth", 1600, 320, 3840);
            let display = int_arg(args, "display", 0, 0, 16) as usize;
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
            input_op(move || input::mouse_move(x, y), "Mouse moved".into()).await
        }
        "mouse_down" | "mouse_up" => {
            let at = match opt_screen(ctx, args) {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let button = str_arg(args, "button").to_string();
            let press = tool == "mouse_down";
            input_op(move || input::mouse_button(at, &button, press), if press { "Pressed" } else { "Released" }.into()).await
        }
        "hold_key" => {
            let k = str_arg(args, "keys").to_string();
            let ms = (args.get("seconds").and_then(Value::as_f64).unwrap_or(1.0).clamp(0.05, 10.0) * 1000.0) as u64;
            input_op(move || input::hold_keys(&k, ms), "Keys released".into()).await
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
        "focus_window" => {
            let handle = args.get("handle").and_then(Value::as_i64).map(|h| h as isize);
            let title = str_arg(args, "title").to_string();
            let action = str_arg(args, "action").to_string();
            let out = blocking(move || winmgr::act(winmgr::find(handle, &title)?, &action)).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
            out
        }
        "open_app" => {
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
            let out = blocking(move || winmgr::open(&target, &a, &cwd).map(|_| format!("Opened {target}"))).await;
            if out.ok {
                tokio::time::sleep(Duration::from_millis(1500)).await; // wait for the window to appear
            }
            out
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
            input_op(move || input::click(x, y, &button, count, &mods), "Clicked".into()).await
        }
        "scroll" => {
            let (x, y) = match to_screen(ctx, args, "x", "y") {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let amount = int_arg(args, "amount", 3, -50, 50) as i32;
            let horizontal = args.get("horizontal").and_then(Value::as_bool).unwrap_or(false);
            input_op(move || input::scroll(x, y, amount, horizontal), "Scrolled".into()).await
        }
        "drag" => {
            let from = to_screen(ctx, args, "fromX", "fromY");
            let to = to_screen(ctx, args, "toX", "toY");
            let ((x1, y1), (x2, y2)) = match (from, to) {
                (Ok(a), Ok(b)) => (a, b),
                (Err(e), _) | (_, Err(e)) => return fail(e),
            };
            input_op(move || input::drag(x1, y1, x2, y2), "Dragged".into()).await
        }
        "type_text" => {
            let t = str_arg(args, "text").to_string();
            if t.is_empty() {
                return fail("text is empty");
            }
            input_op(move || input::type_text(&t), "Typed".into()).await
        }
        "press_keys" => {
            let k = str_arg(args, "keys").to_string();
            input_op(move || input::press_keys(&k), "Keys pressed".into()).await
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

/// Before a tool runs: where it will click, where it will drag to, which window it acts on
pub fn fx_before(ctx: &Ctx, tool: &str, args: &Value) -> Value {
    use serde_json::json;
    let pt = |kx: &str, ky: &str| to_screen(ctx, args, kx, ky).ok().map(|(x, y)| json!([x, y]));
    let cur = || input::cursor_pos().ok().map(|(x, y)| json!([x, y]));
    match tool {
        "click" | "scroll" | "mouse_move" => json!({ "at": pt("x", "y") }),
        "mouse_down" | "mouse_up" => json!({ "at": pt("x", "y").or_else(cur) }),
        "drag" => json!({ "from": pt("fromX", "fromY"), "to": pt("toX", "toY") }),
        "zoom" => json!({ "from": pt("x0", "y0"), "to": pt("x1", "y1") }),
        "type_text" | "press_keys" | "hold_key" => json!({ "at": cur() }),
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
        _ => json!({}),
    }
}
