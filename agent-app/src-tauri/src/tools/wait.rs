//! wait_for: return the moment something happens on screen, instead of waiting a guessed time

use super::*;

/// Alternatives separated by |, e.g. "Save|保存": any of them counts
fn alternatives(s: &str) -> Vec<String> {
    s.split('|').map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect()
}

/// Poll until a window, a control or some text shows up (or goes away), or the screen settles.
/// Returns as soon as it happens, so the AI never has to guess how long to wait.
pub(super) async fn wait_for(ctx: &Ctx, args: &Value) -> Output {
    let window = alternatives(str_arg(args, "window"));
    let element = alternatives(str_arg(args, "element"));
    let text_q = alternatives(str_arg(args, "text"));
    let stable = args.get("stable").and_then(Value::as_bool).unwrap_or(false);
    let gone = args.get("gone").and_then(Value::as_bool).unwrap_or(false);
    let given = [!window.is_empty(), !element.is_empty(), !text_q.is_empty(), stable].iter().filter(|b| **b).count();
    if given != 1 {
        return fail("Give exactly one of window, element, text or stable");
    }
    let timeout = Duration::from_secs_f64(args.get("timeoutSec").and_then(Value::as_f64).unwrap_or(10.0).clamp(0.5, 120.0));
    let g = *ctx.geom.lock().unwrap();
    let handle = args.get("handle").and_then(Value::as_i64).map(|h| h as isize);
    let title = str_arg(args, "title").to_string();
    // A region or display that can't be read is an error now; a window may still be on its way, so it is
    // looked up again on every poll
    if !text_q.is_empty() && handle.is_none() && title.is_empty()
        && let Err(e) = text_tools::ocr_area(g, args) {
            return fail(e);
        }
    let args = args.clone();
    let r = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let start = std::time::Instant::now();
        if stable {
            let at = winmgr::find(handle, &title).ok().and_then(|h| winmgr::rect_of(h.0 as isize)).map(|(l, t, r, b)| ((l + r) / 2, (t + b) / 2));
            // Slow to write a part off as moving on its own: a spinner means the page is still loading
            let ok = screen::wait_stable(screen::watch_area(at), Duration::from_millis(400), timeout, Duration::from_millis(1500));
            let s = start.elapsed().as_secs_f64();
            return if ok { Ok(format!("Screen settled after {s:.1} s")) } else { Err(format!("The screen was still changing after {s:.1} s")) };
        }
        let what = if !window.is_empty() {
            format!("window \"{}\"", window.join("|"))
        } else if !element.is_empty() {
            format!("control \"{}\"", element.join("|"))
        } else {
            format!("text \"{}\"", text_q.join("|"))
        };
        let mut last_err = String::new();
        loop {
            // Some(details) when the thing is there
            let found: Option<String> = if !window.is_empty() {
                let ws = winmgr::list();
                let hit = ws.iter().find(|w| {
                    let (t, p) = (w.title.to_lowercase(), w.process.to_lowercase());
                    window.iter().any(|q| t.contains(&q.to_lowercase()) || p.contains(&q.to_lowercase()))
                });
                hit.map(|w| {
                    // A window exists before it has drawn anything: let it finish, so the next step can read it
                    if !gone {
                        let (l, t, r, b) = w.rect;
                        if r - l > 16 && b - t > 16 && !w.minimized {
                            let area = capture::Area { left: l, top: t, width: r - l, height: b - t };
                            screen::wait_stable(area, Duration::from_millis(120), Duration::from_millis(1500), screen::NOISE_QUICK);
                        }
                    }
                    win_line(g, w)
                })
            } else if !element.is_empty() {
                let hwnd = if handle.is_some() || !title.trim().is_empty() { winmgr::find(handle, &title).ok() } else { Some(uia::foreground()) };
                let mut out = None;
                if let Some(h) = hwnd {
                    for q in &element {
                        let to_shot = |x, y| g.and_then(|g: Geom| g.screen_to_shot(x, y));
                        match uia::list_counted(h, q, 50, false, &to_shot) {
                            Ok((listing, n)) if n > 0 => {
                                // The whole window, so every control's id is ready for element_action
                                out = Some(uia::list(h, "", 300, false, &to_shot).unwrap_or(listing));
                                break;
                            }
                            Ok(_) => {}
                            Err(e) => last_err = e,
                        }
                    }
                }
                out
            } else {
                match text_tools::ocr_area(g, &args).ok().map(ocr::read) {
                    Some(Ok(lines)) => text_q.iter().find_map(|q| {
                        let hits = ocr::find(&lines, q);
                        hits.first().map(|h| {
                            let (a, b, c, d) = h.rect;
                            format!("\"{q}\" {} in \"{}\"", at_text(g, (a + c) / 2, (b + d) / 2), h.line)
                        })
                    }),
                    Some(Err(e)) => return Err(e),
                    None => None,
                }
            };
            let s = start.elapsed().as_secs_f64();
            match (found, gone) {
                (Some(d), false) => return Ok(format!("{what} appeared after {s:.1} s:\n{d}")),
                (None, true) => return Ok(format!("{what} is gone after {s:.1} s")),
                _ => {}
            }
            if start.elapsed() >= timeout {
                let fg = winmgr::list().into_iter().find(|w| w.foreground).map(|w| format!(" The window in front is \"{}\" ({}).", w.title, w.process)).unwrap_or_default();
                let note = if last_err.is_empty() { String::new() } else { format!(" Last error: {last_err}.") };
                return Err(format!("Timed out after {s:.1} s: {what} {}.{fg}{note}", if gone { "is still there" } else { "did not appear" }));
            }
            std::thread::sleep(Duration::from_millis(if text_q.is_empty() { 120 } else { 250 }));
        }
    })
    .await;
    match r {
        Ok(Ok(s)) => text(s),
        Ok(Err(e)) => fail(e),
        Err(e) => fail(e.to_string()),
    }
}
