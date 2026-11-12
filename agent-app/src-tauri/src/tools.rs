//! 被控端能执行的工具。基础脚本和 Node 版 src/agent/tools.ts 保持一致。

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

/// 最近一次截图的位置和缩放，用来把截图上的坐标换算成屏幕坐标
#[derive(Clone, Copy)]
struct Geom {
    left: i32,
    top: i32,
    scale: f64,
    width: i32,
    height: i32,
}

impl Geom {
    /// 截图坐标 -> 屏幕坐标
    fn shot_to_screen(&self, x: f64, y: f64) -> (i32, i32) {
        (self.left + (x / self.scale).round() as i32, self.top + (y / self.scale).round() as i32)
    }

    /// 屏幕坐标 -> 截图坐标；不在这张截图范围内时返回 None
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
    /// 技术员断开时调用：结束 PowerShell 会话和所有后台任务
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
        s = s.chars().take(MAX_TEXT).collect::<String>() + &format!("\n…（已截断，共 {total} 字符）");
    }
    Output { ok: true, text: Some(s), ..Default::default() }
}

async fn ps_text(script: &str, timeout_secs: u64) -> Output {
    let r = powershell::run(script, Duration::from_secs(timeout_secs)).await;
    if r.timed_out {
        return fail(format!("超时（{timeout_secs} 秒）"));
    }
    let mut parts = vec![r.stdout.trim().to_string()];
    if !r.stderr.trim().is_empty() {
        parts.push(format!("[stderr]\n{}", r.stderr.trim()));
    }
    if r.exit_code != Some(0) {
        parts.push(format!("[exit code {:?}]", r.exit_code));
    }
    let joined = parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("\n\n");
    text(if joined.is_empty() { "(无输出)".into() } else { joined })
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
if ($f) { "[复制的文件]"; $f | ForEach-Object { $_.FullName }; return }
$img = Get-Clipboard -Format Image
if ($img) { "[剪贴板里是一张图片 $($img.Width)x$($img.Height)]"; return }
$t = Get-Clipboard -Format Text -Raw
if ($null -eq $t -or $t -eq '') { '(剪贴板是空的)' } else { $t }
"#;

fn str_arg<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("")
}

/// 给界面显示的一句话说明
pub fn describe(tool: &str, args: &Value) -> String {
    match tool {
        "screenshot" => "查看了你的屏幕".into(),
        "system_info" => "查看了电脑基本信息".into(),
        "list_processes" => "查看了正在运行的程序".into(),
        "event_log" => "查看了系统错误日志".into(),
        "run_powershell" if args.get("background").and_then(Value::as_bool) == Some(true) => {
            format!("执行命令：{}（后台运行）", str_arg(args, "script").trim())
        }
        "run_powershell" => format!("执行命令：{}", str_arg(args, "script").trim()),
        "reset_shell" => "重置了命令窗口".into(),
        "click" => {
            let what = match (str_arg(args, "button"), int_arg(args, "count", 1, 1, 3)) {
                ("right", _) => "右键点击",
                (_, 2) => "双击",
                _ => "点击",
            };
            format!("{what}了屏幕")
        }
        "scroll" => "滚动了页面".into(),
        "zoom" => "放大查看了屏幕的一部分".into(),
        "mouse_move" => "移动了鼠标".into(),
        "mouse_down" => "按住了鼠标".into(),
        "mouse_up" => "松开了鼠标".into(),
        "cursor_position" => "查看了鼠标位置".into(),
        "hold_key" => format!("按住了 {}", str_arg(args, "keys")),
        "read_clipboard" => "查看了剪贴板".into(),
        "write_clipboard" => "往剪贴板里放了内容".into(),
        "list_windows" => "查看了打开的窗口".into(),
        "focus_window" => match str_arg(args, "action") {
            "minimize" => "最小化了一个窗口".into(),
            "maximize" => "最大化了一个窗口".into(),
            "restore" => "还原了一个窗口".into(),
            "close" => "关闭了一个窗口".into(),
            _ => "切换了窗口".into(),
        },
        "open_app" => format!("打开了 {} {}", str_arg(args, "target"), str_arg(args, "args")).trim_end().to_string(),
        "read_file" => format!("查看了文件 {}", str_arg(args, "path")),
        "write_file" => format!("写入了文件 {}", str_arg(args, "path")),
        "edit_file" => format!("修改了文件 {}", str_arg(args, "path")),
        "list_dir" => format!("查看了文件夹 {}", str_arg(args, "path")),
        "find_files" => format!("在 {} 里找文件 {}", str_arg(args, "path"), str_arg(args, "pattern")),
        "search_files" => format!("在 {} 里搜索 {}", str_arg(args, "path"), str_arg(args, "pattern")),
        "job_output" => "查看了后台任务的进度".into(),
        "stop_job" => "停止了一个后台任务".into(),
        "drag" => "拖动了鼠标".into(),
        "type_text" => {
            let t = str_arg(args, "text");
            let short: String = t.chars().take(40).collect();
            format!("输入了文字：{short}{}", if t.chars().count() > 40 { "…" } else { "" })
        }
        "press_keys" => format!("按下了 {}", str_arg(args, "keys")),
        "upload_file" => format!("传了一个文件到 {}", str_arg(args, "path")),
        "download_file" => format!("取走了文件 {}", str_arg(args, "path")),
        other => format!("未知操作：{other}"),
    }
}

