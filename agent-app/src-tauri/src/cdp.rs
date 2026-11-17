//! Asctrl's own browser, driven through the Chrome DevTools Protocol (CDP): web pages as pages, not pixels.
//!
//! A browser only takes CDP when it is started for it (`--remote-debugging-port`), and Chrome (since 136) and
//! Edge refuse that for the user's everyday profile. So this is a browser of Asctrl's own: Chrome, else Edge,
//! with its own profile under %LOCALAPPDATA%\Asctrl\browser, kept between runs (sign in there once and it stays
//! signed in). It is started by `open_app` with `browser: "asctrl"`; one left running from an earlier run of
//! the driver is picked up again.
//!
//! The debugging port is chosen by the browser (port 0, read back from its DevToolsActivePort file) and only
//! listens on 127.0.0.1. `ASCTRL_BROWSER` names another Chromium-based browser to use.
//!
//! One WebSocket to the browser; each tab is reached through a session on it (flat mode). Events are kept per
//! session for a moment, so an action can tell whether the page started loading another one.
//!
//! The person's own browser (Chrome or Edge with Asctrl's extension, see `browser_ext`) is reached the same way:
//! the extension answers the same calls over native messaging, so a `Conn` is either.
//!
//! So are Electron programs (VS Code, Slack, Teams, ...) that `open_app` started with `electron: true`: they are
//! Chromium inside, started with a debugging port of their own (`attach_app`), and their windows are pages.

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, mpsc, oneshot};

/// Longest a call waits for the browser's answer
const CALL: Duration = Duration::from_secs(10);
/// Longest the browser is given to come up
const START: Duration = Duration::from_secs(15);
/// Events kept per connection
const KEEP: usize = 512;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// A frame of a tab that runs in a process of its own (an iframe from another site): reached through a session
/// of its own, attached as the tab's session is (auto-attach, flat mode)
#[derive(Clone, Debug)]
pub struct Frame {
    pub session: String,
    /// Its frame id (the target id of an iframe target), to find its element in the page above
    pub id: String,
}

type Frames = Arc<Mutex<HashMap<String, Vec<Frame>>>>;

/// Something the browser said on its own
#[derive(Clone, Debug)]
pub struct Event {
    pub at: Instant,
    pub session: String,
    pub method: String,
    pub params: Value,
}

pub struct Conn {
    tx: mpsc::UnboundedSender<String>,
    pending: Pending,
    next: AtomicU64,
    alive: Arc<AtomicBool>,
    events: Arc<Mutex<VecDeque<Event>>>,
    arrived: Arc<Notify>,
    /// The browser's own process: its windows are the ones this connection drives
    pub pid: u32,
    /// Which browser (for replies)
    pub name: String,
    /// The person's own browser (through the extension), started without Asctrl's flags: a window others cover
    /// stops drawing, and input to it waits for a frame
    pub theirs: bool,
    /// Tab (target id) -> session id, with Page events on
    sessions: Mutex<HashMap<String, String>>,
    /// A tab's session -> the frames of other sites in it
    frames: Frames,
}

/// A tab
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Tab {
    pub id: String,
    pub session: String,
    pub title: String,
    pub url: String,
}

static CONN: Mutex<Option<Arc<Conn>>> = Mutex::new(None);
/// One start or reconnect at a time
static STARTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub fn profile_dir() -> std::path::PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| std::env::var("USERPROFILE").unwrap_or_default());
    std::path::Path::new(&base).join("Asctrl").join("browser")
}

fn port_file() -> std::path::PathBuf {
    profile_dir().join("DevToolsActivePort")
}

fn pid_file() -> std::path::PathBuf {
    profile_dir().join("asctrl-browser.pid")
}

/// The browser to start: ASCTRL_BROWSER, else Chrome, else Edge
fn browser_exe() -> Option<(std::path::PathBuf, String)> {
    if let Ok(p) = std::env::var("ASCTRL_BROWSER")
        && std::path::Path::new(&p).is_file()
    {
        return Some((p.into(), "the browser in ASCTRL_BROWSER".into()));
    }
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let tries = [
        (format!("{}\\Google\\Chrome\\Application\\chrome.exe", env("ProgramFiles")), "Chrome"),
        (format!("{}\\Google\\Chrome\\Application\\chrome.exe", env("ProgramFiles(x86)")), "Chrome"),
        (format!("{}\\Google\\Chrome\\Application\\chrome.exe", env("LOCALAPPDATA")), "Chrome"),
        (format!("{}\\Microsoft\\Edge\\Application\\msedge.exe", env("ProgramFiles(x86)")), "Edge"),
        (format!("{}\\Microsoft\\Edge\\Application\\msedge.exe", env("ProgramFiles")), "Edge"),
    ];
    tries.into_iter().find(|(p, _)| std::path::Path::new(p).is_file()).map(|(p, n)| (p.into(), n.to_string()))
}

