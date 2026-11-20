//! recover: what a program's step (a plan's too) does here, without a round trip, when the text it names fits
//! several controls or none.
//!
//! Before anyone on the controller's side (the decision model, or the AI) has to step in, the step tries what
//! needs no judgment, in the order a person would:
//! 1. A popup in the way that the program or plan said how to handle (`on`): press what it says, try again.
//! 2. Nothing showing that name right after an input that didn't wait for the screen to rest: give it a moment.
//! 3. Still unresolved: the candidates to choose among go back with the miss — the equal matches, then the
//!    controls of the window most like the text, then the rest of the right kind, at most 40.
//!
//! A click, type_text or set_value naming its target by text takes part (see `Wish::of`); a step may say how with
//! `recover: {kind: "click" | "field", late: bool}` (`late: false` when it has its own timeout, which then is all
//! the waiting it gets), or `recover: false`.

use super::*;
use crate::uia::Node;
use locate::{Ask, Cand, MissKind};
use std::collections::HashSet;

/// Most candidates handed back
const MAX_CANDS: usize = 40;
/// Most of them chosen for being like the text; the rest are just the right kind of control
const MAX_NAMED: usize = 8;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    /// Something to press
    Click,
    /// Something to put a value in
    Field,
}

impl Kind {
    fn fits(self, role: &str) -> bool {
        match self {
            Kind::Click => matches!(role, "Button" | "SplitButton" | "Link" | "MenuItem" | "TabItem" | "ListItem" | "TreeItem" | "DataItem" | "CheckBox" | "RadioButton" | "Hyperlink"),
            Kind::Field => matches!(role, "Edit" | "ComboBox" | "Document" | "Spinner" | "CheckBox" | "RadioButton" | "Slider"),
        }
    }
}

/// How a step asks to be recovered, if it does
pub struct Wish {
    pub kind: Kind,
    pub late: bool,
}

impl Wish {
    /// How `step` (a call of `tool`) is recovered: as it says (`recover: {kind, late}`), not at all
    /// (`recover: false`, or nothing named by text), or else by what the tool does to its target
    pub fn of(tool: &str, step: &Value) -> Option<Wish> {
        match step.get("recover") {
            Some(Value::Bool(false)) => None,
            Some(r) if r.is_object() => {
                let kind = if r.get("kind").and_then(Value::as_str) == Some("field") { Kind::Field } else { Kind::Click };
                Some(Wish { kind, late: r.get("late").and_then(Value::as_bool).unwrap_or(true) })
            }
            _ => {
                let target = step.get("target")?.as_str()?.trim();
                // A reference is exact: there is nothing to choose among
                let reference = target.strip_prefix('e').is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
                if target.is_empty() || reference {
                    return None;
                }
                match tool {
                    "click" => Some(Wish { kind: Kind::Click, late: true }),
                    "type_text" | "set_value" => Some(Wish { kind: Kind::Field, late: true }),
                    _ => None,
                }
            }
        }
    }
}

/// Lower case, runs of white space (and invisible marks) as one space
fn fold(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut space = false;
    for c in lower.chars() {
        if c.is_whitespace() || ('\u{200b}'..='\u{200f}').contains(&c) || ('\u{202a}'..='\u{202e}').contains(&c) {
            space = !out.is_empty();
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
        }
    }
    out
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

/// Words of two letters or more, and every pair of neighbouring characters
fn grams(s: &str) -> HashSet<String> {
    let t = fold(s);
    let mut g: HashSet<String> = words(&t).into_iter().filter(|w| w.chars().count() > 1).collect();
    let c: Vec<char> = t.chars().collect();
    for w in c.windows(2) {
        g.insert(w.iter().collect());
    }
    g
}

/// How much a control's name has in common with `want`; whole words count for more than letters inside other
/// words ("To" in "To recipients", not in "Toggle"), and a name that starts with them most
fn likeness(name: &str, want: &str) -> f64 {
    let w = grams(want);
    let want_words = words(&fold(want));
    let mut s = 0.0;
    for g in grams(name) {
        if w.contains(&g) {
            s += g.chars().count() as f64;
        }
    }
    let n = fold(name);
    let own: HashSet<String> = words(&n).into_iter().collect();
    for x in &want_words {
        if own.contains(x) {
            s += 4.0;
        }
    }
    if !want_words.is_empty() && n.starts_with(&want_words.join(" ")) {
        s += 6.0;
    }
    s
}

/// The candidates worth choosing among for `want`: the closest by name, then controls of the right kind
pub fn shortlist(nodes: &[Node], want: &str, kind: Kind, skip: &HashSet<u32>) -> Vec<Cand> {
    let mut scored: Vec<(f64, usize)> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| !skip.contains(&n.rf) && (!n.name.is_empty() || kind.fits(n.role)))
        .map(|(i, n)| (likeness(&n.name, want) + if kind.fits(n.role) { 0.5 } else { 0.0 }, i))
        .filter(|(s, _)| *s >= 1.0)
        .collect();
    // Stable: equal scores keep reading order
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let named: Vec<usize> = scored.into_iter().take(MAX_NAMED).map(|(_, i)| i).collect();
    let mut out: Vec<Cand> = named.iter().map(|&i| Cand::of(&nodes[i])).collect();
    for (i, n) in nodes.iter().enumerate() {
        if out.len() >= MAX_CANDS {
            break;
        }
        if kind.fits(n.role) && !n.name.is_empty() && !skip.contains(&n.rf) && !named.contains(&i) {
            out.push(Cand::of(n));
        }
    }
    out
}

