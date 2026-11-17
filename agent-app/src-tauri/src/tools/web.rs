//! The page channel: in Asctrl's own browser (see `cdp`), a target named by its text is found in the page
//! itself and acted on through the browser, not the mouse and keyboard.
//!
//! - Found among the page's elements, as a person reads them: a control by its label, a field by its label or
//!   placeholder, text by what it says (and then the link or button around it). Elements in open shadow roots
//!   count; frames don't yet.
//! - Pressed and typed into with input the browser itself delivers (CDP `Input`): the user's mouse and keyboard
//!   are left alone, and it works while other windows cover the browser.
//! - Waited on by the page's own events: what changed in the document (a MutationObserver), and whether it went
//!   on to load another page (and when that one had loaded), instead of comparing pixels.
//!
//! Anything this channel can't settle on its own goes the usual way, unchanged: a reference or a point, a
//! right / double / modified click, text that fits several elements or none (the usual lookup reports those,
//! with candidates), typing with line breaks or tabs.

use super::locate::{Ask, Miss, MissKind, Target};
use super::report::Did;
use super::*;
use crate::cdp::{self, Conn, Tab};
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;

/// Finds `want` among the page's elements. `kind`: "click" or "field". Returns how many fit equally well and,
/// when one is chosen (the only one, or `nth`), where it is in the page's viewport after scrolling it into
/// view, and what is on top of it there. The chosen element is kept as window.__asctrlPick.
const FIND: &str = r#"((want, nth, kind) => {
  const norm = s => (s || '').replace(/[\s\u200b-\u200f\u202a-\u202e]+/g, ' ').trim().toLowerCase();
  const w = norm(want);
  const FIELD = 'input:not([type=button]):not([type=submit]):not([type=reset]):not([type=image]):not([type=hidden]),textarea,select,[contenteditable=""],[contenteditable=true],[role=textbox],[role=combobox],[role=searchbox]';
  const INTER = FIELD + ',a[href],button,summary,[role=button],[role=link],[role=menuitem],[role=tab],[role=checkbox],[role=radio],[role=option],[role=switch],[onclick],[tabindex]:not([tabindex="-1"])';
  const all = [];
  const walk = root => { for (const el of root.querySelectorAll('*')) { all.push(el); if (el.shadowRoot) walk(el.shadowRoot); } };
  walk(document);
  const byId = (el, id) => (el.getRootNode().getElementById ? el.getRootNode().getElementById(id) : null) || document.getElementById(id);
  const name = el => {
    const a = el.getAttribute('aria-label'); if (a && a.trim()) return a;
    const lb = el.getAttribute('aria-labelledby');
    if (lb) { const t = lb.split(/\s+/).map(id => (byId(el, id) || {}).innerText || '').join(' '); if (t.trim()) return t; }
    if (el.matches(FIELD)) {
      if (el.labels && el.labels.length) return el.labels[0].innerText;
      return el.placeholder || el.getAttribute('aria-placeholder') || el.title || el.getAttribute('name') || '';
    }
    if (el.tagName === 'INPUT') return el.value || el.title || '';
    if (el.tagName === 'IMG') return el.alt || el.title || '';
    return el.innerText || el.title || '';
  };
  const shown = el => { const r = el.getBoundingClientRect(); if (r.width < 1 || r.height < 1) return false; const s = getComputedStyle(el); return s.visibility !== 'hidden' && s.display !== 'none' && s.opacity !== '0'; };
  const role = el => el.getAttribute('role') || ({ A: 'link', BUTTON: 'button', SUMMARY: 'button', TEXTAREA: 'field', SELECT: 'list', LABEL: 'label',
    INPUT: el.type === 'checkbox' ? 'checkbox' : el.type === 'radio' ? 'radio' : ['button', 'submit', 'reset'].includes(el.type) ? 'button' : 'field' })[el.tagName] || (el.isContentEditable ? 'field' : 'text');
  const found = new Map();
  for (const el of all) {
    const inter = el.matches(INTER);
    let n;
    if (inter || el.tagName === 'LABEL') n = name(el);
    else if ([...el.childNodes].some(c => c.nodeType === 3 && c.textContent.trim())) n = el.innerText;
    else continue;
    const shownAs = (n || '').replace(/[\s\u200b-\u200f\u202a-\u202e]+/g, ' ').trim();
    n = shownAs.toLowerCase();
    if (!n || n.length > 400) continue;
    const score = n === w ? 3 : n.startsWith(w) ? 2 : n.includes(w) ? 1 : 0;
    if (!score || !shown(el)) continue;
    // What a person would act on: a label's field, the control around matching text
    let t = el;
    if (el.tagName === 'LABEL' && el.control) t = el.control;
    else if (!inter && el.parentElement) t = el.parentElement.closest(INTER) || el;
    if (kind === 'field' && !t.matches(FIELD)) continue;
    const s = score + (t.matches(INTER) ? 0.5 : 0);
    const prev = found.get(t);
    if (!prev || prev.score < s) found.set(t, { el: t, score: s, name: shownAs });
  }
  let list = [...found.values()];
  // Controls inside controls: the innermost one that fits as well
  list = list.filter(c => !list.some(o => o !== c && o.score >= c.score && c.el.contains(o.el)));
  const best = Math.max(0, ...list.map(c => c.score));
  const top = list.filter(c => c.score === best);
  const desc = c => ({ name: c.name.slice(0, 80), role: role(c.el), tag: c.el.tagName.toLowerCase() });
  const res = { count: top.length, top: top.slice(0, 10).map(desc) };
  const i = nth ? nth - 1 : (top.length === 1 ? 0 : -1);
  if (i < 0 || i >= top.length) return res;
  const el = top[i].el;
  let r = el.getBoundingClientRect();
  if (r.top < 0 || r.left < 0 || r.bottom > innerHeight || r.right > innerWidth) { el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' }); r = el.getBoundingClientRect(); }
  const x = Math.min(Math.max(r.left + r.width / 2, 1), innerWidth - 1), y = Math.min(Math.max(r.top + r.height / 2, 1), innerHeight - 1);
  let hit = document.elementFromPoint(x, y);
  while (hit && hit.shadowRoot) { const h = hit.shadowRoot.elementFromPoint(x, y); if (!h || h === hit) break; hit = h; }
  const over = hit && !(el === hit || el.contains(hit) || hit.contains(el) || (hit.tagName === 'LABEL' && hit.control === el))
    ? (hit.tagName.toLowerCase() + (norm(hit.innerText) ? ' "' + norm(hit.innerText).slice(0, 60) + '"' : '')) : null;
  window.__asctrlPick = el;
  return Object.assign(res, { pick: Object.assign(desc(top[i]), { x, y, over }) });
})"#;

