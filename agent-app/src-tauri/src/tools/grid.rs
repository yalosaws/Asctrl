//! read_grid: what each cell of a grid drawn on screen holds (a game board, a calendar, a keypad).
//!
//! The OCR engine reads words, not single glyphs: a cell on its own comes back empty, and a whole board
//! comes back as noise (grid lines and highlights get in the way). So each cell's glyph is cut out where
//! its pixels stand out from the cell's own background, the glyphs are laid side by side on a clean strip,
//! black on white at one height with a gap between them, and the strip is read as one line. Every glyph
//! found must come back as one character in its own place; any that doesn't is marked "?" rather than
//! guessed.

use super::*;
use crate::ocr;

/// Height of a glyph on the strip, and the gap around it
const GLYPH_H: i32 = 48;
const GAP: i32 = 40;
/// How far a pixel's brightness must be from its cell's background to count as part of the glyph
const INK: f64 = 60.0;
/// A cell whose background is this far from the board's usual one has something over it (a banner, a popup)
const COVERED: f64 = 80.0;

struct Glyph {
    r: usize,
    c: usize,
    /// Its box in the capture
    rect: (i32, i32, i32, i32),
    bg: f64,
    /// Where it sits on the strip, from x to x
    slot: (i32, i32),
}

fn lum(px: &[u8], w: i32, x: i32, y: i32) -> f64 {
    let i = ((y * w + x) * 4) as usize;
    0.114 * px[i] as f64 + 0.587 * px[i + 1] as f64 + 0.299 * px[i + 2] as f64
}

/// The glyphs in a capture `w` x `h` cut into `rows` x `cols` cells, and the cells something lies over
fn glyphs(px: &[u8], w: i32, h: i32, rows: usize, cols: usize) -> (Vec<Glyph>, Vec<(usize, usize)>) {
    let (cw, ch) = (w as f64 / cols as f64, h as f64 / rows as f64);
    // Each cell inside a margin, so the grid lines are left out, with its background: the commonest brightness
    // in it (a highlight is a background too)
    let cells: Vec<(usize, usize, winmgr::Rect, f64)> = (0..rows)
        .flat_map(|r| (0..cols).map(move |c| (r, c)))
        .filter_map(|(r, c)| {
            let (x0, x1) = ((c as f64 * cw + cw * 0.15) as i32, ((c + 1) as f64 * cw - cw * 0.15) as i32);
            let (y0, y1) = ((r as f64 * ch + ch * 0.12) as i32, ((r + 1) as f64 * ch - ch * 0.12) as i32);
            if x1 <= x0 || y1 <= y0 {
                return None;
            }
            let mut hist = [0u32; 64];
            for y in (y0..y1).step_by(2) {
                for x in (x0..x1).step_by(2) {
                    hist[(lum(px, w, x, y) as usize / 4).min(63)] += 1;
                }
            }
            let bg = (hist.iter().enumerate().max_by_key(|(_, n)| **n).map(|(i, _)| i).unwrap_or(63) * 4 + 2) as f64;
            Some((r, c, (x0, y0, x1, y1), bg))
        })
        .collect();
    // The board's usual background: the middle one of all the cells'
    let mut bgs: Vec<f64> = cells.iter().map(|x| x.3).collect();
    bgs.sort_by(f64::total_cmp);
    let usual = bgs.get(bgs.len() / 2).copied().unwrap_or(255.0);
    let mut out = vec![];
    let mut covered = vec![];
    for (r, c, (x0, y0, x1, y1), bg) in cells {
        if (bg - usual).abs() > COVERED {
            covered.push((r, c));
            continue;
        }
        {
            let (mut l, mut t, mut rr, mut b, mut n) = (i32::MAX, i32::MAX, -1, -1, 0);
            for y in y0..y1 {
                for x in x0..x1 {
                    if (lum(px, w, x, y) - bg).abs() > INK {
                        (l, t, rr, b, n) = (l.min(x), t.min(y), rr.max(x), b.max(y), n + 1);
                    }
                }
            }
            // A glyph is a real part of the cell's height, not a speck
            if n >= 6 && (b - t) as f64 > ch * 0.25 {
                out.push(Glyph { r, c, rect: (l, t, rr + 1, b + 1), bg, slot: (0, 0) });
            }
        }
    }
    (out, covered)
}

