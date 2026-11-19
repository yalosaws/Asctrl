//! The shell and files: everything that is not looking at or operating the screen.

use super::*;
use crate::{files, shell::Shell};

const MAX_FILE: usize = 20 * 1024 * 1024;

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
        // A folder's contents; with pattern, the files under it whose names match, newest first
        "list_files" => {
            let p = match str_arg(args, "path") {
                "" => "~",
                p => p,
            };
            let path = match resolve_path(p) {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let pattern = str_arg(args, "pattern").trim().to_string();
            if pattern.is_empty() {
                return blocking(move || files::list_dir(&path)).await;
            }
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
                files::write_whole(&path, &bytes)?;
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
        // A background job: its new output (all of it with all), or stop it; without jobId, the list
        "job" => {
            let mut jobs = ctx.jobs.lock().unwrap();
            let r = match (args.get("jobId").and_then(Value::as_u64), bool_arg(args, "stop") == Some(true)) {
                (None, false) => Ok(jobs.list()),
                (None, true) => Err("Say which job to stop (jobId)".to_string()),
                (Some(id), true) => jobs.stop(id as u32),
                (Some(id), false) => jobs.output(id as u32, bool_arg(args, "all").unwrap_or(false)),
            };
            match r {
                Ok(s) => text(s),
                Err(e) => fail(e),
            }
        }
        "run_powershell" => {
            let script = str_arg(args, "script");
            // A fresh session first (directory and variables cleared, background jobs stopped)
            if bool_arg(args, "reset") == Some(true) {
                ctx.reset_shell().await;
                if script.trim().is_empty() {
                    return text("Reset. The next command runs in a fresh PowerShell (back in the home directory)".into());
                }
            }
            if script.trim().is_empty() {
                return fail("script is empty");
            }
            if bool_arg(args, "background") == Some(true) {
                let r = ctx.jobs.lock().unwrap().start(script);
                return match r {
                    Ok(id) => text(format!(
                        "Started in the background as job {id}. Use job (jobId) to see progress and output, and job with stop: true to stop it. \
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
