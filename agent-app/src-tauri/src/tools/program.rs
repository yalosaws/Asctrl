//! Programs (exec / resume): the AI writes a short JavaScript program, the driver runs it here, on the computer
//! it works, and the AI comes back only when the program needs it.
//!
//! A plan (`run`) is a straight list of steps; a program has the language: loops over rows, ifs on what the
//! screen shows, its own variables, functions, try / catch. It runs in QuickJS on a thread of its own, with
//! nothing of the computer's but what is given to it below: no files, no network, no processes (those are
//! tools too, and go through the same calls as the AI's). Each action it takes is a driver tool, run exactly as
//! when the AI calls it (the overlay shows it, the page channel and recovery apply).
//!
//! When an action fails, the program does not end: it stops at that line and waits (stuck). The AI looks,
//! fixes what it must with single actions, and resumes it: retry the action, skip it (with a value for the
//! program), do another action instead, or abort (the program gets an error it may catch). A program can also
//! ask a question itself (`ask`) and goes on with the answer. Failures it means to handle itself it calls with
//! `attempt`, which returns the reply instead.
//!
//! The API, as globals (actions return the tool's reply: { ok, text, error, data }):
//!   click(target, opts) · press(target) · type(target, text, opts) (target null: where the focus is)
//!   fill({ label: value, … }) · keys("ctrl+s") · open(appOrUrl, opts) · waitFor(text, opts) · waitGone(text)
//!   expect(text, opts) (fails, so it stops there, when the text doesn't show within 5 s) · shows(text) → bool
//!   read(windowOrOpts) → text · look(windowOrOpts) → observe's reply · tool(name, args) · attempt(name, args)
//!   ask(question, details) → the answer · sleep(ms) · log(…) · args (what exec was given)
//! opts are the tool's own arguments (window, nth, role, timeoutSec, browser, …).

use super::*;
use rquickjs::{Context, Function, Runtime};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as smpsc;
use std::time::Instant;

const PRELUDE: &str = r#"(() => {
  const host = globalThis.__host; delete globalThis.__host;
  const where = () => { const m = /program:(\d+)/.exec(String(new Error().stack || '')); return m ? +m[1] : null; };
  const call = (op, a) => {
    const r = JSON.parse(host(op, JSON.stringify(a === undefined ? null : a)));
    if (r && typeof r === 'object' && r.__error !== undefined) { const e = new Error(r.__error); e.result = r.result; throw e; }
    return r;
  };
  const step = (name, a, lenient) => call('tool', { name, args: a, line: where(), lenient: !!lenient });
  const quiet = o => Object.assign({ after: 'none' }, o);
  const win = o => typeof o === 'string' || typeof o === 'number' ? { window: o } : o;
  globalThis.tool = (name, a = {}) => step(name, a, false);
  globalThis.attempt = (name, a = {}) => step(name, a, true);
  globalThis.click = (target, o = {}) => step('click', quiet({ target, ...o }));
  globalThis.press = (target, o = {}) => step('element_action', quiet({ target, action: 'press', ...o }));
  globalThis.type = (target, text, o = {}) => step('type_text', quiet(target == null ? { text, ...o } : { target, text, ...o }));
  globalThis.fill = (fields, o = {}) => Object.entries(fields).map(([k, v]) => step('set_value', quiet({ target: k, value: v, ...o })));
  globalThis.keys = (k, o = {}) => step('press_keys', quiet({ keys: k, ...o }));
  globalThis.open = (target, o = {}) => step('open_app', quiet({ target, ...o }));
  globalThis.waitFor = (text, o = {}) => step('wait_for', quiet({ appears: text, ...o }));
  globalThis.waitGone = (text, o = {}) => step('wait_for', quiet({ appears: text, gone: true, ...o }));
  globalThis.expect = (text, o = {}) => step('wait_for', quiet({ appears: text, timeoutSec: 5, ...o }));
  globalThis.shows = (text, o = {}) => step('wait_for', quiet({ appears: text, timeoutSec: 0, ...o }), true).ok;
  globalThis.read = (o = {}) => step('read_text', win(o)).text;
  globalThis.look = (o = {}) => step('observe', win(o));
  globalThis.sleep = ms => call('sleep', ms);
  globalThis.log = (...a) => call('log', a.map(x => typeof x === 'string' ? x : JSON.stringify(x)).join(' '));
  globalThis.ask = (question, details) => call('ask', { question, details: details === undefined ? null : details, line: where() });
  globalThis.console = { log: globalThis.log, info: globalThis.log, warn: globalThis.log, error: globalThis.log };
})();"#;

