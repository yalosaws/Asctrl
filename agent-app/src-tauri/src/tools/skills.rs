//! Skills: programs that worked, kept to be run again without planning (see docs/architecture-v2.md, L4).
//!
//! A program sent with exec and `learn: { name, description }` that ends well is kept as a skill: its script
//! (already written over `args`, so its parameters are the names of the args it was given; their values are not
//! kept), the programs it worked in, and for each window it acted in a mark of that window's make-up: the named
//! buttons, menu items, tabs and fields (what the window is built of, not what it shows). exec with `skill: name`
//! runs one again with new args.
//!
//! `skills` finds them: by what they are for (words of the query in the name and description) and by how well
//! their marks fit the window in front now, so "I'm in Outlook's new-mail window" finds the skills made there.
//! Each keeps how its runs went; one that got stuck says where, and learning it again under its name replaces it.
//!
//! Kept on the computer the skills are for, in %LOCALAPPDATA%\Asctrl\skills.json (ASCTRL_SKILLS_FILE elsewhere).

use super::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;

/// Most names in a window's mark
const MARK_MAX: usize = 80;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub script: String,
    /// The args it takes (the names of those it was learned with)
    pub params: Vec<String>,
    /// The programs it worked in
    pub apps: Vec<String>,
    /// For each window it acted in: that window's mark
    pub marks: Vec<Vec<String>>,
    pub runs: u32,
    pub done: u32,
    pub stuck: u32,
    /// Where it last got stuck
    pub problem: Option<String>,
    /// Seconds since 1970
    pub learned: u64,
    pub used: Option<u64>,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn file() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("ASCTRL_SKILLS_FILE") {
        return p.into();
    }
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| std::env::var("USERPROFILE").unwrap_or_default());
    std::path::Path::new(&base).join("Asctrl").join("skills.json")
}

/// Read, change and write the list as one step
static LOCK: Mutex<()> = Mutex::new(());

fn load() -> Vec<Skill> {
    std::fs::read_to_string(file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn store(all: &[Skill]) -> Result<(), String> {
    let f = file();
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("could not keep skills in {}: {e}", d.display()))?;
    }
    let tmp = f.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(all).unwrap_or_default()).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &f).map_err(|e| format!("could not write {}: {e}", f.display()))
}

fn change<R>(f: impl FnOnce(&mut Vec<Skill>) -> R) -> Result<R, String> {
    let _one = LOCK.lock().unwrap();
    let mut all = load();
    let r = f(&mut all);
    store(&all)?;
    Ok(r)
}

pub fn get(name: &str) -> Option<Skill> {
    let _one = LOCK.lock().unwrap();
    load().into_iter().find(|s| s.name.eq_ignore_ascii_case(name))
}

/// Keep `s`, replacing one of the same name (and keeping how that one's runs went)
pub fn learn(mut s: Skill) -> Result<bool, String> {
    change(|all| {
        let old = all.iter().position(|x| x.name.eq_ignore_ascii_case(&s.name));
        s.learned = now();
        if let Some(i) = old {
            let o = all.remove(i);
            (s.runs, s.done, s.stuck, s.used) = (o.runs, o.done, o.stuck, o.used);
        }
        all.push(s);
        old.is_some()
    })
}

/// How a run of skill `name` went: it began, it got stuck (where), or it ended well
pub enum Outcome {
    Began,
    Stuck(String),
    Done,
}

pub fn record(name: &str, o: Outcome) {
    let _ = change(|all| {
        if let Some(s) = all.iter_mut().find(|x| x.name.eq_ignore_ascii_case(name)) {
            match o {
                Outcome::Began => {
                    s.runs += 1;
                    s.used = Some(now());
                }
                Outcome::Stuck(at) => {
                    s.stuck += 1;
                    s.problem = Some(at);
                }
                Outcome::Done => s.done += 1,
            }
        }
    });
}

