//! 被控端后台：连接中继、处理用户同意、执行工具，并把状态推给界面。
//! 协议与 ../../src/shared/protocol.ts 一致。

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

/// 等用户点"允许"的最长时间
const CONSENT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Default)]
struct AgentState {
    code: Mutex<String>,
    /// 协助码是启动参数指定的（例如提权重启后），撞码时不换码而是重试
    fixed_code: bool,
    tech_name: Mutex<Option<String>>,
    /// 等待用户回答的问题（连接同意 / 管理员同意）
    answer: Mutex<Option<oneshot::Sender<bool>>>,
    /// 用户点了"结束协助"
    kick: Notify,
    /// 最近一次状态，界面刚加载时用来补齐
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

/// 中继地址：启动参数 > 环境变量 > exe 旁边的 relay.txt > 编译时指定 > 本机
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
    option_env!("ASCTRL_RELAY_URL").unwrap_or("ws://localhost:8787").to_string()
}

fn auto_accept() -> bool {
    cfg!(debug_assertions) && std::env::var("ASCTRL_AUTO_ACCEPT").is_ok()
}

fn is_elevated() -> bool {
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin().as_bool() }
}

/// 以管理员身份重新启动自己，保持同一个协助码。会弹出 UAC 窗口，用户点"否"时返回 false。
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

fn activity(app: &AppHandle, text: String) {
    let _ = app.emit("activity", json!({ "text": text }));
}

/// 悬浮层一直在最上面，这里只是确保它可见
fn bring_to_front(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_always_on_top(true);
    }
}

/// 发给界面的参数：去掉大块数据，长文本截断
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

/// 会动鼠标的工具：执行期间暂停悬浮层的交互，免得点到小精灵身上
fn moves_mouse(tool: &str) -> bool {
    matches!(tool, "click" | "scroll" | "drag" | "mouse_move" | "mouse_down" | "mouse_up")
}

/// 弹出一个问题让用户回答，超时算拒绝
async fn ask_user(app: &AppHandle, status: Value) -> bool {
    let (otx, orx) = oneshot::channel::<bool>();
    *app.state::<AgentState>().answer.lock().unwrap() = Some(otx);
    set_status(app, status);
    bring_to_front(app);
    matches!(tokio::time::timeout(CONSENT_TIMEOUT, orx).await, Ok(Ok(true)))
}

