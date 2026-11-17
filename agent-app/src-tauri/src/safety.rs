//! L5: what an action does to the world, asking the person at the computer before what can't be taken back, and
//! a record of what was done.
//!
//! Each action has an effect level:
//! - read: looks only (observe, screenshot, read_text, wait_for, ...).
//! - reversible: changes something that can be changed back (typing, clicking most things, moving windows).
//! - irreversible: can't be undone (delete, remove, erase, discard, uninstall, empty the trash, ...).
//! - external: goes out to someone else (send, submit, post, publish, pay, buy, place an order, transfer, ...).
//!
//! The level comes from what the action is aimed at (its target's words: the text named, or the name of the
//! control a reference stands for) and from what the caller declares (`effect`). A declaration can only raise
//! the level, never lower it: the AI can't talk its way past the check. Besides clicks:
//! - A PowerShell command that deletes, formats, uninstalls, shuts down or changes the system's protection is
//!   irreversible; one that sends mail or posts to the web is external.
//! - Enter (typed with the text, or pressed) in a messaging program or in a message field sends: external.
//!
//! The levels that need the person's yes are set on this computer (`ASCTRL_CONFIRM`: "off", "external", or the
//! default "external,irreversible"), never by the caller. Asking shows on the overlay, with what is about to be
//! done; no answer in time is a no, and a no means nothing was done.
//!
//! Every action (not the looking) is appended to %LOCALAPPDATA%\Asctrl\actions.log, one JSON line each: when, who
//! drove, the tool, what it aimed at, its level, whether the person was asked and said yes, and how it went.
//! Typed text and values are recorded by their length only.

use crate::tools::Output;
use serde_json::{Value, json};
use std::io::Write;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    Read,
    Reversible,
    Irreversible,
    External,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Read => "read",
            Level::Reversible => "reversible",
            Level::Irreversible => "irreversible",
            Level::External => "external",
        }
    }

    fn parse(s: &str) -> Option<Level> {
        match s {
            "read" => Some(Level::Read),
            "reversible" => Some(Level::Reversible),
            "irreversible" => Some(Level::Irreversible),
            "external" => Some(Level::External),
            _ => None,
        }
    }
}

const LOOKING: [&str; 10] = ["observe", "screenshot", "read_text", "wait_for", "read_grid", "watch", "skills", "exec", "resume", "timeline"];

/// Words of things that go out to someone else
fn external_words() -> &'static regex::Regex {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        regex::Regex::new(r"(?i)\b(send|submit|post|publish|reply|tweet|pay|purchase|buy|checkout|check out|place order|confirm order|order now|transfer|donate|subscribe)\b|发送|提交|发布|发表|回复|支付|付款|购买|下单|结算|转账|捐赠|订阅").unwrap()
    })
}

/// Words of things that can't be undone
fn irreversible_words() -> &'static regex::Regex {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        regex::Regex::new(r"(?i)\b(delete|remove|erase|discard|uninstall|format|wipe|destroy|empty (the )?(trash|recycle bin)|permanently|reset|overwrite|revoke|close account|deactivate)\b|删除|移除|清空|丢弃|卸载|格式化|永久|重置|覆盖|注销").unwrap()
    })
}

/// PowerShell that can't be taken back
fn shell_irreversible() -> &'static regex::Regex {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        regex::Regex::new(r"(?i)(^|[\s;|&({])(remove-item|ri|rm|del|erase|rmdir|rd|clear-content|clear-recyclebin|format-volume|format-disk|clear-disk|initialize-disk|remove-partition|diskpart|format|stop-computer|restart-computer|shutdown|uninstall-package|uninstall-module|remove-appxpackage|remove-itemproperty|remove-localuser|disable-localuser|set-executionpolicy|bcdedit|vssadmin|takeown|stop-service|disable-netadapter|remove-netfirewallrule|set-mppreference|disable-computerrestore)(\.exe)?([\s;|&)]|$)|reg(\.exe)?\s+delete|msiexec(\.exe)?\s+/x|wbadmin\s+delete|cipher(\.exe)?\s+/w|netsh\s+advfirewall\s+set|icacls\b.*/(remove|reset|deny)").unwrap()
    })
}

