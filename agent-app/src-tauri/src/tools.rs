//! Tools the agent can run. The base scripts match the Node version in src/agent/tools.ts.

use crate::{files, input, jobs::Jobs, powershell, shell::Shell, winmgr};
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
    jobs: Mutex<Jobs>,
}

impl Ctx {
    /// Called when the technician disconnects: ends the PowerShell session and all background jobs
    pub async fn reset(&self) {
        if let Some(s) = self.shell.lock().await.take() {
            s.kill();
        }
        let mut jobs = std::mem::take(&mut *self.jobs.lock().unwrap());
        let _ = tokio::task::spawn_blocking(move || jobs.stop_all()).await;
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

const SCREENSHOT_PS: &str = r#"
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type -Namespace W -Name U -MemberDefinition '[DllImport("user32.dll")] public static extern bool SetProcessDPIAware();'
[W.U]::SetProcessDPIAware() | Out-Null
$screens = [System.Windows.Forms.Screen]::AllScreens
"SCREENS " + (($screens | ForEach-Object { $i = [array]::IndexOf($screens, $_) + 1; "$i=$($_.Bounds.Width)x$($_.Bounds.Height)@$($_.Bounds.X),$($_.Bounds.Y)$(if ($_.Primary) { '*' })" }) -join ' ')
$b = __RECT__
if ($null -eq $b -or $b.Width -le 0 -or $b.Height -le 0) { "NO_DISPLAY"; exit 1 }
$src = New-Object System.Drawing.Bitmap $b.Width, $b.Height
$g = [System.Drawing.Graphics]::FromImage($src)
$g.CopyFromScreen($b.Left, $b.Top, 0, 0, $src.Size)
$scale = [Math]::Min(1.0, __MAXW__ / $b.Width)
$w = [int]($b.Width * $scale); $h = [int]($b.Height * $scale)
$dst = New-Object System.Drawing.Bitmap $w, $h
$g2 = [System.Drawing.Graphics]::FromImage($dst)
$g2.InterpolationMode = 'HighQualityBicubic'
$g2.DrawImage($src, 0, 0, $w, $h)
$codec = [System.Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object { $_.MimeType -eq 'image/jpeg' }
$ep = New-Object System.Drawing.Imaging.EncoderParameters 1
$ep.Param[0] = New-Object System.Drawing.Imaging.EncoderParameter ([System.Drawing.Imaging.Encoder]::Quality), 80L
$ms = New-Object System.IO.MemoryStream
$dst.Save($ms, $codec, $ep)
"GEOM $($b.Left) $($b.Top) $scale $w $h"
[Convert]::ToBase64String($ms.ToArray())
"#;

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

/// One-line description shown in the UI
pub fn describe(tool: &str, args: &Value) -> String {
    match tool {
        "screenshot" => "Looked at your screen".into(),
        "system_info" => "Checked basic computer info".into(),
        "list_processes" => "Checked running programs".into(),
        "event_log" => "Checked the system error log".into(),
        "run_powershell" if args.get("background").and_then(Value::as_bool) == Some(true) => {
            format!("Ran in background: {}", str_arg(args, "script").trim())
        }
        "run_powershell" => format!("Ran: {}", str_arg(args, "script").trim()),
        "reset_shell" => "Reset the command shell".into(),
        "click" => {
            let what = match (str_arg(args, "button"), int_arg(args, "count", 1, 1, 3)) {
                ("right", _) => "Right-clicked",
                (_, 2) => "Double-clicked",
                _ => "Clicked",
            };
            format!("{what} the screen")
        }
        "scroll" => "Scrolled the page".into(),
        "zoom" => "Zoomed into part of the screen".into(),
        "mouse_move" => "Moved the mouse".into(),
        "mouse_down" => "Pressed the mouse button".into(),
        "mouse_up" => "Released the mouse button".into(),
        "cursor_position" => "Checked the mouse position".into(),
        "hold_key" => format!("Held {}", str_arg(args, "keys")),
        "read_clipboard" => "Read the clipboard".into(),
        "write_clipboard" => "Put something on the clipboard".into(),
        "list_windows" => "Listed open windows".into(),
        "focus_window" => match str_arg(args, "action") {
            "minimize" => "Minimized a window".into(),
            "maximize" => "Maximized a window".into(),
            "restore" => "Restored a window".into(),
            "close" => "Closed a window".into(),
            _ => "Switched windows".into(),
        },
        "open_app" => format!("Opened {} {}", str_arg(args, "target"), str_arg(args, "args")).trim_end().to_string(),
        "read_file" => format!("Read file {}", str_arg(args, "path")),
        "write_file" => format!("Wrote file {}", str_arg(args, "path")),
        "edit_file" => format!("Edited file {}", str_arg(args, "path")),
        "list_dir" => format!("Listed folder {}", str_arg(args, "path")),
        "find_files" => format!("Looked for files {1} in {0}", str_arg(args, "path"), str_arg(args, "pattern")),
        "search_files" => format!("Searched {0} for {1}", str_arg(args, "path"), str_arg(args, "pattern")),
        "job_output" => "Checked background job progress".into(),
        "stop_job" => "Stopped a background job".into(),
        "drag" => "Dragged the mouse".into(),
        "type_text" => {
            let t = str_arg(args, "text");
            let short: String = t.chars().take(40).collect();
            format!("Typed: {short}{}", if t.chars().count() > 40 { "…" } else { "" })
        }
        "press_keys" => format!("Pressed {}", str_arg(args, "keys")),
        "upload_file" => format!("Uploaded a file to {}", str_arg(args, "path")),
        "download_file" => format!("Downloaded file {}", str_arg(args, "path")),
        other => format!("Unknown action: {other}"),
    }
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

struct Capture {
    geom: Geom,
    data: String,
    screens: String,
}

/// Capture a region of the screen. `rect` is a PowerShell expression that yields a System.Drawing.Rectangle
async fn capture(rect: &str, maxw: i64) -> Result<Capture, String> {
    let script = SCREENSHOT_PS.replace("__RECT__", rect).replace("__MAXW__", &maxw.to_string());
    let r = powershell::run(&script, Duration::from_secs(20)).await;
    let mut screens = String::new();
    let mut geom = None;
    let mut data = String::new();
    for l in r.stdout.lines() {
        if let Some(v) = l.strip_prefix("SCREENS ") {
            screens = v.trim().to_string();
        } else if let Some(v) = l.strip_prefix("GEOM ") {
            let v: Vec<&str> = v.split(' ').collect();
            geom = (|| {
                Some(Geom {
                    left: v.first()?.parse().ok()?,
                    top: v.get(1)?.parse().ok()?,
                    scale: v.get(2)?.parse().ok()?,
                    width: v.get(3)?.parse().ok()?,
                    height: v.get(4)?.parse().ok()?,
                })
            })();
        } else {
            data.push_str(l.trim());
        }
    }
    if data == "NO_DISPLAY" {
        return Err("No such display".into());
    }
    match geom.filter(|_| !data.is_empty() && r.exit_code == Some(0)) {
        Some(geom) => Ok(Capture { geom, data, screens }),
        None => {
            let err = r.stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("no output").to_string();
            Err(format!("Screenshot failed: {err}"))
        }
    }
}

/// "1=1920x1080@0,0* 2=..." -> human-readable description
fn describe_screens(s: &str) -> String {
    let parts: Vec<&str> = s.split(' ').filter(|p| !p.is_empty()).collect();
    if parts.len() <= 1 {
        return String::new();
    }
    let list = parts
        .iter()
        .map(|p| {
            let (n, rest) = p.split_once('=').unwrap_or((p, ""));
            let primary = rest.ends_with('*');
            let size = rest.trim_end_matches('*').split('@').next().unwrap_or("");
            format!("#{n} {size}{}", if primary { " (primary)" } else { "" })
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("\n{} displays: {list}. Pass `display` to capture just one, for a clearer image.", parts.len())
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
    while let Some(a) = s.find('%') {
        let Some(b) = s[a + 1..].find('%').map(|i| a + 1 + i) else { break };
        let val = std::env::var(&s[a + 1..b]).map_err(|_| format!("Environment variable {} does not exist", &s[a + 1..b]))?;
        s.replace_range(a..=b, &val);
    }
    let path = PathBuf::from(s);
    Ok(if path.is_absolute() { path } else { PathBuf::from(home).join(path) })
}

async fn shell_run(ctx: &Ctx, script: &str, timeout_secs: u64) -> Output {
    let mut guard = ctx.shell.lock().await;
    if guard.is_none() {
        match Shell::start().await {
            Ok(s) => *guard = Some(s),
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
            let display = int_arg(args, "display", 0, 0, 16);
            let rect = if display == 0 {
                "[System.Windows.Forms.SystemInformation]::VirtualScreen".to_string()
            } else {
                format!("$(if ($screens.Count -ge {display}) {{ $screens[{}].Bounds }})", display - 1)
            };
            match capture(&rect, maxw).await {
                Ok(c) => {
                    let g = c.geom;
                    *ctx.geom.lock().unwrap() = Some(g);
                    let which = if display == 0 { String::new() } else { format!("display #{display}, ") };
                    let screens = if display == 0 { describe_screens(&c.screens) } else { String::new() };
                    Output {
                        ok: true,
                        image: Some(Image { data: c.data, mime_type: "image/jpeg".into() }),
                        text: Some(format!(
                            "Screenshot {which}{}x{}. Use pixel coordinates on this image for click/scroll/drag etc.{screens}",
                            g.width, g.height
                        )),
                        ..Default::default()
                    }
                }
                Err(e) if e.contains("No such display") => fail(format!("There is no display #{display}")),
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
            match capture(&format!("New-Object System.Drawing.Rectangle {l}, {t}, {}, {}", r - l, btm - t), maxw).await {
                Ok(c) => Output {
                    ok: true,
                    image: Some(Image { data: c.data, mime_type: "image/jpeg".into() }),
                    text: Some(format!(
                        "Zoomed image {}x{} ({:.1}x the last screenshot). This image is for viewing only; click coordinates still refer to the last screenshot.",
                        c.geom.width,
                        c.geom.height,
                        c.geom.scale / g.scale
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
            let t = str_arg(args, "text");
            let b64 = base64::engine::general_purpose::STANDARD.encode(t.as_bytes());
            let n = t.chars().count();
            ps_text(&format!("Set-Clipboard -Value ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{b64}'))); 'Copied to clipboard ({n} chars)'"), 15).await
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
            let path = match resolve_path(str_arg(args, "path")) {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let bytes = match base64::engine::general_purpose::STANDARD.decode(str_arg(args, "data")) {
                Ok(b) => b,
                Err(e) => return fail(format!("data is not valid base64: {e}")),
            };
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match std::fs::write(&path, &bytes) {
                Ok(()) => text(format!("Saved to {} ({} bytes)", path.display(), bytes.len())),
                Err(e) => fail(format!("Failed to write {}: {e}", path.display())),
            }
        }
        "download_file" => {
            let path = match resolve_path(str_arg(args, "path")) {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            match std::fs::metadata(&path) {
                Ok(m) if m.is_dir() => return fail("This is a folder; only single files can be downloaded"),
                Ok(m) if m.len() as usize > MAX_FILE => return fail(format!("File is too large ({} MB); the limit is 20 MB", m.len() / 1024 / 1024)),
                Err(e) => return fail(format!("Failed to read {}: {e}", path.display())),
                _ => {}
            }
            match std::fs::read(&path) {
                Ok(bytes) => Output {
                    ok: true,
                    text: Some(format!("{} ({} bytes)", path.display(), bytes.len())),
                    file: Some(FileData {
                        name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                        data: base64::engine::general_purpose::STANDARD.encode(bytes),
                    }),
                    ..Default::default()
                },
                Err(e) => fail(format!("Failed to read {}: {e}", path.display())),
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
