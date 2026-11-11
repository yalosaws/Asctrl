// 被控端能执行的工具。每个工具返回文本或图片。
import { runPowerShell } from "./powershell.ts";
import type { ToolCall, ToolResult } from "../shared/protocol.ts";

type Output = Omit<ToolResult, "type" | "id">;

const MAX_TEXT = 60_000;

function textOut(s: string): Output {
  if (s.length > MAX_TEXT) s = s.slice(0, MAX_TEXT) + `\n…（已截断，共 ${s.length} 字符）`;
  return { ok: true, text: s };
}

async function psText(script: string, timeoutMs?: number): Promise<Output> {
  const r = await runPowerShell(script, timeoutMs);
  if (r.timedOut) return { ok: false, error: `超时（${timeoutMs ?? 30000}ms）\n${r.stdout}` };
  const parts = [r.stdout.trim()];
  if (r.stderr.trim()) parts.push(`[stderr]\n${r.stderr.trim()}`);
  if (r.exitCode !== 0) parts.push(`[exit code ${r.exitCode}]`);
  return textOut(parts.filter(Boolean).join("\n\n") || "(无输出)");
}

const SCREENSHOT_PS = (maxWidth: number) => `
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type -Namespace W -Name U -MemberDefinition '[DllImport("user32.dll")] public static extern bool SetProcessDPIAware();'
[W.U]::SetProcessDPIAware() | Out-Null
$b = [System.Windows.Forms.SystemInformation]::VirtualScreen
$src = New-Object System.Drawing.Bitmap $b.Width, $b.Height
$g = [System.Drawing.Graphics]::FromImage($src)
$g.CopyFromScreen($b.Left, $b.Top, 0, 0, $src.Size)
$scale = [Math]::Min(1.0, ${maxWidth} / $b.Width)
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
`;

const SYSTEM_INFO_PS = `
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
`;

const PROCESSES_PS = (top: number) => `
Get-Process | Sort-Object WorkingSet64 -Descending | Select-Object -First ${top} @{n='Name';e={$_.ProcessName}}, Id,
  @{n='MemMB';e={[math]::Round($_.WorkingSet64/1MB)}}, @{n='CpuSec';e={[math]::Round($_.CPU,1)}},
  @{n='Title';e={$_.MainWindowTitle}} | Format-Table -AutoSize | Out-String -Width 200
`;

const EVENT_LOG_PS = (log: string, hours: number, max: number) => `
$f = @{ LogName='${log}'; Level=1,2,3; StartTime=(Get-Date).AddHours(-${hours}) }
Get-WinEvent -FilterHashtable $f -MaxEvents ${max} -ErrorAction SilentlyContinue |
  Select-Object @{n='Time';e={$_.TimeCreated.ToString('MM-dd HH:mm:ss')}}, LevelDisplayName, ProviderName, Id,
  @{n='Message';e={($_.Message -split "\`n")[0]}} | Format-List | Out-String -Width 300
`;

function clampInt(v: unknown, def: number, min: number, max: number) {
  const n = Number(v ?? def);
  return Number.isFinite(n) ? Math.min(max, Math.max(min, Math.round(n))) : def;
}

export async function runTool(call: ToolCall): Promise<Output> {
  const a = call.args ?? {};
  switch (call.tool) {
    case "screenshot": {
      const r = await runPowerShell(SCREENSHOT_PS(clampInt(a.maxWidth, 1600, 320, 3840)), 20_000);
      const data = r.stdout.trim();
      if (!data || r.exitCode !== 0) return { ok: false, error: `截图失败：${r.stderr || "无输出"}` };
      return { ok: true, image: { data, mimeType: "image/jpeg" } };
    }
    case "system_info":
      return psText(SYSTEM_INFO_PS);
    case "list_processes":
      return psText(PROCESSES_PS(clampInt(a.top, 30, 1, 200)));
    case "event_log": {
      const log = ["System", "Application"].includes(String(a.log)) ? String(a.log) : "System";
      return psText(EVENT_LOG_PS(log, clampInt(a.hours, 24, 1, 720), clampInt(a.max, 30, 1, 200)), 60_000);
    }
    case "run_powershell": {
      const script = String(a.script ?? "");
      if (!script.trim()) return { ok: false, error: "script 为空" };
      return psText(script, clampInt(a.timeoutSec, 60, 1, 600) * 1000);
    }
    default:
      return { ok: false, error: `未知工具：${String(call.tool)}` };
  }
}
