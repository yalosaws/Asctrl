//! The keyboard: typing text and pressing keys. Both say where the keys went, and typing reads the field
//! back when it can, so text that landed in the wrong place or was changed on the way shows at once.

use super::locate::{Ask, Located, Miss};
use super::report::Did;
use super::*;
use crate::until;

/// type_text pastes text longer than this (method auto)
const PASTE_OVER: usize = 200;

/// How long a program gets to read the clipboard after ctrl+v when nothing tells us it has
const PASTE_GRACE: Duration = Duration::from_millis(350);
/// How long a field gets to show what was just typed before its content is called wrong
const READBACK_GRACE: Duration = Duration::from_millis(250);
/// The longest a field that is still changing is waited for
const READBACK_MAX: Duration = Duration::from_millis(1500);

/// The clipboard being put back after a paste: the thread doing it, and a way to tell it not to wait any longer
struct Restore {
    thread: std::thread::JoinHandle<()>,
    now: std::sync::mpsc::Sender<()>,
}

static RESTORING: std::sync::Mutex<Option<Restore>> = std::sync::Mutex::new(None);

/// Wait until the clipboard is the user's own again (a paste may still be putting it back)
pub(super) fn clipboard_settled() {
    let pending = RESTORING.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(r) = pending {
        // `now` stays alive until the thread is done: dropping it first would cut the wait short
        let _ = r.thread.join();
        drop(r.now);
    }
}

/// The pasted text is known to have arrived (the field reads it): the clipboard can go back right away
pub(super) fn clipboard_release() {
    if let Some(r) = RESTORING.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        let _ = r.now.send(());
    }
}

/// Paste `t` with ctrl+v, then put the clipboard back as it was. The program reads the clipboard when it
/// handles the keystroke, a moment later, so it is put back after a pause (or as soon as the text is seen to
/// have arrived) — in the background: the reply does not wait for it, and the next action does (see `acting`).
/// If someone else put something new on the clipboard in the meantime, it is theirs and is left alone.
fn paste(t: &str, saved: Option<String>, previous: u32) -> Result<(), String> {
    let serial = winmgr::replace_clipboard(Some(t), Some(previous))?;
    let r = input::press_keys("ctrl+v");
    let (now, wait) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        let _ = wait.recv_timeout(PASTE_GRACE);
        let _ = winmgr::replace_clipboard(saved.as_deref(), Some(serial));
    });
    *RESTORING.lock().unwrap_or_else(|e| e.into_inner()) = Some(Restore { thread, now });
    r
}

/// Whether `t` goes in through the clipboard
fn pastes(t: &str, method: &str) -> bool {
    method == "paste" || (method != "keys" && t.chars().count() > PASTE_OVER)
}

/// Type or paste, by method and length. Says how it went in.
pub(super) fn enter_text(t: &str, method: &str) -> Result<&'static str, String> {
    if pastes(t, method) {
        clipboard_settled();
        match winmgr::clipboard_text() {
            Ok((saved, serial)) => {
                paste(t, saved, serial)?;
                return Ok(" (pasted; the clipboard was put back)");
            }
            Err(_) if method == "paste" => return Err("The clipboard contains formats that pasting would lose; use method keys".into()),
            // Something on the clipboard that couldn't be put back: type instead
            Err(_) => {}
        }
    }
    let mut sync = LineSync::default();
    input::type_text(t, method == "keys", &mut |line| sync.wait(line))?;
    Ok("")
}

/// The longest one line is waited for before its Enter
const LINE_WAIT_MAX: Duration = Duration::from_millis(1500);
/// Before Enter in a field that can't tell what it holds: a pause long enough for most programs
const LINE_PAUSE: Duration = Duration::from_millis(40);

/// Waits, before each Enter, until the program has taken in the line just typed: under a Chinese, Japanese or
/// Korean input method Enter would otherwise overtake its last characters (Notepad turned "34⏎35" into "⏎3435"
/// and dropped digits). It reads the characters before the caret; a field that doesn't say, or that changes
/// what is typed into it (autocomplete), gets a short pause instead, from then on for the rest of the text.
#[derive(Default)]
struct LineSync {
    blind: bool,
}