/// What window `hwnd` is built of: its program, and the names of its buttons, menu items, tabs and fields
pub fn mark(hwnd: isize) -> Option<(String, Vec<String>)> {
    let scene = uia::snapshot(hwnd, false).ok()?;
    let app = winmgr::process_name(winmgr::pid_of(hwnd));
    let mut seen = HashSet::new();
    let mut out = vec![];
    for n in &scene.nodes {
        let kind = match n.role {
            "Button" | "SplitButton" | "MenuItem" | "TabItem" | "CheckBox" | "RadioButton" | "Hyperlink" => "press",
            "Edit" | "ComboBox" | "Document" => "field",
            _ => continue,
        };
        let name = n.name.trim();
        // Long names are content (a message's subject, a file's name), not the window's make-up
        if name.is_empty() || name.chars().count() > 40 || name.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }
        let m = format!("{kind}:{}", name.to_lowercase());
        if seen.insert(m.clone()) {
            out.push(m);
        }
        if out.len() >= MARK_MAX {
            break;
        }
    }
    (!out.is_empty()).then_some((app, out))
}

/// How much two marks have in common: the share of the smaller one found in the other (0..1)
pub fn fit(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let sa: HashSet<&String> = a.iter().collect();
    let common = b.iter().filter(|x| sa.contains(x)).count();
    common as f64 / a.len().min(b.len()) as f64
}

/// How well a skill answers a query: the share of the query's words in its name and description
fn matches(s: &Skill, query: &str) -> f64 {
    let hay = format!("{} {}", s.name, s.description).to_lowercase();
    let words: Vec<String> = query.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect();
    if words.is_empty() {
        return 0.0;
    }
    // Words, or for scripts written without spaces (Chinese), the query's characters
    // Whole words; a longer one also inside a word ("mail" in "emails")
    let own: HashSet<&str> = hay.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let hits = words.iter().filter(|w| own.contains(w.as_str()) || (w.chars().count() >= 4 && hay.contains(w.as_str()))).count() as f64 / words.len() as f64;
    let chars: Vec<char> = query.chars().filter(|c| !c.is_ascii() && c.is_alphanumeric()).collect();
    if chars.is_empty() {
        return hits;
    }
    hits.max(chars.iter().filter(|c| hay.contains(**c)).count() as f64 / chars.len() as f64)
}

fn ago(t: u64) -> String {
    let d = now().saturating_sub(t);
    match d {
        0..=119 => "just now".into(),
        120..=7199 => format!("{} min ago", d / 60),
        7200..=172_799 => format!("{} h ago", d / 3600),
        _ => format!("{} days ago", d / 86_400),
    }
}

