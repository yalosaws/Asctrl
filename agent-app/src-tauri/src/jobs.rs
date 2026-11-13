//! Background jobs: run long commands (sfc, DISM, installers, ...) in a separate PowerShell process,
//! free of the per-call timeout; new output can be read or the job stopped at any time. All stop when the technician disconnects.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Output kept per job. A job that prints more keeps its most recent output; the oldest is dropped.
const MAX_BUF: usize = 2 * 1024 * 1024;

#[derive(Default)]
struct Buf {
    text: String,
    /// Bytes already dropped from the front of `text`
    dropped: usize,
}

impl Buf {
    fn push(&mut self, s: &str) {
        self.text.push_str(s);
        if self.text.len() > MAX_BUF {
            // Trim well below the cap so this does not run again on every chunk
            let mut cut = self.text.len() - MAX_BUF * 3 / 4;
            while !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            self.text.drain(..cut);
            self.dropped += cut;
        }
    }
}

struct Job {
    script: String,
    started: Instant,
    pid: Option<u32>,
    out: Arc<Mutex<Buf>>,
    /// How much output (counted from the job's start) was already returned to the technician
    read_pos: usize,
    exit: Arc<Mutex<Option<String>>>,
}

#[derive(Default)]
pub struct Jobs {
    next: u32,
    map: HashMap<u32, Job>,
}

fn fmt_secs(secs: u64) -> String {
    format!("{}m{}s", secs / 60, secs % 60)
}

/// Kill a process and every process it started
pub fn kill_tree(pid: u32) {
    let mut cmd = std::process::Command::new("taskkill");
    cmd.args(["/T", "/F", "/PID", &pid.to_string()]).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let _ = cmd.status();
}

fn pump(mut r: impl AsyncReadExt + Unpin + Send + 'static, out: Arc<Mutex<Buf>>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        let mut pending: Vec<u8> = vec![];
        while let Ok(n) = r.read(&mut buf).await {
            if n == 0 {
                break;
            }
            pending.extend_from_slice(&buf[..n]);
            // Only append complete UTF-8 sequences, so a multi-byte character is never split
            let valid = match std::str::from_utf8(&pending) {
                Ok(_) => pending.len(),
                Err(e) => e.valid_up_to(),
            };
            out.lock().unwrap().push(&String::from_utf8_lossy(&pending[..valid]));
            pending.drain(..valid);
            // Bytes that can never become valid UTF-8 (a native program writing in the ANSI code page)
            // must not pile up and hold back everything after them
            if pending.len() >= 4 {
                out.lock().unwrap().push(&String::from_utf8_lossy(&pending));
                pending.clear();
            }
        }
        if !pending.is_empty() {
            out.lock().unwrap().push(&String::from_utf8_lossy(&pending));
        }
    })
}

impl Jobs {
    pub fn start(&mut self, script: &str) -> Result<u32, String> {
        self.next += 1;
        let id = self.next;
        // Write a .ps1 and run it with -File: no escaping needed for multi-line scripts or quotes
        let file = std::env::temp_dir().join(format!("asctrl-job-{}-{id}.ps1", std::process::id()));
        let body = format!(
            "[Console]::OutputEncoding=[Text.Encoding]::UTF8; $ProgressPreference='SilentlyContinue'; $ConfirmPreference='None'\r\n\
             Set-Location $env:USERPROFILE\r\n& {{\r\n{script}\r\n}} 2>&1 | Out-String -Stream -Width 250\r\nexit $LASTEXITCODE\r\n"
        );
        std::fs::write(&file, [&[0xEF, 0xBB, 0xBF][..], body.as_bytes()].concat()).map_err(|e| format!("Failed to write the temp script: {e}"))?;

        let mut cmd = Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-NoLogo", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&file)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        let mut child = cmd.spawn().map_err(|e| {
            let _ = std::fs::remove_file(&file);
            format!("Failed to start PowerShell: {e}")
        })?;

        let out = Arc::new(Mutex::new(Buf::default()));
        let exit = Arc::new(Mutex::new(None));
        let a = pump(child.stdout.take().unwrap(), out.clone());
        let b = pump(child.stderr.take().unwrap(), out.clone());
        let pid = child.id();
        let exit2 = exit.clone();
        let started = Instant::now();
        tokio::spawn(async move {
            let status = child.wait().await;
            let _ = a.await;
            let _ = b.await;
            let _ = std::fs::remove_file(&file);
            let took = fmt_secs(started.elapsed().as_secs());
            *exit2.lock().unwrap() = Some(match status {
                Ok(s) => match s.code() {
                    Some(c) => format!("Finished, exit code {c} (took {took})"),
                    None => format!("Finished (took {took})"),
                },
                Err(e) => format!("Error: {e}"),
            });
        });
        self.map.insert(id, Job { script: script.into(), started, pid, out, read_pos: 0, exit });
        Ok(id)
    }

    fn status(j: &Job) -> String {
        match j.exit.lock().unwrap().clone() {
            Some(s) => s,
            None => format!("Running ({} so far)", fmt_secs(j.started.elapsed().as_secs())),
        }
    }

    /// Return output since the last read; all output when `all` is true
    pub fn output(&mut self, id: u32, all: bool) -> Result<String, String> {
        let j = self.map.get_mut(&id).ok_or(format!("No background job {id}"))?;
        const TAIL: usize = 50_000;
        let (new, omitted) = {
            let buf = j.out.lock().unwrap();
            // Positions count from the job's start; whatever was dropped from the front is gone either way
            let mut from = if all { 0 } else { j.read_pos.saturating_sub(buf.dropped).min(buf.text.len()) };
            let mut omitted = if all { buf.dropped } else { buf.dropped.saturating_sub(j.read_pos) };
            j.read_pos = buf.dropped + buf.text.len();
            let rest = buf.text[from..].trim_end();
            if rest.len() > TAIL {
                let mut skip = rest.len() - TAIL;
                while !rest.is_char_boundary(skip) {
                    skip += 1;
                }
                from += skip;
                omitted += skip;
            }
            (buf.text[from..].trim_end().to_string(), omitted)
        };
        let new = if omitted > 0 { format!("… (first {omitted} bytes omitted)\n{new}") } else { new };
        let label = if all { "all output" } else { "new output" };
        Ok(format!(
            "Job {id}: {}\n---- {label} ----\n{}",
            Self::status(j),
            if new.is_empty() { "(none yet)" } else { &new }
        ))
    }

    pub fn list(&self) -> String {
        if self.map.is_empty() {
            return "No background jobs".into();
        }
        let mut ids: Vec<&u32> = self.map.keys().collect();
        ids.sort();
        ids.iter()
            .map(|id| {
                let j = &self.map[id];
                let first: String = j.script.trim().lines().next().unwrap_or("").chars().take(80).collect();
                format!("[{id}] {} — {first}", Self::status(j))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn stop(&mut self, id: u32) -> Result<String, String> {
        let j = self.map.get(&id).ok_or(format!("No background job {id}"))?;
        if j.exit.lock().unwrap().is_some() {
            return Ok(format!("Job {id} has already finished"));
        }
        if let Some(pid) = j.pid {
            kill_tree(pid);
        }
        Ok(format!("Stopped job {id}"))
    }

    pub fn stop_all(&mut self) {
        for (_, j) in self.map.drain() {
            if j.exit.lock().unwrap().is_none()
                && let Some(pid) = j.pid {
                    kill_tree(pid);
                }
        }
    }
}
