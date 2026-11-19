//! Waiting for something to be so, instead of for a while.
//!
//! An action's wait ends the moment what it waits for is so: a field holds the text, a control has the focus,
//! a window is up. The duration given is only how long to keep trying before giving up, never how long to
//! wait. The condition is checked at once, then again after 5 ms, 10, 20 ... at most every 50 ms, so what is
//! already so costs nothing and what is quick is seen quickly.

use std::time::{Duration, Instant};

const FIRST: Duration = Duration::from_millis(5);
const EVERY_MAX: Duration = Duration::from_millis(50);

/// `f` until it gives Some, for at most `max`: that value, or None when `max` ran out first
pub fn until<T>(max: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let start = Instant::now();
    let mut every = FIRST;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        let left = max.saturating_sub(start.elapsed());
        if left.is_zero() {
            return None;
        }
        std::thread::sleep(every.min(left));
        every = (every * 2).min(EVERY_MAX);
    }
}

/// `f` until it is true, for at most `max`: whether it became so
pub fn holds(max: Duration, mut f: impl FnMut() -> bool) -> bool {
    until(max, || f().then_some(())).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_already_so_costs_nothing() {
        let t = Instant::now();
        assert!(holds(Duration::from_secs(5), || true));
        assert!(t.elapsed() < Duration::from_millis(5));
    }

    #[test]
    fn what_becomes_so_is_seen_soon_after() {
        let t = Instant::now();
        let n = until(Duration::from_secs(5), || (t.elapsed() >= Duration::from_millis(40)).then_some(7));
        assert_eq!(n, Some(7));
        assert!(t.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn what_never_is_gives_up_at_the_limit() {
        let t = Instant::now();
        assert!(!holds(Duration::from_millis(60), || false));
        let e = t.elapsed();
        assert!(e >= Duration::from_millis(60) && e < Duration::from_millis(150));
    }
}
