//! 被控端后台：连接中继、处理用户同意、执行工具，并把状态推给界面。
//! 协议与 ../../src/shared/protocol.ts 一致。

mod powershell;
mod tools;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State, UserAttentionType};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as WsMsg;

/// 等用户点"允许"的最长时间
const CONSENT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Default)]
struct AgentState {
    code: Mutex<String>,
    tech_name: Mutex<Option<String>>,
    consent: Mutex<Option<oneshot::Sender<bool>>>,
    /// 用户点了"断开"
    kick: Notify,
    /// 最近一次状态，界面刚加载时用来补齐
    status: Mutex<Value>,
}

fn new_code() -> String {
    #[cfg(debug_assertions)]
    if let Ok(c) = std::env::var("ASCTRL_CODE") {
        return c;
    }
    format!("{:06}", rand::random_range(0..1_000_000))
}

/// 中继地址：环境变量 > exe 旁边的 relay.txt > 编译时指定 > 本机
fn relay_url() -> String {
    if let Ok(u) = std::env::var("ASCTRL_RELAY_URL") {
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

fn set_status(app: &AppHandle, status: Value) {
    *app.state::<AgentState>().status.lock().unwrap() = status.clone();
    let _ = app.emit("status", status);
}

fn activity(app: &AppHandle, text: String) {
    let _ = app.emit("activity", json!({ "text": text }));
}

fn bring_to_front(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        let _ = w.request_user_attention(Some(UserAttentionType::Critical));
    }
}

async fn relay_loop(app: AppHandle) {
    loop {
        set_status(&app, json!({ "state": "connecting" }));
        if let Err(e) = connect_once(&app).await {
            set_status(&app, json!({ "state": "offline", "message": e }));
        }
        *app.state::<AgentState>().tech_name.lock().unwrap() = None;
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
                activity(app, "你断开了连接".into());
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
            let (otx, orx) = oneshot::channel::<bool>();
            *st.consent.lock().unwrap() = Some(otx);
            set_status(app, json!({ "state": "consent", "techName": tech, "timeoutSec": CONSENT_TIMEOUT.as_secs() }));
            bring_to_front(app);

            let (app, tx) = (app.clone(), tx.clone());
            tauri::async_runtime::spawn(async move {
                let allow = if auto_accept() {
                    true
                } else {
                    matches!(tokio::time::timeout(CONSENT_TIMEOUT, orx).await, Ok(Ok(true)))
                };
                let _ = tx.send(json!({ "type": "consent_reply", "allow": allow }).to_string());
                let st = app.state::<AgentState>();
                let code = st.code.lock().unwrap().clone();
                if allow {
                    *st.tech_name.lock().unwrap() = Some(tech.clone());
                    set_status(&app, json!({ "state": "connected", "techName": tech, "code": code }));
                    activity(&app, format!("你允许了 {tech} 连接"));
                } else {
                    set_status(&app, json!({ "state": "ready", "code": code }));
                }
            });
        }
        Some("call") => {
            let id = s("id");
            let tool = s("tool");
            let args = msg.get("args").cloned().unwrap_or(json!({}));
            activity(app, tools::describe(&tool, &args));
            let tx = tx.clone();
            tauri::async_runtime::spawn(async move {
                let out = tools::run(&tool, &args).await;
                let mut reply = serde_json::to_value(out).unwrap_or(json!({ "ok": false }));
                reply["type"] = json!("result");
                reply["id"] = json!(id);
                let _ = tx.send(reply.to_string());
            });
        }
        Some("peer_left") => {
            st.consent.lock().unwrap().take();
            let who = st.tech_name.lock().unwrap().take();
            if let Some(who) = who {
                activity(app, format!("{who} 已断开"));
            }
            let code = st.code.lock().unwrap().clone();
            set_status(app, json!({ "state": "ready", "code": code }));
        }
        Some("error") => {
            // 多半是连接码撞了，换一个重新注册
            *st.code.lock().unwrap() = new_code();
            return Err(s("message"));
        }
        _ => {}
    }
    Ok(())
}

// ---- 给界面调用的命令 ----

#[tauri::command]
fn get_status(st: State<AgentState>) -> Value {
    st.status.lock().unwrap().clone()
}

#[tauri::command]
fn respond_consent(st: State<AgentState>, allow: bool) {
    if let Some(tx) = st.consent.lock().unwrap().take() {
        let _ = tx.send(allow);
    }
}

#[tauri::command]
fn disconnect(st: State<AgentState>) {
    st.kick.notify_one();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let state = AgentState::default();
    *state.code.lock().unwrap() = new_code();
    *state.status.lock().unwrap() = json!({ "state": "connecting" });

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![get_status, respond_consent, disconnect])
        .setup(|app| {
            tauri::async_runtime::spawn(relay_loop(app.handle().clone()));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("启动失败");
}