/// Programs running (or waiting) at once
const MAX_LIVE: usize = 4;
/// Programs kept after they end, to report on
const KEEP_ENDED: usize = 16;
/// Lines of log kept per program
const LOG_KEEP: usize = 200;
/// How long a stuck or asking program waits for the AI before it gives up
const DECISION_MAX: Duration = Duration::from_secs(30 * 60);
/// Most of a reply's text a program is handed
const TEXT_MAX: usize = 20_000;

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Phase {
    #[default]
    Running,
    /// An action failed: waiting for the AI to say what to do
    Stuck,
    /// The program asked a question
    Asking,
    Done,
    Failed,
    Stopped,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Phase::Running => "running",
            Phase::Stuck => "stuck",
            Phase::Asking => "asking",
            Phase::Done => "done",
            Phase::Failed => "failed",
            Phase::Stopped => "stopped",
        }
    }

    fn waiting(self) -> bool {
        matches!(self, Phase::Stuck | Phase::Asking)
    }

    fn over(self) -> bool {
        matches!(self, Phase::Done | Phase::Failed | Phase::Stopped)
    }
}

#[derive(Default)]
struct Status {
    phase: Phase,
    log: VecDeque<String>,
    /// Actions taken
    actions: u32,
    /// The program's line of the action now running, stuck or asking
    line: Option<u64>,
    result: Option<Value>,
    error: Option<String>,
    /// The action it is stuck on, { tool, args }, and that action's reply
    call: Option<Value>,
    failure: Option<Value>,
    /// What it asks, { question, details }
    question: Option<Value>,
    ended: Option<Instant>,
}

/// What the AI says to a waiting program
#[derive(Debug)]
pub enum Decision {
    Retry,
    Skip(Value),
    Instead(String, Value),
    Answer(Value),
    Abort(String),
}

pub struct Program {
    id: u32,
    started: Instant,
    status: Mutex<Status>,
    changed: tokio::sync::Notify,
    decide: Mutex<Option<smpsc::Sender<Decision>>>,
    stop: Arc<AtomicBool>,
}

impl Program {
    fn set(&self, f: impl FnOnce(&mut Status)) {
        f(&mut self.status.lock().unwrap());
        self.changed.notify_waiters();
    }

    fn phase(&self) -> Phase {
        self.status.lock().unwrap().phase
    }

    fn log(&self, line: String) {
        let mut s = self.status.lock().unwrap();
        if s.log.len() == LOG_KEEP {
            s.log.pop_front();
        }
        s.log.push_back(line);
    }
}

#[derive(Default)]
pub struct Programs {
    next: u32,
    map: HashMap<u32, Arc<Program>>,
}

/// An action the program asks the driver to take, and where its reply goes
struct Req {
    tool: String,
    args: Value,
    reply: smpsc::Sender<Value>,
}

/// A tool's reply as the program sees it
fn as_js(out: &Output) -> Value {
    let clip = |s: &Option<String>| s.as_ref().map(|t| t.chars().take(TEXT_MAX).collect::<String>());
    json!({ "ok": out.ok, "text": clip(&out.text), "error": clip(&out.error), "data": out.data, "image": out.image.is_some() })
}

/// What the program's host calls do. Blocking: they run on the program's own thread.
struct Host {
    p: Arc<Program>,
    tools: tokio::sync::mpsc::UnboundedSender<Req>,
    decisions: smpsc::Receiver<Decision>,
    deadline: Instant,
}

