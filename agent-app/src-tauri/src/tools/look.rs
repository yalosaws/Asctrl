//! Looking at the screen: a window's controls as text, with the other windows (observe), or where something is
//! (observe with query); pictures, whole or a region again at full resolution (screenshot); text (read_text).

use super::*;
use crate::ocr;
use crate::uia::Scene;

/// Tell the AI about the other displays when there are several
fn describe_displays(ds: &[capture::Display]) -> String {
    if ds.len() <= 1 {
        return String::new();
    }
    let list = ds.iter().enumerate().map(|(i, d)| format!("#{} {}x{}{}", i + 1, d.area.width, d.area.height, if d.primary { " (primary)" } else { "" })).collect::<Vec<_>>().join(", ");
    format!(" {} displays: {list}; pass display to capture one, for a sharper image.", ds.len())
}

fn clip_to_screen(r: winmgr::Rect) -> Option<capture::Area> {
    let v = capture::virtual_screen();
    let (l, t, rr, b) = (r.0.max(v.left), r.1.max(v.top), r.2.min(v.left + v.width), r.3.min(v.top + v.height));
    (rr - l >= 8 && b - t >= 8).then_some(capture::Area { left: l, top: t, width: rr - l, height: b - t })
}

/// What a view captures right now: the area, and how to call it. A window view falls back to the whole
/// screen while its window is not the one being worked in (closed, minimized, or behind another program).
fn view_area(view: View) -> (capture::Area, String) {
    match view.scope {
        Scope::Window(h) => {
            let fg = winmgr::foreground();
            let in_front = fg == h || (fg != 0 && winmgr::pid_of(fg) == winmgr::pid_of(h));
            if let Some(area) = winmgr::rect_of(h).filter(|_| in_front).and_then(clip_to_screen) {
                return (area, format!("of \"{}\"", uia::clip(&winmgr::title_of(h), 50)));
            }
        }
        Scope::Display(n) => {
            if let Some(d) = capture::displays().get(n - 1) {
                return (d.area, format!("of display #{n}"));
            }
        }
        Scope::Screen => {}
    }
    (capture::virtual_screen(), "of the whole screen".into())
}

/// Take a screenshot in the current view. It becomes the picture coordinates refer to, and what the AI has seen.
pub(super) async fn capture_view(ctx: &Ctx) -> Result<(Image, String), String> {
    let view = ctx.view();
    let (area, what, layout) = tokio::task::spawn_blocking(move || {
        let (area, what) = view_area(view);
        (area, what, winmgr::layout())
    })
    .await
    .map_err(|e| e.to_string())?;
    let (shot, image) = shoot(area, view.max_width).await?;
    let frame = Frame { left: area.left, top: area.top, scale: shot.scale, width: shot.width, height: shot.height, own: None };
    // Positions the AI was given so far are on the picture before this one (or the one a plain screenshot
    // would have been): when this one is framed differently, they point somewhere else now
    let reframed = if ctx.frame() != frame { " It is framed differently from before: @(x,y) positions in earlier replies no longer apply." } else { "" };
    *ctx.frame.lock().unwrap() = Some(frame);
    ctx.saw(layout);
    Ok((image, format!("Screenshot {what}, {}x{}; x and y refer to this image.{reframed}", shot.width, shot.height)))
}

pub(super) async fn screenshot(ctx: &Ctx, args: &Value) -> Output {
    // A region of the latest screenshot: that part again at full resolution
    if args.get("x0").is_some_and(|v| !v.is_null()) {
        return zoom(ctx, args).await;
    }
    let window = match winmgr::find_arg(args.get("window")) {
        Ok(w) => w.map(|h| h.0 as isize),
        Err(e) => return fail(e),
    };
    let display = int_arg(args, "display", 0, 0, 16) as usize;
    let displays = capture::displays();
    if display > displays.len() {
        return fail(format!("There is no display #{display} ({} connected)", displays.len()));
    }
    let scope = match (window, display) {
        (Some(h), _) => Scope::Window(h),
        (None, 0) => Scope::Screen,
        (None, n) => Scope::Display(n),
    };
    let max_width = match args.get("maxWidth") {
        Some(_) => int_arg(args, "maxWidth", 1600, 320, 3840) as i32,
        None => ctx.view().max_width,
    };
    *ctx.view.lock().unwrap() = View { scope, max_width };
    // A window that others cover is taken as it draws itself, where it is: nothing on the screen moves
    if let Some(h) = window
        && let Some(r) = own_picture(ctx, h, max_width).await
    {
        return match r {
            Ok((image, note)) => Output { ok: true, image: Some(image), text: Some(note), ..Default::default() },
            Err(e) => fail(e),
        };
    }
    // Otherwise a picture of a window shows that window: bring it forward when something covers it
    if let Some(h) = window
        && winmgr::foreground() != h
    {
        let r = tokio::task::spawn_blocking(move || {
            winmgr::bring_to_front(winmgr::hwnd(h))?;
            if let Some((l, t, r, b)) = winmgr::rect_of(h) {
                screen::wait_stable(capture::Area { left: l, top: t, width: r - l, height: b - t }, Duration::from_millis(100), Duration::from_millis(600), screen::NOISE_QUICK);
            }
            Ok::<_, String>(())
        })
        .await;
        if let Ok(Err(e)) = r {
            return fail(e);
        }
    }
    match capture_view(ctx).await {
        Ok((image, note)) => {
            let others = if scope == Scope::Screen { describe_displays(&displays) } else { String::new() };
            Output { ok: true, image: Some(image), text: Some(format!("{note}{others}")), ..Default::default() }
        }
        Err(e) => fail(e),
    }
}