/// The strip: each glyph scaled to GLYPH_H, ink dark on white, GAP apart. Returns (pixels, width, height).
fn strip(px: &[u8], w: i32, gs: &mut [Glyph]) -> (Vec<u8>, i32, i32) {
    let sh = GLYPH_H + 2 * GAP / 2;
    let widths: Vec<i32> = gs.iter().map(|g| (((g.rect.2 - g.rect.0) as f64 * GLYPH_H as f64 / (g.rect.3 - g.rect.1) as f64).round() as i32).max(4)).collect();
    let sw = GAP + widths.iter().map(|x| x + GAP).sum::<i32>();
    let mut out = vec![255u8; (sw * sh * 4) as usize];
    let mut x = GAP;
    for (g, gw) in gs.iter_mut().zip(&widths) {
        let s = (g.rect.3 - g.rect.1) as f64 / GLYPH_H as f64;
        for v in 0..GLYPH_H {
            for u in 0..*gw {
                // Bilinear over the glyph's ink, so the strokes stay smooth when enlarged
                let (fx, fy) = (g.rect.0 as f64 + (u as f64 + 0.5) * s - 0.5, g.rect.1 as f64 + (v as f64 + 0.5) * s - 0.5);
                let (ix, iy) = (fx.floor() as i32, fy.floor() as i32);
                let (ax, ay) = (fx - ix as f64, fy - iy as f64);
                let ink = |x: i32, y: i32| {
                    let (x, y) = (x.clamp(g.rect.0, g.rect.2 - 1), y.clamp(g.rect.1, g.rect.3 - 1));
                    ((lum(px, w, x, y) - g.bg).abs() / 128.0).min(1.0)
                };
                let k = ink(ix, iy) * (1.0 - ax) * (1.0 - ay) + ink(ix + 1, iy) * ax * (1.0 - ay) + ink(ix, iy + 1) * (1.0 - ax) * ay + ink(ix + 1, iy + 1) * ax * ay;
                let o = (((GAP / 2 + v) * sw + x + u) * 4) as usize;
                let val = (255.0 * (1.0 - k)).round() as u8;
                out[o..o + 3].fill(val);
            }
        }
        g.slot = (x, x + gw);
        x += gw + GAP;
    }
    (out, sw, sh)
}

/// What each glyph reads as: one character per glyph, in its own place; "?" where that can't be told
fn assign(gs: &[Glyph], lines: &[ocr::Line]) -> Vec<String> {
    let mut got = vec!["?".to_string(); gs.len()];
    for word in lines.iter().flat_map(|l| &l.words) {
        let (l, _, r, _) = word.rect;
        // The glyphs this word covers, by their centre
        let under: Vec<usize> = gs.iter().enumerate().filter(|(_, g)| (g.slot.0 + g.slot.1) / 2 >= l - GAP / 4 && (g.slot.0 + g.slot.1) / 2 <= r + GAP / 4).map(|(i, _)| i).collect();
        let chars: Vec<char> = word.text.chars().filter(|c| !c.is_whitespace()).collect();
        // Only letters and digits: anything else read off a glyph (a bracket, a stray mark) is a misreading
        if chars.len() == under.len() && chars.iter().all(|c| c.is_alphanumeric()) {
            for (i, ch) in under.iter().zip(chars) {
                got[*i] = ch.to_string();
            }
        }
    }
    got
}

