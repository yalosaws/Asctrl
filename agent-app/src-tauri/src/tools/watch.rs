//! wait_for with until: keep reading a number (or some text) off a region of the screen, many times a second, and return
//! the moment it meets a condition. A price that crosses a line, a timer that runs out, a score: the AI says
//! where and when once, and the reaction (the next step of its plan) goes in within a fraction of a second,
//! instead of each look costing it a round trip.

use super::report::Did;
use super::*;
use crate::ocr;

#[derive(Debug, Clone, PartialEq)]
enum Cond {
    Above(f64, bool),
    Below(f64, bool),
    Equal(f64),
    /// The text shows up in the region
    Shows(String),
}

impl Cond {
    fn parse(s: &str) -> Result<Cond, String> {
        let s = s.trim();
        let num = |r: &str| r.trim().replace(',', "").parse::<f64>().map_err(|_| format!("\"{s}\": \"{}\" is not a number", r.trim()));
        Ok(if let Some(r) = s.strip_prefix(">=") {
            Cond::Above(num(r)?, true)
        } else if let Some(r) = s.strip_prefix("<=") {
            Cond::Below(num(r)?, true)
        } else if let Some(r) = s.strip_prefix('>') {
            Cond::Above(num(r)?, false)
        } else if let Some(r) = s.strip_prefix('<') {
            Cond::Below(num(r)?, false)
        } else if let Some(r) = s.strip_prefix('=') {
            Cond::Equal(num(r)?)
        } else {
            Cond::Shows(s.to_string())
        })
    }

    fn numeric(&self) -> bool {
        !matches!(self, Cond::Shows(_))
    }

    fn met(&self, value: Option<f64>, text: &str) -> bool {
        match (self, value) {
            (Cond::Above(n, eq), Some(v)) => v > *n || (*eq && v == *n),
            (Cond::Below(n, eq), Some(v)) => v < *n || (*eq && v == *n),
            (Cond::Equal(n), Some(v)) => (v - n).abs() < 1e-9,
            (Cond::Shows(q), _) => squash(text).contains(&squash(q)),
            _ => false,
        }
    }
}

fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect()
}

/// What the engine reads for the characters of a number, put back: full-width forms, the dashes a CJK engine
/// reads for a minus, and letters it mistakes for digits next to digits
fn normalize(s: &str) -> String {
    let mapped: Vec<char> = s
        .chars()
        .map(|c| match c {
            '０'..='９' => char::from_u32(c as u32 - '０' as u32 + '0' as u32).unwrap_or(c),
            '．' | '。' | '·' | '•' | '∙' | '・' => '.',
            '，' => ',',
            '％' => '%',
            '＄' => '$',
            '＋' => '+',
            '一' | '—' | '–' | '−' | '－' | '‐' => '-',
            _ => c,
        })
        // Spaces, and the apostrophes some locales group thousands with, are not part of a number
        .filter(|c| !c.is_whitespace() && !matches!(c, '\'' | '’' | '`'))
        .collect();
    let digitish = |c: Option<&char>| c.is_some_and(|c| c.is_ascii_digit() || *c == '.');
    (0..mapped.len())
        .map(|i| {
            let c = mapped[i];
            let near = digitish(mapped.get(i + 1)) || (i > 0 && digitish(mapped.get(i - 1)));
            match c {
                ']' | '[' | '|' | 'l' | 'I' | '!' if near => '1',
                'O' | 'o' if near => '0',
                _ => c,
            }
        })
        .collect()
}

/// The first number in what was read, with its sign: "-$69" → -69, "$1,234.5" → 1234.5, "+1.85%" → 1.85
fn first_number(text: &str) -> Option<f64> {
    let s = normalize(text);
    let cs: Vec<char> = s.chars().collect();
    let start = cs.iter().position(|c| c.is_ascii_digit())?;
    let mut end = start;
    // A stray mark the engine reads between the digits of one number ("4》836") is not where it ends; a sign,
    // a percent or currency mark, or a letter is
    let glue = |c: char| !c.is_alphanumeric() && !"+-%$()/:=".contains(c);
    while end < cs.len() && (cs[end].is_ascii_digit() || cs[end] == '.' || cs[end] == ',' || (glue(cs[end]) && cs.get(end + 1).is_some_and(|n| n.is_ascii_digit()))) {
        end += 1;
    }
    let body: String = cs[start..end].iter().filter(|c| c.is_ascii_digit() || **c == '.').collect();
    let body = body.trim_end_matches('.');
    // "4.846.54": only the last dot is the decimal point, the others group thousands
    let body = match body.rfind('.') {
        Some(d) if body[..d].contains('.') => format!("{}{}", body[..d].replace('.', ""), &body[d..]),
        _ => body.to_string(),
    };
    // A sign just before it, past a currency mark ("-$69", "-s69" as the engine may read it)
    let mut j = start;
    while j > 0 && matches!(cs[j - 1], '$' | 's' | 'S' | '¥' | '€' | '£') {
        j -= 1;
    }
    let neg = j > 0 && cs[j - 1] == '-';
    body.parse::<f64>().ok().map(|v| if neg { -v } else { v })
}

