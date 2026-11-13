//! Agent backend: connects to the relay, handles user consent, runs tools, and pushes state to the UI.
//! The protocol matches ../../src/shared/protocol.ts.

mod capture;
mod files;
mod input;
mod jobs;
mod overlay;
mod powershell;
mod shell;
mod tools;
mod winmgr;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as WsMsg;

/// How long to wait for the user to click "Allow"
const CONSENT_TIMEOUT: Duration = Duration::from_secs(120);
/// The agent can sit idle for a long time while the person reads the code out. Without traffic a home
/// router forgets the connection and the code silently stops working, so ping the relay regularly;
/// a ping that can't be delivered also makes a dead connection surface as an error.
const PING_EVERY: Duration = Duration::from_secs(25);

#[derive(Default)]
struct AgentState {
    code: Mutex<String>,
    /// The code was given on the command line (e.g. after an elevated restart); on a collision, retry instead of picking a new one
    fixed_code: bool,
    tech_name: Mutex<Option<String>>,
    /// Question awaiting the user's answer (connection consent / admin consent)
    answer: Mutex<Option<oneshot::Sender<bool>>>,
    /// The user clicked "End session"
    kick: Notify,
    /// Latest status, replayed to the UI when it first loads
    status: Mutex<Value>,
    ctx: tools::Ctx,
}

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn random_code() -> String {
    format!("{:06}", rand::random_range(0..1_000_000))
}

/// Relay URL: command-line flag > environment variable > relay.txt next to the exe > compile-time value >
/// hosted relay (release) / localhost (debug)
fn relay_url() -> String {
    if let Some(u) = arg("--relay").or_else(|| std::env::var("ASCTRL_RELAY_URL").ok()) {
        return u;
    }
    if let Some(u) = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::read_to_string(p.with_file_name("relay.txt")).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return u;
    }
    // A release exe built without ASCTRL_RELAY_URL used to fall back to localhost and silently
    // register its code with a relay the console never sees, so release builds default to the hosted one.
    let fallback = if cfg!(debug_assertions) { "ws://localhost:8787" } else { "ws://124.222.182.40:8787" };
    option_env!("ASCTRL_RELAY_URL").unwrap_or(fallback).to_string()
}

fn auto_accept() -> bool {
    cfg!(debug_assertions) && std::env::var("ASCTRL_AUTO_ACCEPT").is_ok()
}

fn is_elevated() -> bool {
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin().as_bool() }
}

/// Restart this program as administrator, keeping the same code. Shows a UAC prompt; returns false if the user clicks "No".
fn relaunch_elevated(code: &str) -> bool {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR, w};
    let Ok(exe) = std::env::current_exe() else { return false };
    let params = format!("--code {code} --relay \"{}\"", relay_url());
    let r = unsafe {
        ShellExecuteW(None, w!("runas"), &HSTRING::from(exe.as_os_str()), &HSTRING::from(params), PCWSTR::null(), SW_SHOWNORMAL)
    };
    r.0 as isize > 32
}

fn set_status(app: &AppHandle, status: Value) {
    *app.state::<AgentState>().status.lock().unwrap() = status.clone();
    let _ = app.emit("status", status);
}

/// The overlay is always on top; this just makes sure it is visible
fn bring_to_front(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_always_on_top(true);
    }
}

/// Arguments sent to the UI: large data removed, long text truncated
fn fx_args(args: &Value) -> Value {
    let mut a = args.clone();
    if let Some(o) = a.as_object_mut() {
        o.remove("data");
        for v in o.values_mut() {
            if let Some(t) = v.as_str() {
                if t.chars().count() > 4000 {
                    *v = json!(t.chars().take(4000).collect::<String>());
                }
            }
        }
    }
    a
}

fn fx_text(out: &tools::Output) -> String {
    let t = out.text.as_deref().or(out.error.as_deref()).unwrap_or("");
    t.chars().take(6000).collect()
}

/// Tools that move the mouse: pause overlay interaction while they run so they don't hit the orb
fn moves_mouse(tool: &str) -> bool {
    matches!(tool, "click" | "scroll" | "drag" | "mouse_move" | "mouse_look" | "mouse_down" | "mouse_up")
}