impl LineSync {
    fn wait(&mut self, line: &str) {
        if self.blind {
            return std::thread::sleep(LINE_PAUSE);
        }
        // The end of the line is enough to tell it has all arrived, and in order
        let tail: Vec<char> = line.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
        let want: String = tail.iter().collect();
        let start = std::time::Instant::now();
        loop {
            match uia::text_before_caret(tail.len() as i32) {
                Some(t) if t.ends_with(&want) => return,
                None => break,
                Some(_) if start.elapsed() >= LINE_WAIT_MAX => break,
                Some(_) => std::thread::sleep(Duration::from_millis(4)),
            }
        }
        self.blind = true;
        std::thread::sleep(LINE_PAUSE);
    }
}

/// How long a control given the focus directly may take to say it has it, before it is clicked instead
const FOCUS_DIRECT: Duration = Duration::from_millis(100);
/// How long a clicked control (a page's, often) may take to say it has the focus. A page may first close
/// what the last field opened (Gmail's address suggestions under "To" take most of a second); only a
/// control that never takes it waits this long.
const FOCUS_CLICK: Duration = Duration::from_millis(1200);

/// Put the keyboard focus on a located target: a control of an ordinary program takes it directly; anything
/// else (a web page's field, a spot on a canvas, a control that won't take it) is clicked.
pub(super) fn focus_target(loc: &Located, call: &str, part: &str) -> Result<(), String> {
    if let Some(l) = &loc.live {
        if !l.enabled() {
            return Err(format!("{} is disabled", loc.desc));
        }
        pointer::activate_for(loc);
        if !l.is_web() && l.focus().is_ok() && until::holds(FOCUS_DIRECT, || l.has_focus()) {
            return Ok(());
        }
    }
    locate::uncover(loc)?;
    let r = input::click(loc.point.0, loc.point.1, "", 1, "");
    fx::click(call, part, loc.point, 1, r.is_ok());
    r?;
    // Keys sent next queue up behind the click, so nothing needs waiting for — except to know where the focus
    // went, for a control that says (a page handles the click a moment later, in its own process)
    if let Some(l) = &loc.live {
        until::holds(FOCUS_CLICK, || l.has_focus());
    }
    Ok(())
}

/// Another text field kept the keyboard focus after `loc` was focused (a dropdown or a field's own
/// autocomplete took the click): the field that has it, else None. A focus inside or around the target (its
/// label, the box wrapping it) counts as the target's.
fn other_field_focused(loc: &Located) -> Option<String> {
    let live = loc.live.as_ref()?;
    if live.has_focus() {
        return None;
    }
    let target = live.rect()?;
    let fo = uia::focused()?;
    let r = fo.rect?;
    let overlaps = r.0 < target.2 && target.0 < r.2 && r.1 < target.3 && target.1 < r.3;
    (matches!(fo.role, "Edit" | "ComboBox" | "Document") && !overlaps).then_some(fo.label)
}

/// Focus `loc` for typing, and make sure the keys will go there: once more if another field kept the focus,
/// then refuse rather than type into the wrong one
pub(super) fn focus_for_typing(loc: &Located, call: &str) -> Result<(), String> {
    focus_target(loc, call, "f")?;
    if other_field_focused(loc).is_none() {
        return Ok(());
    }
    // The click may have landed on a field's dropdown drawn over the target (a page's own popup, which
    // UI Automation doesn't see as covering it): give the target the focus directly instead
    if !loc.live.as_ref().is_some_and(|l| l.focus().is_ok() && until::holds(FOCUS_CLICK, || l.has_focus())) {
        focus_target(loc, call, "f")?;
    }
    match other_field_focused(loc) {
        None => Ok(()),
        Some(other) => Err(format!("Nothing typed: {} did not take the keyboard focus, which stayed on {other}", loc.desc)),
    }
}