/// Counts what changes in the document from now on (and when it last did); a document loaded since has none
const ARM: &str = r#"(() => {
  const m = window.__asctrlMut = { n: 0, last: performance.now() };
  if (window.__asctrlObs) window.__asctrlObs.disconnect();
  const o = window.__asctrlObs = new MutationObserver(r => { m.n += r.length; m.last = performance.now(); });
  o.observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
  return true;
})()"#;

/// No change within this long after an action: nothing happened (or not yet), as for the screen
const QUIET: Duration = Duration::from_millis(300);
/// Once the document changed: no further change for this long and it has settled
const CALM: Duration = Duration::from_millis(150);
/// Longest wait for the document to stop changing
const SETTLE_MAX: Duration = Duration::from_millis(1500);
/// Longest wait for a page it went on to load
const LOAD_MAX: Duration = Duration::from_secs(10);

struct Page {
    c: Arc<Conn>,
    tab: Tab,
    hwnd: isize,
}

impl Page {
    async fn eval(&self, js: &str) -> Result<Value, String> {
        cdp::eval(&self.c, &self.tab, js).await
    }

    async fn input(&self, method: &str, params: Value) -> Result<Value, String> {
        self.c.call(Some(&self.tab.session), method, params).await
    }
}

/// The page shown in the call's window (or the one in front), when it is Asctrl's browser
async fn page(args: &Value) -> Option<Page> {
    let hwnd = match winmgr::find_arg(args.get("window")) {
        Ok(Some(h)) => h.0 as isize,
        Ok(None) => winmgr::foreground(),
        Err(_) => return None,
    };
    let (c, tab) = cdp::tab_in(hwnd).await?;
    Some(Page { c, tab, hwnd })
}