impl Conn {
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Call a method, on a tab's session or on the browser itself
    pub async fn call(&self, session: Option<&str>, method: &str, params: Value) -> Result<Value, String> {
        if !self.alive() {
            return Err("the browser has closed".into());
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if self.tx.send(msg.to_string()).is_err() {
            self.pending.lock().unwrap().remove(&id);
            return Err("the browser has closed".into());
        }
        match tokio::time::timeout(CALL, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("the browser has closed".into()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!("the browser did not answer {method} in time"))
            }
        }
    }

    /// The page tabs, in the browser's order
    pub async fn pages(&self) -> Result<Vec<(String, String, String)>, String> {
        let r = self.call(None, "Target.getTargets", json!({})).await?;
        Ok(r["targetInfos"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|t| t["type"] == "page")
                    .map(|t| (t["targetId"].as_str().unwrap_or("").to_string(), t["title"].as_str().unwrap_or("").to_string(), t["url"].as_str().unwrap_or("").to_string()))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// A session on tab `id`, with its page events on
    pub async fn session(&self, id: &str) -> Result<String, String> {
        if let Some(s) = self.sessions.lock().unwrap().get(id) {
            return Ok(s.clone());
        }
        let r = self.call(None, "Target.attachToTarget", json!({ "targetId": id, "flatten": true })).await?;
        let s = r["sessionId"].as_str().ok_or("the browser gave no session")?.to_string();
        self.call(Some(&s), "Page.enable", json!({})).await?;
        // Iframes from other sites run in processes of their own: have them attached as they come
        let _ = self.call(Some(&s), "Target.setAutoAttach", json!({ "autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true })).await;
        self.sessions.lock().unwrap().insert(id.to_string(), s.clone());
        Ok(s)
    }

    pub async fn tab(&self, id: &str, title: &str, url: &str) -> Result<Tab, String> {
        Ok(Tab { id: id.into(), session: self.session(id).await?, title: title.into(), url: url.into() })
    }

    /// The frames of other sites in the tab whose session is `session`
    pub fn frames_of(&self, session: &str) -> Vec<Frame> {
        self.frames.lock().unwrap().get(session).cloned().unwrap_or_default()
    }

    /// A frame that no longer answers (its page went, or the browser let go of the tab)
    pub fn forget_frame(&self, session: &str) {
        for v in self.frames.lock().unwrap().values_mut() {
            v.retain(|f| f.session != session);
        }
    }

    /// Events on `session` since `t`
    pub fn events_since(&self, session: &str, t: Instant) -> Vec<Event> {
        self.events.lock().unwrap().iter().filter(|e| e.at >= t && e.session == session).cloned().collect()
    }

    /// Wait until an event arrives, or `max`
    pub async fn wait_event(&self, max: Duration) {
        let _ = tokio::time::timeout(max, self.arrived.notified()).await;
    }
}

/// Connect to a browser listening at `port` (`path` from its DevToolsActivePort file)
async fn connect(port: u16, path: &str, pid: u32, name: String) -> Result<Arc<Conn>, String> {
    connect_url(&format!("ws://127.0.0.1:{port}{path}"), pid, name).await
}

/// Electron programs started with a debugging port (open_app electron: true)
static APPS: Mutex<Vec<Arc<Conn>>> = Mutex::new(Vec::new());

/// A port nobody listens on now
pub fn free_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok().map(|a| a.port())
}

/// What `GET path` on 127.0.0.1:`port` answers (the body), if it answers
async fn http_get(port: u16, path: &str) -> Option<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::time::timeout(Duration::from_millis(500), tokio::net::TcpStream::connect(("127.0.0.1", port))).await.ok()?.ok()?;
    // The host with its port: Chromium writes the WebSocket address it answers with from this header, and
    // without the port that address pointed at port 80
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").as_bytes()).await.ok()?;
    // Read until the body is all there (by its Content-Length): the server need not close the connection
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut buf = vec![];
    let mut chunk = [0u8; 4096];
    loop {
        let text = String::from_utf8_lossy(&buf).to_string();
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let len = head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok()));
            if len.is_some_and(|n| body.len() >= n) {
                return Some(body.to_string());
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, s.read(&mut chunk)).await {
            Ok(Ok(0)) => return String::from_utf8_lossy(&buf).split_once("\r\n\r\n").map(|(_, b)| b.to_string()),
            Ok(Ok(n)) => buf.extend_from_slice(&chunk[..n]),
            _ => return None,
        }
    }
}

/// An Electron program started with `--remote-debugging-port=port`: wait up to `wait` for the port, connect,
/// and keep it. Its windows are its main process's: that pid is asked of the program itself.
pub async fn attach_app(port: u16, wait: Duration, pid: u32) -> Result<Arc<Conn>, String> {
    let start = Instant::now();
    let mut last = "it opened no debugging port (was it running already? It must be started fresh: close it first)".to_string();
    // A program may answer on the port and then hand over to another process of its own (VS Code does as it
    // starts): keep trying until a connection holds
    while start.elapsed() < wait {
        if let Some(b) = http_get(port, "/json/version").await
            && let Ok(version) = serde_json::from_str::<Value>(&b)
            && let Some(url) = version["webSocketDebuggerUrl"].as_str()
        {
            // The program's own name in its user agent (Code/1.142.0), not Chrome's or Electron's
            let ua = version["User-Agent"].as_str().unwrap_or("");
            let name = ua
                .split_whitespace()
                .filter_map(|t| t.split_once('/').map(|(n, _)| n))
                .find(|n| !matches!(*n, "Mozilla" | "AppleWebKit" | "Chrome" | "Electron" | "Safari" | "Gecko)"))
                .unwrap_or("the program")
                .to_string();
            // An address without a port (a program that writes it from a Host header without one) is on this port
            let url = match url.strip_prefix("ws://127.0.0.1/").or_else(|| url.strip_prefix("ws://localhost/")) {
                Some(rest) => format!("ws://127.0.0.1:{port}/{rest}"),
                None => url.to_string(),
            };
            match connect_url(&url, pid, name).await {
                Ok(c) if c.call(None, "Target.getTargets", json!({})).await.is_ok() => {
                    let mut all = APPS.lock().unwrap();
                    all.retain(|x| x.alive());
                    all.push(c.clone());
                    return Ok(c);
                }
                Ok(_) => last = "its debugging connection closed at once".into(),
                Err(e) => last = e,
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    Err(last)
}

/// Connect to the DevTools WebSocket at `url`
async fn connect_url(url: &str, pid: u32, name: String) -> Result<Arc<Conn>, String> {
    let (ws, _) = tokio::time::timeout(Duration::from_secs(5), tokio_tungstenite::connect_async(url))
        .await
        .map_err(|_| "the browser did not take the connection".to_string())?
        .map_err(|e| format!("could not connect to the browser: {e}"))?;
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let (in_tx, in_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if sink.send(tokio_tungstenite::tungstenite::Message::text(m)).await.is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            if let Ok(text) = msg.to_text()
                && in_tx.send(text.to_string()).is_err()
            {
                break;
            }
        }
    });
    let c = wire(tx, in_rx, pid, name, false);
    // Tabs opened from a page (target=_blank, window.open) are new targets: say so as they come
    let _ = c.call(None, "Target.setDiscoverTargets", json!({ "discover": true })).await;
    Ok(c)
}

/// The protocol over a line transport: `tx` takes the messages to send, `rx` gives those that arrive; the
/// connection is closed when `rx` ends.
pub fn wire(tx: mpsc::UnboundedSender<String>, mut rx: mpsc::UnboundedReceiver<String>, pid: u32, name: String, theirs: bool) -> Arc<Conn> {
    let pending: Pending = Arc::default();
    let alive = Arc::new(AtomicBool::new(true));
    let events: Arc<Mutex<VecDeque<Event>>> = Arc::default();
    let arrived = Arc::new(Notify::new());
    let frames: Frames = Arc::default();
    {
        let (pending, alive, events, arrived, frames) = (pending.clone(), alive.clone(), events.clone(), arrived.clone(), frames.clone());
        tokio::spawn(async move {
            while let Some(text) = rx.recv().await {
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                if let Some(id) = v["id"].as_u64() {
                    if let Some(tx) = pending.lock().unwrap().remove(&id) {
                        let r = match v.get("error") {
                            Some(e) => Err(e["message"].as_str().unwrap_or("the browser refused").to_string()),
                            None => Ok(v["result"].clone()),
                        };
                        let _ = tx.send(r);
                    }
                } else if let Some(method) = v["method"].as_str() {
                    let parent = v["sessionId"].as_str().unwrap_or("");
                    match method {
                        "Target.attachedToTarget" if v["params"]["targetInfo"]["type"] == "iframe" => {
                            let f = Frame {
                                session: v["params"]["sessionId"].as_str().unwrap_or("").to_string(),
                                id: v["params"]["targetInfo"]["targetId"].as_str().unwrap_or("").to_string(),
                            };
                            let mut all = frames.lock().unwrap();
                            let list = all.entry(parent.to_string()).or_default();
                            list.retain(|x| x.id != f.id);
                            list.push(f);
                        }
                        "Target.detachedFromTarget" => {
                            let gone = v["params"]["sessionId"].as_str().unwrap_or("");
                            for l in frames.lock().unwrap().values_mut() {
                                l.retain(|x| x.session != gone);
                            }
                        }
                        _ => {}
                    }
                    let mut evs = events.lock().unwrap();
                    if evs.len() == KEEP {
                        evs.pop_front();
                    }
                    evs.push_back(Event { at: Instant::now(), session: v["sessionId"].as_str().unwrap_or("").to_string(), method: method.to_string(), params: v["params"].clone() });
                    drop(evs);
                    arrived.notify_waiters();
                }
            }
            alive.store(false, Ordering::SeqCst);
            for (_, tx) in pending.lock().unwrap().drain() {
                let _ = tx.send(Err("the browser has closed".into()));
            }
            arrived.notify_waiters();
        });
    }
    Arc::new(Conn { tx, pending, next: AtomicU64::new(1), alive, events, arrived, pid, name, sessions: Mutex::default(), frames, theirs })
}

/// The port and path a running browser left in its DevToolsActivePort file
fn read_port() -> Option<(u16, String)> {
    let s = std::fs::read_to_string(port_file()).ok()?;
    let mut lines = s.lines();
    let port = lines.next()?.trim().parse().ok()?;
    Some((port, lines.next()?.trim().to_string()))
}

/// The connection, if Asctrl's browser is running: the one in hand, or one left running from before
pub async fn current() -> Option<Arc<Conn>> {
    if let Some(c) = CONN.lock().unwrap().as_ref().filter(|c| c.alive()) {
        return Some(c.clone());
    }
    // Cheap when it isn't running: no file, nothing to try
    if !port_file().is_file() {
        return None;
    }
    let _one = STARTING.lock().await;
    if let Some(c) = CONN.lock().unwrap().as_ref().filter(|c| c.alive()) {
        return Some(c.clone());
    }
    let (port, path) = read_port()?;
    let pid: u32 = std::fs::read_to_string(pid_file()).ok()?.trim().parse().ok()?;
    if !crate::winmgr::process_alive(pid) {
        let _ = std::fs::remove_file(port_file());
        return None;
    }
    let name = browser_exe().map(|(_, n)| n).unwrap_or_else(|| "the browser".into());
    match connect(port, &path, pid, name).await {
        Ok(c) => {
            *CONN.lock().unwrap() = Some(c.clone());
            Some(c)
        }
        Err(_) => None,
    }
}

/// Asctrl's browser, started if it isn't running. `url` is opened in it: in a new tab when it was already
/// running, as its first page when it is started now. Returns the connection and the tab's id.
pub async fn open(url: &str) -> Result<(Arc<Conn>, String), String> {
    if let Some(c) = current().await {
        let r = c.call(None, "Target.createTarget", json!({ "url": url })).await?;
        let id = r["targetId"].as_str().ok_or("the browser opened no tab")?.to_string();
        let _ = c.call(None, "Target.activateTarget", json!({ "targetId": id })).await;
        return Ok((c, id));
    }
    let _one = STARTING.lock().await;
    let (exe, name) = browser_exe().ok_or("Neither Chrome nor Edge is installed (or set ASCTRL_BROWSER to a Chromium-based browser)")?;
    let dir = profile_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not make the browser's profile folder {}: {e}", dir.display()))?;
    let _ = std::fs::remove_file(port_file());
    let child = std::process::Command::new(&exe)
        .arg(format!("--user-data-dir={}", dir.display()))
        .arg("--remote-debugging-port=0")
        .arg("--remote-debugging-address=127.0.0.1")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--new-window")
        // A window that others cover keeps drawing and running at full pace: Chrome otherwise stops drawing it,
        // and input sent to it waits for a frame that doesn't come (seconds), and its timers slow down
        .arg("--disable-features=CalculateNativeWinOcclusion")
        .arg("--disable-backgrounding-occluded-windows")
        .arg("--disable-renderer-backgrounding")
        .arg("--disable-background-timer-throttling")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start {name}: {e}"))?;
    let pid = child.id();
    let _ = std::fs::write(pid_file(), pid.to_string());
    let start = Instant::now();
    let (port, path) = loop {
        if let Some(p) = read_port() {
            break p;
        }
        if start.elapsed() > START {
            return Err(format!("{name} started but did not open its debugging port (another copy may be using the profile {})", dir.display()));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let c = connect(port, &path, pid, name).await?;
    *CONN.lock().unwrap() = Some(c.clone());
    // Its first tab is the page asked for
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let pages = c.pages().await?;
        if let Some((id, _, _)) = pages.iter().find(|(_, _, u)| !u.is_empty() && u != "about:blank").or(pages.first()) {
            return Ok((c, id.clone()));
        }
        if Instant::now() > deadline {
            return Err("the browser opened no page".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The tab showing in window `hwnd`, when the window is Asctrl's browser. The tab whose title the window's
/// title starts with (the browser names its window after the tab in front), the longest such; with only one
/// tab, that one.
pub async fn tab_in(hwnd: isize) -> Option<(Arc<Conn>, Tab)> {
    if hwnd == 0 {
        return None;
    }
    let pid = crate::winmgr::pid_of(hwnd);
    let c = match current().await.filter(|c| c.pid == pid) {
        Some(c) => c,
        None => match crate::browser_ext::conn_for(pid) {
            Some(c) => c,
            None => APPS.lock().unwrap().iter().find(|c| c.alive() && c.pid == pid).cloned()?,
        },
    };
    let title = crate::winmgr::title_of(hwnd);
    let pages = c.pages().await.ok()?;
    let pick = pages
        .iter()
        .filter(|(_, t, _)| !t.is_empty() && title.starts_with(t.as_str()))
        .max_by_key(|(_, t, _)| t.len())
        .or_else(|| (pages.len() == 1).then(|| &pages[0]))?
        .clone();
    let tab = c.tab(&pick.0, &pick.1, &pick.2).await.ok()?;
    Some((c, tab))
}

/// The browser window showing tab `id` (by its title), else any window of the browser
pub async fn window_of(c: &Conn, id: &str) -> Option<isize> {
    let pages = c.pages().await.ok()?;
    let title = pages.iter().find(|(i, _, _)| i == id).map(|(_, t, _)| t.clone()).unwrap_or_default();
    let wins: Vec<_> = crate::winmgr::list().into_iter().filter(|w| crate::winmgr::pid_of(w.handle) == c.pid).collect();
    wins.iter().find(|w| !title.is_empty() && w.title.starts_with(&title)).or(wins.first()).map(|w| w.handle)
}

/// Where frame `id`'s viewport starts in its tab's viewport (CSS pixels): the content box of its iframe element
pub async fn frame_origin(c: &Conn, tab: &Tab, id: &str) -> Option<(f64, f64)> {
    let owner = c.call(Some(&tab.session), "DOM.getFrameOwner", json!({ "frameId": id })).await.ok()?;
    let node = owner["backendNodeId"].as_u64()?;
    let b = c.call(Some(&tab.session), "DOM.getBoxModel", json!({ "backendNodeId": node })).await.ok()?;
    let q = b["model"]["content"].as_array()?;
    Some((q.first()?.as_f64()?, q.get(1)?.as_f64()?))
}

/// Open `url` in a new tab of the person's own browser (through Asctrl's extension), in front
pub async fn open_in_theirs(url: &str) -> Result<(Arc<Conn>, String), String> {
    let c = crate::browser_ext::any().ok_or(crate::browser_ext::MISSING)?;
    let r = c.call(None, "Target.createTarget", json!({ "url": url })).await?;
    let id = r["targetId"].as_str().ok_or("the browser opened no tab")?.to_string();
    let _ = c.call(None, "Target.activateTarget", json!({ "targetId": id })).await;
    Ok((c, id))
}

/// Run `expr` (JavaScript) in the tab and give back its value; a promise is waited for
pub async fn eval(c: &Conn, tab: &Tab, expr: &str) -> Result<Value, String> {
    eval_in(c, &tab.session, expr).await
}

/// `eval` in a session: a tab's, or a frame's
pub async fn eval_in(c: &Conn, session: &str, expr: &str) -> Result<Value, String> {
    let r = c.call(Some(session), "Runtime.evaluate", json!({ "expression": expr, "returnByValue": true, "awaitPromise": true })).await?;
    if let Some(e) = r.get("exceptionDetails") {
        let msg = e["exception"]["description"].as_str().or(e["text"].as_str()).unwrap_or("the page's script failed");
        return Err(msg.lines().next().unwrap_or("").to_string());
    }
    Ok(r["result"]["value"].clone())
}