/// What the focused field holds after typing `typed`: (what has the focus, a note for the reply, the
/// field's content when it was seen to hold the text, whether the text was seen to arrive: Some(false) only
/// when it plainly can't have — the focus is on something that takes no text). A program takes keys in a moment after they are
/// sent, so a field that does not show the text yet is read again for a short while before it is called wrong.
fn readback(typed: &str) -> (Option<String>, String, Option<String>, Option<bool>) {
    let flat = |s: &str| uia::newlines(s);
    let want = flat(typed);
    let want = want.trim_end();
    let started = std::time::Instant::now();
    let mut last = String::new();
    let mut changed = started;
    loop {
        let Some(fo) = uia::focused() else { return (None, String::new(), None, None) };
        let label = fo.label.clone();
        if matches!(fo.role, "Button" | "CheckBox" | "RadioButton" | "Link" | "MenuItem" | "TabItem" | "SplitButton") {
            let note = format!(". Careful: the keyboard focus is on {label}, which is not a text field, so the text may have gone nowhere");
            return (Some(label), note, None, Some(false));
        }
        if fo.password || typed.contains('\n') || typed.is_empty() {
            return (Some(label), String::new(), None, None);
        }
        let Some(v) = fo.value else { return (Some(label), String::new(), None, None) };
        let v = flat(&v);
        if v.contains(want) {
            let note = format!("; it now reads \"{}\"", uia::clip(&v, 100));
            return (Some(label), note, Some(v), Some(true));
        }
        if v != last {
            last = v.clone();
            changed = std::time::Instant::now();
        }
        // Keys still arriving: the field keeps changing (a browser's address bar can take a second for
        // thirty keystrokes, completing as it goes), so wait for it to settle before calling it wrong
        let arriving = changed.elapsed() < Duration::from_millis(200) && started.elapsed() < READBACK_MAX;
        if started.elapsed() < READBACK_GRACE || arriving {
            std::thread::sleep(Duration::from_millis(30));
            continue;
        }
        let note = if v.trim().is_empty() {
            ". Careful: the field is still empty — the keys went elsewhere, or the field does not report its content".into()
        } else {
            format!(". Careful: it now reads \"{}\", which is not what was typed (an input method, autocomplete or formatting may have changed it)", uia::clip(&v, 100))
        };
        return (Some(label), note, None, None);
    }
}

pub(super) async fn type_text(ctx: &Ctx, args: &Value) -> Output {
    let t = str_arg(args, "text").to_string();
    let replace = bool_arg(args, "replace") == Some(true);
    let enter = bool_arg(args, "enter") == Some(true);
    if t.is_empty() && !replace && !enter {
        return fail("text is empty");
    }
    let method = str_arg(args, "method").to_string();
    if !matches!(method.as_str(), "" | "auto" | "keys" | "paste") {
        return fail(format!("method: \"{method}\" is not one of auto, keys, paste"));
    }
    let ask = match Ask::of(ctx, args) {
        Ok(a) => a.map(Ask::editing),
        Err(e) => return fail(e),
    };
    // Without a target, window only says which window the keys are for
    let window = match (&ask, winmgr::find_arg(args.get("window"))) {
        (Some(_), _) => None,
        (None, Ok(w)) => w.map(|h| h.0 as isize),
        (None, Err(e)) => return fail(e),
    };
    let (pace, call) = (Pace::current(), fx::call_id());
    let turn = match acting().await {
        Ok(t) => t,
        Err(o) => return o,
    };
    let r = tokio::task::spawn_blocking(move || -> Result<Did, Miss> {
        let mut at = None;
        let mut scene = None;
        if let Some(a) = &ask {
            let loc = locate::resolve(a)?;
            if t.is_empty() { focus_target(&loc, &call, "f")?; } else { focus_for_typing(&loc, &call)?; }
            at = Some(loc.point);
            scene = loc.scene;
        } else if let Some(h) = window {
            winmgr::bring_to_front(winmgr::hwnd(h))?;
        }
        let fg = winmgr::real_foreground();
        let before = pace.before(at);
        // Keys go to the program in the order they are sent: none of them needs a pause before the next
        if replace {
            input::press_keys("ctrl+a")?;
            if t.is_empty() {
                input::press_keys("delete")?;
            }
        }
        // What the field holds before a paste, to tell afterwards that the paste really arrived
        let prior = if !t.is_empty() && !pace.fast && pastes(&t, &method) { uia::focused().and_then(|f| f.value).map(|v| v.replace('\r', "")) } else { None };
        let how = if t.is_empty() { "" } else { enter_text(&t, &method)? };
        // Read the field back before Enter sends it away. Fast mode too: this waits for the text itself, not for
        // the screen, so it costs nothing when the text is there
        let (into, note, landed, verified) = if t.is_empty() { (None, String::new(), None, None) } else { readback(&t) };
        // Asked to type into a field, and the keys went to something that takes no text
        let verified = verified.filter(|v| *v || ask.is_some());
        // The field holds the text and did not before: the program has read the clipboard, so it can go back now.
        // (A field that already read the same proves nothing: the paste may still be on its way.)
        if let (Some(now), Some(was)) = (&landed, &prior)
            && now != was
        {
            clipboard_release();
        }
        if enter {
            input::press_keys("enter")?;
        }
        // The text was seen in the field, and nothing else was sent: that was the point, so the screen needn't be
        // watched until it stops moving (Enter can set off anything, so then it is)
        let change = if verified == Some(true) && !enter && pace.watch.is_none() { None } else { pace.after(before) };
        let n = t.chars().count();
        let what = match (n, replace) {
            (0, true) => "Cleared".to_string(),
            (0, false) => "Pressed Enter in".to_string(),
            (_, true) => format!("Replaced the content with {n} characters in"),
            (_, false) => format!("Typed {n} characters into"),
        };
        let place = match into {
            Some(label) => label,
            None => format!("\"{}\"", uia::clip(&winmgr::title_of(fg), 50)),
        };
        let sent = if enter && n > 0 { ", then pressed Enter" } else { "" };
        Ok(Did { done: format!("{what} {place}{how}{note}{sent}"), change, hwnd: scene.or(Some(fg).filter(|h| *h != 0)), fresh: false, delivered: false, verified })
    })
    .await;
    drop(turn);
    match r {
        Ok(Ok(did)) => report::conclude(ctx, args, did, true).await,
        Ok(Err(m)) => report::missed(ctx, m).await,
        Err(e) => fail(e.to_string()),
    }
}

