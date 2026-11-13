//! Reading and clicking text on screen with OCR, for programs UI Automation can't see into

use super::*;

/// Where text is on screen; without a query, every line
pub(super) async fn find_text(ctx: &Ctx, args: &Value) -> Output {
    let query = str_arg(args, "text").trim().to_string();
    let area = match ocr_area(*ctx.geom.lock().unwrap(), args) {
        Ok(a) => a,
        Err(e) => return fail(e),
    };
    let g = *ctx.geom.lock().unwrap();
    blocking(move || {
        let lines = ocr::read(area)?;
        if query.is_empty() {
            if lines.is_empty() {
                return Ok("No text found in that area".into());
            }
            let shown: Vec<String> = lines
                .iter()
                .take(400)
                .map(|l| {
                    let (a, b, c, d) = l.rect();
                    format!("\"{}\" {}", l.text(), at_text(g, (a + c) / 2, (b + d) / 2))
                })
                .collect();
            let more = if lines.len() > 400 { format!("\n… {} more lines", lines.len() - 400) } else { String::new() };
            return Ok(format!("Text on screen, top to bottom ({} lines; @ is the centre):\n{}{more}", lines.len(), shown.join("\n")));
        }
        let hits = ocr::find(&lines, &query);
        if hits.is_empty() {
            return Err(not_on_screen(&query, lines.len()));
        }
        let shown: Vec<String> = hits
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let (a, b, c, d) = h.rect;
                format!("{}. {} in \"{}\"{}", i + 1, at_text(g, (a + c) / 2, (b + d) / 2), h.line, if h.exact { " (whole line)" } else { "" })
            })
            .collect();
        Ok(format!("Found \"{query}\" {} time(s):\n{}", hits.len(), shown.join("\n")))
    })
    .await
}

/// Find text with OCR and click it
pub(super) async fn click_text(ctx: &Ctx, args: &Value) -> Output {
    let query = str_arg(args, "text").trim().to_string();
    if query.is_empty() {
        return fail("text is empty");
    }
    let area = match ocr_area(*ctx.geom.lock().unwrap(), args) {
        Ok(a) => a,
        Err(e) => return fail(e),
    };
    let nth = int_arg(args, "occurrence", 1, 1, 100) as usize;
    let q = query.clone();
    let found = tokio::task::spawn_blocking(move || {
        // Retry briefly: the text is often on its way (a menu opening, a page still drawing)
        let start = std::time::Instant::now();
        loop {
            let lines = ocr::read(area)?;
            let hits = ocr::find(&lines, &q);
            // A label that is the whole line (a button, a menu item) beats the same words inside a sentence
            let exact: Vec<&ocr::Hit> = hits.iter().filter(|h| h.exact).collect();
            let pool: Vec<&ocr::Hit> = if exact.is_empty() { hits.iter().collect() } else { exact };
            let r = match pool.get(nth - 1) {
                Some(h) => return Ok((h.rect, pool.len())),
                None if pool.is_empty() => Err(not_on_screen(&q, lines.len())),
                None => Err(format!("\"{q}\" is on screen only {} time(s)", pool.len())),
            };
            if start.elapsed() >= Duration::from_millis(1500) {
                return r;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    })
    .await;
    let ((l, t, r, b), total) = match found {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return fail(e),
        Err(e) => return fail(e.to_string()),
    };
    let (x, y) = ((l + r) / 2, (t + b) / 2);
    let button = str_arg(args, "button").to_string();
    let count = int_arg(args, "count", 1, 1, 3) as u32;
    let mods = str_arg(args, "modifiers").to_string();
    // Shown like an ordinary click; the real click doesn't wait for the animation
    let fx_id = format!("{}.text", pace::CALL_ID.try_with(Clone::clone).unwrap_or_default());
    let zone = winmgr::top_window_at(x, y).and_then(winmgr::rect_of).map(rect_json);
    pace::emit(serde_json::json!({ "phase": "start", "id": fx_id, "tool": "click", "args": { "count": count }, "geo": { "at": [x, y], "zone": zone, "zoneIsWindow": true } }));
    let g = *ctx.geom.lock().unwrap();
    let which = if total > 1 { format!(" (match {nth} of {total})") } else { String::new() };
    let out = input_op(ctx, Some((x, y)), move || input::click(x, y, &button, count, &mods), format!("Clicked \"{query}\" {}{which}", at_text(g, x, y))).await;
    pace::emit(serde_json::json!({ "phase": "end", "id": fx_id, "tool": "click", "ok": out.ok, "text": "", "geo": {} }));
    out
}

pub(super) fn not_on_screen(q: &str, lines: usize) -> String {
    format!("\"{q}\" is not on screen (OCR read {lines} lines of text). Try a shorter part of it, or look with screenshot")
}

/// The part of the screen to read: a rectangle on the last screenshot (x0,y0,x1,y1), one display, or everything
pub(super) fn ocr_area(g: Option<Geom>, args: &Value) -> Result<capture::Area, String> {
    if args.get("x0").is_some() {
        let a = shot_to_screen(g, args, "x0", "y0")?;
        let b = shot_to_screen(g, args, "x1", "y1")?;
        let (l, t, r, btm) = (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
        if r - l < 8 || btm - t < 8 {
            return Err("Region is too small".into());
        }
        return Ok(capture::Area { left: l, top: t, width: r - l, height: btm - t });
    }
    // A window by handle or title: just its visible part
    let handle = args.get("handle").and_then(Value::as_i64).map(|h| h as isize);
    let title = str_arg(args, "title");
    if handle.is_some() || !title.trim().is_empty() {
        let h = winmgr::find(handle, title)?;
        let (l, t, r, b) = winmgr::rect_of(h.0 as isize).ok_or("That window is minimized or hidden")?;
        let v = capture::virtual_screen();
        let (l, t, r, b) = (l.max(v.left), t.max(v.top), r.min(v.left + v.width), b.min(v.top + v.height));
        return Ok(capture::Area { left: l, top: t, width: (r - l).max(1), height: (b - t).max(1) });
    }
    match int_arg(args, "display", 0, 0, 16) as usize {
        0 => Ok(capture::virtual_screen()),
        n => capture::displays().get(n - 1).map(|d| d.area).ok_or(format!("There is no display #{n}")),
    }
}
