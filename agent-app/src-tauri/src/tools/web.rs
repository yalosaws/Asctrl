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
/// Same-origin iframes are searched too (their elements' places are given in the top page's viewport);
/// cross-origin ones run in another process and are left to the other ways of finding.
const FIND: &str = r#"((want, nth, kind) => {
  const norm = s => (s || '').replace(/[\s\u200b-\u200f\u202a-\u202e]+/g, ' ').trim().toLowerCase();
  const w = norm(want);
  const FIELD = 'input:not([type=button]):not([type=submit]):not([type=reset]):not([type=image]):not([type=hidden]),textarea,select,[contenteditable=""],[contenteditable=true],[role=textbox],[role=combobox],[role=searchbox]';
  const INTER = FIELD + ',a[href],button,summary,[role=button],[role=link],[role=menuitem],[role=tab],[role=checkbox],[role=radio],[role=option],[role=switch],[onclick],[tabindex]:not([tabindex="-1"])';
  const all = [];
  const walk = root => { for (const el of root.querySelectorAll('*')) {
    all.push(el);
    if (el.shadowRoot) walk(el.shadowRoot);
    if (el.tagName === 'IFRAME' || el.tagName === 'FRAME') { let d = null; try { d = el.contentDocument; } catch (e) {} if (d && d.documentElement) walk(d); }
  } };
  walk(document);
  // The frames an element sits in, innermost first, and where its viewport starts in the top page's
  const frames = el => { const out = []; let w = el.ownerDocument.defaultView; while (w && w !== window && w.frameElement) { out.push(w.frameElement); w = w.parent; } return out; };
  const origin = el => { let x = 0, y = 0; for (const f of frames(el)) { const r = f.getBoundingClientRect(), cs = f.ownerDocument.defaultView.getComputedStyle(f);
    x += r.left + f.clientLeft + parseFloat(cs.paddingLeft); y += r.top + f.clientTop + parseFloat(cs.paddingTop); } return { x, y }; };
  const byId = (el, id) => (el.getRootNode().getElementById ? el.getRootNode().getElementById(id) : null) || el.ownerDocument.getElementById(id);
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
  const shown = el => { const r = el.getBoundingClientRect(); if (r.width < 1 || r.height < 1) return false; const s = el.ownerDocument.defaultView.getComputedStyle(el); return s.visibility !== 'hidden' && s.display !== 'none' && s.opacity !== '0'; };
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
  const view = r => { const o = origin(el); return { left: r.left + o.x, top: r.top + o.y, right: r.right + o.x, bottom: r.bottom + o.y, width: r.width, height: r.height }; };
  let r = view(el.getBoundingClientRect());
  const fv = el.ownerDocument.defaultView, fr = el.getBoundingClientRect();
  if (r.top < 0 || r.left < 0 || r.bottom > innerHeight || r.right > innerWidth || fr.top < 0 || fr.left < 0 || fr.bottom > fv.innerHeight || fr.right > fv.innerWidth) {
    el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' }); r = view(el.getBoundingClientRect()); }
  const x = Math.min(Math.max(r.left + r.width / 2, 1), innerWidth - 1), y = Math.min(Math.max(r.top + r.height / 2, 1), innerHeight - 1);
  // What is on top there: down through the frames to the one the element is in, then its shadow roots
  let hit = document.elementFromPoint(x, y);
  while (hit && (hit.tagName === 'IFRAME' || hit.tagName === 'FRAME')) { let d = null; try { d = hit.contentDocument; } catch (e) {} if (!d) break;
    const o = origin(d.documentElement); const h = d.elementFromPoint(x - o.x, y - o.y); if (!h) break; hit = h; }
  while (hit && hit.shadowRoot) { const o = origin(hit); const h = hit.shadowRoot.elementFromPoint(x - o.x, y - o.y); if (!h || h === hit) break; hit = h; }
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
  const opts = { subtree: true, childList: true, attributes: true, characterData: true };
  o.observe(document, opts);
  for (const f of document.querySelectorAll('iframe,frame')) { try { if (f.contentDocument) o.observe(f.contentDocument, opts); } catch (e) {} }
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

/// One action at a time in each tab. Not the turn to act with the real mouse and keyboard: a page action uses
/// neither, nor the focus, so it goes ahead while the mouse works elsewhere, while another tab is busy, and
/// while the person at the computer uses it.
static LANES: std::sync::Mutex<Vec<(String, Arc<tokio::sync::Mutex<()>>)>> = std::sync::Mutex::new(Vec::new());

pub(super) async fn lane(tab: &str) -> tokio::sync::OwnedMutexGuard<()> {
    let m = {
        let mut lanes = LANES.lock().unwrap();
        match lanes.iter().find(|(t, _)| t == tab) {
            Some((_, m)) => m.clone(),
            None => {
                // Tabs long closed: their lanes are free, drop them
                if lanes.len() >= 64 {
                    lanes.retain(|(_, m)| Arc::strong_count(m) > 1);
                }
                let m = Arc::new(tokio::sync::Mutex::new(()));
                lanes.push((tab.to_string(), m.clone()));
                m
            }
        }
    };
    m.lock_owned().await
}

#[derive(Clone)]
struct Page {
    c: Arc<Conn>,
    tab: Tab,
    hwnd: isize,
    /// The frame of another site the element acted on is in: its session (scripts run there)
    frame: Option<String>,
}

impl Page {
    async fn eval(&self, js: &str) -> Result<Value, String> {
        cdp::eval_in(&self.c, self.frame.as_deref().unwrap_or(&self.tab.session), js).await
    }

    /// Input through the browser. The mouse goes to the tab, at a point of its viewport (the browser passes it on
    /// to the frame under it); keys and text to the frame that has the focus, else the tab
    async fn input(&self, method: &str, params: Value) -> Result<Value, String> {
        if method != "Input.dispatchMouseEvent"
            && let Some(f) = &self.frame
            && let Ok(r) = self.c.call(Some(f), method, params.clone()).await
        {
            return Ok(r);
        }
        self.c.call(Some(&self.tab.session), method, params).await
    }

    /// The same page, acting in the frame where `k` was found
    fn at(&self, k: &Pick) -> Page {
        Page { frame: k.frame.clone(), ..self.clone() }
    }
}

/// The page shown in the call's window (or the one in front), when it is Asctrl's browser
async fn page(args: &Value) -> Option<Page> {
    let hwnd = match winmgr::find_arg(args.get("window")) {
        Ok(Some(h)) => h.0 as isize,
        Ok(None) => winmgr::foreground(),
        Err(_) => return None,
    };
    page_of(hwnd).await
}

async fn page_of(hwnd: isize) -> Option<Page> {
    let (c, tab) = cdp::tab_in(hwnd).await?;
    Some(Page { c, tab, hwnd, frame: None })
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
    /// In a frame of another site: its session (x, y are in the tab's viewport all the same)
    frame: Option<String>,
}

impl Pick {
    fn desc(&self) -> String {
        format!("{} \"{}\"", self.role, uia::clip(&self.name, 60))
    }
}

/// Look `want` up in the page until it shows, up to `wait`. None when it fits several elements equally well (and
/// no `nth` says which) or none: the usual lookup takes over and says so.
/// The page first, then the frames of other sites in it (each a page of its own to the browser).
async fn find(p: &Page, want: &str, nth: Option<usize>, kind: &str, wait: Duration) -> Option<Pick> {
    let call = format!("{FIND}({}, {}, {})", json!(want), nth.map_or("null".to_string(), |n| n.to_string()), json!(kind));
    let pick = |k: &Value, frame: Option<String>, (dx, dy): (f64, f64)| -> Option<Pick> {
        Some(Pick {
            role: k["role"].as_str().unwrap_or("element").into(),
            name: k["name"].as_str().unwrap_or("").into(),
            x: k["x"].as_f64()? + dx,
            y: k["y"].as_f64()? + dy,
            over: k["over"].as_str().map(str::to_string),
            frame,
        })
    };
    let start = Instant::now();
    loop {
        let r = p.eval(&call).await.ok()?;
        if let Some(k) = r.get("pick").filter(|k| k.is_object()) {
            return pick(k, None, (0.0, 0.0));
        }
        let mut count = r["count"].as_u64().unwrap_or(0);
        if count == 0 {
            for f in p.c.frames_of(&p.tab.session) {
                let r = match cdp::eval_in(&p.c, &f.session, &call).await {
                    Ok(r) => r,
                    Err(_) => {
                        p.c.forget_frame(&f.session);
                        continue;
                    }
                };
                if let Some(k) = r.get("pick").filter(|k| k.is_object()) {
                    let origin = cdp::frame_origin(&p.c, &p.tab, &f.id).await?;
                    return pick(k, Some(f.session.clone()), origin);
                }
                count += r["count"].as_u64().unwrap_or(0);
                if count > 0 {
                    break;
                }
            }
        }
        if count > 1 || start.elapsed() >= wait {
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
    let Some((want, nth, wait)) = text_target(ctx, args) else { return point_click(ctx, args).await };
    let p = page(args).await?;
    let turn = lane(&p.tab.id).await;
    let k = find(&p, &want, nth, "click", wait).await?;
    let p = p.at(&k);
    if let Some(o) = covered(ctx, &k).await {
        return Some(o);
    }
    Some(press(ctx, args, &p, &k, turn).await)
}

/// A click by reference or by a point on a screenshot, in a browser window that others cover: the browser is
/// given the click at that place of its page, so it stays where it is (in front, the mouse does it as usual)
async fn point_click(ctx: &Ctx, args: &Value) -> Option<Output> {
    let ask = Ask::of(ctx, args).ok()??;
    let hwnd = match (&ask.target, winmgr::find_arg(args.get("window")).ok().flatten()) {
        (_, Some(h)) => h.0 as isize,
        (Target::Ref(rf), None) => uia::hwnd_of(*rf)?,
        (Target::Point(..), None) => ctx.frame().own?,
        _ => return None,
    };
    if winmgr::foreground() == hwnd || winmgr::pid_of(winmgr::foreground()) == winmgr::pid_of(hwnd) {
        return None;
    }
    let p = page_of(hwnd).await?;
    // Where on the screen: the point, or the middle of the control the reference stands for
    let a2 = ask.clone();
    let (sx, sy) = tokio::task::spawn_blocking(move || locate::resolve(&a2).ok().map(|l| l.point)).await.ok()??;
    let (l, t, _, _) = winmgr::page_rect(hwnd, "Chrome_RenderWidgetHostHWND")?;
    let turn = lane(&p.tab.id).await;
    let dpr = p.eval("devicePixelRatio").await.ok()?.as_f64().unwrap_or(1.0).max(0.25);
    let (x, y) = ((sx - l) as f64 / dpr, (sy - t) as f64 / dpr);
    let what = p.eval(&format!("(() => {{ const e = document.elementFromPoint({x}, {y}); if (!e) return null; window.__asctrlPick = e; const t = (e.getAttribute('aria-label') || e.innerText || e.value || e.title || '').replace(/\\s+/g, ' ').trim(); return [e.tagName.toLowerCase(), t.slice(0, 60)]; }})()")).await.ok()?;
    if what.is_null() {
        return None;
    }
    let k = Pick { role: what[0].as_str().unwrap_or("element").into(), name: what[1].as_str().unwrap_or("").into(), x, y, over: None, frame: None };
    Some(press(ctx, args, &p, &k, turn).await)
}

/// The person's own browser doesn't draw a window others cover (it runs without Asctrl's flags), and input sent
/// to it waits seconds for a frame. There the element is clicked by the page's script instead: at once, but the
/// page can tell (its events are not "trusted"), and a few pages ignore such clicks.
const SCRIPT_CLICK: &str = r#"(() => {
  const el = window.__asctrlPick; if (!el) return false;
  const W = el.ownerDocument.defaultView, o = { bubbles: true, cancelable: true, composed: true, view: W, button: 0, buttons: 1 };
  for (const t of ['pointerdown', 'mousedown', 'pointerup', 'mouseup']) el.dispatchEvent(new (t.startsWith('pointer') ? W.PointerEvent : W.MouseEvent)(t, o));
  if (el.focus) el.focus();
  el.click();
  return true;
})()"#;

/// Whether `p` is a window of the person's own browser that other windows cover now
fn theirs_covered(p: &Page) -> bool {
    p.c.theirs && winmgr::rect_of(p.hwnd).is_some_and(|(l, t, r, b)| winmgr::covered(p.hwnd, crate::capture::Area { left: l, top: t, width: r - l, height: b - t }))
}

/// Draw an action on `k` on the overlay, as for one done with the mouse: a pointer flies there and presses, in a
/// frame round the window. Only where the person can see it (nothing covers the window there); the page is
/// worked without the mouse, so this is the only sign on screen of what happened. Never waits for anything.
fn page_fx(p: &Page, k: &Pick) -> Option<crate::fx::Fx> {
    let call = fx::call_id();
    if call.is_empty() {
        return None;
    }
    let (l, t, _, _) = winmgr::page_rect(p.hwnd, "Chrome_RenderWidgetHostHWND")?;
    let s = winmgr::scale_of(p.hwnd);
    let at = (l + (k.x * s).round() as i32, t + (k.y * s).round() as i32);
    if winmgr::top_window_at(at.0, at.1) != Some(p.hwnd) {
        return None;
    }
    let win = winmgr::rect_of(p.hwnd).map(fx::rect_json);
    Some(crate::fx::Fx::start(&call, "page", "click", json!({ "count": 1 }), json!({ "at": fx::pt_json(at), "zone": win, "zoneIsWindow": true, "win": win, "app": win })))
}

/// An action on `k` that is already done (typed into, set): drawn the same way, all at once
fn page_mark(p: &Page, k: &Pick, ok: bool) {
    if let Some(a) = page_fx(p, k) {
        a.step(0, ok);
        a.end(ok);
    }
}

/// Press at `k` in the page, wait for what it set off, and say so
async fn press(ctx: &Ctx, args: &Value, p: &Page, k: &Pick, turn: tokio::sync::OwnedMutexGuard<()>) -> Output {
    if theirs_covered(p) {
        let _ = p.eval(ARM).await;
        let t0 = Instant::now();
        if p.eval(SCRIPT_CLICK).await.ok() != Some(Value::Bool(true)) {
            return fail(format!("Could not click {} in the page", k.desc()));
        }
        let effect = settle(p, t0).await;
        drop(turn);
        return reply(ctx, args, p, format!("Clicked {} in the page by its script (the window is covered, and this browser doesn't draw covered windows, so a real click would wait for it; the page can tell a script click, and a few pages ignore one).", k.desc()), effect).await;
    }
    let _ = p.eval(ARM).await;
    let anim = page_fx(p, k);
    let t0 = Instant::now();
    let at = |kind: &str| json!({ "type": kind, "x": k.x, "y": k.y, "button": if kind == "mouseMoved" { "none" } else { "left" }, "clickCount": 1 });
    for kind in ["mouseMoved", "mousePressed"] {
        if let Err(e) = p.input("Input.dispatchMouseEvent", at(kind)).await {
            if let Some(a) = anim {
                a.step(0, false);
                a.end(false);
            }
            return fail(format!("The browser did not take the click: {e}"));
        }
    }
    if let Some(a) = anim {
        a.step(0, true);
        a.end(true);
    }
    // A page that opens an alert from the click holds the release's answer until the alert is closed
    let released = tokio::time::timeout(Duration::from_secs(2), p.input("Input.dispatchMouseEvent", at("mouseReleased"))).await;
    let alert = p.c.events_since(&p.tab.session, t0).into_iter().find(|e| e.method == "Page.javascriptDialogOpening");
    let effect = match (&released, &alert) {
        (_, Some(a)) => {
            drop(turn);
            let msg = a.params["message"].as_str().unwrap_or("");
            return text(format!("Clicked {} in the page, through the browser. The page opened a dialog that waits for an answer: \"{}\" (press_keys enter accepts it, escape dismisses it).", k.desc(), uia::clip(msg, 200)));
        }
        (Ok(Err(e)), _) => return fail(format!("The browser did not take the click: {e}")),
        _ => settle(p, t0).await,
    };
    drop(turn);
    reply(ctx, args, p, format!("Clicked {} in the page, through the browser (the mouse did not move, and the window stayed where it is).", k.desc()), effect).await
}

/// Put `text` in the picked field: at its end, or in place of what it holds
async fn put(p: &Page, text: &str, replace: bool) -> Result<(), String> {
    let focus = format!(
        r#"(() => {{
  const el = window.__asctrlPick, doc = el.ownerDocument; el.focus();
  if ({replace}) {{ if (el.select) el.select(); else doc.execCommand('selectAll'); }}
  else if (typeof el.value === 'string' && el.setSelectionRange) {{ try {{ el.setSelectionRange(el.value.length, el.value.length); }} catch (e) {{}} }}
  else {{ const s = doc.defaultView.getSelection(); s.selectAllChildren(el); s.collapseToEnd(); }}
  return doc.activeElement === el || el.contains(doc.activeElement) || (el.shadowRoot && el.shadowRoot.activeElement !== null);
}})()"#
    );
    if p.eval(&focus).await? != Value::Bool(true) {
        return Err("the field did not take the focus".into());
    }
    if replace && text.is_empty() {
        p.eval("window.__asctrlPick.ownerDocument.execCommand('delete')").await?;
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
    let turn = lane(&p.tab.id).await;
    let k = find(&p, &want, nth, "field", wait).await?;
    let p = p.at(&k);
    // A date, time, colour or range field takes no typed text (the browser draws its own picker): what is typed
    // is set as its value, as the picker would
    if p.eval("['date', 'time', 'datetime-local', 'month', 'week', 'color', 'range'].includes(window.__asctrlPick.type)").await.ok() == Some(Value::Bool(true)) {
        let _ = p.eval(ARM).await;
        let t0 = Instant::now();
        let Ok(Value::String(v)) = p.eval(&format!("({SET_VALUE})({})", json!(t))).await else {
            return Some(fail(format!("Could not set {}", k.desc())));
        };
        page_mark(&p, &k, !v.is_empty());
        if v.is_empty() {
            return Some(fail(format!("{} did not take \"{}\": give it in the form it shows (a date as 2026-12-24, a time as 20:00)", k.desc(), uia::clip(&t, 80))));
        }
        let effect = settle(&p, t0).await.typed();
        drop(turn);
        return Some(reply(ctx, args, &p, format!("Set {} to \"{}\" in the page, through the browser (it takes a value rather than typing).", k.desc(), uia::clip(&v, 200)), effect).await);
    }
    let _ = p.eval(ARM).await;
    let t0 = Instant::now();
    let put = put(&p, &t, replace).await;
    page_mark(&p, &k, put.is_ok());
    if let Err(e) = put {
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
    let turn = lane(&p.tab.id).await;
    let k = find(&p, &want, nth, "field", wait).await?;
    let p = p.at(&k);
    let _ = p.eval(ARM).await;
    let t0 = Instant::now();
    // Date, time, colour and range fields take no typed text (the browser draws their own pickers): their value is set
    let kind = p.eval("(() => { const el = window.__asctrlPick; return el.tagName === 'SELECT' ? 'select' : (el.type === 'checkbox' || el.type === 'radio') ? 'check' : ['date', 'time', 'datetime-local', 'month', 'week', 'color', 'range'].includes(el.type) ? 'value' : 'text'; })()").await.ok()?;
    let done = match kind.as_str() {
        Some("text") => match put(&p, &value, true).await {
            Ok(()) => format!("Set {} to \"{}\" in the page, through the browser", k.desc(), uia::clip(&reads(&p).await, 200)),
            Err(e) => return Some(fail(format!("Could not set {}: {e}", k.desc()))),
        },
        Some("value") => match p.eval(&format!("({SET_VALUE})({})", json!(value))).await {
            Ok(Value::String(v)) if !v.is_empty() => format!("Set {} to \"{}\" in the page, through the browser", k.desc(), uia::clip(&v, 200)),
            _ => return Some(fail(format!("{} did not take \"{}\": give it in the form it shows (a date as 2026-12-24, a time as 20:00)", k.desc(), uia::clip(&value, 80)))),
        },
        Some("select") => {
            // The option whose text (else value) is the one asked for, chosen as a person's pick would be
            let js = format!(
                r#"(() => {{ const el = window.__asctrlPick, w = {}.trim().toLowerCase();
  const o = [...el.options].find(o => o.text.trim().toLowerCase() === w) || [...el.options].find(o => o.value.toLowerCase() === w) || [...el.options].find(o => o.text.trim().toLowerCase().includes(w));
  if (!o) return null; const E = el.ownerDocument.defaultView.Event; el.value = o.value; el.dispatchEvent(new E('input', {{ bubbles: true }})); el.dispatchEvent(new E('change', {{ bubbles: true }})); return o.text.trim(); }})()"#,
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
    page_mark(&p, &k, true);
    let effect = if kind == "text" { settle(&p, t0).await.typed() } else { settle(&p, t0).await };
    drop(turn);
    Some(reply(ctx, args, &p, format!("{done}."), effect).await)
}

/// The text of document `d` as a person reads it: the whole root element, with what open shadow roots show, and
/// non-breaking spaces as the spaces they look like (VS Code's editor draws every space as one, so "hello world"
/// in a file read as "hello\u00a0world" and matched nothing)
const PAGE_TEXT: &str = r"((d.documentElement ? d.documentElement.innerText : '') + [...d.querySelectorAll('*')].filter(h => h.shadowRoot).map(h => [...h.shadowRoot.children].map(c => c.innerText || '').join('\n')).filter(Boolean).map(x => '\n' + x).join('')).replace(/\u00a0/g, ' ')";

/// Set the picked date, time, colour or range field to a value, as its own picker would: the value as given, or
/// else read as a date or a time ("24 December 2026", "8:00 PM") and put in the field's own form. The page sees
/// the input and change events a pick makes. Gives the value the field took ("" when none).
const SET_VALUE: &str = r#"(raw) => {
  const el = window.__asctrlPick, t = el.type, pad = (n) => String(n).padStart(2, '0');
  raw = String(raw).trim();
  const tries = [raw];
  const d = new Date(raw);
  if (!isNaN(d)) {
    const ymd = d.getFullYear() + '-' + pad(d.getMonth() + 1) + '-' + pad(d.getDate()), hm = pad(d.getHours()) + ':' + pad(d.getMinutes());
    tries.push(t === 'date' ? ymd : t === 'month' ? ymd.slice(0, 7) : t === 'datetime-local' ? ymd + 'T' + hm : t === 'time' ? hm : raw);
  }
  const m = /^(\d{1,2})[:.](\d{2})\s*([ap])?\.?m?\.?$/i.exec(raw);
  if (m) {
    let h = +m[1];
    if (m[3] && /p/i.test(m[3]) && h < 12) h += 12;
    if (m[3] && /a/i.test(m[3]) && h === 12) h = 0;
    tries.push(pad(h) + ':' + m[2]);
  }
  const set = Object.getOwnPropertyDescriptor(el.ownerDocument.defaultView.HTMLInputElement.prototype, 'value').set;
  for (const v of tries) { set.call(el, v); if (el.value) break; }
  const E = el.ownerDocument.defaultView.Event;
  el.dispatchEvent(new E('input', { bubbles: true }));
  el.dispatchEvent(new E('change', { bubbles: true }));
  return el.value;
}"#;