fn connected_status(app: &AppHandle) -> Value {
    let st = app.state::<AgentState>();
    let tech = st.tech_name.lock().unwrap().clone().unwrap_or_default();
    json!({ "state": "connected", "techName": tech, "code": *st.code.lock().unwrap() })
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
        st.ctx.reset().await;
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn connect_once(app: &AppHandle) -> Result<(), String> {
    let st = app.state::<AgentState>();
    let (ws, _) = tokio_tungstenite::connect_async(relay_url()).await.map_err(|e| e.to_string())?;
    let (mut sink, mut stream) = ws.split();

    // 所有发出的消息都走这个通道，读和写互不阻塞
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(s) = rx.recv().await {
            if sink.send(WsMsg::text(s)).await.is_err() {
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
                activity(app, "你结束了协助".into());
                break Ok(());
            }
            msg = stream.next() => match msg {
                None => break Err("与服务器的连接断开了".into()),
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

    drop(tx);
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
                let _ = tx.send(json!({ "type": "consent_reply", "allow": allow }).to_string());
                let st = app.state::<AgentState>();
                if allow {
                    *st.tech_name.lock().unwrap() = Some(tech.clone());
                    set_status(&app, connected_status(&app));
                    activity(&app, format!("你允许了 {tech} 连接"));
                } else {
                    let code = st.code.lock().unwrap().clone();
                    set_status(&app, json!({ "state": "ready", "code": code }));
                }
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
                let desc = tools::describe(&tool, &args);
                activity(&app, desc.clone());
                tauri::async_runtime::spawn(async move {
                    let st = app.state::<AgentState>();
                    let ov = app.state::<overlay::Overlay>();
                    if moves_mouse(&tool) {
                        ov.suppress(Duration::from_secs(5));
                    }
                    let before = tools::fx_before(&st.ctx, &tool, &args);
                    let _ = app.emit(
                        "fx",
                        json!({ "phase": "start", "id": id, "tool": tool, "desc": desc, "args": fx_args(&args), "geo": before }),
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
            let who = st.tech_name.lock().unwrap().take();
            if let Some(who) = who {
                activity(app, format!("{who} 已断开"));
            }
            let code = st.code.lock().unwrap().clone();
            set_status(app, json!({ "state": "ready", "code": code }));
            let app = app.clone();
            tauri::async_runtime::spawn(async move { app.state::<AgentState>().ctx.reset().await });
        }
        Some("error") => {
            // 多半是协助码撞了：随机码就换一个；指定码（刚提权重启）就等旧进程退出后重试
            if !st.fixed_code {
                *st.code.lock().unwrap() = random_code();
            }
            return Err(s("message"));
        }
        _ => {}
    }
    Ok(())
}

async fn request_admin(app: AppHandle, tx: mpsc::UnboundedSender<String>, id: String) {
    if is_elevated() {
        return reply_text(&tx, &id, true, "已经是管理员权限");
    }
    let tech = app.state::<AgentState>().tech_name.lock().unwrap().clone().unwrap_or_default();
    activity(&app, "请求管理员权限".into());
    let allow = ask_user(&app, json!({ "state": "admin", "techName": tech, "timeoutSec": CONSENT_TIMEOUT.as_secs() })).await;
    set_status(&app, connected_status(&app));
    if !allow {
        activity(&app, "你拒绝了管理员权限".into());
        return reply_text(&tx, &id, false, "对方拒绝了管理员权限");
    }

    let code = app.state::<AgentState>().code.lock().unwrap().clone();
    let ok = tokio::task::spawn_blocking(move || relaunch_elevated(&code)).await.unwrap_or(false);
    if !ok {
        activity(&app, "Windows 确认窗口被取消".into());
        return reply_text(&tx, &id, false, "对方在 Windows 确认窗口里点了「否」，仍是普通权限");
    }
    reply_text(
        &tx,
        &id,
        true,
        "对方已同意，程序正在以管理员身份重启。约 5 秒后用同一个协助码重新 connect，对方需要再点一次允许。",
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    app.exit(0);
}

const RUN_ONCE: &str = r"Software\Microsoft\Windows\CurrentVersion\RunOnce";
const RUN_ONCE_NAME: &str = "AsctrlResume";

/// 登记"下次登录时自动打开一次本程序，用同一个协助码"
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
    if r.is_ok() { Ok(()) } else { Err(format!("写入开机启动项失败：{r:?}")) }
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
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let out = cmd.output().await.map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("shutdown 失败（{}）", out.status.code().unwrap_or(-1)))
    }
}

/// 重启电脑：先问用户，登记重启后自动打开（协助码不变），然后倒计时重启
async fn restart_computer(app: AppHandle, tx: mpsc::UnboundedSender<String>, id: String, args: Value) {
    let flag = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);
    let code = app.state::<AgentState>().code.lock().unwrap().clone();

    if flag("cancel") {
        activity(&app, "取消了重启".into());
        disarm_resume();
        let r = shutdown_exe(&["/a"]).await;
        return reply_text(&tx, &id, true, if r.is_ok() { "已取消重启" } else { "当前没有待执行的重启，已清除自动打开的登记" });
    }

    if flag("prepareOnly") {
        activity(&app, "设置了重启后自动打开协助程序".into());
        return match arm_resume(&code) {
            Ok(()) => reply_text(
                &tx,
                &id,
                true,
                &format!("已登记：下次登录后会自动打开协助程序，协助码仍是 {code}。电脑重启（例如安装程序要求重启、或请对方手动重启）后用同一个协助码 connect。"),
            ),
            Err(e) => reply_text(&tx, &id, false, &e),
        };
    }

    let tech = app.state::<AgentState>().tech_name.lock().unwrap().clone().unwrap_or_default();
    activity(&app, "请求重启电脑".into());
    let allow = ask_user(&app, json!({ "state": "reboot", "techName": tech, "timeoutSec": CONSENT_TIMEOUT.as_secs() })).await;
    set_status(&app, connected_status(&app));
    if !allow {
        activity(&app, "你拒绝了重启".into());
        return reply_text(&tx, &id, false, "对方拒绝了重启（可能还有没保存的东西）");
    }
    if let Err(e) = arm_resume(&code) {
        return reply_text(&tx, &id, false, &e);
    }
    let delay = args.get("delaySec").and_then(Value::as_u64).unwrap_or(15).clamp(5, 600);
    let msg = format!("远程协助：电脑将在 {delay} 秒后重启，重启后协助程序会自动打开。");
    let d = delay.to_string();
    if let Err(e) = shutdown_exe(&["/r", "/t", &d, "/c", &msg]).await {
        disarm_resume();
        return reply_text(&tx, &id, false, &e);
    }
    activity(&app, format!("电脑将在 {delay} 秒后重启"));
    reply_text(
        &tx,
        &id,
        true,
        &format!(
            "对方已同意，电脑将在 {delay} 秒后重启（restart_computer cancel=true 可取消）。重启并登录后程序会自动打开，协助码仍是 {code}。通常 1~3 分钟后用同一个协助码 connect，对方需要再点一次允许；如果对方电脑有登录密码，要等对方登录进桌面。"
        ),
    );
}

// ---- 给界面调用的命令 ----

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
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
        .expect("启动失败");
}