/// The skills tool: find, show or forget
pub(super) async fn skills(_ctx: &Ctx, args: &Value) -> Output {
    if let Some(name) = args.get("forget").and_then(Value::as_str) {
        return match change(|all| {
            let n = all.len();
            all.retain(|s| !s.name.eq_ignore_ascii_case(name));
            n != all.len()
        }) {
            Ok(true) => text(format!("Forgot the skill \"{name}\"")),
            Ok(false) => fail(format!("There is no skill \"{name}\"")),
            Err(e) => fail(e),
        };
    }
    if let Some(name) = args.get("show").and_then(Value::as_str) {
        let Some(s) = get(name) else { return fail(format!("There is no skill \"{name}\"")) };
        let mut out = text(format!(
            "Skill \"{}\": {}\nTakes args: {}\nWorks in: {}\nRuns: {} ({} ended well, {} got stuck){}\nScript:\n{}",
            s.name,
            s.description,
            if s.params.is_empty() { "none".into() } else { s.params.join(", ") },
            s.apps.join(", "),
            s.runs,
            s.done,
            s.stuck,
            s.problem.as_ref().map(|p| format!("; last stuck {p}")).unwrap_or_default(),
            s.script
        ));
        out.data = Some(json!({ "skill": s }));
        return out;
    }
    let query = str_arg(args, "query").to_string();
    let window = match winmgr::find_arg(args.get("window")) {
        Ok(Some(h)) => h.0 as isize,
        Ok(None) => winmgr::foreground(),
        Err(e) => return fail(e),
    };
    let (all, here) = tokio::task::spawn_blocking(move || {
        let _one = LOCK.lock().unwrap();
        (load(), mark(window))
    })
    .await
    .unwrap_or_default();
    if all.is_empty() {
        return text("No skills yet. A program sent with exec and learn: { name, description } becomes one when it ends well.".into());
    }
    let mut ranked: Vec<(f64, f64, f64, &Skill)> = all
        .iter()
        .map(|s| {
            let words = if query.is_empty() { 0.0 } else { matches(s, &query) };
            let fits = here.as_ref().map_or(0.0, |(app, m)| {
                let best = s.marks.iter().map(|x| fit(x, m)).fold(0.0, f64::max);
                // The same program at least: a little
                best.max(if s.apps.iter().any(|a| a.eq_ignore_ascii_case(app)) { 0.2 } else { 0.0 })
            });
            (words * 2.0 + fits, words, fits, s)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| b.3.used.cmp(&a.3.used)));
    // A query picks by what they are for (the window only orders them); without one, the window does
    let shown: Vec<&(f64, f64, f64, &Skill)> = ranked.iter().filter(|r| query.is_empty() || r.1 > 0.0).take(int_arg(args, "max", 10, 1, 50) as usize).collect();
    if shown.is_empty() {
        return text(format!("No skill fits \"{query}\" ({} kept; skills with no query lists them by how well they fit the window in front)", all.len()));
    }
    let lines: Vec<String> = shown
        .iter()
        .map(|(_, _, fits, s)| {
            format!(
                "- \"{}\" ({}): {} · works in {} · fits the window in front {:.0}% · {} runs, {} ended well{}{}",
                s.name,
                if s.params.is_empty() { "no args".into() } else { format!("args: {}", s.params.join(", ")) },
                s.description,
                s.apps.join(", "),
                fits * 100.0,
                s.runs,
                s.done,
                s.used.map(|t| format!(", last {}", ago(t))).unwrap_or_default(),
                s.problem.as_ref().map(|p| format!(" · last stuck {p}")).unwrap_or_default()
            )
        })
        .collect();
    let mut out = text(format!("Skills{}, best first (run one with exec skill: name, args):\n{}", if query.is_empty() { String::new() } else { format!(" for \"{query}\"") }, lines.join("\n")));
    out.data = Some(json!({ "skills": shown.iter().map(|(score, words, fits, s)| json!({ "name": s.name, "description": s.description, "params": s.params, "apps": s.apps, "fits": fits, "matches": words, "score": score, "runs": s.runs, "done": s.done, "stuck": s.stuck, "problem": s.problem })).collect::<Vec<_>>() }));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn marks_fit_by_what_the_smaller_one_shares() {
        let compose = m(&["press:send", "field:to", "field:subject", "press:attach"]);
        let same_window_more_buttons = m(&["press:send", "field:to", "field:subject", "press:attach", "press:format", "press:discard"]);
        let other = m(&["press:save", "field:name"]);
        assert_eq!(fit(&compose, &same_window_more_buttons), 1.0);
        assert_eq!(fit(&compose, &other), 0.0);
        assert_eq!(fit(&compose, &[]), 0.0);
    }

    #[test]
    fn a_query_finds_skills_by_their_words_or_characters() {
        let s = Skill { name: "send-mail".into(), description: "Send an email in Outlook; 发送邮件".into(), ..Default::default() };
        assert_eq!(matches(&s, "send email"), 1.0);
        assert_eq!(matches(&s, "send a fax"), 1.0 / 3.0);
        assert_eq!(matches(&s, "发邮件"), 1.0);
        assert_eq!(matches(&s, ""), 0.0);
    }

    #[test]
    fn learning_again_replaces_and_keeps_the_record() {
        let f = std::env::temp_dir().join(format!("asctrl-skills-test-{}.json", std::process::id()));
        // SAFETY: tests in this module are the only ones that read ASCTRL_SKILLS_FILE
        unsafe { std::env::set_var("ASCTRL_SKILLS_FILE", &f) };
        let _ = std::fs::remove_file(&f);
        assert_eq!(learn(Skill { name: "Fill".into(), script: "v1".into(), ..Default::default() }), Ok(false));
        record("fill", Outcome::Began);
        record("fill", Outcome::Stuck("at line 2".into()));
        assert_eq!(learn(Skill { name: "fill".into(), script: "v2".into(), ..Default::default() }), Ok(true));
        let s = get("FILL").unwrap();
        assert_eq!((s.script.as_str(), s.runs, s.stuck), ("v2", 1, 1));
        assert!(s.problem.is_none(), "the new version has not got stuck");
        let _ = std::fs::remove_file(&f);
    }
}