/// The text the call names as its target, when it names one by text (no reference, point or role)
fn text_target(ctx: &Ctx, args: &Value) -> Option<(String, Option<usize>, Duration)> {
    let ask = Ask::of(ctx, args).ok()??;
    if !ask.role.is_empty() {
        return None;
    }
    match ask.target {
        Target::Text(t) => Some((t, ask.nth, ask.wait)),
        _ => None,
    }
}

/// An element found in the page
struct Pick {
    role: String,
    name: String,
    x: f64,
    y: f64,
    /// Another element on top of it where it would be pressed
    over: Option<String>,
}

impl Pick {
    fn desc(&self) -> String {
        format!("{} \"{}\"", self.role, uia::clip(&self.name, 60))
    }
}

/// Look `want` up in the page until it shows, up to `wait`. None when it fits several elements equally well (and
/// no `nth` says which) or none: the usual lookup takes over and says so.
async fn find(p: &Page, want: &str, nth: Option<usize>, kind: &str, wait: Duration) -> Option<Pick> {
    let call = format!("{FIND}({}, {}, {})", json!(want), nth.map_or("null".to_string(), |n| n.to_string()), json!(kind));
    let start = Instant::now();
    loop {
        let r = p.eval(&call).await.ok()?;
        if let Some(k) = r.get("pick").filter(|k| k.is_object()) {
            return Some(Pick {
                role: k["role"].as_str().unwrap_or("element").into(),
                name: k["name"].as_str().unwrap_or("").into(),
                x: k["x"].as_f64()?,
                y: k["y"].as_f64()?,
                over: k["over"].as_str().map(str::to_string),
            });
        }
        if r["count"].as_u64().unwrap_or(0) > 1 || start.elapsed() >= wait {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// How the page took an action
enum Effect {
    /// It went on to load another page: its title and address, and how long that took
    Loaded { title: String, url: String, ms: u128 },
    /// The address changed without a new page (a single-page app)
    Moved { url: String, changes: u64, ms: u128 },
    Changed { changes: u64, ms: u128 },
    Busy { changes: u64, ms: u128 },
    Unchanged { ms: u128 },
    /// Nothing else changed, and nothing needed to: typing shows in the field's value, read back in the reply
    Typed,
    /// Not waited for (a fast plan step)
    Unwatched,
}

impl Effect {
    fn say(&self) -> String {
        match self {
            Effect::Loaded { title, url, ms } => format!(" It loaded \"{}\" ({}) in {ms} ms.", uia::clip(title, 80), uia::clip(url, 120)),
            Effect::Moved { url, changes, ms } => format!(" The page changed ({changes} changes) and its address is now {} (settled in {ms} ms).", uia::clip(url, 120)),
            Effect::Changed { changes, ms } => format!(" The page changed ({changes} changes; settled in {ms} ms)."),
            Effect::Busy { changes, ms } => format!(" The page changed ({changes} changes) and was still changing after {ms} ms (loading or animating)."),
            Effect::Unchanged { ms } => format!(" Nothing in the page changed within {ms} ms: it may not respond to this, or is still working."),
            Effect::Typed | Effect::Unwatched => String::new(),
        }
    }

    /// For typing: a page that doesn't redraw anything as text goes in is as it should be
    fn typed(self) -> Effect {
        match self {
            Effect::Unchanged { .. } => Effect::Typed,
            e => e,
        }
    }

    fn data(&self) -> Option<&'static str> {
        match self {
            Effect::Unchanged { .. } | Effect::Typed => Some("unchanged"),
            Effect::Unwatched => None,
            _ => Some("changed"),
        }
    }
}

/// Whether the main frame started loading another document since `t0`
fn loading(p: &Page, t0: Instant) -> bool {
    p.c.events_since(&p.tab.session, t0).iter().any(|e| {
        matches!(e.method.as_str(), "Page.frameStartedLoading" | "Page.frameRequestedNavigation" | "Page.frameNavigated")
            // A link that opens in a new tab leaves this page where it is
            && matches!(e.params["disposition"].as_str(), None | Some("currentTab"))
            && e.params["frameId"].as_str().or(e.params["frame"]["id"].as_str()) == Some(p.tab.id.as_str())
            && e.params["frame"]["parentId"].is_null()
    })
}

/// Wait for what an action set off in the page, from `t0` (just before it)
async fn settle(p: &Page, t0: Instant) -> Effect {
    if Pace::current().fast {
        return Effect::Unwatched;
    }
    let ms = || t0.elapsed().as_millis();
    loop {
        // A new page on its way: wait until it has loaded
        if loading(p, t0) || p.eval("!!window.__asctrlMut").await.map(|v| v == false).unwrap_or(true) {
            let deadline = Instant::now() + LOAD_MAX;
            while Instant::now() < deadline {
                if p.c.events_since(&p.tab.session, t0).iter().any(|e| e.method == "Page.loadEventFired") {
                    break;
                }
                if p.eval("document.readyState === 'complete' && !window.__asctrlMut").await.ok() == Some(Value::Bool(true)) {
                    break;
                }
                p.c.wait_event(Duration::from_millis(100)).await;
            }
            let r = p.eval("[document.title, location.href]").await.unwrap_or_default();
            return Effect::Loaded { title: r[0].as_str().unwrap_or("").into(), url: r[1].as_str().unwrap_or("").into(), ms: ms() };
        }
        let m = p.eval("(() => { const m = window.__asctrlMut; return m ? [m.n, performance.now() - m.last] : null })()").await.unwrap_or_default();
        let (n, idle) = (m[0].as_u64().unwrap_or(0), Duration::from_secs_f64(m[1].as_f64().unwrap_or(0.0).max(0.0) / 1000.0));
        let elapsed = t0.elapsed();
        let moved = p.c.events_since(&p.tab.session, t0).iter().rev().find(|e| e.method == "Page.navigatedWithinDocument").and_then(|e| e.params["url"].as_str().map(str::to_string));
        if n == 0 && elapsed >= QUIET {
            return match moved {
                Some(url) => Effect::Moved { url, changes: 0, ms: ms() },
                None => Effect::Unchanged { ms: ms() },
            };
        }
        if n > 0 && idle >= CALM {
            return match moved {
                Some(url) => Effect::Moved { url, changes: n, ms: ms() },
                None => Effect::Changed { changes: n, ms: ms() },
            };
        }
        if elapsed >= SETTLE_MAX {
            return Effect::Busy { changes: n, ms: ms() };
        }
        p.c.wait_event(Duration::from_millis(30)).await;
    }
}

/// The reply: as any action's (with what changed among the window's controls, when the AI reads them), with
/// how the page took it
async fn reply(ctx: &Ctx, args: &Value, p: &Page, done: String, effect: Effect) -> Output {
    let mut did = Did::new(format!("{done}{}", effect.say()).trim_end_matches('.').to_string());
    did.hwnd = Some(p.hwnd);
    did.delivered = true;
    let mut out = report::conclude(ctx, args, did, false).await;
    let mut data = out.data.take().unwrap_or_else(|| json!({}));
    data["channel"] = json!("page");
    if let Some(e) = effect.data() {
        data["effect"] = json!(e);
    }
    out.data = Some(data);
    out
}

/// What the page has on top of an element where it would be pressed: a person's click would land there
async fn covered(ctx: &Ctx, k: &Pick) -> Option<Output> {
    let over = k.over.as_ref()?;
    Some(report::missed(ctx, Miss::new(MissKind::Other, format!("Not done: {} is covered in the page by {over}, which a click there would hit instead. Close or scroll that away first.", k.desc()))).await)
}

/// A left click on an element named by its text. None: not for this channel.
pub(super) async fn click(ctx: &Ctx, args: &Value) -> Option<Output> {
    let plain = matches!(str_arg(args, "button"), "" | "left") && int_arg(args, "count", 1, 1, 3) == 1 && str_arg(args, "modifiers").trim().is_empty();
    if !plain || matches!(str_arg(args, "mode"), "mouse" | "direct") {
        return None;
    }
    let (want, nth, wait) = text_target(ctx, args)?;
    let p = page(args).await?;
    let turn = acting().await;
    let k = find(&p, &want, nth, "click", wait).await?;
    if let Some(o) = covered(ctx, &k).await {
        return Some(o);
    }
    let _ = p.eval(ARM).await;
    let t0 = Instant::now();
    let at = |kind: &str| json!({ "type": kind, "x": k.x, "y": k.y, "button": if kind == "mouseMoved" { "none" } else { "left" }, "clickCount": 1 });
    for kind in ["mouseMoved", "mousePressed"] {
        if let Err(e) = p.input("Input.dispatchMouseEvent", at(kind)).await {
            return Some(fail(format!("The browser did not take the click: {e}")));
        }
    }
    // A page that opens an alert from the click holds the release's answer until the alert is closed
    let released = tokio::time::timeout(Duration::from_secs(2), p.input("Input.dispatchMouseEvent", at("mouseReleased"))).await;
    let alert = p.c.events_since(&p.tab.session, t0).into_iter().find(|e| e.method == "Page.javascriptDialogOpening");
    let effect = match (&released, &alert) {
        (_, Some(a)) => {
            drop(turn);
            let msg = a.params["message"].as_str().unwrap_or("");
            return Some(text(format!("Clicked {} in the page, through the browser. The page opened a dialog that waits for an answer: \"{}\" (press_keys enter accepts it, escape dismisses it).", k.desc(), uia::clip(msg, 200))));
        }
        (Ok(Err(e)), _) => return Some(fail(format!("The browser did not take the click: {e}"))),
        _ => settle(&p, t0).await,
    };
    drop(turn);
    Some(reply(ctx, args, &p, format!("Clicked {} in the page, through the browser (the mouse did not move).", k.desc()), effect).await)
}

/// Put `text` in the picked field: at its end, or in place of what it holds
async fn put(p: &Page, text: &str, replace: bool) -> Result<(), String> {
    let focus = format!(
        r#"(() => {{
  const el = window.__asctrlPick; el.focus();
  if ({replace}) {{ if (el.select) el.select(); else document.execCommand('selectAll'); }}
  else if (typeof el.value === 'string' && el.setSelectionRange) {{ try {{ el.setSelectionRange(el.value.length, el.value.length); }} catch (e) {{}} }}
  else {{ const s = getSelection(); s.selectAllChildren(el); s.collapseToEnd(); }}
  return document.activeElement === el || el.contains(document.activeElement) || (el.shadowRoot && el.shadowRoot.activeElement !== null);
}})()"#
    );
    if p.eval(&focus).await? != Value::Bool(true) {
        return Err("the field did not take the focus".into());
    }
    if replace && text.is_empty() {
        p.eval("document.execCommand('delete')").await?;
        return Ok(());
    }
    p.input("Input.insertText", json!({ "text": text })).await.map(|_| ())
}