/// PowerShell that sends something out
fn shell_external() -> &'static regex::Regex {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"(?i)send-mailmessage|(invoke-webrequest|invoke-restmethod|iwr|irm|curl|wget)\b[^\n]*(-method\s+(post|put|patch|delete)|\s-x\s*(post|put|patch|delete)|\s(-d|--data|-body|-infile)\b)").unwrap())
}

/// Where Enter sends a message: messaging programs, mail, and fields for a message, a reply or a comment
fn sends_on_enter() -> &'static regex::Regex {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        regex::Regex::new(r"(?i)微信|企业微信|wechat|\bqq\b|slack|teams|discord|telegram|whatsapp|signal|messenger|钉钉|dingtalk|飞书|feishu|lark|outlook|gmail|\bline\b|skype|\b(message|reply|comment|chat)\b|消息|回复|评论|聊天").unwrap()
    })
}

/// The title of the window an action goes to
fn window_title(args: &Value) -> String {
    match args.get("window") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.as_i64().map(|h| crate::winmgr::title_of(h as isize)).unwrap_or_default(),
        _ => crate::winmgr::title_of(crate::winmgr::foreground()),
    }
}

/// The words an action is aimed at: the text it names, else the name of the control its reference stands for
fn aimed_at(args: &Value) -> String {
    let t = args.get("target").or_else(|| args.get("from"));
    match t {
        Some(Value::String(s)) => {
            let r = s.trim().trim_start_matches('[').trim_end_matches(']');
            match r.strip_prefix('e').or_else(|| r.strip_prefix('E')).and_then(|d| d.parse::<u32>().ok()) {
                Some(n) => crate::uia::label_of(n).unwrap_or_else(|| s.clone()),
                None => s.clone(),
            }
        }
        Some(Value::Number(n)) => n.as_u64().and_then(|n| crate::uia::label_of(n as u32)).unwrap_or_default(),
        _ => String::new(),
    }
}

/// The level of `tool` with `args`, and what it is aimed at
pub fn level(tool: &str, args: &Value) -> (Level, String) {
    if LOOKING.contains(&tool) || (tool == "clipboard" && args.get("text").is_none_or(Value::is_null)) {
        return (Level::Read, String::new());
    }
    let mut what = aimed_at(args);
    let mut l = Level::Reversible;
    if tool == "run_powershell" {
        let script = args.get("script").and_then(Value::as_str).unwrap_or("");
        what = script.lines().map(str::trim).find(|s| !s.is_empty()).unwrap_or("").chars().take(120).collect();
        if shell_irreversible().is_match(script) {
            l = Level::Irreversible;
        } else if shell_external().is_match(script) {
            l = Level::External;
        }
    }
    // Enter that sends: with the text, or pressed (alone or with ctrl, which sends in many messaging programs)
    let enter = match tool {
        "type_text" => args.get("enter").and_then(Value::as_bool) == Some(true) || args.get("text").and_then(Value::as_str).is_some_and(|t| t.ends_with('\n')),
        "press_keys" => args.get("keys").and_then(Value::as_str).is_some_and(|k| k.split_whitespace().any(|c| matches!(c.to_lowercase().as_str(), "enter" | "return" | "ctrl+enter" | "ctrl+return"))),
        _ => false,
    };
    if enter && (sends_on_enter().is_match(&window_title(args)) || sends_on_enter().is_match(&what)) {
        l = Level::External;
    }
    // Pressing, clicking, choosing: the words of what is pressed say what it does
    if matches!(tool, "click" | "element_action" | "mouse") && !what.is_empty() {
        if external_words().is_match(&what) {
            l = Level::External;
        } else if irreversible_words().is_match(&what) {
            l = Level::Irreversible;
        }
    }
    if let Some(d) = args.get("effect").and_then(Value::as_str).and_then(Level::parse) {
        l = l.max(d);
    }
    (l, what)
}