/// Ask the user a question; a timeout counts as a refusal
async fn ask_user(app: &AppHandle, status: Value) -> bool {
    let (otx, orx) = oneshot::channel::<bool>();
    *app.state::<AgentState>().answer.lock().unwrap() = Some(otx);
    set_status(app, status);
    bring_to_front(app);
    matches!(tokio::time::timeout(CONSENT_TIMEOUT, orx).await, Ok(Ok(true)))
}

/// The status when no question is on screen: connected while a technician is paired, otherwise ready
fn idle_status(app: &AppHandle) -> Value {
    let st = app.state::<AgentState>();
    let code = st.code.lock().unwrap().clone();
    match st.tech_name.lock().unwrap().clone() {
        Some(tech) => json!({ "state": "connected", "techName": tech, "code": code }),
        None => json!({ "state": "ready", "code": code }),
    }
}

fn reply(tx: &mpsc::UnboundedSender<String>, id: &str, out: tools::Output) {
    let mut v = serde_json::to_value(out).unwrap_or(json!({ "ok": false }));
    v["type"] = json!("result");
    v["id"] = json!(id);
    let _ = tx.send(v.to_string());
}

fn reply_text(tx: &mpsc::UnboundedSender<String>, id: &str, ok: bool, msg: &str) {
    let out = if ok {
        tools::Output { ok, text: Some(msg.into()), ..Default::default() }
    } else {
        tools::Output { ok, error: Some(msg.into()), ..Default::default() }
    };
    reply(tx, id, out);
}

