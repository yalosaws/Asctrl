//! Asctrl driver: the program on the computer being used. It gives an AI eyes and hands there (see `tools`),
//! reachable two ways (see `link`): directly on this machine, or from anywhere through a relay with a
//! 6-digit code. Whatever the AI does is shown on screen by the overlay.
//! The wire protocol matches ../../src/shared/protocol.ts.

mod events;
mod admin;
mod capture;
mod browser_ext;
mod cdp;
mod files;
mod fx;
mod input;
mod jobs;
mod link;
mod ocr;
mod overlay;
mod presence;
mod safety;
mod screen;
mod shell;
mod tools;
mod uia;
mod winmgr;
mod wgc;

use serde_json::{Value, json};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{Notify, oneshot};

/// How long to wait for the user to click "Allow"
const CONSENT_TIMEOUT: Duration = Duration::from_secs(120);

/// How a controller reached us
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Via {
    /// Through the relay, with the code
    Relay,
    /// Straight from this machine
    Local,
}

struct Controller {
    name: String,
    via: Via,
}

#[derive(Default)]
enum RelayState {
    #[default]
    Connecting,
    Ready,
    Offline(String),
}

#[derive(Default)]
struct AgentState {
    code: Mutex<String>,
    /// The code was given on the command line (e.g. after an elevated restart); on a collision, retry instead of picking a new one
    fixed_code: bool,
    /// Who is driving. One at a time: there is one mouse and one keyboard.
    controller: Mutex<Option<Controller>>,
    relay: Mutex<RelayState>,
    /// The question on screen (administrator access, restart), as the status the UI shows for it
    question: Mutex<Option<Value>>,
    /// Where the answer to it goes
    answer: Mutex<Option<oneshot::Sender<bool>>>,
    /// The user clicked "End session": each link drops its controller
    kick_relay: Notify,
    kick_local: Notify,
    /// Latest status, replayed to the UI when it first loads
    status: Mutex<Value>,
    ctx: std::sync::Arc<tools::Ctx>,
}

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn random_code() -> String {
    format!("{:06}", rand::random_range(0..1_000_000))
}

fn is_elevated() -> bool {
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin().as_bool() }
}

/// What the UI should show right now: a question if one is up, else who is connected, else how the relay is doing
fn current_status(st: &AgentState) -> Value {
    if let Some(q) = st.question.lock().unwrap().clone() {
        return q;
    }
    let code = st.code.lock().unwrap().clone();
    if let Some(c) = &*st.controller.lock().unwrap() {
        return json!({ "state": "connected", "techName": c.name, "code": code });
    }
    match &*st.relay.lock().unwrap() {
        RelayState::Ready => json!({ "state": "ready", "code": code }),
        RelayState::Connecting => json!({ "state": "connecting" }),
        RelayState::Offline(m) => json!({ "state": "offline", "message": m }),
    }
}

/// Work out the status again and tell the UI
fn publish(app: &AppHandle) {
    let st = app.state::<AgentState>();
    let status = current_status(&st);
    *st.status.lock().unwrap() = status.clone();
    let _ = app.emit("status", status);
}

/// The overlay is always on top; this just makes sure it is visible
fn bring_to_front(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        overlay::show(&w);
        let _ = w.set_always_on_top(true);
    }
}

/// A controller asks to take over. Refused while another one is driving.
fn attach(app: &AppHandle, name: &str, via: Via) -> Result<(), String> {
    let st = app.state::<AgentState>();
    {
        let mut c = st.controller.lock().unwrap();
        if let Some(cur) = &*c {
            return Err(format!("{} is already controlling this computer", if cur.name.is_empty() { "Someone" } else { &cur.name }));
        }
        *c = Some(Controller { name: name.to_string(), via });
    }
    publish(app);
    bring_to_front(app);
    Ok(())
}

/// The controller that came in over `via` has left (or its link dropped): end what it started
async fn detach(app: &AppHandle, via: Via) {
    let st = app.state::<AgentState>();
    {
        let mut c = st.controller.lock().unwrap();
        if c.as_ref().is_none_or(|c| c.via != via) {
            return;
        }
        *c = None;
    }
    // A question from the controller that just left can no longer be answered
    st.question.lock().unwrap().take();
    st.answer.lock().unwrap().take();
    publish(app);
    st.ctx.reset().await;
}

/// The app, for what is asked of the person from inside the tools
static APP: std::sync::OnceLock<AppHandle> = std::sync::OnceLock::new();

/// Who is driving, for the action log
pub fn controller_name() -> String {
    APP.get().and_then(|a| a.state::<AgentState>().controller.lock().unwrap().as_ref().map(|c| c.name.clone())).unwrap_or_default()
}

/// Ask the person at the computer whether the AI may do `what` (`level`: irreversible or external), on the overlay
pub async fn confirm_act(what: &str, level: &str) -> bool {
    let Some(app) = APP.get() else { return false };
    ask_user(app, json!({ "state": "act", "techName": controller_name(), "timeoutSec": CONSENT_TIMEOUT.as_secs(), "what": what, "level": level })).await
}

/// Ask the user a question; a timeout counts as a refusal
async fn ask_user(app: &AppHandle, status: Value) -> bool {
    let st = app.state::<AgentState>();
    let (otx, orx) = oneshot::channel::<bool>();
    *st.answer.lock().unwrap() = Some(otx);
    *st.question.lock().unwrap() = Some(status);
    publish(app);
    bring_to_front(app);
    let allow = matches!(tokio::time::timeout(CONSENT_TIMEOUT, orx).await, Ok(Ok(true)));
    st.question.lock().unwrap().take();
    publish(app);
    allow
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
    match st.controller.lock().unwrap().as_ref().map(|c| c.via) {
        Some(Via::Relay) => st.kick_relay.notify_one(),
        Some(Via::Local) => st.kick_local.notify_one(),
        None => {}
    }
}

#[tauri::command]
fn quit(app: AppHandle) {
    app.exit(0);
}

pub fn run() {
    // Started by a browser as the extension's native host: only relay, no driver
    if browser_ext::relay_if_asked() {
        return;
    }
    let fixed = arg("--code")
        .filter(|c| c.len() == 6 && c.bytes().all(|b| b.is_ascii_digit()))
        .or_else(|| if cfg!(debug_assertions) { std::env::var("ASCTRL_CODE").ok() } else { None });
    let state = AgentState { fixed_code: fixed.is_some(), ctx: tools::Ctx::shared(), ..Default::default() };
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
            let _ = APP.set(app.handle().clone());
            // The timeline of the desktop's windows, before anything waits on it
            events::start();
            // Whose input is whose: the person at the computer comes first
            presence::start();
            overlay::setup(app.handle());
            fx::init(app.handle().clone());
            tauri::async_runtime::spawn(link::relay_loop(app.handle().clone()));
            tauri::async_runtime::spawn(link::local_server(app.handle().clone()));
            // The person's own browsers, through Asctrl's extension
            tauri::async_runtime::spawn(browser_ext::serve());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to start")
        .run(|_, event| {
            if let tauri::RunEvent::Exit = event {
                link::forget_local();
            }
        });
}