pub(super) async fn read_grid(ctx: &Ctx, args: &Value) -> Output {
    let area = match ctx.frame().area(args, 8) {
        Ok(a) => a,
        Err(e) => return fail(e),
    };
    let (rows, cols) = (int_arg(args, "rows", 0, 0, 100) as usize, int_arg(args, "cols", 0, 0, 100) as usize);
    if rows == 0 || cols == 0 {
        return fail("Give rows and cols (1 to 100)");
    }
    // The grid was given on a screenshot: what is there now must still be what was there then (the same
    // window, perhaps moved), at its centre and its corners, or another window would be read in its place
    let mut area = area;
    let (seen, now) = (ctx.seen(), winmgr::layout());
    let (r, b) = (area.left + area.width - 3, area.top + area.height - 3);
    let points = [(area.left + area.width / 2, area.top + area.height / 2), (area.left + 2, area.top + 2), (r, area.top + 2), (area.left + 2, b), (r, b)];
    let mut shift = (0, 0);
    for (i, &(x, y)) in points.iter().enumerate() {
        match locate::guard(&seen, &now, x, y, &winmgr::title_of) {
            Ok((nx, ny, _)) if i == 0 => shift = (nx - x, ny - y),
            Ok(_) => {}
            Err(why) => return report::refuse_stale(ctx, format!("Not read: since you last looked, {why}. Take a screenshot and give the grid again.")).await,
        }
    }
    (area.left, area.top) = (area.left + shift.0, area.top + shift.1);
    let r = tokio::task::spawn_blocking(move || -> Result<(String, Value), String> {
        let px = capture::pixels(area, area.width, area.height, false)?;
        let (mut gs, covered) = glyphs(&px, area.width, area.height, rows, cols);
        let read = if gs.is_empty() {
            vec![]
        } else {
            let (img, w, h) = strip(&px, area.width, &mut gs);
            assign(&gs, &ocr::read_image(&img, w, h)?)
        };
        let mut cells = vec![vec![".".to_string(); cols]; rows];
        for (g, t) in gs.iter().zip(&read) {
            cells[g.r][g.c] = t.clone();
        }
        for &(r, c) in &covered {
            cells[r][c] = "?".into();
        }
        let unsure = read.iter().filter(|t| *t == "?").count() + covered.len();
        let mut text = format!(
            "Grid {rows}×{cols} read off the pixels: {} filled, {unsure} unsure (\".\" empty, \"?\" a mark that couldn't be read or a cell something lies over)",
            gs.len() + covered.len()
        );
        if !covered.is_empty() {
            text += &format!("\n{} cell(s) have something over them (a banner, a popup): read again once it is gone", covered.len());
        }
        for row in &cells {
            text += "\n";
            text += &row.join(" ");
        }
        Ok((text, serde_json::json!({ "cells": cells, "unsure": unsure })))
    })
    .await;
    match r {
        Ok(Ok((t, data))) => Output { ok: true, text: Some(t), data: Some(data), ..Default::default() },
        Ok(Err(e)) => fail(e),
        Err(e) => fail(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, l: i32, r: i32) -> ocr::Word {
        ocr::Word { text: text.into(), rect: (l, 0, r, 10) }
    }

    #[test]
    fn each_glyph_gets_the_character_over_it_or_a_question_mark() {
        let g = |x: i32| Glyph { r: 0, c: 0, rect: (0, 0, 1, 1), bg: 255.0, slot: (x, x + 20) };
        let gs = [g(40), g(100), g(160), g(220)];
        // One word per glyph, then two glyphs read as one word, then a word that doesn't fit its glyphs
        let lines = [ocr::Line { words: vec![word("6", 40, 60), word("97", 100, 180)] }];
        assert_eq!(assign(&gs, &lines), ["6", "9", "7", "?"]);
        let lines = [ocr::Line { words: vec![word("123", 40, 120)] }];
        assert_eq!(assign(&gs, &lines), ["?", "?", "?", "?"]);
        // A bracket read off a glyph is a misreading, not a mark
        let lines = [ocr::Line { words: vec![word("《", 40, 60), word("6", 100, 120)] }];
        assert_eq!(assign(&gs, &lines), ["?", "6", "?", "?"]);
    }

    #[test]
    fn a_cell_with_a_banner_over_it_is_covered_not_empty() {
        // 3 x 1 cells of 40 x 40, all light but the last, which a dark banner lies over
        let (w, h) = (120, 40);
        let mut px = vec![240u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 80..120 {
                px[((y * w + x) * 4) as usize..][..3].fill(60);
            }
        }
        let (gs, covered) = glyphs(&px, w, h, 1, 3);
        assert!(gs.is_empty());
        assert_eq!(covered, [(0, 2)]);
    }

    #[test]
    fn a_dark_glyph_on_a_light_cell_is_found_and_grid_lines_are_not() {
        // 2 x 1 cells of 40 x 40: the left has a dark bar, the right only a grid line along its edge
        let (w, h) = (80, 40);
        let mut px = vec![240u8; (w * h * 4) as usize];
        let mut dark = |x: i32, y: i32| px[((y * w + x) * 4) as usize..][..3].fill(20);
        for y in 10..30 {
            for x in 18..22 {
                dark(x, y);
            }
            dark(79, y);
        }
        let (gs, covered) = glyphs(&px, w, h, 1, 2);
        assert!(covered.is_empty());
        assert_eq!(gs.len(), 1);
        assert_eq!((gs[0].c, gs[0].rect), (0, (18, 10, 22, 30)));
    }
}