fn geom(ctx: &Ctx) -> Result<Geom, String> {
    ctx.geom.lock().unwrap().ok_or_else(|| "请先调用 screenshot，坐标以截图为准".to_string())
}

/// 截图坐标 -> 屏幕坐标
fn to_screen(ctx: &Ctx, args: &Value, kx: &str, ky: &str) -> Result<(i32, i32), String> {
    let g = geom(ctx)?;
    let x = args.get(kx).and_then(Value::as_f64).ok_or(format!("缺少 {kx}"))?;
    let y = args.get(ky).and_then(Value::as_f64).ok_or(format!("缺少 {ky}"))?;
    if x < 0.0 || y < 0.0 || x > g.width as f64 || y > g.height as f64 {
        return Err(format!("坐标超出截图范围（{}×{}）", g.width, g.height));
    }
    Ok(g.shot_to_screen(x, y))
}

/// 可选坐标：两个都没传返回 None
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

/// 截取屏幕的一块区域。rect 是 PowerShell 表达式，结果是 System.Drawing.Rectangle
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
        return Err("没有这个显示器".into());
    }
    match geom.filter(|_| !data.is_empty() && r.exit_code == Some(0)) {
        Some(geom) => Ok(Capture { geom, data, screens }),
        None => {
            let err = r.stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("无输出").to_string();
            Err(format!("截图失败：{err}"))
        }
    }
}

/// "1=1920x1080@0,0* 2=..." -> 给人看的说明
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
            format!("{n} 号 {size}{}", if primary { "（主屏）" } else { "" })
        })
        .collect::<Vec<_>>()
        .join("，");
    format!("\n共 {} 个显示器：{list}。传 display 可以只截某一个，看得更清楚。", parts.len())
}

