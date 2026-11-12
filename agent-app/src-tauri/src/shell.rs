//! Persistent PowerShell session: the current directory and variables carry over between commands.
//! Each script is sent to stdin as a single base64 line and run with Invoke-Expression in the same scope;
//! the output ends with a random end marker.

use base64::Engine;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

pub struct Shell {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

pub struct ShellOutput {
    pub text: String,
    pub exit_code: Option<i64>,
}

const INIT: &str = "[Console]::OutputEncoding=[Text.Encoding]::UTF8; $ProgressPreference='SilentlyContinue'; \
$ConfirmPreference='None'; Set-Location $env:USERPROFILE\n";

impl Shell {
    pub async fn start() -> Result<Shell, String> {
        let mut cmd = Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-NoLogo", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = cmd.spawn().map_err(|e| format!("Failed to start PowerShell: {e}"))?;
        let mut stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        stdin.write_all(INIT.as_bytes()).await.map_err(|e| e.to_string())?;
        Ok(Shell { child, stdin, stdout })
    }

    /// Run a script. A timeout returns Err and the caller should drop this session (it may be stuck on an interactive prompt).
    pub async fn run(&mut self, script: &str, timeout: Duration) -> Result<ShellOutput, String> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(script.as_bytes());
        let marker = format!("__ASCTRL_DONE_{:016x}__", rand::random::<u64>());
        // Read $? and $LASTEXITCODE before Out-String so they are not overwritten
        let line = format!(
            "$global:LASTEXITCODE=$null; try {{ Invoke-Expression ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{b64}'))) 2>&1 | Out-String -Width 250 }} \
             catch {{ \"[error] \" + ($_ | Out-String) }}; \"{marker} $LASTEXITCODE\"\n"
        );
        self.stdin.write_all(line.as_bytes()).await.map_err(|e| format!("The session is no longer valid: {e}"))?;
        self.stdin.flush().await.map_err(|e| e.to_string())?;

        let read = async {
            let mut out = String::new();
            let mut buf = Vec::new();
            loop {
                buf.clear();
                let n = self.stdout.read_until(b'\n', &mut buf).await.map_err(|e| e.to_string())?;
                if n == 0 {
                    return Err("The PowerShell session exited unexpectedly".to_string());
                }
                let l = String::from_utf8_lossy(&buf);
                if let Some(rest) = l.trim_end().strip_prefix(&marker) {
                    let exit_code = rest.trim().parse::<i64>().ok();
                    return Ok(ShellOutput { text: out.trim_end().to_string(), exit_code });
                }
                out.push_str(&l);
            }
        };
        match tokio::time::timeout(timeout, read).await {
            Ok(r) => r,
            Err(_) => Err(format!("Timed out ({} s). The command may be waiting for input; the session was reset and the current directory and variables are lost", timeout.as_secs())),
        }
    }

    pub fn kill(mut self) {
        let _ = self.child.start_kill();
    }
}
