//! wait_for: return the moment something happens on screen, instead of waiting a guessed time

use super::report::Did;
use super::*;
use crate::ocr;

/// Alternatives separated by |, e.g. "Save|保存": any of them counts
fn alternatives(s: &str) -> Vec<String> {
    s.split('|').map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect()
}

/// Can `q` be read off the pixels inside `rect`? None when the pixels can't be read at all (no OCR), so the
/// caller falls back to trusting the control.
fn on_pixels(rect: uia::Rect, q: &str) -> Option<bool> {
    let v = capture::virtual_screen();
    // A little margin: the control's box can sit tight on the glyphs, which the engine then misses
    let (l, t, r, b) = ((rect.0 - 4).max(v.left), (rect.1 - 4).max(v.top), (rect.2 + 4).min(v.left + v.width), (rect.3 + 4).min(v.top + v.height));
    if r - l < 4 || b - t < 4 {
        return Some(false);
    }
    let lines = ocr::read(capture::Area { left: l, top: t, width: r - l, height: b - t }).ok()?;
    if !ocr::find(&lines, q).is_empty() {
        return Some(true);
    }
    // Text that wraps comes back as several lines: look across the breaks too
    let norm = |s: &str| s.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect::<String>();
    Some(norm(&lines.iter().map(|l| l.text()).collect::<String>()).contains(&norm(q)))
}

/// Is any of `texts` showing in `hwnd`? Among its controls first; failing that, on its pixels.
/// Some((what was found, among the controls?)) when it is.
fn showing(hwnd: isize, texts: &[String], f: &Frame, use_ocr: bool, problem: &mut String) -> Option<(String, bool)> {
    let at = |x: i32, y: i32| f.at(x, y);
    let mut thin = true;
    match uia::snapshot(hwnd, false) {
        Ok(scene) => {
            thin = scene.thin();
            for q in texts {
                // Web pages keep text they hide with CSS (transparent, covered, a loader that has finished) in the
                // tree as an ordinary on-screen control. Plain text shows what it says, so it only counts when the
                // pixels there say it too; buttons and links may show just an icon, so they are taken on trust.
                let hit = locate::matches(&scene, q, "").into_iter().find(|n| n.role != "Text" || on_pixels(n.rect, q) != Some(false));
                if let Some(n) = hit {
                    return Some((n.line(&at), true));
                }
            }
        }
        Err(e) => *problem = e,
    }
    // Reading pixels costs a few hundred milliseconds: by default only where the controls say little
    if !use_ocr && !thin {
        return None;
    }
    let (l, t, r, b) = winmgr::rect_of(hwnd)?;
    let v = capture::virtual_screen();
    let (l, t, r, b) = (l.max(v.left), t.max(v.top), r.min(v.left + v.width), b.min(v.top + v.height));
    if r - l < 16 || b - t < 16 {
        return None;
    }
    match ocr::read(capture::Area { left: l, top: t, width: r - l, height: b - t }) {
        Ok(lines) => texts.iter().find_map(|q| {
            ocr::find(&lines, q).first().map(|h| {
                let (a, b, c, d) = h.rect;
                (format!("\"{q}\" {} in \"{}\" (read off the pixels)", f.at((a + c) / 2, (b + d) / 2), uia::clip(&h.line, 80)), false)
            })
        }),
        Err(e) => {
            *problem = e;
            None
        }
    }
}