async fn relay_loop(app: AppHandle) {
    loop {
        set_status(&app, json!({ "state": "connecting" }));
        if let Err(e) = connect_once(&app).await {
            set_status(&app, json!({ "state": "offline", "message": e }));
        }
        let st = app.state::<AgentState>();
        *st.tech_name.lock().unwrap() = None;
        // A question from the connection that just ended can no longer be answered
        st.answer.lock().unwrap().take();
        st.ctx.reset().await;
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn connect_once(app: &AppHandle) -> Result<(), String> {
    let st = app.state::<AgentState>();
    let (ws, _) = tokio_tungstenite::connect_async(relay_url()).await.map_err(|e| e.to_string())?;
    let (mut sink, mut stream) = ws.split();

    // Every outgoing message goes through this channel so reads and writes never block each other.
    // The writer stops on `stop`, not when the last sender is dropped: tool calls still running hold
    // senders for as long as they run (up to 10 minutes), and "End session" must close the connection now.
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let (stop, mut stopped) = oneshot::channel::<()>();
    let writer = tokio::spawn(async move {
        let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + PING_EVERY, PING_EVERY);
        loop {
            // Biased: results already queued are sent before the stop is honoured
            let sent = tokio::select! {
                biased;
                msg = rx.recv() => match msg {
                    Some(s) => sink.send(WsMsg::text(s)).await,
                    None => break,
                },
                _ = &mut stopped => break,
                _ = ping.tick() => sink.send(WsMsg::Ping(Default::default())).await,
            };
            if sent.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let code = st.code.lock().unwrap().clone();
    let hostname = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown".into());
    let _ = tx.send(json!({ "type": "agent_hello", "code": code, "hostname": hostname }).to_string());
    set_status(app, json!({ "state": "ready", "code": code }));

    let result = loop {
        tokio::select! {
            _ = st.kick.notified() => {
                break Ok(());
            }
            msg = stream.next() => match msg {
                None => break Err("Lost the connection to the server".into()),
                Some(Err(e)) => break Err(e.to_string()),
                Some(Ok(WsMsg::Text(t))) => {
                    if let Ok(v) = serde_json::from_str::<Value>(t.as_str()) {
                        if let Err(e) = handle(app, &tx, v) {
                            break Err(e);
                        }
                    }
                }
                Some(Ok(_)) => {}
            }
        }
    };

    let _ = stop.send(());
    let _ = writer.await;
    result
}

fn handle(app: &AppHandle, tx: &mpsc::UnboundedSender<String>, msg: Value) -> Result<(), String> {
    let st = app.state::<AgentState>();
    let s = |k: &str| msg.get(k).and_then(Value::as_str).unwrap_or("").to_string();

    match msg.get("type").and_then(Value::as_str) {
        Some("consent_request") => {
            let tech = s("techName");
            let (app, tx) = (app.clone(), tx.clone());
            tauri::async_runtime::spawn(async move {
                let allow = auto_accept()
                    || ask_user(&app, json!({ "state": "consent", "techName": tech, "timeoutSec": CONSENT_TIMEOUT.as_secs() })).await;
                // The connection this request came from is gone: its status is no longer ours to set
                if tx.send(json!({ "type": "consent_reply", "allow": allow }).to_string()).is_err() {
                    return;
                }
                if allow {
                    *app.state::<AgentState>().tech_name.lock().unwrap() = Some(tech);
                }
                set_status(&app, idle_status(&app));
            });
        }
        Some("call") => {
            let id = s("id");
            let tool = s("tool");
            let args = msg.get("args").cloned().unwrap_or(json!({}));
            let (app, tx) = (app.clone(), tx.clone());
            if tool == "request_admin" {
                tauri::async_runtime::spawn(request_admin(app, tx, id));
            } else if tool == "restart_computer" {
                tauri::async_runtime::spawn(restart_computer(app, tx, id, args));
            } else {
                tauri::async_runtime::spawn(async move {
                    let st = app.state::<AgentState>();
                    let ov = app.state::<overlay::Overlay>();
                    if moves_mouse(&tool) {
                        ov.suppress(Duration::from_secs(5));
                    }
                    let before = tools::fx_before(&st.ctx, &tool, &args);
                    let _ = app.emit(
                        "fx",
                        json!({ "phase": "start", "id": id, "tool": tool, "args": fx_args(&args), "geo": before }),
                    );
                    let out = tools::run(&st.ctx, &tool, &args).await;
                    if moves_mouse(&tool) {
                        ov.suppress(Duration::from_millis(1200));
                    }
                    let after = tools::fx_after(&st.ctx, &tool, &before);
                    let _ = app.emit(
                        "fx",
                        json!({ "phase": "end", "id": id, "tool": tool, "ok": out.ok, "text": fx_text(&out), "geo": after }),
                    );
                    reply(&tx, &id, out);
                });
            }
        }
        Some("peer_left") => {
            st.answer.lock().unwrap().take();
            st.tech_name.lock().unwrap().take();
            set_status(app, idle_status(app));
            let app = app.clone();
            tauri::async_runtime::spawn(async move { app.state::<AgentState>().ctx.reset().await });
        }
        Some("error") => {
            // Most likely a code collision: pick a new random code; with a fixed code (just restarted elevated), wait for the old process to exit and retry
            if !st.fixed_code {
                *st.code.lock().unwrap() = random_code();
            }
            return Err(s("message"));
        }
        _ => {}
    }
    Ok(())
}

/// False once the technician who asked has left or the relay connection ended while the question was
/// on screen. The status was already updated by whoever noticed, and nobody is waiting for the result;
/// carrying on would show "connected" with nobody connected, or restart the computer for no one.
fn still_paired(app: &AppHandle, tx: &mpsc::UnboundedSender<String>) -> bool {
    !tx.is_closed() && app.state::<AgentState>().tech_name.lock().unwrap().is_some()
}

async fn request_admin(app: AppHandle, tx: mpsc::UnboundedSender<String>, id: String) {
    if is_elevated() {
        return reply_text(&tx, &id, true, "Already running as administrator");
    }
    let tech = app.state::<AgentState>().tech_name.lock().unwrap().clone().unwrap_or_default();
    let allow = ask_user(&app, json!({ "state": "admin", "techName": tech, "timeoutSec": CONSENT_TIMEOUT.as_secs() })).await;
    if !still_paired(&app, &tx) {
        return;
    }
    set_status(&app, idle_status(&app));
    if !allow {
        return reply_text(&tx, &id, false, "The user declined administrator access");
    }

    let code = app.state::<AgentState>().code.lock().unwrap().clone();
    let ok = tokio::task::spawn_blocking(move || relaunch_elevated(&code)).await.unwrap_or(false);
    if !ok {
        return reply_text(&tx, &id, false, "The user clicked \"No\" in the Windows confirmation; still running without admin rights");
    }
    reply_text(
        &tx,
        &id,
        true,
        "The user agreed; the app is restarting as administrator. In about 5 seconds, connect again with the same code; the user will need to click Allow once more.",
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
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
    let r = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN_ONCE),
            &HSTRING::from(RUN_ONCE_NAME),
            REG_SZ.0,
            Some(data.as_ptr() as *const _),
            (data.len() * 2) as u32,
        )
    };
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
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("shutdown failed ({})", out.status.code().unwrap_or(-1)))
    }
}