/// A picture of window `h` as it draws itself (Windows Graphics Capture), when other programs' windows cover
/// part of it. None when nothing covers it, or it is minimized, or it can't be taken that way: then it is
/// brought forward and taken off the screen as usual.
async fn own_picture(ctx: &Ctx, h: isize, max_width: i32) -> Option<Result<(Image, String), String>> {
    let got = tokio::task::spawn_blocking(move || {
        if winmgr::is_minimized(h) {
            return None;
        }
        let (l, t, r, b) = winmgr::rect_of(h)?;
        if !winmgr::covered(h, capture::Area { left: l, top: t, width: r - l, height: b - t }) {
            return None;
        }
        let p = crate::wgc::capture(h).ok()?;
        let (scale, w, hh) = capture::scaled(p.area, max_width);
        let px = p.shrink(w, hh);
        Some(capture::jpeg(&px, w, hh).map(|jpeg| (p.area, scale, w, hh, jpeg, winmgr::layout())))
    })
    .await
    .ok()??;
    Some(got.map(|(area, scale, w, hh, jpeg, layout)| {
        let frame = Frame { left: area.left, top: area.top, scale, width: w, height: hh, own: Some(h) };
        let reframed = if ctx.frame() != frame { " It is framed differently from before: @(x,y) positions in earlier replies no longer apply." } else { "" };
        *ctx.frame.lock().unwrap() = Some(frame);
        ctx.saw(layout);
        let image = Image { data: base64::engine::general_purpose::STANDARD.encode(&jpeg), mime_type: "image/jpeg".into() };
        let note = format!(
            "Screenshot of \"{}\", {w}x{hh}; x and y refer to this image. Other windows cover it, so this is the window as it draws itself, left where it is; a click on it brings it forward first.{reframed}",
            uia::clip(&winmgr::title_of(h), 50)
        );
        (image, note)
    }))
}

async fn zoom(ctx: &Ctx, args: &Value) -> Output {
    let g = ctx.frame();
    let area = match g.area(args, 4) {
        Ok(a) => a,
        Err(e) => return fail(e),
    };
    let maxw = int_arg(args, "maxWidth", 1600, 320, 3840) as i32;
    match shoot(area, maxw).await {
        Ok((shot, image)) => Output {
            ok: true,
            image: Some(image),
            text: Some(format!(
                "Zoomed image {}x{} ({:.1}x the last screenshot). It is for reading only: x and y still refer to the last screenshot.",
                shot.width,
                shot.height,
                shot.scale / g.scale
            )),
            ..Default::default()
        },
        Err(e) => fail(e),
    }
}

fn win_flags(w: &winmgr::Win) -> String {
    let mut flags = vec![];
    if w.foreground {
        flags.push("in front".to_string());
    }
    if w.minimized {
        flags.push("minimized".into());
    }
    if w.maximized {
        flags.push("maximized".into());
    }
    if let Some(o) = w.owner {
        let t = winmgr::title_of(o);
        flags.push(if t.is_empty() { "dialog".into() } else { format!("dialog of \"{}\"", uia::clip(&t, 30)) });
    }
    if flags.is_empty() { String::new() } else { format!(" [{}]", flags.join(", ")) }
}

