//! 被控端能执行的工具。脚本和 Node 版 src/agent/tools.ts 保持一致。

use crate::powershell;
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

#[derive(Serialize, Default)]
pub struct Output {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<Image>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct Image {
    pub data: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
}

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
$b = [System.Windows.Forms.SystemInformation]::VirtualScreen
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

/// 给界面显示的一句话说明
pub fn describe(tool: &str, args: &Value) -> String {
    match tool {
        "screenshot" => "查看了你的屏幕".into(),
        "system_info" => "查看了电脑基本信息".into(),
        "list_processes" => "查看了正在运行的程序".into(),
        "event_log" => "查看了系统错误日志".into(),
        "run_powershell" => format!("执行命令：{}", args.get("script").and_then(Value::as_str).unwrap_or("").trim()),
        other => format!("未知操作：{other}"),
    }
}

pub async fn run(tool: &str, args: &Value) -> Output {
    match tool {
        "screenshot" => {
            let maxw = int_arg(args, "maxWidth", 1600, 320, 3840);
            let r = powershell::run(&SCREENSHOT_PS.replace("__MAXW__", &maxw.to_string()), Duration::from_secs(20)).await;
            let data = r.stdout.trim().to_string();
            if data.is_empty() || r.exit_code != Some(0) {
                return fail(format!("截图失败：{}", if r.stderr.is_empty() { "无输出" } else { &r.stderr }));
            }
            Output { ok: true, image: Some(Image { data, mime_type: "image/jpeg".into() }), ..Default::default() }
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
            let script = args.get("script").and_then(Value::as_str).unwrap_or("");
            if script.trim().is_empty() {
                return fail("script 为空");
            }
            ps_text(script, int_arg(args, "timeoutSec", 60, 1, 600) as u64).await
        }
        other => fail(format!("未知工具：{other}")),
    }
}