impl Host {
    fn wait_decision(&self) -> Option<Decision> {
        let left = self.deadline.saturating_duration_since(Instant::now()).min(DECISION_MAX);
        let start = Instant::now();
        // In steps, so a stop is noticed
        while start.elapsed() < left {
            if self.p.stop.load(Ordering::SeqCst) {
                return None;
            }
            match self.decisions.recv_timeout(Duration::from_millis(200)) {
                Ok(d) => return Some(d),
                Err(smpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => return None,
            }
        }
        None
    }

    fn call(&self, op: &str, a: Value) -> Value {
        match op {
            "log" => {
                self.p.log(a.as_str().unwrap_or("").chars().take(2000).collect());
                Value::Null
            }
            "sleep" => {
                let end = Instant::now() + Duration::from_millis(a.as_u64().unwrap_or(0).min(3_600_000));
                while Instant::now() < end && !self.p.stop.load(Ordering::SeqCst) {
                    std::thread::sleep(end.saturating_duration_since(Instant::now()).min(Duration::from_millis(50)));
                }
                Value::Null
            }
            "ask" => {
                self.p.set(|s| {
                    s.phase = Phase::Asking;
                    s.line = a["line"].as_u64();
                    s.question = Some(json!({ "question": a["question"], "details": a["details"] }));
                });
                let d = self.wait_decision();
                self.p.set(|s| {
                    s.phase = Phase::Running;
                    s.question = None;
                });
                match d {
                    Some(Decision::Answer(v)) | Some(Decision::Skip(v)) => v,
                    Some(Decision::Abort(why)) => json!({ "__error": why }),
                    Some(_) => Value::Null,
                    None => json!({ "__error": "no answer came" }),
                }
            }
            "tool" => self.tool(a["name"].as_str().unwrap_or("").to_string(), a["args"].clone(), a["line"].as_u64(), a["lenient"] == true),
            other => json!({ "__error": format!("no such call: {other}") }),
        }
    }

    fn tool(&self, mut name: String, mut args: Value, line: Option<u64>, lenient: bool) -> Value {
        loop {
            if self.p.stop.load(Ordering::SeqCst) {
                return json!({ "__error": "the program was stopped" });
            }
            if matches!(name.as_str(), "exec" | "resume") {
                return json!({ "__error": format!("a program can't call {name}") });
            }
            self.p.set(|s| s.line = line);
            let (tx, rx) = smpsc::channel();
            if self.tools.send(Req { tool: name.clone(), args: args.clone(), reply: tx }).is_err() {
                return json!({ "__error": "the session has ended" });
            }
            let Ok(out) = rx.recv() else { return json!({ "__error": "the session has ended" }) };
            self.p.set(|s| s.actions += 1);
            if out["ok"] == true || lenient {
                return out;
            }
            // Stuck at this line until the AI says what to do
            let call = json!({ "tool": name, "args": args });
            self.p.set(|s| {
                s.phase = Phase::Stuck;
                s.call = Some(call);
                s.failure = Some(out.clone());
            });
            let d = self.wait_decision();
            self.p.set(|s| {
                s.phase = Phase::Running;
                s.call = None;
                s.failure = None;
            });
            match d {
                Some(Decision::Retry) | Some(Decision::Answer(_)) => continue,
                Some(Decision::Instead(t, a)) => (name, args) = (t, a),
                Some(Decision::Skip(v)) => return json!({ "ok": true, "skipped": true, "value": v, "text": "skipped" }),
                Some(Decision::Abort(why)) => return json!({ "__error": why, "result": out }),
                None => return json!({ "__error": "no decision came for the action that failed", "result": out }),
            }
        }
    }
}

/// How a program ended, as the engine saw it
#[derive(Debug, PartialEq)]
pub enum End {
    Done(Value),
    Failed { error: String, line: Option<u64> },
    /// Stopped, or out of time, mid-way
    Interrupted,
}

/// Run `src` with `input` as `args`. `host(op, json)` answers the program's calls (see PRELUDE). Blocking.
pub fn run_js(src: &str, input: &Value, deadline: Instant, stop: Arc<AtomicBool>, host: impl Fn(&str, Value) -> Value + 'static) -> End {
    let Ok(rt) = Runtime::new() else { return End::Failed { error: "could not start the script engine".into(), line: None } };
    rt.set_memory_limit(64 << 20);
    rt.set_max_stack_size(512 << 10);
    let halt = stop.clone();
    rt.set_interrupt_handler(Some(Box::new(move || halt.load(Ordering::SeqCst) || Instant::now() > deadline)));
    let Ok(cx) = Context::full(&rt) else { return End::Failed { error: "could not start the script engine".into(), line: None } };
    // Line 1 of the wrapped source is line 1 of the program: its errors and stack name the AI's own lines
    let wrapped = format!(
        "globalThis.__done = null; (async () => {{{src}\n}})().then(v => {{ try {{ __done = JSON.stringify({{ ok: true, value: v === undefined ? null : v }}); }} catch (e) {{ __done = JSON.stringify({{ ok: true, value: String(v) }}); }} }}, e => {{ __done = JSON.stringify({{ ok: false, error: String(e && e.message !== undefined ? e.message : e), stack: String(e && e.stack || '') }}); }});"
    );
    // An interrupt (stop, deadline) reaches the program as an error it can't catch: it was stopped, not failed
    let stopped = stop.clone();
    let failed = move |e: String, stack: String| {
        if stopped.load(Ordering::SeqCst) || Instant::now() > deadline {
            End::Interrupted
        } else {
            End::Failed { line: line_of(&stack), error: e }
        }
    };
    let started = cx.with(|c| -> Result<(), End> {
        let f = Function::new(c.clone(), move |op: String, a: String| -> String {
            let v = serde_json::from_str(&a).unwrap_or(Value::Null);
            host(&op, v).to_string()
        })
        .map_err(|e| End::Failed { error: e.to_string(), line: None })?;
        c.globals().set("__host", f).map_err(|e| End::Failed { error: e.to_string(), line: None })?;
        let opts = |name: &str| {
            let mut o = rquickjs::context::EvalOptions::default();
            o.filename = Some(name.into());
            o.strict = false;
            o
        };
        let exc = |c: &rquickjs::Ctx| {
            let v = c.catch();
            match v.as_exception() {
                Some(x) => (x.message().unwrap_or_default(), x.stack().unwrap_or_default()),
                None => (format!("{v:?}"), String::new()),
            }
        };
        if c.eval_with_options::<(), _>(PRELUDE, opts("prelude")).is_err() {
            let (m, _) = exc(&c);
            return Err(End::Failed { error: format!("the program's library failed: {m}"), line: None });
        }
        if c.eval_with_options::<(), _>(format!("globalThis.args = {};", input), opts("args")).is_err() {
            return Err(End::Failed { error: "args could not be given to the program".into(), line: None });
        }
        if c.eval_with_options::<(), _>(wrapped, opts("program")).is_err() {
            let (m, s) = exc(&c);
            return Err(failed(m, s));
        }
        Ok(())
    });
    if let Err(e) = started {
        return e;
    }
    while rt.is_job_pending() {
        if rt.execute_pending_job().is_err() {
            break;
        }
    }
    let done: Option<String> = cx.with(|c| c.globals().get::<_, Option<String>>("__done").ok().flatten());
    let Some(done) = done else { return End::Interrupted };
    let v: Value = serde_json::from_str(&done).unwrap_or(Value::Null);
    if v["ok"] == true {
        End::Done(v["value"].clone())
    } else {
        failed(v["error"].as_str().unwrap_or("failed").to_string(), v["stack"].as_str().unwrap_or("").to_string())
    }
}

/// The program's line an error's stack points at
fn line_of(stack: &str) -> Option<u64> {
    let i = stack.find("program:")?;
    stack[i + 8..].split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

fn secs(args: &Value, key: &str, def: f64, min: f64, max: f64) -> Duration {
    Duration::from_secs_f64(args.get(key).and_then(Value::as_f64).unwrap_or(def).clamp(min, max))
}

/// Start a program
pub(super) async fn exec(ctx: &Ctx, args: &Value) -> Output {
    let src = str_arg(args, "script").to_string();
    if src.trim().is_empty() {
        return fail("script is empty");
    }
    let Some(me) = ctx.me.upgrade() else { return fail("no session to run a program in") };
    let input = args.get("args").cloned().unwrap_or(Value::Null);
    let wait = secs(args, "waitSec", 30.0, 0.0, 300.0);
    let life = secs(args, "timeoutSec", 600.0, 1.0, 3600.0);
    let p = {
        let mut ps = ctx.programs.lock().unwrap();
        if ps.map.values().filter(|p| !p.phase().over()).count() >= MAX_LIVE {
            return fail(format!("{MAX_LIVE} programs are already running or waiting: resume or stop one first"));
        }
        // Forget the oldest ended ones
        let mut ended: Vec<(u32, Instant)> = ps.map.values().filter_map(|p| p.status.lock().unwrap().ended.map(|t| (p.id, t))).collect();
        ended.sort_by_key(|(_, t)| *t);
        for (id, _) in ended.iter().take(ended.len().saturating_sub(KEEP_ENDED - 1)) {
            ps.map.remove(id);
        }
        ps.next += 1;
        let p = Arc::new(Program { id: ps.next, started: Instant::now(), status: Mutex::default(), changed: tokio::sync::Notify::new(), decide: Mutex::new(None), stop: Arc::new(AtomicBool::new(false)) });
        ps.map.insert(p.id, p.clone());
        p
    };
    let (dtx, drx) = smpsc::channel();
    *p.decide.lock().unwrap() = Some(dtx);
    let (ttx, mut trx) = tokio::sync::mpsc::unbounded_channel::<Req>();
    // The program's actions, run on the session as the AI's are
    {
        let p = p.clone();
        tokio::spawn(async move {
            let mut n = 0;
            while let Some(r) = trx.recv().await {
                n += 1;
                let out = batch::run_step(&me, &r.tool, &r.args, format!("p{}-{n}", p.id), false).await;
                let _ = r.reply.send(as_js(&out));
            }
        });
    }
    {
        let p = p.clone();
        let deadline = Instant::now() + life;
        let pt = p.clone();
        let spawned = std::thread::Builder::new().name(format!("asctrl-program-{}", p.id)).stack_size(8 << 20).spawn(move || {
            let p = pt;
            let host = Host { p: p.clone(), tools: ttx, decisions: drx, deadline };
            let stop = p.stop.clone();
            let end = run_js(&src, &input, deadline, stop, move |op, a| host.call(op, a));
            let timed_out = Instant::now() > deadline;
            p.set(|s| {
                s.ended = Some(Instant::now());
                match end {
                    End::Done(v) => {
                        s.phase = Phase::Done;
                        s.result = Some(v);
                    }
                    End::Failed { error, line } => {
                        s.phase = Phase::Failed;
                        s.error = Some(error);
                        s.line = line.or(s.line);
                    }
                    End::Interrupted => {
                        s.phase = Phase::Stopped;
                        s.error = Some(if timed_out { format!("out of time ({} s)", life.as_secs()) } else { "stopped".into() });
                    }
                }
            });
        });
        if let Err(e) = spawned {
            p.set(|s| {
                s.phase = Phase::Failed;
                s.error = Some(format!("could not start the program: {e}"));
                s.ended = Some(Instant::now());
            });
        }
    }
    report(&p, wait).await
}

/// Answer a waiting program, or wait for one again
pub(super) async fn resume(ctx: &Ctx, args: &Value) -> Output {
    let id = match args.get("id") {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0) as u32,
        Some(Value::String(s)) => s.trim_start_matches(['p', 'P']).parse().unwrap_or(0),
        _ => 0,
    };
    let Some(p) = ctx.programs.lock().unwrap().map.get(&id).cloned() else { return fail(format!("There is no program {id}")) };
    let wait = secs(args, "waitSec", 30.0, 0.0, 300.0);
    if bool_arg(args, "stop") == Some(true) {
        p.stop.store(true, Ordering::SeqCst);
        return report(&p, wait.max(Duration::from_secs(2))).await;
    }
    let decision = if let Some(a) = args.get("answer").filter(|v| !v.is_null()) {
        Some(Decision::Answer(a.clone()))
    } else if bool_arg(args, "retry") == Some(true) {
        Some(Decision::Retry)
    } else if bool_arg(args, "skip") == Some(true) {
        Some(Decision::Skip(args.get("value").cloned().unwrap_or(Value::Null)))
    } else if let Some(i) = args.get("instead").filter(|v| v.is_object()) {
        let tool = i["tool"].as_str().unwrap_or("").to_string();
        if tool.is_empty() {
            return fail("instead: give { tool, args }");
        }
        Some(Decision::Instead(tool, i.get("args").cloned().unwrap_or(json!({}))))
    } else if let Some(a) = args.get("abort").filter(|v| !v.is_null() && **v != false) {
        Some(Decision::Abort(a.as_str().map(str::to_string).unwrap_or_else(|| "aborted by the AI".into())))
    } else {
        None
    };
    if let Some(d) = decision {
        let phase = p.phase();
        if !phase.waiting() {
            return fail(format!("Program {id} is {}, not waiting for a decision", phase.as_str()));
        }
        let fits = matches!((&d, phase), (Decision::Answer(_), Phase::Asking) | (Decision::Abort(_), _) | (Decision::Skip(_), _) | (Decision::Retry | Decision::Instead(..), Phase::Stuck));
        if !fits {
            return fail(match phase {
                Phase::Asking => format!("Program {id} asked a question: give answer (or abort)"),
                _ => format!("Program {id} is stuck on an action: give retry, skip (with value), instead {{ tool, args }} or abort"),
            });
        }
        let sent = p.decide.lock().unwrap().as_ref().map(|tx| tx.send(d).is_ok()).unwrap_or(false);
        if !sent {
            return fail(format!("Program {id} has ended"));
        }
        // Let it take the decision before waiting for what it does next
        let start = Instant::now();
        while p.phase().waiting() && start.elapsed() < Duration::from_secs(1) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    report(&p, wait).await
}

/// Wait up to `wait` for the program to end or to need the AI, then say where it stands
async fn report(p: &Program, wait: Duration) -> Output {
    let deadline = Instant::now() + wait;
    loop {
        let n = p.changed.notified();
        tokio::pin!(n);
        n.as_mut().enable();
        let phase = p.phase();
        if phase != Phase::Running || Instant::now() >= deadline {
            break;
        }
        let _ = tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), n).await;
    }
    let s = p.status.lock().unwrap();
    let secs = s.ended.unwrap_or_else(Instant::now).duration_since(p.started).as_secs_f64();
    let at = s.line.map(|l| format!(" at line {l}")).unwrap_or_default();
    let mut text = format!("Program {} is {} after {secs:.1} s, {} action(s).", p.id, s.phase.as_str(), s.actions);
    match s.phase {
        Phase::Done => text += &format!("\nResult: {}", uia::clip(&s.result.clone().unwrap_or(Value::Null).to_string(), 4000)),
        Phase::Failed | Phase::Stopped => text += &format!("\nError{at}: {}", s.error.as_deref().unwrap_or("")),
        Phase::Stuck => {
            let call = s.call.clone().unwrap_or_default();
            let f = s.failure.clone().unwrap_or_default();
            let why = f["error"].as_str().or(f["text"].as_str()).unwrap_or("");
            text += &format!(
                "\nStuck{at}: {} {} failed:\n{}\nIt waits there. Fix what is needed (single actions), then resume it with retry, skip (value: what the program gets back), instead {{ tool, args }}, or abort (the program gets an error it may catch).",
                call["tool"].as_str().unwrap_or(""),
                uia::clip(&call["args"].to_string(), 300),
                uia::clip(why, 3000)
            );
        }
        Phase::Asking => {
            let q = s.question.clone().unwrap_or_default();
            let details = if q["details"].is_null() { String::new() } else { format!("\nDetails: {}", uia::clip(&q["details"].to_string(), 3000)) };
            text += &format!("\nIt asks{at}: {}{details}\nAnswer with resume (id, answer).", q["question"].as_str().map(str::to_string).unwrap_or_else(|| q["question"].to_string()));
        }
        Phase::Running => text += &format!("\nStill running{at}: resume (id) waits for it again; resume (id, stop: true) stops it."),
    }
    if !s.log.is_empty() {
        let from = s.log.len().saturating_sub(20);
        text += &format!("\nLog{}:\n{}", if from > 0 { format!(" (last 20 of {})", s.log.len()) } else { String::new() }, s.log.iter().skip(from).cloned().collect::<Vec<_>>().join("\n"));
    }
    let data = json!({
        "program": {
            "id": p.id, "state": s.phase.as_str(), "line": s.line, "actions": s.actions, "result": s.result, "error": s.error,
            "call": s.call, "failure": s.failure, "question": s.question, "log": s.log.iter().cloned().collect::<Vec<_>>(),
        }
    });
    Output { ok: !matches!(s.phase, Phase::Failed | Phase::Stopped), text: Some(text), data: Some(data), ..Default::default() }
}

