//! Telling whether the screen changed after an action, and waiting for it to settle, from small low-resolution
//! captures. The overlay is excluded from capture and the mouse pointer is never in it, so neither counts.
//! Only the window the action is aimed at is watched: something always moves somewhere on a real desktop
//! (a video, a spinner, a chat streaming text), and it must not hold every action up.

use crate::capture::{self, Area};
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
    let w = (area.width / STEP).clamp(1, 1600);
    let h = ((area.height as i64 * w as i64 / area.width.max(1) as i64) as i32).max(1);
    let bgra = capture::pixels(area, w, h, false).ok()?;
    let px = bgra.chunks_exact(4).map(|p| ((p[0] as u32 + 2 * p[1] as u32 + p[2] as u32) / 4) as u8).collect();
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
    Area { left: l, top: t, width: r - l, height: b - t }
}

/// What differs between two prints: how many samples, and their bounding box in screen pixels
pub struct Diff {
    pub count: usize,
    pub rect: Option<(i32, i32, i32, i32)>,
}

impl Diff {
    /// A change worth reporting. A text caret blinking is a thin vertical line, which doesn't count.
    pub fn significant(&self) -> bool {
        match self.rect {
            None => false,
            Some((l, t, r, b)) => self.count >= 3 && !((r - l) <= 3 * STEP && (b - t) <= 40),
        }
    }
}

pub fn diff(a: &Print, b: &Print) -> Diff {
    if (a.w, a.h, a.area.left, a.area.top) != (b.w, b.h, b.area.left, b.area.top) {
        let r = (b.area.left, b.area.top, b.area.left + b.area.width, b.area.top + b.area.height);
        return Diff { count: usize::MAX, rect: Some(r) };
    }
    let (mut count, mut l, mut t, mut r, mut btm) = (0usize, i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for y in 0..a.h {
        let row = (y * a.w) as usize;
        for x in 0..a.w {
            let i = row + x as usize;
            if a.px[i].abs_diff(b.px[i]) > THRESHOLD {
                count += 1;
                l = l.min(x);
                r = r.max(x);
                t = t.min(y);
                btm = btm.max(y);
            }
        }
    }
    if count == 0 {
        return Diff { count, rect: None };
    }
    let sx = a.area.width as f64 / a.w as f64;
    let sy = a.area.height as f64 / a.h as f64;
    let to = |v: i32, s: f64, o: i32| o + (v as f64 * s).round() as i32;
    Diff {
        count,
        rect: Some((to(l, sx, a.area.left), to(t, sy, a.area.top), to(r + 1, sx, a.area.left), to(btm + 1, sy, a.area.top))),
    }
}

/// The state of things before an action, to compare with afterwards
pub struct Before {
    print: Option<Print>,
    area: Area,
    fg: isize,
}

pub fn before(at: Option<(i32, i32)>) -> Before {
    let area = watch_area(at);
    Before { print: print(area), area, fg: crate::winmgr::foreground() }
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
}

/// Wait until the screen has reacted to an action and stopped changing.
/// No change within `quiet` ends the wait (nothing happened, or not yet); once something changes, wait until two
/// captures in a row match, up to `max` in all. A different window coming to the front counts as a change.
pub fn settle(b: &Before, quiet: Duration, max: Duration) -> Change {
    let start = Instant::now();
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
        let fg = front();
        if !seen && (fg.is_some() || matches!((&b.print, &cur), (Some(p), Some(c)) if diff(p, c).significant())) {
            seen = true;
        }
        if seen {
            calm = match (&prev, &cur) {
                (Some(p), Some(c)) if !diff(p, c).significant() => calm + 1,
                (_, None) => calm + 1,
                _ => 0,
            };
        }
        let done = if seen { calm >= 2 } else { elapsed >= quiet };
        if done || elapsed >= max {
            let d = match (&b.print, &cur) {
                (Some(p), Some(c)) => diff(p, c),
                _ => Diff { count: 0, rect: None },
            };
            let new_front = front().and_then(|h| crate::winmgr::list().into_iter().find(|w| w.handle == h).map(|w| w.title));
            let changed = d.significant() || new_front.is_some();
            return Change { changed, rect: d.rect.filter(|_| d.significant()), busy: !done, waited: elapsed, new_front };
        }
        prev = cur;
    }
}

/// Wait until `area` stops changing for `calm_for`, up to `max`. True when it did.
pub fn wait_stable(area: Area, calm_for: Duration, max: Duration) -> bool {
    let start = Instant::now();
    let Some(mut prev) = print(area) else { return true };
    let mut calm_since = Instant::now();
    while start.elapsed() < max {
        std::thread::sleep(POLL);
        let Some(cur) = print(area) else { return true };
        if diff(&prev, &cur).significant() {
            calm_since = Instant::now();
        } else if calm_since.elapsed() >= calm_for {
            return true;
        }
        prev = cur;
    }
    false
}