pub(super) fn win_line(f: &Frame, w: &winmgr::Win) -> String {
    let pos = if w.minimized { String::new() } else { format!(" {}", f.rect(w.rect)) };
    format!("[{}] {} \"{}\"{}{pos}", w.handle, w.process, uia::clip(&w.title, 80), win_flags(w))
}

/// Text read off a window's pixels that its controls don't already say
fn pixel_text(scene: &Scene, f: &Frame, all: bool) -> Result<Vec<String>, String> {
    let Some(area) = winmgr::rect_of(scene.hwnd).and_then(clip_to_screen) else { return Ok(vec![]) };
    let lines = ocr::read_in(scene.hwnd, area)?;
    let said: Vec<String> = if all { vec![] } else { scene.nodes.iter().flat_map(|n| [uia::norm(&n.name), uia::norm(&n.value)]).filter(|s| !s.is_empty()).collect() };
    Ok(lines
        .iter()
        .filter_map(|l| {
            let t = l.text();
            let n = uia::norm(&t);
            if n.chars().count() < 2 || said.iter().any(|s| s.contains(&n)) {
                return None;
            }
            let (a, b, c, d) = l.rect();
            Some(format!("\"{}\" {}", uia::clip(&t, 120), f.at((a + c) / 2, (b + d) / 2)))
        })
        .collect())
}

/// Read a window's controls as text, with references every action accepts. The one look that makes the rest
/// cheap: afterwards, each action's reply says what changed.
pub(super) async fn observe(ctx: &Ctx, args: &Value) -> Output {
    // Looking for something: the controls that match it, best first, and where it shows on the pixels
    if !str_arg(args, "query").trim().is_empty() {
        let mut a = args.clone();
        a["text"] = a["query"].clone();
        return find(ctx, &a).await;
    }
    let window = match winmgr::find_arg(args.get("window")) {
        Ok(w) => w.map(|h| h.0 as isize),
        Err(e) => return fail(e),
    };
    let max = int_arg(args, "max", 150, 1, 2000) as usize;
    let offscreen = bool_arg(args, "offscreen").unwrap_or(false);
    let ocr_mode = bool_arg(args, "ocr");
    let want_image = bool_arg(args, "image") == Some(true);

    // With a picture: take it first, so every @(x,y) below is a point on it
    let mut image = None;
    let mut shot_note = String::new();
    if want_image {
        if let Some(h) = window {
            let mw = ctx.view().max_width;
            *ctx.view.lock().unwrap() = View { scope: Scope::Window(h), max_width: mw };
            let _ = tokio::task::spawn_blocking(move || winmgr::bring_to_front(winmgr::hwnd(h))).await;
        }
        match capture_view(ctx).await {
            Ok((img, note)) => {
                image = Some(img);
                shot_note = note;
            }
            Err(e) => shot_note = format!("(no screenshot: {e})"),
        }
    }
    let f = ctx.frame();
    let has_shot = image.is_some();
    let r = tokio::task::spawn_blocking(move || -> Result<(String, Vec<winmgr::Slot>), String> {
        let fg = winmgr::foreground();
        let hwnd = window.unwrap_or(fg);
        let at = |x: i32, y: i32| f.at(x, y);
        let mut out = String::new();
        // Where things stand
        let wins = winmgr::list();
        match winmgr::info(fg).filter(|_| fg != 0) {
            Some(w) => out += &format!("In front: {}.", win_line(&f, &w)),
            None => out += "No window is in front.",
        }
        if let Ok((x, y)) = input::cursor_pos() {
            out += &format!(" Pointer {}.", f.at(x, y));
        }
        // Every other window, front to back, with the handle to pass as window
        let others: Vec<String> = wins.iter().filter(|w| w.handle != fg && w.handle != hwnd).map(|w| win_line(&f, w)).collect();
        if !others.is_empty() {
            out += &format!("\nAlso open, front to back ([handle] program \"title\"; pass the handle or part of the title as window):\n{}", others.join("\n"));
        }
        if hwnd == 0 {
            return Ok((out + "\nNothing to read: say which window.", winmgr::layout()));
        }
        let scene = uia::snapshot(hwnd, offscreen)?;
        let title = uia::clip(&scene.title, 80);
        if winmgr::is_minimized(hwnd) {
            out += &format!("\n\"{title}\" is minimized: focus_window restores it.");
            return Ok((out, winmgr::layout()));
        }
        let behind = if hwnd != fg { " (not in front)" } else { "" };
        let popups = if scene.popups > 0 { format!(", including {} open popup(s)", scene.popups) } else { String::new() };
        out += &format!(
            "\nControls of \"{title}\"{behind} — {}{popups}. Pass a reference such as \"e12\" as target; @(x,y) is the centre{}.\n",
            scene.nodes.len(),
            if has_shot { " on the screenshot" } else { "" }
        );
        out += &uia::render(&scene, &uia::Render { max, at: &at });
        // Programs that draw their own UI say little through their controls: read the pixels too
        if ocr_mode == Some(true) || (ocr_mode.is_none() && scene.thin()) {
            match pixel_text(&scene, &f, scene.thin()) {
                Ok(lines) if !lines.is_empty() => {
                    let more = if lines.len() > 150 { format!("\n… {} more lines (read_text reads a region)", lines.len() - 150) } else { String::new() };
                    out += &format!("\nText read off the pixels (click it by passing the text as target, or by x and y):\n{}{more}", lines.iter().take(150).cloned().collect::<Vec<_>>().join("\n"));
                }
                Ok(_) => {}
                Err(e) => out += &format!("\n(could not read text off the pixels: {e})"),
            }
        }
        uia::remember(&scene);
        Ok((out, winmgr::layout()))
    })
    .await;
    match r {
        Ok(Ok((mut s, layout))) => {
            ctx.saw(layout);
            if !shot_note.is_empty() {
                s = format!("{s}\n{shot_note}");
            }
            let mut o = text(s);
            o.image = image;
            // The windows, front to back, for a program reading the reply
            let wins = tokio::task::spawn_blocking(winmgr::list).await.unwrap_or_default();
            let list: Vec<Value> = wins.iter().map(|w| serde_json::json!({ "handle": w.handle, "program": w.process, "title": w.title, "front": w.foreground })).collect();
            o.data = Some(serde_json::json!({ "windows": list }));
            o
        }
        Ok(Err(e)) => {
            let mut o = fail(e);
            o.image = image;
            o
        }
        Err(e) => fail(e.to_string()),
    }
}

