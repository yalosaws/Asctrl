//! Telling whether the screen changed after an action, and waiting for it to settle, from small low-resolution
//! captures. The overlay is excluded from capture and the mouse pointer is never in it, so neither counts.
//! Only the window the action is aimed at is watched: something always moves somewhere on a real desktop
//! (a video, a spinner, a chat streaming text), and it must not hold every action up.
//!
//! Inside that window, too, there are places to watch and places to ignore. The area around the action point
//! is always watched: that is where a click shows its effect. Elsewhere, a part that keeps changing on its own
//! (a video ad, a looping animation) is noticed while waiting and left out from then on, and remembered for
//! the next actions in the same window, so it holds none of them up.

use crate::capture::{self, Area};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A coarse grey-scale copy of part of the screen
pub struct Print {
    area: Area,
    w: i32,
    h: i32,
    px: Vec<u8>,
}

/// One sampled pixel covers about this many screen pixels across
const STEP: i32 = 2;
/// Grey levels two samples must differ by to count as a change
const THRESHOLD: u8 = 18;
const POLL: Duration = Duration::from_millis(30);

pub fn print(area: Area) -> Option<Print> {
    print_in(None, area)
}

fn print_in(hwnd: Option<isize>, area: Area) -> Option<Print> {
    let w = (area.width / STEP).clamp(1, 1600);
    let h = ((area.height as i64 * w as i64 / area.width.max(1) as i64) as i32).max(1);
    let bgra = match hwnd {
        Some(win) if crate::winmgr::is_minimized(win)
            || crate::winmgr::rect_of(win) != Some((area.left, area.top, area.left + area.width, area.top + area.height)) => return None,
        Some(win) if crate::winmgr::covered(win, area) => crate::wgc::capture(win).ok()?.cut(area, w, h).ok()?,
        _ => capture::pixels(area, w, h, false).ok()?,
    };
    let px = bgra.as_chunks::<4>().0.iter().map(|p| ((p[0] as u32 + 2 * p[1] as u32 + p[2] as u32) / 4) as u8).collect();
    Some(Print { area, w, h, px })
}

/// The part of the screen an action at `at` (or, without a point, a keystroke) shows its effect in: the window
/// under the point, else the window in front, clipped to the screen; the whole screen when there is neither
pub fn watch_area(at: Option<(i32, i32)>) -> Area {
    let v = capture::virtual_screen();
    let win = match at {
        Some((x, y)) => crate::winmgr::top_window_at(x, y),
        None => Some(crate::winmgr::foreground()).filter(|&h| h != 0),
    };
    let Some((l, t, r, b)) = win.and_then(crate::winmgr::rect_of) else { return v };
    let (l, t, r, b) = (l.max(v.left), t.max(v.top), r.min(v.left + v.width), b.min(v.top + v.height));
    if r - l < 16 || b - t < 16 {
        return v;
    }
    // The outermost pixels are the window's border, which Windows repaints on its own (it fades between the
    // active and inactive colour, and recomposes when windows above it move): not a reaction to anything
    let edge = if r - l > 64 && b - t > 64 { FRAME_EDGE } else { 0 };
    Area { left: l + edge, top: t + edge, width: r - l - 2 * edge, height: b - t - 2 * edge }
}

/// How far in from a window's edge its border and resize frame reach, in screen pixels
const FRAME_EDGE: i32 = 4;

/// What differs between two prints: how many samples, and their bounding box in screen pixels
pub struct Diff {
    pub count: usize,
    pub rect: Option<(i32, i32, i32, i32)>,
}

impl Diff {
    const NONE: Diff = Diff { count: 0, rect: None };

    /// A change worth reporting. A text caret blinking is a thin vertical line, which doesn't count.
    pub fn significant(&self) -> bool {
        match self.rect {
            None => false,
            Some((l, t, r, b)) => self.count >= 3 && !((r - l) <= 3 * STEP && (b - t) <= 40),
        }
    }
}

/// Samples across one tile (about 32 screen pixels). Movement is followed per tile.
const TILE: i32 = 16;

fn tiles_across(n: i32) -> i32 {
    (n + TILE - 1) / TILE
}

/// The changed samples in one tile and their bounds, in samples
#[derive(Clone, Copy)]
struct TileDiff {
    n: usize,
    l: i32,
    t: i32,
    r: i32,
    b: i32,
}