fn win_line(g: Option<Geom>, w: &winmgr::Win) -> String {
    let mut flags = vec![];
    if w.foreground {
        flags.push("前台");
    }
    if w.minimized {
        flags.push("最小化");
    }
    if w.maximized {
        flags.push("最大化");
    }
    let (l, t, r, b) = w.rect;
    let pos = if w.minimized {
        String::new()
    } else {
        match g.and_then(|g| g.screen_to_shot((l + r) / 2, (t + b) / 2)) {
            Some((cx, cy)) => format!(" 中心在截图坐标({cx},{cy})"),
            None => " 不在当前截图范围内".into(),
        }
    };
    let flags = if flags.is_empty() { String::new() } else { format!(" [{}]", flags.join("，")) };
    format!("[{}] {} (pid {}) 「{}」{flags}{pos}", w.handle, w.process, w.pid, w.title)
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

/// 在后台线程跑会阻塞的输入操作，完成后稍等让界面刷新
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

/// 远程路径：支持 ~ 和 %环境变量%，相对路径以用户目录为准
fn resolve_path(p: &str) -> Result<PathBuf, String> {
    let p = p.trim();
    if p.is_empty() {
        return Err("path 为空".into());
    }
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let mut s = p.replacen('~', &home, if p.starts_with('~') { 1 } else { 0 });
    while let Some(a) = s.find('%') {
        let Some(b) = s[a + 1..].find('%').map(|i| a + 1 + i) else { break };
        let val = std::env::var(&s[a + 1..b]).map_err(|_| format!("环境变量 {} 不存在", &s[a + 1..b]))?;
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
            let mut s = if out.text.is_empty() { "(无输出)".to_string() } else { out.text };
            if let Some(code) = out.exit_code.filter(|c| *c != 0) {
                s.push_str(&format!("\n\n[exit code {code}]"));
            }
            text(s)
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
                    let which = if display == 0 { String::new() } else { format!("{display} 号显示器，") };
                    let screens = if display == 0 { describe_screens(&c.screens) } else { String::new() };
                    Output {
                        ok: true,
                        image: Some(Image { data: c.data, mime_type: "image/jpeg".into() }),
                        text: Some(format!(
                            "截图 {which}{}×{}。click/scroll/drag 等的坐标直接用这张图上的像素坐标。{screens}",
                            g.width, g.height
                        )),
                        ..Default::default()
                    }
                }
                Err(e) if e.contains("没有这个显示器") => fail(format!("没有 {display} 号显示器")),
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
                return fail("区域太小");
            }
            let maxw = int_arg(args, "maxWidth", 1600, 320, 3840);
            match capture(&format!("New-Object System.Drawing.Rectangle {l}, {t}, {}, {}", r - l, btm - t), maxw).await {
                Ok(c) => Output {
                    ok: true,
                    image: Some(Image { data: c.data, mime_type: "image/jpeg".into() }),
                    text: Some(format!(
                        "放大图 {}×{}（比上一张截图放大 {:.1} 倍）。这张图只用来看，点击坐标仍然以上一张 screenshot 为准。",
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
                Some(Some((px, py))) => text(format!("鼠标在截图坐标 ({px}, {py})")),
                Some(None) => text(format!("鼠标在屏幕坐标 ({x}, {y})，不在上一张截图范围内")),
                None => text(format!("鼠标在屏幕坐标 ({x}, {y})。先 screenshot 才能换算成截图坐标")),
            }
        }
        "mouse_move" => {
            let (x, y) = match to_screen(ctx, args, "x", "y") {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            input_op(move || input::mouse_move(x, y), "已移动鼠标".into()).await
        }
        "mouse_down" | "mouse_up" => {
            let at = match opt_screen(ctx, args) {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let button = str_arg(args, "button").to_string();
            let press = tool == "mouse_down";
            input_op(move || input::mouse_button(at, &button, press), if press { "已按下" } else { "已松开" }.into()).await
        }
        "hold_key" => {
            let k = str_arg(args, "keys").to_string();
            let ms = (args.get("seconds").and_then(Value::as_f64).unwrap_or(1.0).clamp(0.05, 10.0) * 1000.0) as u64;
            input_op(move || input::hold_keys(&k, ms), "已松开按键".into()).await
        }
        "read_clipboard" => ps_text(CLIPBOARD_READ_PS, 15).await,
        "write_clipboard" => {
            let t = str_arg(args, "text");
            let b64 = base64::engine::general_purpose::STANDARD.encode(t.as_bytes());
            let n = t.chars().count();
            ps_text(&format!("Set-Clipboard -Value ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{b64}'))); '已放入剪贴板（{n} 字）'"), 15).await
        }
        "list_windows" => {
            let g = *ctx.geom.lock().unwrap();
            blocking(move || {
                let ws = winmgr::list();
                if ws.is_empty() {
                    return Ok("没有可见窗口".into());
                }
                let lines: Vec<String> = ws.iter().map(|w| win_line(g, w)).collect();
                Ok(format!("{} 个窗口（从前到后，方括号里是 handle）：\n{}", ws.len(), lines.join("\n")))
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
                return fail("target 为空");
            }
            let a = str_arg(args, "args").to_string();
            let cwd = match str_arg(args, "cwd") {
                "" => String::new(),
                c => match resolve_path(c) {
                    Ok(p) => p.to_string_lossy().into_owned(),
                    Err(e) => return fail(e),
                },
            };
            let out = blocking(move || winmgr::open(&target, &a, &cwd).map(|_| format!("已打开 {target}"))).await;
            if out.ok {
                tokio::time::sleep(Duration::from_millis(1500)).await; // 等窗口出来
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
                return fail("pattern 为空");
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
            let Some(id) = args.get("jobId").and_then(Value::as_u64) else { return fail("缺少 jobId") };
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
            input_op(move || input::click(x, y, &button, count, &mods), "已点击".into()).await
        }
        "scroll" => {
            let (x, y) = match to_screen(ctx, args, "x", "y") {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let amount = int_arg(args, "amount", 3, -50, 50) as i32;
            let horizontal = args.get("horizontal").and_then(Value::as_bool).unwrap_or(false);
            input_op(move || input::scroll(x, y, amount, horizontal), "已滚动".into()).await
        }
        "drag" => {
            let from = to_screen(ctx, args, "fromX", "fromY");
            let to = to_screen(ctx, args, "toX", "toY");
            let ((x1, y1), (x2, y2)) = match (from, to) {
                (Ok(a), Ok(b)) => (a, b),
                (Err(e), _) | (_, Err(e)) => return fail(e),
            };
            input_op(move || input::drag(x1, y1, x2, y2), "已拖动".into()).await
        }
        "type_text" => {
            let t = str_arg(args, "text").to_string();
            if t.is_empty() {
                return fail("text 为空");
            }
            input_op(move || input::type_text(&t), "已输入".into()).await
        }
        "press_keys" => {
            let k = str_arg(args, "keys").to_string();
            input_op(move || input::press_keys(&k), "已按键".into()).await
        }
        "upload_file" => {
            let path = match resolve_path(str_arg(args, "path")) {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let bytes = match base64::engine::general_purpose::STANDARD.decode(str_arg(args, "data")) {
                Ok(b) => b,
                Err(e) => return fail(format!("data 不是有效的 base64：{e}")),
            };
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match std::fs::write(&path, &bytes) {
                Ok(()) => text(format!("已保存到 {}（{} 字节）", path.display(), bytes.len())),
                Err(e) => fail(format!("写入 {} 失败：{e}", path.display())),
            }
        }
        "download_file" => {
            let path = match resolve_path(str_arg(args, "path")) {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            match std::fs::metadata(&path) {
                Ok(m) if m.is_dir() => return fail("这是一个文件夹，只能下载单个文件"),
                Ok(m) if m.len() as usize > MAX_FILE => return fail(format!("文件太大（{} MB），上限 20 MB", m.len() / 1024 / 1024)),
                Err(e) => return fail(format!("读取 {} 失败：{e}", path.display())),
                _ => {}
            }
            match std::fs::read(&path) {
                Ok(bytes) => Output {
                    ok: true,
                    text: Some(format!("{}（{} 字节）", path.display(), bytes.len())),
                    file: Some(FileData {
                        name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                        data: base64::engine::general_purpose::STANDARD.encode(bytes),
                    }),
                    ..Default::default()
                },
                Err(e) => fail(format!("读取 {} 失败：{e}", path.display())),
            }
        }
        "reset_shell" => {
            ctx.reset().await;
            text("已重置，下一条命令会在新的 PowerShell 里运行（回到用户目录）".into())
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
                return fail("script 为空");
            }
            if args.get("background").and_then(Value::as_bool) == Some(true) {
                let r = ctx.jobs.lock().unwrap().start(script);
                return match r {
                    Ok(id) => text(format!(
                        "已在后台启动，任务编号 {id}。用 job_output 查看进度和输出，stop_job 停止。\
                         后台任务在独立的 PowerShell 里运行，不共享会话里的目录和变量。"
                    )),
                    Err(e) => fail(e),
                };
            }
            shell_run(ctx, script, int_arg(args, "timeoutSec", 60, 1, 600) as u64).await
        }
        other => fail(format!("未知工具：{other}")),
    }
}

// ---- 给动画用的位置信息（屏幕物理像素） ----

fn rect_json(r: (i32, i32, i32, i32)) -> Value {
    serde_json::json!([r.0, r.1, r.2 - r.0, r.3 - r.1])
}

/// 工具开始前：要点哪里、拖到哪里、操作哪个窗口
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

/// 工具结束后：截了哪块屏幕、窗口变成了什么样
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
        "list_windows" => {
            let rects: Vec<Value> =
                winmgr::list().iter().filter(|w| !w.minimized).take(24).map(|w| rect_json(w.rect)).collect();
            json!({ "windows": rects })
        }
        _ => json!({}),
    }
}