/// Where something is: every control whose name or value contains the text, best first, and — in programs
/// that draw their own UI — every place the text is on the pixels
async fn find(ctx: &Ctx, args: &Value) -> Output {
    let q = str_arg(args, "text").trim().to_string();
    if q.is_empty() {
        return fail("text is empty");
    }
    let window = match winmgr::find_arg(args.get("window")) {
        Ok(w) => w.map(|h| h.0 as isize),
        Err(e) => return fail(e),
    };
    let role = str_arg(args, "role").trim().to_lowercase();
    let max = int_arg(args, "max", 20, 1, 200) as usize;
    let force_ocr = bool_arg(args, "ocr");
    let f = ctx.frame();
    let r = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let hwnd = window.unwrap_or_else(winmgr::foreground);
        if hwnd == 0 {
            return Err("No window is in front; say which window".into());
        }
        let at = |x: i32, y: i32| f.at(x, y);
        let scene = uia::snapshot(hwnd, false)?;
        let title = uia::clip(&scene.title, 60);
        let hits = locate::matches(&scene, &q, &role);
        let mut out = vec![];
        if !hits.is_empty() {
            let more = if hits.len() > max { format!("\n… {} more", hits.len() - max) } else { String::new() };
            out.push(format!("{} control(s) in \"{title}\" match \"{q}\", best first:\n{}{more}", hits.len(), hits.iter().take(max).map(|n| n.line(&at)).collect::<Vec<_>>().join("\n")));
        }
        if role.is_empty() && (force_ocr == Some(true) || (force_ocr.is_none() && (hits.is_empty() || scene.thin())))
            && let Some(area) = winmgr::rect_of(hwnd).and_then(clip_to_screen)
        {
            let lines = ocr::read_in(hwnd, area)?;
            let found = ocr::find(&lines, &q);
            if !found.is_empty() {
                let shown: Vec<String> = found
                    .iter()
                    .take(max)
                    .enumerate()
                    .map(|(i, h)| {
                        let (a, b, c, d) = h.rect;
                        format!("{}. {} in \"{}\"{}", i + 1, f.at((a + c) / 2, (b + d) / 2), uia::clip(&h.line, 80), if h.exact { " (the whole line)" } else { "" })
                    })
                    .collect();
                out.push(format!("On the pixels of \"{title}\", \"{q}\" appears {} time(s) (nth counts these when you pass the text as target):\n{}", found.len(), shown.join("\n")));
            }
        }
        if uia::known_cost(hwnd).is_none() {
            uia::remember(&scene);
        }
        if out.is_empty() {
            return Err(format!("Nothing in \"{title}\" shows \"{q}\". It may be in another window (pass window), scrolled out of view, or not there yet (wait_for waits for it)."));
        }
        Ok(out.join("\n"))
    })
    .await;
    match r {
        Ok(Ok(s)) => text(s),
        Ok(Err(e)) => fail(e),
        Err(e) => fail(e.to_string()),
    }
}