pub(super) async fn press_keys(ctx: &Ctx, args: &Value) -> Output {
    let keys = str_arg(args, "keys").trim().to_string();
    if keys.is_empty() {
        return fail("No keys given");
    }
    let repeat = int_arg(args, "repeat", 1, 1, 100) as usize;
    let hold_ms = args.get("holdSec").and_then(Value::as_f64).map(|s| (s.clamp(0.05, 10.0) * 1000.0) as u64);
    if hold_ms.is_some() && keys.split_whitespace().count() > 1 {
        return fail("holdSec holds one key or combination, not a sequence");
    }
    let window = match winmgr::find_arg(args.get("window")) {
        Ok(w) => w.map(|h| h.0 as isize),
        Err(e) => return fail(e),
    };
    let pace = Pace::current();
    let turn = match acting().await {
        Ok(t) => t,
        Err(o) => return o,
    };
    let r = tokio::task::spawn_blocking(move || -> Result<Did, String> {
        if let Some(h) = window {
            winmgr::bring_to_front(winmgr::hwnd(h))?;
        }
        let fg = winmgr::real_foreground();
        let title = winmgr::title_of(fg);
        let before = pace.before(None);
        match hold_ms {
            Some(ms) => input::hold_keys(&keys, ms)?,
            None => {
                for i in 0..repeat {
                    if i > 0 {
                        std::thread::sleep(Duration::from_millis(30));
                    }
                    input::press_sequence(&keys)?;
                }
            }
        }
        let change = pace.after(before);
        let times = match (hold_ms, repeat) {
            (Some(ms), _) => format!(" (held {:.1} s)", ms as f64 / 1000.0),
            (None, 1) => String::new(),
            (None, n) => format!(" ×{n}"),
        };
        let place = if title.trim().is_empty() { String::new() } else { format!(" in \"{}\"", uia::clip(&title, 50)) };
        Ok(Did { done: format!("Pressed {keys}{times}{place}"), change, hwnd: Some(fg).filter(|h| *h != 0), fresh: false, delivered: false, verified: None })
    })
    .await;
    drop(turn);
    match r {
        Ok(Ok(did)) => report::conclude(ctx, args, did, true).await,
        Ok(Err(e)) => fail(e),
        Err(e) => fail(e.to_string()),
    }
}
