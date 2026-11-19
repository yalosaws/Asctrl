//! The person's own browser, through Asctrl's extension (browser-extension/): web pages worked as pages in the
//! browser they use every day, with the sites they are signed in to, without the mouse.
//!
//! The way in is native messaging, not a network port: the browser starts this same program with the
//! extension's origin as its argument (`relay_if_asked`), and that relay passes the extension's messages on to
//! the running driver over a named pipe of the user's own (`serve`). The driver registers itself as the
//! extension's native host each time it starts (`register`), so the extension finds whichever copy runs.
//!
//! What the extension speaks is the browser's DevTools protocol, cut down to what `cdp` uses, so each browser
//! with the extension is a `cdp::Conn` like Asctrl's own browser.

use crate::cdp::{self, Conn};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
use tokio::sync::mpsc;

/// The native host's name, as the extension asks for it
pub const HOST: &str = "com.asctrl.browser";
/// The extension's id (fixed by the key in its manifest)
pub const EXTENSION_ID: &str = "kojacdaioifogkfhkclpfommlfooaaim";
/// What to tell the AI when no browser has the extension
pub const MISSING: &str = "Asctrl's extension is not connected in any browser. It is installed once, by the person: open chrome://extensions (or edge://extensions), turn on Developer mode, choose Load unpacked, and pick Asctrl's browser-extension folder. Asctrl's own browser (browser: \"asctrl\") needs no extension.";

/// The user's own pipe (`ASCTRL_BROWSER_PIPE` names another, for tests: the browser a test starts passes it on to
/// the relay)
fn pipe_name() -> String {
    match std::env::var("ASCTRL_BROWSER_PIPE") {
        Ok(p) if !p.is_empty() => format!(r"\\.\pipe\{p}"),
        _ => format!(r"\\.\pipe\asctrl-browser-{}", std::env::var("USERNAME").unwrap_or_default()),
    }
}

/// Browsers connected through the extension
static CONNS: Mutex<Vec<Arc<Conn>>> = Mutex::new(Vec::new());

/// The connection to the browser whose process is `pid`
pub fn conn_for(pid: u32) -> Option<Arc<Conn>> {
    CONNS.lock().unwrap().iter().rev().find(|c| c.alive() && c.pid == pid).cloned()
}

/// A connected browser, the latest to connect
pub fn any() -> Option<Arc<Conn>> {
    CONNS.lock().unwrap().iter().rev().find(|c| c.alive()).cloned()
}

// ---------- In the driver

/// Who may open the pipe, as a security descriptor kept for the driver's life: this user and the system, with a
/// medium integrity label. Without it a driver running as administrator made a pipe only administrators could
/// open, and the relay a browser starts (as the user, not elevated) was refused for good.
fn pipe_security() -> Option<usize> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW};
    use windows::Win32::Security::{GetTokenInformation, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::{HSTRING, PWSTR};
    static SD: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    *SD.get_or_init(|| unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut buf = vec![0u8; 256];
        let mut len = 0u32;
        let got = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr() as *mut _), buf.len() as u32, &mut len);
        let _ = CloseHandle(token);
        got.ok()?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut s).ok()?;
        let sid = s.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(s.0 as *mut _)));
        let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;{})S:(ML;;NW;;;ME)", sid?);
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(sddl), 1, &mut sd, None).ok()?;
        Some(sd.0 as usize)
    })
}

fn create_pipe(name: &str, first: bool) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    use windows::Win32::Security::SECURITY_ATTRIBUTES;
    let mut opts = ServerOptions::new();
    opts.first_pipe_instance(first);
    match pipe_security() {
        Some(sd) => {
            let mut sa = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd as *mut _, bInheritHandle: false.into() };
            // SAFETY: `sa` and the descriptor it points at outlive the call (the descriptor lives as long as the driver)
            unsafe { opts.create_with_security_attributes_raw(name, &mut sa as *mut _ as *mut _) }
        }
        None => opts.create(name),
    }
}

