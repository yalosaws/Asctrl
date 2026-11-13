//! How a controller reaches the driver, and how its tool calls are run.
//!
//! Two links carry the same messages ({type:"call"} in, {type:"result"} out):
//! - local: a WebSocket on 127.0.0.1, for an AI running on this computer. No relay, no code, no round trip
//!   over the internet. The port and a random token are written to %LOCALAPPDATA%\Asctrl\local.json, which
//!   only this user can read; a connection must present the token (and must not come from a web page).
//! - relay: both sides connect out to a relay and are paired by the 6-digit code on screen, so the
//!   computer can be anywhere, behind any router.

use crate::{AgentState, RelayState, Via, admin, attach, detach, fx, overlay, publish, random_code, tools};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as WsMsg;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

/// The agent can sit idle for a long time while the person reads the code out. Without traffic a home
/// router forgets the connection and the code silently stops working, so ping the relay regularly;
/// a ping that can't be delivered also makes a dead connection surface as an error.
const PING_EVERY: Duration = Duration::from_secs(25);

/// Uploads travel as one message: 20 MB of file is 27 MB of base64
fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default().max_message_size(Some(64 << 20)).max_frame_size(Some(64 << 20))
}

/// Relay URL: command-line flag > environment variable > relay.txt next to the exe > compile-time value >
/// hosted relay (release) / localhost (debug)
pub fn relay_url() -> String {
    if let Some(u) = crate::arg("--relay").or_else(|| std::env::var("ASCTRL_RELAY_URL").ok()) {
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

// ---- Running a call ----

/// Arguments sent to the UI: large data removed, long text truncated
fn fx_args(args: &Value) -> Value {
    let mut a = args.clone();
    if let Some(o) = a.as_object_mut() {
        o.remove("data");
        for v in o.values_mut() {
            if let Some(t) = v.as_str()
                && t.chars().count() > 4000
            {
                *v = json!(t.chars().take(4000).collect::<String>());
            }
        }
    }
    a
}

/// Tools that may move the mouse: pause overlay interaction while they run so they don't hit the orb
fn moves_mouse(tool: &str) -> bool {
    matches!(tool, "click" | "batch" | "scroll" | "drag" | "mouse" | "type_text" | "set_value")
}

pub fn reply(tx: &mpsc::UnboundedSender<String>, id: &str, out: tools::Output) {
    let mut v = serde_json::to_value(out).unwrap_or(json!({ "ok": false }));
    v["type"] = json!("result");
    v["id"] = json!(id);
    let _ = tx.send(v.to_string());
}

pub fn reply_text(tx: &mpsc::UnboundedSender<String>, id: &str, ok: bool, msg: &str) {
    let out = if ok {
        tools::Output { ok, text: Some(msg.into()), ..Default::default() }
    } else {
        tools::Output { ok, error: Some(msg.into()), ..Default::default() }
    };
    reply(tx, id, out);
}

/// False once the controller who asked has left or its connection ended while a question was on screen.
/// Nobody is waiting for the result; carrying on would restart the computer for no one.
pub fn still_paired(app: &AppHandle, tx: &mpsc::UnboundedSender<String>) -> bool {
    !tx.is_closed() && app.state::<AgentState>().controller.lock().unwrap().is_some()
}

/// Run one tool call from a controller; the result goes back on `tx`
fn dispatch(app: &AppHandle, tx: &mpsc::UnboundedSender<String>, msg: &Value) {
    let s = |k: &str| msg.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let (id, tool) = (s("id"), s("tool"));
    let args = msg.get("args").cloned().unwrap_or(json!({}));
    let (app, tx) = (app.clone(), tx.clone());
    match tool.as_str() {
        "request_admin" => {
            tauri::async_runtime::spawn(admin::request_admin(app, tx, id));
        }
        "restart_computer" => {
            tauri::async_runtime::spawn(admin::restart_computer(app, tx, id, args));
        }
        _ => {
            tauri::async_runtime::spawn(async move {
                let st = app.state::<AgentState>();
                let ov = app.state::<overlay::Overlay>();
                let mouse = moves_mouse(&tool);
                if mouse {
                    ov.suppress(Duration::from_secs(5));
                }
                let before = tools::fx_before(&st.ctx, &tool, &args);
                let _ = app.emit("fx", json!({ "phase": "start", "id": id, "tool": tool, "args": fx_args(&args), "geo": before }));
                let out = fx::CALL_ID.scope(id.clone(), tools::run(&st.ctx, &tool, &args)).await;
                if mouse {
                    ov.suppress(Duration::from_millis(1200));
                }
                let after = tools::fx_after(&st.ctx, &tool, &before, &out);
                let shown: String = out.body().chars().take(6000).collect();
                let _ = app.emit("fx", json!({ "phase": "end", "id": id, "tool": tool, "ok": out.ok, "text": shown, "geo": after }));
                reply(&tx, &id, out);
            });
        }
    }
}

// ---- Relay ----

pub async fn relay_loop(app: AppHandle) {
    loop {
        set_relay(&app, RelayState::Connecting);
        if let Err(e) = relay_once(&app).await {
            set_relay(&app, RelayState::Offline(e));
        }
        detach(&app, Via::Relay).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

fn set_relay(app: &AppHandle, s: RelayState) {
    *app.state::<AgentState>().relay.lock().unwrap() = s;
    publish(app);
}

async fn relay_once(app: &AppHandle) -> Result<(), String> {
    let st = app.state::<AgentState>();
    let (ws, _) = tokio_tungstenite::connect_async_with_config(relay_url(), Some(ws_config()), true).await.map_err(|e| e.to_string())?;
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
    set_relay(app, RelayState::Ready);

    let result = loop {
        tokio::select! {
            _ = st.kick_relay.notified() => break Ok(()),
            msg = stream.next() => match msg {
                None => break Err("Lost the connection to the server".into()),
                Some(Err(e)) => break Err(e.to_string()),
                Some(Ok(WsMsg::Text(t))) => {
                    if let Ok(v) = serde_json::from_str::<Value>(t.as_str())
                        && let Err(e) = relay_message(app, &tx, v).await {
                            break Err(e);
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

async fn relay_message(app: &AppHandle, tx: &mpsc::UnboundedSender<String>, msg: Value) -> Result<(), String> {
    let st = app.state::<AgentState>();
    let s = |k: &str| msg.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match msg.get("type").and_then(Value::as_str) {
        Some("consent_request") => {
            // Having the code is enough to connect: the person handed it out on purpose, and the session
            // is shown on their screen the whole time. Only administrator access and restarts still ask.
            // Refused while someone else is driving (an AI on this machine).
            let allow = attach(app, &s("techName"), Via::Relay).is_ok();
            let _ = tx.send(json!({ "type": "consent_reply", "allow": allow }).to_string());
        }
        // Only the controller that is attached through the relay may call
        Some("call") if st.controller.lock().unwrap().as_ref().is_some_and(|c| c.via == Via::Relay) => dispatch(app, tx, &msg),
        Some("peer_left") => detach(app, Via::Relay).await,
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

// ---- Local ----

/// Where the address file goes. ASCTRL_LOCAL_FILE moves it, so a second driver (a test run) never
/// overwrites the one the user's own driver wrote.
fn local_file() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("ASCTRL_LOCAL_FILE").filter(|p| !p.is_empty()) {
        return Some(p.into());
    }
    Some(std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Asctrl").join("local.json"))
}

/// Remove the address file on exit, unless a newer instance (the elevated one) has already written its own
pub fn forget_local() {
    let Some(path) = local_file() else { return };
    let ours = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()).and_then(|v| v.get("pid").and_then(Value::as_u64)) == Some(std::process::id() as u64);
    if ours {
        let _ = std::fs::remove_file(path);
    }
}

/// Listen on the loopback interface for a controller on this machine
pub async fn local_server(app: AppHandle) {
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(l) => l,
        Err(e) => return eprintln!("local link unavailable: {e}"),
    };
    let Ok(port) = listener.local_addr().map(|a| a.port()) else { return };
    let token: String = (0..32).map(|_| format!("{:x}", rand::random_range(0..16u8))).collect();
    if let Some(path) = local_file() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let info = json!({
            "port": port,
            "token": token,
            "pid": std::process::id(),
            "exe": std::env::current_exe().ok().map(|p| p.to_string_lossy().into_owned()),
            "version": env!("CARGO_PKG_VERSION"),
            "admin": crate::is_elevated(),
        });
        if let Err(e) = std::fs::write(&path, info.to_string()) {
            eprintln!("could not write {}: {e}", path.display());
        }
    }
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };
        tokio::spawn(local_session(app.clone(), stream, token.clone()));
    }
}

async fn local_session(app: AppHandle, stream: tokio::net::TcpStream, token: String) {
    use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
    let _ = stream.set_nodelay(true);
    // A web page can open a WebSocket to localhost, and a browser always says where from: refuse those
    let no_browsers = |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        if req.headers().contains_key("origin") {
            let mut r = ErrorResponse::new(Some("not for web pages".into()));
            *r.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::FORBIDDEN;
            return Err(r);
        }
        Ok(resp)
    };
    let Ok(ws) = tokio_tungstenite::accept_hdr_async_with_config(stream, no_browsers, Some(ws_config())).await else { return };
    let (mut sink, mut stream) = ws.split();

    // The first message must be the hello with the token, promptly
    let hello = match tokio::time::timeout(Duration::from_secs(5), stream.next()).await {
        Ok(Some(Ok(WsMsg::Text(t)))) => serde_json::from_str::<Value>(t.as_str()).unwrap_or(Value::Null),
        _ => return,
    };
    let given = hello.get("token").and_then(Value::as_str).unwrap_or("");
    let refuse = if hello.get("type").and_then(Value::as_str) != Some("local_hello") || given.len() != token.len() || !given.bytes().zip(token.bytes()).fold(true, |ok, (a, b)| ok & (a == b)) {
        Some("Wrong token".to_string())
    } else {
        let name = hello.get("name").and_then(Value::as_str).unwrap_or("").trim().chars().take(64).collect::<String>();
        attach(&app, if name.is_empty() { "Local AI" } else { &name }, Via::Local).err()
    };
    if let Some(message) = refuse {
        let _ = sink.send(WsMsg::text(json!({ "type": "error", "message": message }).to_string())).await;
        let _ = sink.close().await;
        return;
    }
    let hostname = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "this computer".into());
    let paired = json!({ "type": "paired", "hostname": hostname, "local": true, "admin": crate::is_elevated() }).to_string();

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let (stop, mut stopped) = oneshot::channel::<()>();
    let _ = tx.send(paired);
    let writer = tokio::spawn(async move {
        loop {
            let sent = tokio::select! {
                biased;
                msg = rx.recv() => match msg {
                    Some(s) => sink.send(WsMsg::text(s)).await,
                    None => break,
                },
                _ = &mut stopped => break,
            };
            if sent.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let st = app.state::<AgentState>();
    loop {
        tokio::select! {
            _ = st.kick_local.notified() => break,
            msg = stream.next() => match msg {
                Some(Ok(WsMsg::Text(t))) => {
                    if let Ok(v) = serde_json::from_str::<Value>(t.as_str())
                        && v.get("type").and_then(Value::as_str) == Some("call") {
                            dispatch(&app, &tx, &v);
                        }
                }
                Some(Ok(WsMsg::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            }
        }
    }
    let _ = stop.send(());
    let _ = writer.await;
    detach(&app, Via::Local).await;
}