/// Restart the computer: ask the user, register auto-open after restart (same code), then count down and restart
async fn restart_computer(app: AppHandle, tx: mpsc::UnboundedSender<String>, id: String, args: Value) {
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
                &format!("Registered: the assist app will open automatically at next sign-in, with code {code}. After the computer restarts (e.g. an installer asks for it, or the user restarts manually), connect with the same code."),
            ),
            Err(e) => reply_text(&tx, &id, false, &e),
        };
    }

    let tech = app.state::<AgentState>().tech_name.lock().unwrap().clone().unwrap_or_default();
    let allow = ask_user(&app, json!({ "state": "reboot", "techName": tech, "timeoutSec": CONSENT_TIMEOUT.as_secs() })).await;
    if !still_paired(&app, &tx) {
        return;
    }
    set_status(&app, idle_status(&app));
    if !allow {
        return reply_text(&tx, &id, false, "The user declined the restart (they may have unsaved work)");
    }
    if let Err(e) = arm_resume(&code) {
        return reply_text(&tx, &id, false, &e);
    }
    let delay = args.get("delaySec").and_then(Value::as_u64).unwrap_or(15).clamp(5, 600);
    let msg = format!("Remote Assist: the computer will restart in {delay} seconds. The assist app will open again afterwards.");
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
            "The user agreed; the computer restarts in {delay} seconds (restart_computer cancel=true cancels it). After restart and sign-in the app opens automatically with code {code}. Usually connect with the same code after 1-3 minutes; the user will need to click Allow again. If the computer has a sign-in password, wait until the user reaches the desktop."
        ),
    );
}

// ---- Commands called by the UI ----

#[tauri::command]
fn get_status(st: State<AgentState>) -> Value {
    st.status.lock().unwrap().clone()
}

#[tauri::command]
fn is_admin() -> bool {
    is_elevated()
}

#[tauri::command]
fn respond(st: State<AgentState>, allow: bool) {
    if let Some(tx) = st.answer.lock().unwrap().take() {
        let _ = tx.send(allow);
    }
}

#[tauri::command]
fn disconnect(st: State<AgentState>) {
    st.kick.notify_one();
}

#[tauri::command]
fn quit(app: AppHandle) {
    app.exit(0);
}

pub fn run() {
    let fixed = arg("--code")
        .filter(|c| c.len() == 6 && c.bytes().all(|b| b.is_ascii_digit()))
        .or_else(|| if cfg!(debug_assertions) { std::env::var("ASCTRL_CODE").ok() } else { None });
    let state = AgentState { fixed_code: fixed.is_some(), ..Default::default() };
    *state.code.lock().unwrap() = fixed.unwrap_or_else(random_code);
    *state.status.lock().unwrap() = json!({ "state": "connecting" });

    tauri::Builder::default()
        .manage(state)
        .manage(overlay::Overlay::default())
        .invoke_handler(tauri::generate_handler![
            get_status,
            is_admin,
            respond,
            disconnect,
            quit,
            overlay::overlay_info,
            overlay::set_hit_regions
        ])
        .setup(|app| {
            overlay::setup(app.handle());
            tauri::async_runtime::spawn(relay_loop(app.handle().clone()));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to start");
}