/// Take the relays' connections, for as long as the driver runs
pub async fn serve() {
    register();
    let name = pipe_name();
    let mut first = true;
    let started = std::time::Instant::now();
    loop {
        // The first instance only if no one else has the name (another copy of the driver keeps its own). A copy
        // on its way out (this one restarted as administrator, or was started again) is waited for a while.
        let server = match create_pipe(&name, first) {
            Ok(s) => s,
            Err(e) => {
                if first && started.elapsed() > Duration::from_secs(15) {
                    eprintln!("browser extension: pipe {name} not available: {e}");
                    return;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        first = false;
        if server.connect().await.is_err() {
            continue;
        }
        tokio::spawn(take(server));
    }
}

/// One browser, through its relay: the relay's first line says which browser process it is
async fn take(pipe: tokio::net::windows::named_pipe::NamedPipeServer) {
    let (r, mut w) = tokio::io::split(pipe);
    let mut lines = BufReader::new(r).lines();
    let Ok(Some(hello)) = lines.next_line().await else { return };
    let hello: serde_json::Value = serde_json::from_str(&hello).unwrap_or_default();
    let pid = hello["relay"]["pid"].as_u64().unwrap_or(0) as u32;
    let exe = hello["relay"]["exe"].as_str().unwrap_or("").to_lowercase();
    let name = match exe.as_str() {
        "chrome.exe" => "Chrome",
        "msedge.exe" => "Edge",
        "brave.exe" => "Brave",
        _ => "the browser",
    }
    .to_string();
    let (tx, mut out) = mpsc::unbounded_channel::<String>();
    let (in_tx, in_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(m) = out.recv().await {
            if w.write_all(format!("{m}\n").as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let c = cdp::wire(tx, in_rx, pid, name, true);
    {
        let mut all = CONNS.lock().unwrap();
        all.retain(|c| c.alive());
        all.push(c);
    }
    while let Ok(Some(l)) = lines.next_line().await {
        if in_tx.send(l).is_err() {
            break;
        }
    }
}

/// Register this program as the extension's native host, for Chrome, Edge and Brave (the current user only)
fn register() {
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(base) = std::env::var_os("LOCALAPPDATA") else { return };
    let dir = std::path::PathBuf::from(base).join("Asctrl");
    let _ = std::fs::create_dir_all(&dir);
    let manifest = dir.join("native-host.json");
    let body = serde_json::json!({
        "name": HOST,
        "description": "Asctrl: lets the driver on this computer work web pages through Asctrl's extension",
        "path": exe,
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{EXTENSION_ID}/")],
    });
    if std::fs::write(&manifest, serde_json::to_string_pretty(&body).unwrap_or_default()).is_err() {
        return;
    }
    for browser in [r"Google\Chrome", r"Microsoft\Edge", r"BraveSoftware\Brave-Browser"] {
        use std::os::windows::process::CommandExt;
        let key = format!(r"HKCU\Software\{browser}\NativeMessagingHosts\{HOST}");
        let _ = std::process::Command::new("reg.exe")
            .args(["add", &key, "/ve", "/t", "REG_SZ", "/d"])
            .arg(&manifest)
            .arg("/f")
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

// ---------- The relay the browser starts

/// When the browser started this program as the extension's native host: relay until the browser lets go, and
/// say so (the caller then exits instead of starting the driver)
pub fn relay_if_asked() -> bool {
    if !std::env::args().skip(1).any(|a| a.starts_with("chrome-extension://")) {
        return false;
    }
    if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
        rt.block_on(relay());
    }
    true
}

async fn relay() {
    let (pid, exe) = browser_process();
    let hello = serde_json::json!({ "relay": { "pid": pid, "exe": exe } }).to_string();
    // To the browser: each message its length (4 bytes, this machine's order), then the JSON
    let (to_browser, mut outbox) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let mut so = tokio::io::stdout();
        while let Some(m) = outbox.recv().await {
            let b = m.as_bytes();
            if so.write_all(&(b.len() as u32).to_ne_bytes()).await.is_err() || so.write_all(b).await.is_err() || so.flush().await.is_err() {
                std::process::exit(0);
            }
        }
    });
    // From the browser, the same way; it closing our input means the extension let go
    let (from_browser, mut inbox) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let mut si = tokio::io::stdin();
        loop {
            let mut n = [0u8; 4];
            if si.read_exact(&mut n).await.is_err() {
                std::process::exit(0);
            }
            let mut b = vec![0u8; u32::from_ne_bytes(n) as usize];
            if si.read_exact(&mut b).await.is_err() {
                std::process::exit(0);
            }
            // One line each to the driver: the JSON has no raw line breaks
            if from_browser.send(String::from_utf8_lossy(&b).replace('\n', " ")).is_err() {
                std::process::exit(0);
            }
        }
    });
    let name = pipe_name();
    loop {
        // The driver may not be running yet, or be restarting: wait for it, the extension stays connected
        let pipe = loop {
            match ClientOptions::new().open(&name) {
                Ok(p) => break p,
                Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
            }
        };
        let (r, mut w) = tokio::io::split(pipe);
        if w.write_all(format!("{hello}\n").as_bytes()).await.is_err() {
            continue;
        }
        let mut lines = BufReader::new(r).lines();
        loop {
            tokio::select! {
                l = lines.next_line() => match l {
                    Ok(Some(l)) => { let _ = to_browser.send(l); }
                    _ => break,
                },
                m = inbox.recv() => match m {
                    Some(m) => if w.write_all(format!("{m}\n").as_bytes()).await.is_err() { break },
                    None => return,
                },
            }
        }
    }
}

/// The browser that started this relay: the first ancestor that is not a command shell (Chrome starts native
/// hosts through cmd.exe)
fn browser_process() -> (u32, String) {
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
    let mut procs: Vec<(u32, u32, String)> = vec![];
    unsafe {
        if let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            let mut ok = Process32FirstW(snap, &mut e).is_ok();
            while ok {
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                procs.push((e.th32ProcessID, e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..len])));
                ok = Process32NextW(snap, &mut e).is_ok();
            }
            let _ = windows::Win32::Foundation::CloseHandle(snap);
        }
    }
    let mut pid = std::process::id();
    for _ in 0..6 {
        let Some(&(_, parent, _)) = procs.iter().find(|p| p.0 == pid) else { break };
        let Some((p, _, exe)) = procs.iter().find(|p| p.0 == parent) else { break };
        if !matches!(exe.to_lowercase().as_str(), "cmd.exe" | "conhost.exe") {
            return (*p, exe.clone());
        }
        pid = *p;
    }
    (0, String::new())
}