/// Most of a page's text in one reply
const MAX_TEXT: usize = 100_000;

/// After fast plan steps in a page: wait for the page (its own changes, not the screen) to settle before a step
/// that reads it. The last step armed the change counter; its changes are counted from then.
pub(super) async fn settle_after_fast(args: &Value) -> Option<(bool, u128)> {
    let p = page(args).await?;
    let _turn = lane(&p.tab.id).await;
    // The action was a moment ago: what has not changed by now counts as quiet sooner
    let t0 = Instant::now().checked_sub(QUIET / 2).unwrap_or_else(Instant::now);
    let e = settle(&p, t0).await;
    Some((!matches!(e, Effect::Busy { .. }), t0.elapsed().as_millis()))
}

/// The text of the whole page (not only what is on screen), for read_text on the browser's window with nothing
/// narrower asked. None: not for this channel.
pub(super) async fn read_text(args: &Value) -> Option<Output> {
    if ["target", "x0", "display"].iter().any(|k| args.get(*k).is_some_and(|v| !v.is_null())) || !matches!(str_arg(args, "source"), "" | "auto" | "page") {
        return None;
    }
    let p = page(args).await?;
    let r = p.eval(&format!("[document.title, location.href, (() => {{ const t = d => {{ let s = {PAGE_TEXT}; for (const f of d.querySelectorAll('iframe,frame')) {{ let c = null; try {{ c = f.contentDocument; }} catch (e) {{}} if (c) s += '\\n' + t(c); }} return s; }}; return t(document); }})()]")).await.ok()?;
    let mut all = r[2].as_str().unwrap_or("").to_string();
    for f in p.c.frames_of(&p.tab.session) {
        if let Ok(Value::String(t)) = cdp::eval_in(&p.c, &f.session, &format!("(() => {{ const d = document; return {PAGE_TEXT}; }})()")).await {
            all.push('\n');
            all.push_str(&t);
        }
    }
    let body = all.as_str();
    let (shown, more) = match body.char_indices().nth(MAX_TEXT) {
        Some((i, _)) => (&body[..i], format!("\n… the page goes on ({} characters in all)", body.chars().count())),
        None => (body, String::new()),
    };
    let mut out = text(format!("The page \"{}\" ({}), all of it, not only what is on screen:\n{shown}{more}", uia::clip(r[0].as_str().unwrap_or(""), 80), r[1].as_str().unwrap_or("")));
    out.data = Some(json!({ "channel": "page" }));
    Some(out)
}

/// Open `url` in Asctrl's own browser, or (`theirs`) in a new tab of the person's own browser through the extension
pub(super) async fn open(ctx: &Ctx, url: &str, theirs: bool) -> Output {
    let t0 = Instant::now();
    let opened = if theirs { cdp::open_in_theirs(url).await } else { cdp::open(url).await };
    let (c, id) = match opened {
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
    let mut out = text(if theirs {
        format!(
            "Opened \"{}\" ({at}) in a new tab of the person's own browser ({}, through Asctrl's extension, with the sites they are signed in to){win}: {how}. In this browser, text targets are found in the page itself and pressed and typed into through the browser, without the mouse.",
            uia::clip(&title, 80),
            c.name
        )
    } else {
        format!(
            "Opened \"{}\" ({at}) in Asctrl's own browser ({}, its own profile in {}){win}: {how}. In this browser, text targets are found in the page itself and pressed and typed into through the browser, without the mouse, even while other windows cover it.",
            uia::clip(&title, 80),
            c.name,
            cdp::profile_dir().display()
        )
    });
    out.data = Some(json!({ "channel": "page", "handle": hwnd, "url": at, "title": title }));
    out
}