pub(super) async fn watch(ctx: &Ctx, args: &Value) -> Output {
    let until = str_arg(args, "until");
    if until.trim().is_empty() {
        return fail("Give until: conditions separated by |, e.g. \">= 141 | <= 133\", or text to wait for");
    }
    let conds: Vec<Cond> = match until.split('|').filter(|c| !c.trim().is_empty()).map(Cond::parse).collect() {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let frame = ctx.frame();
    let hwnd = frame.own.unwrap_or(0);
    let area = match frame.area(args, 6) {
        Ok(a) => a,
        Err(e) => return fail(format!("{e}: give the region to read as x0, y0, x1, y1 on the latest screenshot")),
    };
    let every = Duration::from_millis(int_arg(args, "everyMs", 120, 30, 5000) as u64);
    let timeout = Duration::from_secs_f64(args.get("timeoutSec").and_then(Value::as_f64).unwrap_or(60.0).clamp(0.5, 600.0));
    let numeric = conds.iter().any(Cond::numeric);
    // A number must meet its condition this many readings running: one misread digit must not set off a sale
    let confirm = int_arg(args, "confirm", if numeric { 2 } else { 1 }, 1, 20) as u32;
    let token = session_token();
    let r = tokio::task::spawn_blocking(move || -> Result<Did, String> {
        let start = std::time::Instant::now();
        let (mut reads, mut misses) = (0u32, 0u32);
        let (mut lo, mut hi, mut last): (Option<f64>, Option<f64>, Option<f64>) = (None, None, None);
        let mut last_text: String;
        // The condition that held on the last readings, and on how many in a row
        let mut streak: (Option<usize>, u32) = (None, 0);
        // The number read last time, believed or not
        let mut prev: Option<f64> = None;
        // Readings running that break with the number believed last
        let mut breaks = 0u32;
        loop {
            if ended(token) {
                return Err(SESSION_OVER.into());
            }
            let tick = std::time::Instant::now();
            let lines = if numeric { ocr::read_numbers(hwnd, area)? } else { ocr::read_in(hwnd, area)? };
            let text = lines.iter().map(|l| l.text()).collect::<Vec<_>>().join(" ");
            reads += 1;
            let raw = if numeric { first_number(&text) } else { None };
            // A reading counts only when the one before agrees with it: a digit dropped or a stray mark read
            // as one makes a number no tick of the real one would
            let mut value = raw.filter(|v| prev.is_some_and(|p| agree(p, *v)));
            prev = raw;
            // Once a number has been believed, one that breaks with it (the edge of the region cutting off a
            // digit, twice running) is believed only after three readings in a row say so
            if let (Some(v), Some(l)) = (value, last) {
                if agree(l, v) {
                    breaks = 0;
                } else {
                    breaks += 1;
                    if breaks < 3 {
                        value = None;
                    }
                }
            }
            if numeric && value.is_none() {
                misses += 1;
            }
            if let Some(v) = value {
                lo = Some(lo.map_or(v, |l| l.min(v)));
                hi = Some(hi.map_or(v, |h| h.max(v)));
                last = Some(v);
            }
            let held = conds.iter().position(|c| c.met(value, &text));
            streak = match held {
                Some(i) if streak.0 == Some(i) => (Some(i), streak.1 + 1),
                Some(i) => (Some(i), 1),
                None => (None, 0),
            };
            if let Some(c) = held.map(|i| &conds[i]).filter(|c| !c.numeric() || streak.1 >= confirm) {
                let s = start.elapsed().as_secs_f64();
                let what = match (c, value) {
                    (Cond::Shows(q), _) => format!("\"{q}\" showed up"),
                    (_, Some(v)) => format!("read {v} (\"{}\"), meeting {}", uia::clip(&text, 40), cond_text(c)),
                    _ => unreachable!(),
                };
                let range = match (lo, hi) {
                    (Some(l), Some(h)) if l != h => format!("; it ranged {l} – {h}"),
                    _ => String::new(),
                };
                return Ok(Did::new(format!("Watched {s:.1} s, {reads} readings: {what}{range}")));
            }
            last_text = text;
            if start.elapsed() >= timeout {
                let s = start.elapsed().as_secs_f64();
                let seen = match (lo, hi, last) {
                    (Some(l), Some(h), Some(v)) => format!("last {v}, ranged {l} – {h}"),
                    _ => format!("no number could be read (last: \"{}\")", uia::clip(&last_text, 60)),
                };
                let mut unread = if misses > 0 && numeric { format!("; {misses} of {reads} readings had no number") } else { String::new() };
                // The region is read wherever it is on screen: say whose window is over it now, in case that
                // is not the one the AI meant (it was brought up, or the user switched to another)
                let (cx, cy) = (area.left + area.width / 2, area.top + area.height / 2);
                if let Some(w) = winmgr::top_window_at(cx, cy).and_then(winmgr::info) {
                    unread += &format!(". The region is now in \"{}\" ({})", uia::clip(&w.title, 50), w.process);
                }
                return Err(format!("Watched {s:.1} s ({reads} readings) and \"{}\" never held: {seen}{unread}", conds.iter().map(cond_text).collect::<Vec<_>>().join(" | ")));
            }
            if let Some(rest) = every.checked_sub(tick.elapsed()) {
                std::thread::sleep(rest);
            }
        }
    })
    .await;
    match r {
        Ok(Ok(did)) => report::conclude(ctx, args, did, false).await,
        Ok(Err(e)) => fail(e),
        Err(e) => fail(e.to_string()),
    }
}

/// Could two readings a moment apart both be right? Same sign and as many whole digits, or within 10%
fn agree(a: f64, b: f64) -> bool {
    let digits = |v: f64| v.abs().trunc().to_string().len();
    (a.is_sign_negative() == b.is_sign_negative() && digits(a) == digits(b)) || (a - b).abs() <= 0.1 * a.abs().max(b.abs())
}

fn cond_text(c: &Cond) -> String {
    match c {
        Cond::Above(n, true) => format!(">= {n}"),
        Cond::Above(n, false) => format!("> {n}"),
        Cond::Below(n, true) => format!("<= {n}"),
        Cond::Below(n, false) => format!("< {n}"),
        Cond::Equal(n) => format!("= {n}"),
        Cond::Shows(q) => format!("\"{q}\""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_read_with_their_sign_through_what_the_engine_mistakes() {
        assert_eq!(first_number("141.39"), Some(141.39));
        assert_eq!(first_number("+$73"), Some(73.0));
        assert_eq!(first_number("-$89"), Some(-89.0));
        assert_eq!(first_number("一 s69"), Some(-69.0));
        assert_eq!(first_number("一 1 ． 76 ％"), Some(-1.76));
        assert_eq!(first_number("Entry ] 38 ． 83"), Some(138.83));
        assert_eq!(first_number("$13,825"), Some(13825.0));
        assert_eq!(first_number("Net worth"), None);
        assert_eq!(first_number("4 846 · 54"), Some(4846.54));
        assert_eq!(first_number("4.846.54"), Some(4846.54));
        assert_eq!(first_number("4'846.54"), Some(4846.54));
        assert_eq!(first_number("4 》 836 · 06"), Some(4836.06));
        assert_eq!(first_number("+$73 +1.85%"), Some(73.0));
        assert_eq!(first_number("-$124 -4.89%"), Some(-124.0));
    }

    #[test]
    fn conditions_take_either_side_and_text() {
        let c: Vec<Cond> = ">= 141 | <133 | Bought".split('|').map(|s| Cond::parse(s).unwrap()).collect();
        assert!(c[0].met(Some(141.0), ""));
        assert!(!c[0].met(Some(140.9), ""));
        assert!(c[1].met(Some(132.0), ""));
        assert!(!c[1].met(Some(133.0), ""));
        assert!(c[2].met(None, "Bought 28 ROBO"));
        assert!(Cond::parse("> abc").is_err());
    }

    #[test]
    fn a_misread_disagrees_with_the_reading_before() {
        assert!(agree(4831.6, 4820.27));
        assert!(agree(-48.0, -20.0));
        assert!(agree(21.0, 22.5));
        assert!(!agree(4831.6, 4.0));
        assert!(!agree(4820.27, 816.934));
        assert!(!agree(73.0, -73.0));
    }
}