/// Stop every program (the controller left)
pub(super) fn stop_all(ctx: &Ctx) {
    for p in ctx.programs.lock().unwrap().map.values() {
        p.stop.store(true, Ordering::SeqCst);
        if let Some(tx) = p.decide.lock().unwrap().as_ref() {
            let _ = tx.send(Decision::Abort("the session ended".into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str, host: impl Fn(&str, Value) -> Value + 'static) -> End {
        run_js(src, &json!({ "who": "Ann" }), Instant::now() + Duration::from_secs(5), Arc::new(AtomicBool::new(false)), host)
    }

    #[test]
    fn a_program_returns_what_it_returns_and_reads_its_args() {
        assert_eq!(run("return 'hi ' + args.who;", |_, _| Value::Null), End::Done(json!("hi Ann")));
        assert_eq!(run("let n = 0; for (const x of [1, 2, 3]) n += x; return { n };", |_, _| Value::Null), End::Done(json!({ "n": 6 })));
    }

    #[test]
    fn actions_go_to_the_host_and_their_replies_come_back() {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
        let s = seen.clone();
        let end = run("const r = click('Save', { window: 'Notepad' });\nreturn r.text;", move |op, a| {
            s.borrow_mut().push((op.to_string(), a.clone()));
            json!({ "ok": true, "text": "Clicked button \"Save\"" })
        });
        assert_eq!(end, End::Done(json!("Clicked button \"Save\"")));
        let seen = seen.borrow();
        assert_eq!(seen[0].0, "tool");
        assert_eq!(seen[0].1["name"], "click");
        assert_eq!(seen[0].1["args"], json!({ "after": "none", "target": "Save", "window": "Notepad" }));
        assert_eq!(seen[0].1["line"], 1, "the program's own line");
    }

    #[test]
    fn an_error_names_the_programs_line() {
        match run("const a = 1;\nconst b = 2;\nnope();", |_, _| Value::Null) {
            End::Failed { error, line } => {
                assert!(error.contains("nope"), "{error}");
                assert_eq!(line, Some(3));
            }
            e => panic!("{e:?}"),
        }
        // A host call aborted: an error the program may catch
        let end = run("try { click('x'); } catch (e) { return 'caught ' + e.message; }", |_, _| json!({ "__error": "aborted by the AI" }));
        assert_eq!(end, End::Done(json!("caught aborted by the AI")));
    }

    #[test]
    fn a_program_that_never_ends_is_stopped_at_its_deadline() {
        let end = run_js("while (true) {}", &Value::Null, Instant::now() + Duration::from_millis(200), Arc::new(AtomicBool::new(false)), |_, _| Value::Null);
        assert_eq!(end, End::Interrupted);
    }

    #[test]
    fn nothing_of_the_computer_is_reachable_but_the_host() {
        let end = run("return [typeof require, typeof process, typeof std, typeof os, typeof fetch, typeof __host].join(',');", |_, _| Value::Null);
        assert_eq!(end, End::Done(json!("undefined,undefined,undefined,undefined,undefined,undefined")));
    }
}