/// What changed between two prints of the same area, tile by tile; None when they don't line up
fn tile_diff(a: &Print, b: &Print) -> Option<Vec<TileDiff>> {
    if (a.w, a.h) != (b.w, b.h) || a.area != b.area {
        return None;
    }
    let tw = tiles_across(a.w);
    let mut tiles = vec![TileDiff { n: 0, l: i32::MAX, t: i32::MAX, r: i32::MIN, b: i32::MIN }; (tw * tiles_across(a.h)) as usize];
    let w = a.w as usize;
    for y in 0..a.h {
        let row = y as usize * w;
        let (ra, rb) = (&a.px[row..row + w], &b.px[row..row + w]);
        let trow = ((y / TILE) * tw) as usize;
        // Tile by tile along the row: whole runs of identical samples are skipped cheaply
        for (tx, (ca, cb)) in ra.chunks(TILE as usize).zip(rb.chunks(TILE as usize)).enumerate() {
            if ca == cb {
                continue;
            }
            let x0 = tx as i32 * TILE;
            for (k, (pa, pb)) in ca.iter().zip(cb).enumerate() {
                if pa.abs_diff(*pb) > THRESHOLD {
                    let x = x0 + k as i32;
                    let d = &mut tiles[trow + tx];
                    d.n += 1;
                    d.l = d.l.min(x);
                    d.r = d.r.max(x);
                    d.t = d.t.min(y);
                    d.b = d.b.max(y);
                }
            }
        }
    }
    Some(tiles)
}