/// The text a window's controls show, in reading order, without the bookkeeping
fn controls_text(scene: &Scene) -> String {
    let mut out: Vec<String> = vec![];
    for n in &scene.nodes {
        let line = match (n.name.is_empty(), n.value.is_empty() || n.value == n.name) {
            (true, true) => continue,
            (false, true) => n.name.clone(),
            (true, false) => n.value.clone(),
            (false, false) => format!("{}: {}", n.name, n.value),
        };
        if out.last() != Some(&line) {
            out.push(line);
        }
    }
    out.join("\n")
}

/// Read text: one control's full content, a window's, or a region's off the pixels
pub(super) async fn read_text(ctx: &Ctx, args: &Value) -> Output {
    let ask = match locate::Ask::parse(ctx, args, "target", "", "") {
        Ok(a) => a,
        Err(e) => return fail(e),
    };
    let f = ctx.frame();
    if let Some(ask) = ask {
        let r = tokio::task::spawn_blocking(move || -> Result<String, locate::Miss> {
            let loc = locate::resolve(&ask)?;
            Ok(match &loc.live {
                Some(live) => match live.text(200_000) {
                    t if t.trim().is_empty() => format!("{} shows no text", loc.desc),
                    t => t,
                },
                None => format!("{} (read off the pixels; pass a region to read around it)", loc.desc),
            })
        })
        .await;
        return match r {
            Ok(Ok(s)) => text(s),
            Ok(Err(m)) => report::missed(ctx, m).await,
            Err(e) => fail(e.to_string()),
        };
    }
    let region = if args.get("x0").is_some() {
        match f.area(args, 8) {
            Ok(a) => Some(a),
            Err(e) => return fail(e),
        }
    } else {
        None
    };
    let window = match winmgr::find_arg(args.get("window")) {
        Ok(w) => w.map(|h| h.0 as isize),
        Err(e) => return fail(e),
    };
    let display = int_arg(args, "display", 0, 0, 16) as usize;
    let source = str_arg(args, "source").to_string();
    blocking(move || {
        let ocr_lines = |area: capture::Area, what: &str| -> Result<String, String> {
            let lines = ocr::read(area)?;
            if lines.is_empty() {
                return Ok(format!("No text found {what}"));
            }
            let shown: Vec<String> = lines
                .iter()
                .take(500)
                .map(|l| {
                    let (a, b, c, d) = l.rect();
                    format!("\"{}\" {}", l.text(), f.at((a + c) / 2, (b + d) / 2))
                })
                .collect();
            let more = if lines.len() > 500 { format!("\n… {} more lines", lines.len() - 500) } else { String::new() };
            Ok(format!("Text read off the pixels {what}, top to bottom ({} lines; @ is each line's centre):\n{}{more}", lines.len(), shown.join("\n")))
        };
        if let Some(area) = region {
            return ocr_lines(area, "in that region");
        }
        if display > 0 && window.is_none() {
            let d = capture::displays();
            let area = d.get(display - 1).map(|d| d.area).ok_or(format!("There is no display #{display}"))?;
            return ocr_lines(area, &format!("on display #{display}"));
        }
        let hwnd = window.unwrap_or_else(winmgr::foreground);
        if hwnd == 0 {
            return Err("No window is in front; say which window, or pass a region".into());
        }
        let title = uia::clip(&winmgr::title_of(hwnd), 60);
        let area = winmgr::rect_of(hwnd).and_then(clip_to_screen);
        if source != "ocr" {
            match uia::snapshot(hwnd, false) {
                Ok(scene) if !scene.thin() || source == "controls" => {
                    let t = controls_text(&scene);
                    return Ok(if t.is_empty() { format!("\"{title}\" shows no text through its controls; try source: \"ocr\"") } else { format!("Text of \"{title}\" (from its controls, in reading order):\n{t}") });
                }
                Err(e) if source == "controls" => return Err(e),
                _ => {}
            }
        }
        match area {
            Some(a) => ocr_lines(a, &format!("of \"{title}\"")),
            None => Err(format!("\"{title}\" is minimized or off the screen")),
        }
    })
    .await
}