/// The levels that need the person's yes, as set on this computer
fn needs_yes(l: Level) -> bool {
    let set = std::env::var("ASCTRL_CONFIRM").unwrap_or_else(|_| "external,irreversible".into()).to_lowercase();
    if set.trim() == "off" || set.trim().is_empty() {
        return false;
    }
    set.split(',').any(|s| Level::parse(s.trim()) == Some(l))
}

/// Before an action: ask the person when its level needs it. Some(reply) when it must not go ahead.
pub async fn gate(tool: &str, args: &Value) -> (Level, String, Option<bool>, Option<Output>) {
    let (l, what) = level(tool, args);
    if !needs_yes(l) {
        return (l, what, None, None);
    }
    let shown = describe(tool, args, &what);
    let yes = crate::confirm_act(&shown, l.as_str()).await;
    if yes {
        return (l, what, Some(true), None);
    }
    let why = if l == Level::External { "it sends something out to someone else" } else { "it can't be undone" };
    let mut o = crate::tools::fail(format!(
        "The person at the computer did not allow this ({why}): {shown}. Nothing was done. Ask them, or do something else."
    ));
    o.data = Some(json!({ "refused": true, "effect": l.as_str() }));
    (l, what, Some(false), Some(o))
}

/// What is about to be done, in words for the person at the computer
fn describe(tool: &str, args: &Value, what: &str) -> String {
    let win = args.get("window").and_then(|w| w.as_str().map(str::to_string).or_else(|| w.as_i64().map(|h| crate::winmgr::title_of(h as isize)))).unwrap_or_default();
    let in_win = if win.is_empty() { String::new() } else { format!(" in \"{}\"", crate::uia::clip(&win, 60)) };
    let verb = match tool {
        "click" | "mouse" => "Click",
        "element_action" => "Press",
        "type_text" => "Type and press Enter in",
        "set_value" => "Set",
        "press_keys" => "Press Enter",
        "run_powershell" => "Run the command",
        _ => tool,
    };
    if what.is_empty() { format!("{verb}{in_win}") } else { format!("{verb} \"{}\"{in_win}", crate::uia::clip(what, 80)) }
}