/// The tiles that aren't ignored, as one change in screen pixels
fn merge(p: &Print, tiles: &[TileDiff], ignore: &[bool]) -> Diff {
    let (mut count, mut l, mut t, mut r, mut btm) = (0usize, i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for (i, d) in tiles.iter().enumerate() {
        if d.n == 0 || ignore.get(i).copied().unwrap_or(false) {
            continue;
        }
        count += d.n;
        l = l.min(d.l);
        t = t.min(d.t);
        r = r.max(d.r);
        btm = btm.max(d.b);
    }
    if count == 0 {
        return Diff::NONE;
    }
    let sx = p.area.width as f64 / p.w as f64;
    let sy = p.area.height as f64 / p.h as f64;
    let to = |v: i32, s: f64, o: i32| o + (v as f64 * s).round() as i32;
    Diff {
        count,
        rect: Some((to(l, sx, p.area.left), to(t, sy, p.area.top), to(r + 1, sx, p.area.left), to(btm + 1, sy, p.area.top))),
    }
}

/// How `b` differs from `a`, leaving out the ignored tiles
fn diff_ignoring(a: &Print, b: &Print, ignore: &[bool]) -> Diff {
    match tile_diff(a, b) {
        Some(tiles) => merge(b, &tiles, ignore),
        None => Diff { count: usize::MAX, rect: Some((b.area.left, b.area.top, b.area.left + b.area.width, b.area.top + b.area.height)) },
    }
}

/// Around the action point, in screen pixels: where its effect shows first, so it is always watched
const FOCUS: i32 = 160;
/// Outside the focus, a tile still changing this long after it first changed moves on its own (a video, an ad,
/// a looping animation) rather than in answer to the action, and stops being waited for. Pauses in between
/// don't matter: a video has still moments, and something redrawn in pieces changes each tile now and then.
const NOISE_AFTER: Duration = Duration::from_millis(600);
/// Tiles found moving on their own are remembered for the next actions in the same window, and forgotten after
/// staying still through this many waits in a row, or after NOISE_TTL
const NOISE_STRIKES: u8 = 3;
const NOISE_TTL: Duration = Duration::from_secs(20);

/// The tiles of a watched area found moving on their own: how many more still waits each is kept for (0 = not noise)
struct Remembered {
    area: Area,
    w: i32,
    h: i32,
    tiles: Vec<u8>,
    at: Instant,
}

static NOISE: Mutex<Option<Remembered>> = Mutex::new(None);

/// Which tiles of a watched area to wait for, and which to ignore
struct Tracker {
    /// Left out of every comparison: found or remembered moving on their own, and not in the focus
    ignore: Vec<bool>,
    /// Remembered from earlier waits (kept even where the focus overrides them this time)
    known: Vec<u8>,
    focus: Vec<bool>,
    /// When each tile first changed during this wait
    first: Vec<Option<Instant>>,
    noise_after: Duration,
    /// Tiles across
    tw: usize,
}

impl Tracker {
    fn new(p: &Print, at: Option<(i32, i32)>, noise_after: Duration) -> Self {
        let (tw, th) = (tiles_across(p.w), tiles_across(p.h));
        let n = (tw * th) as usize;
        let (sx, sy) = (p.area.width as f64 / p.w as f64, p.area.height as f64 / p.h as f64);
        let focus: Vec<bool> = (0..n as i32)
            .map(|i| {
                let cx = p.area.left + (((i % tw) * TILE + TILE / 2) as f64 * sx) as i32;
                let cy = p.area.top + (((i / tw) * TILE + TILE / 2) as f64 * sy) as i32;
                at.is_some_and(|(x, y)| (cx - x).abs() <= FOCUS && (cy - y).abs() <= FOCUS)
            })
            .collect();
        let known = match NOISE.lock().unwrap().as_ref() {
            Some(m) if m.area == p.area && (m.w, m.h) == (p.w, p.h) && m.at.elapsed() < NOISE_TTL => m.tiles.clone(),
            _ => vec![0; n],
        };
        let ignore = known.iter().zip(&focus).map(|(&k, &f)| k > 0 && !f).collect();
        Tracker { ignore, known, focus, first: vec![None; n], noise_after, tw: tw as usize }
    }

    /// Record the tiles that changed since the last poll. Outside the focus, one that keeps changing is ignored from
    /// now on, and so is one that changes right next to an ignored one: what moves on its own is one block (a video
    /// ad), whose parts that change less often would otherwise each hold the wait up until they are caught.
    /// That spreads by one ring of tiles per poll.
    fn update(&mut self, tiles: &[TileDiff]) {
        let now = Instant::now();
        let was = self.ignore.clone();
        let (tw, n) = (self.tw, self.ignore.len());
        let beside_noise = |i: usize| {
            let (x, y) = ((i % tw) as i64, (i / tw) as i64);
            (-1..=1).any(|dy| {
                (-1..=1).any(|dx| {
                    let (nx, ny) = (x + dx, y + dy);
                    let j = ny * tw as i64 + nx;
                    (dx, dy) != (0, 0) && nx >= 0 && nx < tw as i64 && ny >= 0 && j < n as i64 && was[j as usize]
                })
            })
        };
        for (i, d) in tiles.iter().enumerate() {
            if d.n < 2 {
                continue;
            }
            let first = *self.first[i].get_or_insert(now);
            if !self.focus[i] && (now - first >= self.noise_after || beside_noise(i)) {
                self.ignore[i] = true;
            }
        }
    }

    /// Remember what moves on its own, for the next actions in this area
    fn save(&self, p: &Print) {
        let tiles: Vec<u8> = (0..self.ignore.len())
            .map(|i| match (self.ignore[i] || self.known[i] > 0, self.first[i].is_some()) {
                (true, true) => NOISE_STRIKES,
                (true, false) => self.known[i].saturating_sub(1),
                _ => 0,
            })
            .collect();
        let mut m = NOISE.lock().unwrap();
        if tiles.iter().any(|&t| t > 0) {
            *m = Some(Remembered { area: p.area, w: p.w, h: p.h, tiles, at: Instant::now() });
        } else if m.as_ref().is_some_and(|m| m.area == p.area) {
            *m = None;
        }
    }
}

/// The state of things before an action, to compare with afterwards
pub struct Before {
    print: Option<Print>,
    area: Area,
    /// Where the action shows its effect first: its point, or for a keystroke the mouse (usually where the last click was)
    focus: Option<(i32, i32)>,
    /// A region the caller framed: watched as a whole, with nothing in it ignored
    only: bool,
    fg: isize,
}

pub fn before(at: Option<(i32, i32)>) -> Before {
    let area = watch_area(at);
    let focus = at.or_else(|| crate::input::cursor_pos().ok());
    Before { print: print(area), area, focus, only: false, fg: crate::winmgr::foreground() }
}

/// Like before, but only `area` counts: the caller framed where the effect shows, so everything outside it
/// (an ad, a video, a clock) is never looked at, and nothing inside it is written off as moving on its own
pub fn before_in(area: Area, at: Option<(i32, i32)>) -> Before {
    Before { print: print(area), area, focus: at, only: true, fg: crate::winmgr::foreground() }
}

/// How the screen ended up after an action
pub struct Change {
    pub changed: bool,
    /// Bounding box of what changed in the watched window, in screen pixels
    pub rect: Option<(i32, i32, i32, i32)>,
    /// Still changing when we stopped waiting (an animation, a video, a page still loading)
    pub busy: bool,
    pub waited: Duration,
    /// Another window came to the front: its title
    pub new_front: Option<String>,
    /// ... and its handle
    pub front: Option<isize>,
}

/// Wait until the screen has reacted to an action and stopped changing.
/// No change within `quiet` ends the wait (nothing happened, or not yet); once something changes, wait until two
/// captures in a row match, up to `max` in all. A different window coming to the front counts as a change.
/// What moves on its own away from the action point is left out (see the module comment).
pub fn settle(b: &Before, quiet: Duration, max: Duration) -> Change {
    settle_with(b, quiet, max, 2)
}

/// Like settle, but done the moment the screen reacts (or a window comes to the front), without waiting for
/// it to stop changing: for a caller whose next step waits for what it needs by itself (see `locate`)
pub fn react(b: &Before, quiet: Duration, max: Duration) -> Change {
    settle_with(b, quiet, max, 0)
}

/// `calm_polls`: how many captures in a row must match once the screen has reacted
fn settle_with(b: &Before, quiet: Duration, max: Duration, calm_polls: u32) -> Change {
    let start = Instant::now();
    let mut tracker = b.print.as_ref().filter(|_| !b.only).map(|p| Tracker::new(p, b.focus, NOISE_AFTER));
    let mut prev: Option<Print> = None;
    let mut seen = false;
    let mut calm = 0;
    let front = || {
        let fg = crate::winmgr::foreground();
        (fg != 0 && fg != b.fg).then_some(fg)
    };
    loop {
        std::thread::sleep(POLL);
        let elapsed = start.elapsed();
        let cur = b.print.as_ref().and_then(|_| print(b.area));
        // One comparison per poll: with the screen before the action until it reacts, then with the last capture.
        // Tiles that keep changing on their own become ignored along the way.
        let base = if seen { prev.as_ref() } else { b.print.as_ref() };
        let tiles = match (base, &cur) {
            (Some(p), Some(c)) => tile_diff(p, c),
            _ => None,
        };
        if let (Some(t), Some(ts)) = (tracker.as_mut(), &tiles) {
            t.update(ts);
        }
        let ignore: &[bool] = tracker.as_ref().map_or(&[], |t| &t.ignore);
        let d = match (&tiles, &cur) {
            (Some(ts), Some(c)) => merge(c, ts, ignore),
            _ => Diff::NONE,
        };
        let fg = front();
        if !seen {
            seen = fg.is_some() || d.significant();
        } else {
            calm = if cur.is_none() || !d.significant() { calm + 1 } else { 0 };
        }
        let done = if seen { calm >= calm_polls } else { elapsed >= quiet };
        if done || elapsed >= max {
            if let (Some(t), Some(p)) = (&tracker, &b.print) {
                t.save(p);
            }
            // What changed in all, against the screen before the action (already in hand when it never reacted)
            let total = match (seen, &b.print, cur.as_ref().or(prev.as_ref())) {
                (false, _, _) => d,
                (true, Some(p), Some(c)) => diff_ignoring(p, c, ignore),
                _ => Diff::NONE,
            };
            let front = front();
            // An untitled window in front (a menu, the desktop) is named by its program
            let new_front = front.map(|h| match crate::winmgr::title_of(h) {
                t if t.trim().is_empty() => crate::winmgr::process_name(crate::winmgr::pid_of(h)),
                t => t,
            });
            let changed = total.significant() || front.is_some();
            return Change { changed, rect: total.rect.filter(|_| total.significant()), busy: !done, waited: elapsed, new_front, front };
        }
        if cur.is_some() {
            prev = cur;
        }
    }
}

/// Wait until `area` stops changing for `calm_for`, up to `max`. True when it did. A part that keeps changing
/// for `noise_after` (a video, an ad) is left out; make it long when a spinner should count as still loading.
pub fn wait_stable(area: Area, calm_for: Duration, max: Duration, noise_after: Duration) -> bool {
    wait_stable_in(None, area, calm_for, max, noise_after, &|| false)
}

/// `stop` ends the wait early (the session it was for ended): false, as it did not settle
pub fn wait_stable_in(hwnd: Option<isize>, area: Area, calm_for: Duration, max: Duration, noise_after: Duration, stop: &dyn Fn() -> bool) -> bool {
    let start = Instant::now();
    let Some(mut prev) = print_in(hwnd, area) else { return hwnd.is_none() };
    let mut tracker = Tracker::new(&prev, None, noise_after);
    let mut calm_since = Instant::now();
    while start.elapsed() < max {
        if stop() {
            return false;
        }
        std::thread::sleep(POLL);
        let Some(cur) = print_in(hwnd, area) else { return hwnd.is_none() };
        if let Some(tiles) = tile_diff(&prev, &cur) {
            tracker.update(&tiles);
            if merge(&cur, &tiles, &tracker.ignore).significant() {
                calm_since = Instant::now();
            } else if calm_since.elapsed() >= calm_for {
                tracker.save(&cur);
                return true;
            }
        }
        prev = cur;
    }
    tracker.save(&prev);
    false
}

/// For waits that should leave out what moves on its own as soon as an action's settle does
pub const NOISE_QUICK: Duration = NOISE_AFTER;

#[cfg(test)]
mod tests {
    use super::*;

    /// A 128×128-sample print of a 256×256 screen area (8×8 tiles), with the tiles listed painted `v`
    fn shot(painted: &[(i32, i32)], v: u8) -> Print {
        let mut px = vec![0u8; 128 * 128];
        for &(tx, ty) in painted {
            for y in ty * TILE..(ty + 1) * TILE {
                for x in tx * TILE..(tx + 1) * TILE {
                    px[(y * 128 + x) as usize] = v;
                }
            }
        }
        Print { area: Area { left: 0, top: 0, width: 256, height: 256 }, w: 128, h: 128, px }
    }

    #[test]
    fn what_keeps_moving_away_from_the_action_is_ignored() {
        // The action at the top-left corner: tile (0,0) is in focus, tile (7,7) is 240 px away
        let mut t = Tracker::new(&shot(&[], 0), Some((0, 0)), Duration::from_millis(50));
        let tile = |x: i32, y: i32| (y * 8 + x) as usize;
        assert!(t.focus[tile(0, 0)] && !t.focus[tile(7, 7)]);
        let both = [(0, 0), (7, 7)];
        t.update(&tile_diff(&shot(&[], 0), &shot(&both, 200)).unwrap());
        assert!(!t.ignore[tile(7, 7)], "a first change is a reaction, not noise");
        std::thread::sleep(Duration::from_millis(60));
        t.update(&tile_diff(&shot(&both, 200), &shot(&both, 90)).unwrap());
        assert!(t.ignore[tile(7, 7)], "still changing after noise_after: ignored");
        assert!(!t.ignore[tile(0, 0)], "the focus is never ignored");
        let d = merge(&shot(&both, 0), &tile_diff(&shot(&[], 0), &shot(&both, 200)).unwrap(), &t.ignore);
        assert_eq!(d.rect, Some((0, 0, 32, 32)), "only the focus tile is reported");
    }

    #[test]
    fn noise_is_remembered_for_the_next_action() {
        let tile = |x: i32, y: i32| (y * 8 + x) as usize;
        let mut t = Tracker::new(&shot(&[], 0), Some((0, 0)), Duration::from_millis(50));
        t.update(&tile_diff(&shot(&[], 0), &shot(&[(7, 7)], 200)).unwrap());
        std::thread::sleep(Duration::from_millis(60));
        t.update(&tile_diff(&shot(&[(7, 7)], 200), &shot(&[(7, 7)], 90)).unwrap());
        t.save(&shot(&[], 0));
        let next = Tracker::new(&shot(&[], 0), Some((0, 0)), Duration::from_millis(50));
        assert!(next.ignore[tile(7, 7)], "ignored from the start of the next action");
    }

    #[test]
    fn noise_spreads_to_tiles_changing_next_to_it_but_not_into_the_focus() {
        let tile = |x: i32, y: i32| (y * 8 + x) as usize;
        let mut t = Tracker::new(&shot(&[], 0), Some((0, 0)), Duration::from_secs(60));
        t.ignore[tile(7, 7)] = true;
        // (6,7) changes beside the noise; (5,7) too, but only beside it once (6,7) is ignored; (0,0) is the focus
        let moving = [(6, 7), (5, 7), (0, 0), (1, 0)];
        t.ignore[tile(1, 1)] = true;
        t.update(&tile_diff(&shot(&[], 0), &shot(&moving, 200)).unwrap());
        assert!(t.ignore[tile(6, 7)] && !t.ignore[tile(5, 7)], "one ring per poll");
        assert!(!t.ignore[tile(0, 0)] && !t.ignore[tile(1, 0)], "the focus is never ignored");
        t.update(&tile_diff(&shot(&moving, 200), &shot(&moving, 90)).unwrap());
        assert!(t.ignore[tile(5, 7)]);
    }
}
