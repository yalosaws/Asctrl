//! The shell, files and system information: everything that is not looking at or operating the screen.
//! The system_info / list_processes / event_log scripts match the Node version in src/agent/tools.ts.

use super::*;
use crate::{files, powershell, shell::Shell};

const MAX_FILE: usize = 20 * 1024 * 1024;

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

pub(super) async fn run(ctx: &Ctx, tool: &str, args: &Value) -> Output {
    match tool {
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
            let all = bool_arg(args, "replaceAll").unwrap_or(false);
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
            let ic = bool_arg(args, "ignoreCase").unwrap_or(true);
            let max = int_arg(args, "maxResults", 100, 1, 2000) as usize;
            let context = int_arg(args, "context", 0, 0, 10) as usize;
            blocking(move || files::search(&path, &pattern, &glob, ic, max, context)).await
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
        "job_output" => {
            let mut jobs = ctx.jobs.lock().unwrap();
            match args.get("jobId").and_then(Value::as_u64) {
                None => text(jobs.list()),
                Some(id) => match jobs.output(id as u32, bool_arg(args, "all").unwrap_or(false)) {
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
        "reset_shell" => {
            ctx.reset_shell().await;
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
            if bool_arg(args, "background") == Some(true) {
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