/// The window a step works in, as a handle; the one in front when it names none. None when it names one
/// that isn't there: then the candidates of whatever is in front are not the ones meant.
fn window_of(step: &Value) -> Option<isize> {
    match winmgr::find_arg(step.get("window")) {
        Ok(Some(h)) => Some(h.0 as isize),
        Ok(None) => Some(winmgr::foreground()),
        Err(_) => None,
    }
}

/// Whether `text` shows in the step's window now (once or several times), without waiting
async fn shows(ctx: &Ctx, step: &Value, text: &str) -> bool {
    let mut a = serde_json::json!({ "target": text, "timeoutSec": 0 });
    if let Some(w) = step.get("window") {
        a["window"] = w.clone();
    }
    let Ok(Some(ask)) = Ask::of(ctx, &a) else { return false };
    tokio::task::spawn_blocking(move || match locate::resolve(&ask) {
        Ok(_) => true,
        Err(m) => m.kind == MissKind::Ambiguous,
    })
    .await
    .unwrap_or(false)
}

/// What became of a step that missed
pub struct Outcome {
    /// The step's last reply: ok when one of the tries worked
    pub out: Output,
    /// What was done on the way, one line each, for the plan's report
    pub notes: Vec<String>,
}

/// Recover a step whose reply `out` is a miss. `settled`: the screen was at rest before the step (it came first,
/// or after a wait, or after an input that waited for the screen). `again` runs the step with some arguments
/// changed, the way a program runs a step.
pub async fn recover<F, Fut>(ctx: &Ctx, step: &Value, wish: &Wish, on: &[(String, String)], settled: bool, mut out: Output, mut again: F) -> Outcome
where
    F: FnMut(Value) -> Fut,
    Fut: std::future::Future<Output = Output>,
{
    let mut notes = vec![];
    let kind_of = |o: &Output| match miss_kind(o.data.as_ref()) {
        Some("ambiguous") => Some(MissKind::Ambiguous),
        Some("absent") => Some(MissKind::Absent),
        _ => None,
    };
    // 1. A popup in the way that the plan said how to handle
    for (text, press) in on {
        if kind_of(&out).is_none() {
            return Outcome { out, notes };
        }
        if !shows(ctx, step, text).await {
            continue;
        }
        let mut c = serde_json::json!({ "target": press, "after": "none" });
        if let Some(w) = step.get("window") {
            c["window"] = w.clone();
        }
        if !run_tool(ctx, "click", &c).await.ok {
            continue;
        }
        notes.push(format!("↷ \"{text}\" was in the way: pressed \"{press}\""));
        out = again(step.clone()).await;
        if out.ok {
            return Outcome { out, notes };
        }
    }
    let Some(kind) = kind_of(&out) else { return Outcome { out, notes } };
    // 2. Not there yet rather than called something else: right after an input that didn't wait for the screen,
    // a name that shows nowhere may only be on its way
    if kind == MissKind::Absent && !settled && wish.late {
        let mut s = step.clone();
        s["timeoutSec"] = serde_json::json!(0.8);
        let r = again(s).await;
        if r.ok {
            notes.push("(showed up after a moment)".into());
            return Outcome { out: r, notes };
        }
        if kind_of(&r).is_none() {
            return Outcome { out: r, notes };
        }
        out = r;
    }
    // 3. The candidates to choose among, as the window is now
    let Some(kind) = kind_of(&out) else { return Outcome { out, notes } };
    let named: Vec<Value> = if kind == MissKind::Ambiguous {
        out.data.as_ref().and_then(|d| d["miss"]["candidates"].as_array().cloned()).unwrap_or_default()
    } else {
        vec![]
    };
    let skip: HashSet<u32> = named.iter().filter_map(|c| c["ref"].as_str()?.strip_prefix('e')?.parse().ok()).collect();
    let want = str_arg(step, "target").to_string();
    let Some(hwnd) = window_of(step) else { return Outcome { out, notes } };
    let wk = wish.kind;
    let more = tokio::task::spawn_blocking(move || uia::snapshot(hwnd, false).map(|s| shortlist(&s.nodes, &want, wk, &skip)).unwrap_or_default()).await.unwrap_or_default();
    let all: Vec<Value> = named.into_iter().chain(more.iter().map(Cand::json)).take(MAX_CANDS).collect();
    let win = tokio::task::spawn_blocking(move || winmgr::info(hwnd)).await.ok().flatten();
    let mut data = out.data.take().unwrap_or_else(|| serde_json::json!({}));
    data["miss"] = serde_json::json!({ "kind": kind.as_str(), "candidates": all });
    if let Some(w) = win {
        data["miss"]["window"] = serde_json::json!({ "handle": w.handle, "program": w.process, "title": w.title });
    }
    out.data = Some(data);
    Outcome { out, notes }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(rf: u32, role: &'static str, name: &str) -> Node {
        Node { rf, depth: 0, role, name: name.into(), value: String::new(), rect: (0, 0, 10, 10), flags: 0 }
    }

    #[test]
    fn whole_words_beat_letters_inside_other_words() {
        let nodes = [node(1, "Button", "Toggle"), node(2, "Edit", "To recipients"), node(3, "Edit", "Subject")];
        let c = shortlist(&nodes, "To", Kind::Field, &HashSet::new());
        assert_eq!(c[0].rf, 2);
        // Fields come after, the button is not one
        assert!(c.iter().any(|x| x.rf == 3));
    }

    #[test]
    fn the_equal_matches_already_named_are_left_out_and_the_list_is_capped() {
        let mut nodes: Vec<Node> = (1..=60).map(|i| node(i, "Button", &format!("Item {i}"))).collect();
        nodes.push(node(99, "Button", "Send"));
        let skip: HashSet<u32> = [99].into();
        let c = shortlist(&nodes, "Send", Kind::Click, &skip);
        assert!(c.iter().all(|x| x.rf != 99));
        assert_eq!(c.len(), MAX_CANDS);
    }

    #[test]
    fn labels_say_what_a_control_is_and_how_it_stands() {
        let mut n = node(7, "CheckBox", "Remember me");
        n.flags = crate::uia::flag::CHECKED;
        let c = Cand::of(&n);
        assert_eq!(c.label, "CheckBox \"Remember me\" (checked)");
        assert_eq!(c.json()["ref"], "e7");
    }

    #[test]
    fn a_step_naming_its_target_by_text_is_recovered_by_what_it_does() {
        let kind = |tool: &str, step: Value| Wish::of(tool, &step).map(|w| (w.kind, w.late));
        assert_eq!(kind("click", serde_json::json!({ "target": "Send" })), Some((Kind::Click, true)));
        assert_eq!(kind("set_value", serde_json::json!({ "target": "To" })), Some((Kind::Field, true)));
        assert_eq!(kind("type_text", serde_json::json!({ "target": "Subject", "text": "x" })), Some((Kind::Field, true)));
        // As the step says
        assert_eq!(kind("click", serde_json::json!({ "target": "Send", "recover": { "kind": "field", "late": false } })), Some((Kind::Field, false)));
        // Not at all: asked not to, a reference, a point, a tool without a target
        assert_eq!(kind("click", serde_json::json!({ "target": "Send", "recover": false })), None);
        assert_eq!(kind("click", serde_json::json!({ "target": "e12" })), None);
        assert_eq!(kind("click", serde_json::json!({ "x": 3, "y": 4 })), None);
        assert_eq!(kind("press_keys", serde_json::json!({ "keys": "enter" })), None);
    }

    #[test]
    fn folding_keeps_words_apart() {
        assert_eq!(fold("  Save\u{200b}  As "), "save as");
        assert_eq!(words("message-body, 2"), vec!["message", "body", "2"]);
    }
}
