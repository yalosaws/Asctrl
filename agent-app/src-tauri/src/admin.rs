//! Running as administrator, and restarting the computer.

use crate::link::{relay_url, reply, reply_text};
use crate::{AgentState, is_elevated};
use serde_json::{Value, json};
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;

/// Restart this program as administrator, keeping the same code. Shows a UAC prompt; returns false if the user clicks "No".
fn relaunch_elevated(code: &str) -> bool {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR, w};
    let Ok(exe) = std::env::current_exe() else { return false };
    let params = format!("--code {code} --relay \"{}\"", relay_url());
    let r = unsafe { ShellExecuteW(None, w!("runas"), &HSTRING::from(exe.as_os_str()), &HSTRING::from(params), PCWSTR::null(), SW_SHOWNORMAL) };
    r.0 as isize > 32
}

pub async fn request_admin(app: AppHandle, tx: mpsc::UnboundedSender<String>, id: String) {
    if is_elevated() {
        return reply_text(&tx, &id, true, "Already running as administrator");
    }
    let code = app.state::<AgentState>().code.lock().unwrap().clone();
    let ok = tokio::task::spawn_blocking(move || relaunch_elevated(&code)).await.unwrap_or(false);
    if !ok {
        return reply_text(&tx, &id, false, "The user clicked \"No\" in the Windows confirmation; still running without admin rights");
    }
    let msg = "The app is restarting as administrator with the same code.";
    reply(&tx, &id, crate::tools::Output { ok: true, text: Some(msg.into()), data: Some(json!({ "restarting": true })), ..Default::default() });
    tokio::time::sleep(Duration::from_millis(500)).await;
    crate::note_exit("quitting: restarting as administrator");
    app.exit(0);
}

const RUN_ONCE: &str = r"Software\Microsoft\Windows\CurrentVersion\RunOnce";
const RUN_ONCE_NAME: &str = "AsctrlResume";

/// Register "open this program once at next sign-in, with the same code"
fn arm_resume(code: &str) -> Result<(), String> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
    use windows::core::HSTRING;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let cmd = format!("\"{}\" --code {code} --relay \"{}\"", exe.display(), relay_url());
    let data: Vec<u16> = cmd.encode_utf16().chain([0]).collect();
    let r = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(RUN_ONCE), &HSTRING::from(RUN_ONCE_NAME), REG_SZ.0, Some(data.as_ptr() as *const _), (data.len() * 2) as u32) };
    if r.is_ok() { Ok(()) } else { Err(format!("Failed to write the startup entry: {r:?}")) }
}

fn disarm_resume() {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteKeyValueW};
    use windows::core::HSTRING;
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(RUN_ONCE), &HSTRING::from(RUN_ONCE_NAME));
    }
}

async fn shutdown_exe(args: &[&str]) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new("shutdown.exe");
    cmd.args(args).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let out = cmd.output().await.map_err(|e| e.to_string())?;
    if out.status.success() { Ok(()) } else { Err(format!("shutdown failed ({})", out.status.code().unwrap_or(-1))) }
}

/// Restart the computer: register auto-open after restart (same code), then count down and restart
pub async fn restart_computer(app: AppHandle, tx: mpsc::UnboundedSender<String>, id: String, args: Value) {
    let flag = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);
    let code = app.state::<AgentState>().code.lock().unwrap().clone();

    if flag("cancel") {
        disarm_resume();
        let r = shutdown_exe(&["/a"]).await;
        return reply_text(&tx, &id, true, if r.is_ok() { "Restart cancelled" } else { "No restart was pending; the auto-open entry has been cleared" });
    }

    if flag("prepareOnly") {
        return match arm_resume(&code) {
            Ok(()) => reply_text(
                &tx,
                &id,
                true,
                &format!("Registered: the app will open automatically at next sign-in, with code {code}. After the computer restarts (e.g. an installer asks for it, or the user restarts manually), connect again."),
            ),
            Err(e) => reply_text(&tx, &id, false, &e),
        };
    }

    if let Err(e) = arm_resume(&code) {
        return reply_text(&tx, &id, false, &e);
    }
    let delay = args.get("delaySec").and_then(Value::as_u64).unwrap_or(15).clamp(5, 600);
    let msg = format!("Asctrl: the computer will restart in {delay} seconds. The app will open again afterwards.");
    let d = delay.to_string();
    if let Err(e) = shutdown_exe(&["/r", "/t", &d, "/c", &msg]).await {
        disarm_resume();
        return reply_text(&tx, &id, false, &e);
    }
    reply_text(
        &tx,
        &id,
        true,
        &format!(
            "The computer restarts in {delay} seconds (restart_computer cancel=true cancels it). After restart and sign-in the app opens automatically with code {code}. Connect again after 1-3 minutes (connect with waitSec). If the computer has a sign-in password, wait until the user reaches the desktop."
        ),
    );
}
