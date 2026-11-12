//! 后台任务：在独立的 PowerShell 进程里跑耗时命令（sfc、DISM、安装程序等），
//! 不受单次调用超时限制，随时可以查看新输出或停止。技术员断开时全部停止。

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const MAX_BUF: usize = 2 * 1024 * 1024;

struct Job {
    script: String,
    started: Instant,
    pid: Option<u32>,
    out: Arc<Mutex<String>>,
    /// 已经返回给技术员的输出长度
    read_pos: usize,
    exit: Arc<Mutex<Option<String>>>,
    file: std::path::PathBuf,
}

#[derive(Default)]
pub struct Jobs {
    next: u32,
    map: HashMap<u32, Job>,
}

fn fmt_secs(secs: u64) -> String {
    format!("{}分{}秒", secs / 60, secs % 60)
}

fn kill_tree(pid: u32) {
    let mut cmd = std::process::Command::new("taskkill");
    cmd.args(["/T", "/F", "/PID", &pid.to_string()]).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let _ = cmd.status();
}

fn pump(mut r: impl AsyncReadExt + Unpin + Send + 'static, out: Arc<Mutex<String>>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        let mut pending: Vec<u8> = vec![];
        while let Ok(n) = r.read(&mut buf).await {
            if n == 0 {
                break;
            }
            pending.extend_from_slice(&buf[..n]);
            // 只把完整的 UTF-8 部分放进去，避免把一个汉字切成两半
            let valid = match std::str::from_utf8(&pending) {
                Ok(_) => pending.len(),
                Err(e) => e.valid_up_to(),
            };
            let s = String::from_utf8_lossy(&pending[..valid]).into_owned();
            pending.drain(..valid);
            let mut o = out.lock().unwrap();
            if o.len() < MAX_BUF {
                o.push_str(&s);
            }
        }
        if !pending.is_empty() {
            out.lock().unwrap().push_str(&String::from_utf8_lossy(&pending));
        }
    })
}

impl Jobs {
    pub fn start(&mut self, script: &str) -> Result<u32, String> {
        self.next += 1;
        let id = self.next;
        // 写成 .ps1 再用 -File 运行：多行脚本、引号都不用转义
        let file = std::env::temp_dir().join(format!("asctrl-job-{}-{id}.ps1", std::process::id()));
        let body = format!(
            "[Console]::OutputEncoding=[Text.Encoding]::UTF8; $ProgressPreference='SilentlyContinue'; $ConfirmPreference='None'\r\n\
             Set-Location $env:USERPROFILE\r\n& {{\r\n{script}\r\n}} 2>&1 | Out-String -Stream -Width 250\r\nexit $LASTEXITCODE\r\n"
        );
        std::fs::write(&file, [&[0xEF, 0xBB, 0xBF][..], body.as_bytes()].concat()).map_err(|e| format!("写临时脚本失败：{e}"))?;

        let mut cmd = Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-NoLogo", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&file)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        let mut child = cmd.spawn().map_err(|e| format!("启动 PowerShell 失败：{e}"))?;

        let out = Arc::new(Mutex::new(String::new()));
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
            let took = fmt_secs(started.elapsed().as_secs());
            *exit2.lock().unwrap() = Some(match status {
                Ok(s) => match s.code() {
                    Some(c) => format!("已结束，退出码 {c}（用时 {took}）"),
                    None => format!("已结束（用时 {took}）"),
                },
                Err(e) => format!("异常：{e}"),
            });
        });
        self.map.insert(id, Job { script: script.into(), started, pid, out, read_pos: 0, exit, file });
        Ok(id)
    }

    fn status(j: &Job) -> String {
        match j.exit.lock().unwrap().clone() {
            Some(s) => s,
            None => format!("运行中（已 {}）", fmt_secs(j.started.elapsed().as_secs())),
        }
    }

    /// 返回上次读取之后的新输出；all 为真时返回全部输出
    pub fn output(&mut self, id: u32, all: bool) -> Result<String, String> {
        let j = self.map.get_mut(&id).ok_or(format!("没有编号为 {id} 的后台任务"))?;
        let out = j.out.lock().unwrap().clone();
        let from = if all { 0 } else { j.read_pos.min(out.len()) };
        j.read_pos = out.len();
        let mut new = out[from..].trim_end().to_string();
        const TAIL: usize = 50_000;
        if new.len() > TAIL {
            let mut cut = new.len() - TAIL;
            while !new.is_char_boundary(cut) {
                cut += 1;
            }
            new = format!("…（前面 {} 字节省略）\n{}", cut, &new[cut..]);
        }
        let label = if all { "全部输出" } else { "新输出" };
        Ok(format!(
            "任务 {id}：{}\n---- {label} ----\n{}",
            Self::status(j),
            if new.is_empty() { "(暂无)" } else { &new }
        ))
    }

    pub fn list(&self) -> String {
        if self.map.is_empty() {
            return "没有后台任务".into();
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
        let j = self.map.get(&id).ok_or(format!("没有编号为 {id} 的后台任务"))?;
        if j.exit.lock().unwrap().is_some() {
            return Ok(format!("任务 {id} 已经结束了"));
        }
        if let Some(pid) = j.pid {
            kill_tree(pid);
        }
        Ok(format!("已停止任务 {id}"))
    }

    pub fn stop_all(&mut self) {
        for (_, j) in self.map.drain() {
            if j.exit.lock().unwrap().is_none() {
                if let Some(pid) = j.pid {
                    kill_tree(pid);
                }
            }
            let _ = std::fs::remove_file(&j.file);
        }
    }
}