/// What the picked field holds now
async fn reads(p: &Page) -> String {
    p.eval("(() => { const el = window.__asctrlPick; return typeof el.value === 'string' ? el.value : el.innerText; })()").await.ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

async fn enter(p: &Page) -> Result<(), String> {
    for kind in ["keyDown", "keyUp"] {
        let mut k = json!({ "type": kind, "key": "Enter", "code": "Enter", "windowsVirtualKeyCode": 13, "nativeVirtualKeyCode": 13 });
        if kind == "keyDown" {
            k["text"] = json!("\r");
        }
        p.input("Input.dispatchKeyEvent", k).await?;
    }
    Ok(())
}

/// Typing into a field named by its text (label, placeholder). None: not for this channel.
pub(super) async fn type_text(ctx: &Ctx, args: &Value) -> Option<Output> {
    let t = str_arg(args, "text").to_string();
    let (replace, press_enter) = (bool_arg(args, "replace") == Some(true), bool_arg(args, "enter") == Some(true));
    if t.contains(['\n', '\t']) || !matches!(str_arg(args, "method"), "" | "auto") || (t.is_empty() && !replace && !press_enter) {
        return None;
    }
    let (want, nth, wait) = text_target(ctx, args)?;
    let p = page(args).await?;
    let turn = acting().await;
    let k = find(&p, &want, nth, "field", wait).await?;
    let _ = p.eval(ARM).await;
    let t0 = Instant::now();
    if let Err(e) = put(&p, &t, replace).await {
        return Some(fail(format!("Could not type into {}: {e}", k.desc())));
    }
    let now = reads(&p).await;
    if press_enter && let Err(e) = enter(&p).await {
        return Some(fail(format!("The browser did not take Enter: {e}")));
    }
    let effect = settle(&p, t0).await.typed();
    drop(turn);
    let n = t.chars().count();
    let what = if replace { format!("Replaced what {} held with {n} characters", k.desc()) } else { format!("Typed {n} characters into {}", k.desc()) };
    let enter_note = if press_enter { " and pressed Enter" } else { "" };
    Some(reply(ctx, args, &p, format!("{what} in the page, through the browser; it now reads \"{}\"{enter_note}.", uia::clip(&now, 200)), effect).await)
}

/// Setting one text field (or a list's choice) named by its text. None: not for this channel.
pub(super) async fn set_value(ctx: &Ctx, args: &Value) -> Option<Output> {
    if args.get("fields").is_some_and(|f| !f.is_null()) {
        return None;
    }
    let value = args.get("value")?.as_str()?.to_string();
    if value.contains(['\n', '\t']) {
        return None;
    }
    let (want, nth, wait) = text_target(ctx, args)?;
    let p = page(args).await?;
    let turn = acting().await;
    let k = find(&p, &want, nth, "field", wait).await?;
    let _ = p.eval(ARM).await;
    let t0 = Instant::now();
    let kind = p.eval("(() => { const el = window.__asctrlPick; return el.tagName === 'SELECT' ? 'select' : (el.type === 'checkbox' || el.type === 'radio') ? 'check' : 'text'; })()").await.ok()?;
    let done = match kind.as_str() {
        Some("text") => match put(&p, &value, true).await {
            Ok(()) => format!("Set {} to \"{}\" in the page, through the browser", k.desc(), uia::clip(&reads(&p).await, 200)),
            Err(e) => return Some(fail(format!("Could not set {}: {e}", k.desc()))),
        },
        Some("select") => {
            // The option whose text (else value) is the one asked for, chosen as a person's pick would be
            let js = format!(
                r#"(() => {{ const el = window.__asctrlPick, w = {}.trim().toLowerCase();
  const o = [...el.options].find(o => o.text.trim().toLowerCase() === w) || [...el.options].find(o => o.value.toLowerCase() === w) || [...el.options].find(o => o.text.trim().toLowerCase().includes(w));
  if (!o) return null; el.value = o.value; el.dispatchEvent(new Event('input', {{ bubbles: true }})); el.dispatchEvent(new Event('change', {{ bubbles: true }})); return o.text.trim(); }})()"#,
                json!(value)
            );
            match p.eval(&js).await {
                Ok(Value::String(o)) => format!("Chose \"{}\" in {} in the page, through the browser", uia::clip(&o, 80), k.desc()),
                _ => return Some(report::missed(ctx, Miss::new(MissKind::Absent, format!("{} has no option \"{value}\"", k.desc()))).await),
            }
        }
        // Checkboxes and the rest: the usual way
        _ => return None,
    };
    let effect = if kind == "text" { settle(&p, t0).await.typed() } else { settle(&p, t0).await };
    drop(turn);
    Some(reply(ctx, args, &p, format!("{done}."), effect).await)
}