/// Poll until a window or some text shows up (or goes away), or the screen settles.
/// Returns as soon as it happens, so the AI never has to guess how long to wait.
pub(super) async fn wait_for(ctx: &Ctx, args: &Value) -> Output {
    let window = alternatives(str_arg(args, "window"));
    let texts = alternatives(str_arg(args, "text"));
    let stable = bool_arg(args, "stable").unwrap_or(false);
    let seconds = args.get("seconds").and_then(Value::as_f64);
    let gone = bool_arg(args, "gone").unwrap_or(false);
    let given = [!window.is_empty(), !texts.is_empty(), stable, seconds.is_some()].iter().filter(|b| **b).count();
    if given != 1 {
        return fail("Give exactly one of window, text, stable or seconds");
    }
    if let Some(s) = seconds {
        let s = s.clamp(0.0, 60.0);
        tokio::time::sleep(Duration::from_secs_f64(s)).await;
        return report::conclude(ctx, args, Did::new(format!("Waited {s} s")), false).await;
    }
    let timeout = Duration::from_secs_f64(args.get("timeoutSec").and_then(Value::as_f64).unwrap_or(10.0).clamp(0.2, 300.0));
    let within = match winmgr::find_arg(args.get("within")) {
        Ok(w) => w.map(|h| h.0 as isize),
        // The window to look in may itself still be on its way: look it up again on every poll
        Err(_) if !texts.is_empty() || stable => None,
        Err(e) => return fail(e),
    };
    let within_arg = args.get("within").cloned();
    let ocr_arg = bool_arg(args, "ocr");
    let f = ctx.frame();
    let r = tokio::task::spawn_blocking(move || -> Result<Did, String> {
        let start = std::time::Instant::now();
        let scope = || within.filter(|h| winmgr::exists(*h)).or_else(|| winmgr::find_arg(within_arg.as_ref()).ok().flatten().map(|h| h.0 as isize));
        if stable {
            let at = scope().and_then(winmgr::rect_of).map(|(l, t, r, b)| ((l + r) / 2, (t + b) / 2));
            // Slow to write a part off as moving on its own: a spinner means the page is still loading
            let ok = screen::wait_stable(screen::watch_area(at), Duration::from_millis(400), timeout, Duration::from_millis(1500));
            let s = start.elapsed().as_secs_f64();
            return if ok { Ok(Did { done: format!("Screen settled after {s:.1} s"), change: None, hwnd: scope(), fresh: false, delivered: false }) } else { Err(format!("The screen was still changing after {s:.1} s")) };
        }
        let what = if !window.is_empty() { format!("A window matching \"{}\"", window.join("|")) } else { format!("\"{}\"", texts.join("|")) };
        let mut problem = String::new();
        let mut polls = 0u32;
        loop {
            polls += 1;
            // Some((details, the window it is in, whether to list that window's controls))
            let found: Option<(String, Option<isize>, bool)> = if !window.is_empty() {
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
                    (look::win_line(&f, w), Some(w.handle), true)
                })
            } else {
                let hwnd = scope().unwrap_or_else(winmgr::foreground);
                // Pixels are read every other poll when not asked for outright: controls answer faster
                let use_ocr = ocr_arg.unwrap_or(polls % 2 == 0) && ocr_arg != Some(false);
                if hwnd == 0 { None } else { showing(hwnd, &texts, &f, use_ocr, &mut problem).map(|(d, in_controls)| (d, Some(hwnd), in_controls)) }
            };
            let s = start.elapsed().as_secs_f64();
            match (found, gone) {
                (Some((d, hwnd, list)), false) => return Ok(Did { done: format!("{what} appeared after {s:.1} s: {d}"), change: None, hwnd, fresh: list, delivered: false }),
                (None, true) => return Ok(Did { done: format!("{what} is gone after {s:.1} s"), change: None, hwnd: scope(), fresh: false, delivered: false }),
                _ => {}
            }
            if start.elapsed() >= timeout {
                let fg = winmgr::info(winmgr::foreground()).map(|w| format!(" In front: \"{}\" ({}).", uia::clip(&w.title, 60), w.process)).unwrap_or_default();
                let note = if problem.is_empty() { String::new() } else { format!(" ({problem})") };
                return Err(format!("Timed out after {s:.1} s: {what} {}.{fg}{note}", if gone { "is still there" } else { "did not appear" }));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })
    .await;
    match r {
        Ok(Ok(did)) => report::conclude(ctx, args, did, false).await,
        Ok(Err(e)) => fail(e),
        Err(e) => fail(e.to_string()),
    }
}