/// After an action: one line in the log
pub fn record(tool: &str, args: &Value, l: Level, what: &str, asked: Option<bool>, out: &Output) {
    if l == Level::Read {
        return;
    }
    let mut a = args.clone();
    if let Some(o) = a.as_object_mut() {
        for k in ["text", "value"] {
            if let Some(v) = o.get(k).and_then(Value::as_str).map(|s| s.chars().count()) {
                o.insert(k.into(), json!(format!("<{v} characters>")));
            }
        }
        if let Some(f) = o.get_mut("fields").and_then(Value::as_object_mut) {
            for v in f.values_mut() {
                *v = json!(format!("<{} characters>", v.as_str().map(|s| s.chars().count()).unwrap_or(0)));
            }
        }
    }
    // The reply reads typed text back ("now reads \"...\""): not that either
    let mut result: String = out.body().lines().next().unwrap_or("").chars().take(200).collect();
    let mut typed: Vec<&str> = ["text", "value"].iter().filter_map(|k| args.get(*k).and_then(Value::as_str)).collect();
    typed.extend(args.get("fields").and_then(Value::as_object).into_iter().flat_map(|f| f.values().filter_map(Value::as_str)));
    for t in typed.into_iter().filter(|t| !t.is_empty()) {
        result = result.replace(t, &format!("<{} characters>", t.chars().count()));
    }
    if (result.contains("now reads") || result.contains("it now reads"))
        && let Some(i) = result.find("reads \"")
    {
        result.truncate(i + 6);
        result.push_str("…\" (not kept)");
    }
    let line = json!({
        "at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
        "by": crate::controller_name(),
        "tool": tool,
        "aimed": what,
        "args": a,
        "effect": l.as_str(),
        "asked": asked,
        "ok": out.ok,
        "channel": out.data.as_ref().and_then(|d| d.get("channel")).cloned(),
        "result": result,
    });
    let Some(base) = std::env::var_os("LOCALAPPDATA") else { return };
    let dir = std::path::PathBuf::from(base).join("Asctrl");
    let file = std::env::var_os("ASCTRL_ACTION_LOG").map(std::path::PathBuf::from).unwrap_or_else(|| dir.join("actions.log"));
    let _ = std::fs::create_dir_all(file.parent().unwrap_or(&dir));
    // Kept to about 20 MB: the older half moves aside
    if std::fs::metadata(&file).map(|m| m.len() > 10 << 20).unwrap_or(false) {
        let _ = std::fs::rename(&file, file.with_extension("log.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&file) {
        let _ = writeln!(f, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_come_from_the_words_and_declarations_only_raise_them() {
        let l = |tool: &str, a: Value| level(tool, &a).0;
        assert_eq!(l("observe", json!({})), Level::Read);
        assert_eq!(l("click", json!({ "target": "Save" })), Level::Reversible);
        assert_eq!(l("click", json!({ "target": "Send" })), Level::External);
        assert_eq!(l("click", json!({ "target": "Place order" })), Level::External);
        assert_eq!(l("click", json!({ "target": "发送" })), Level::External);
        assert_eq!(l("click", json!({ "target": "Delete account" })), Level::Irreversible);
        assert_eq!(l("element_action", json!({ "target": "清空回收站" })), Level::Irreversible);
        // Whole words: "Sender" is not "send", "Postcode" is not "post"
        assert_eq!(l("click", json!({ "target": "Sender" })), Level::Reversible);
        assert_eq!(l("click", json!({ "target": "Postcode" })), Level::Reversible);
        // A declaration raises the level, never lowers it
        assert_eq!(l("click", json!({ "target": "OK", "effect": "external" })), Level::External);
        assert_eq!(l("click", json!({ "target": "Send", "effect": "reversible" })), Level::External);
        // Typing is reversible; the button that sends it is what is checked
        assert_eq!(l("type_text", json!({ "target": "Search", "text": "hi" })), Level::Reversible);
        // ... unless Enter goes with it in a messaging program or a message field
        assert_eq!(l("type_text", json!({ "target": "Type a message", "text": "hi", "enter": true })), Level::External);
        assert_eq!(l("press_keys", json!({ "keys": "enter", "window": "微信" })), Level::External);
        assert_eq!(l("press_keys", json!({ "keys": "enter", "window": "Notepad" })), Level::Reversible);
        assert_eq!(l("type_text", json!({ "target": "Search", "text": "cats", "enter": true, "window": "Google Chrome" })), Level::Reversible);
        // The shell: what deletes, formats or shuts down; what posts or mails
        let sh = |s: &str| l("run_powershell", json!({ "script": s }));
        assert_eq!(sh("Get-ChildItem C:\\"), Level::Reversible);
        assert_eq!(sh("Remove-Item -Recurse C:\\temp\\x"), Level::Irreversible);
        assert_eq!(sh("Get-Process | ? CPU -gt 10; rm .\\out.txt"), Level::Irreversible);
        assert_eq!(sh("shutdown /s /t 0"), Level::Irreversible);
        assert_eq!(sh("reg delete HKCU\\Software\\X /f"), Level::Irreversible);
        assert_eq!(sh("Get-Item .\\format.txt"), Level::Reversible);
        assert_eq!(sh("Invoke-RestMethod https://x.example/api -Method Post -Body $b"), Level::External);
        assert_eq!(sh("Invoke-RestMethod https://x.example/api"), Level::Reversible);
        assert_eq!(sh("Send-MailMessage -To a@b.c -Subject hi"), Level::External);
    }
}