/// Most of a page's text in one reply
const MAX_TEXT: usize = 100_000;

/// The text of the whole page (not only what is on screen), for read_text on the browser's window with nothing
/// narrower asked. None: not for this channel.
pub(super) async fn read_text(args: &Value) -> Option<Output> {
    if ["target", "x0", "display"].iter().any(|k| args.get(*k).is_some_and(|v| !v.is_null())) || !matches!(str_arg(args, "source"), "" | "auto" | "page") {
        return None;
    }
    let p = page(args).await?;
    let r = p.eval("[document.title, location.href, document.body ? document.body.innerText : '']").await.ok()?;
    let body = r[2].as_str().unwrap_or("");
    let (shown, more) = match body.char_indices().nth(MAX_TEXT) {
        Some((i, _)) => (&body[..i], format!("\n… the page goes on ({} characters in all)", body.chars().count())),
        None => (body, String::new()),
    };
    Some(text(format!("The page \"{}\" ({}), all of it, not only what is on screen:\n{shown}{more}", uia::clip(r[0].as_str().unwrap_or(""), 80), r[1].as_str().unwrap_or(""))))
}

/// Open `url` in Asctrl's own browser and wait for it to load
pub(super) async fn open(ctx: &Ctx, url: &str) -> Output {
    let t0 = Instant::now();
    let (c, id) = match cdp::open(url).await {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    let session = match c.session(&id).await {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let tab = Tab { id: id.clone(), session, title: String::new(), url: url.into() };
    // Loaded: the document says so (its load event may have come before the session was there to hear it)
    let deadline = Instant::now() + LOAD_MAX;
    let mut loaded = false;
    while Instant::now() < deadline {
        let r = cdp::eval(&c, &tab, "[document.readyState, location.href]").await.unwrap_or_default();
        if r[0] == "complete" && r[1] != "about:blank" {
            loaded = true;
            break;
        }
        c.wait_event(Duration::from_millis(100)).await;
    }
    let r = cdp::eval(&c, &tab, "[document.title, location.href]").await.unwrap_or_default();
    let (title, at) = (r[0].as_str().unwrap_or("").to_string(), r[1].as_str().unwrap_or(url).to_string());
    // Its window, in front
    let mut hwnd = None;
    for _ in 0..40 {
        hwnd = cdp::window_of(&c, &id).await;
        if hwnd.is_some_and(|h| title.is_empty() || winmgr::title_of(h).starts_with(&title)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if let Some(h) = hwnd {
        let _ = tokio::task::spawn_blocking(move || winmgr::bring_to_front(winmgr::hwnd(h))).await;
        *ctx.opened.lock().unwrap() = Some(h);
    }
    let how = if loaded { format!("loaded in {} ms", t0.elapsed().as_millis()) } else { format!("still loading after {} s", LOAD_MAX.as_secs()) };
    let win = hwnd.map(|h| format!(", window handle {h}")).unwrap_or_default();
    let mut out = text(format!(
        "Opened \"{}\" ({at}) in Asctrl's own browser ({}, its own profile in {}){win}: {how}. In this browser, text targets are found in the page itself and pressed and typed into through the browser, without the mouse, even while other windows cover it.",
        uia::clip(&title, 80),
        c.name,
        cdp::profile_dir().display()
    ));
    out.data = Some(json!({ "channel": "page", "handle": hwnd, "url": at, "title": title }));
    out
}
