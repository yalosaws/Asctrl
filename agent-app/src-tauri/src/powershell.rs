use base64::Engine;
use std::time::Duration;
use tokio::process::Command;

pub struct PsResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}

/// Run a PowerShell script with UTF-8 output (so non-ASCII text isn't garbled) and no visible window.
pub async fn run(script: &str, timeout: Duration) -> PsResult {
    let full = format!(
        "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8;$ProgressPreference='SilentlyContinue';\n{script}"
    );
    // -EncodedCommand takes UTF-16LE base64, which sidesteps every quoting problem
    let utf16: Vec<u8> = full.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);

    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &encoded])
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

    match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(out)) => PsResult {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            exit_code: out.status.code(),
            timed_out: false,
        },
        Ok(Err(e)) => PsResult { stdout: String::new(), stderr: e.to_string(), exit_code: Some(-1), timed_out: false },
        Err(_) => PsResult { stdout: String::new(), stderr: String::new(), exit_code: None, timed_out: true },
    }
}
