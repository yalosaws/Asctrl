// run: plans the AI writes once and the driver carries out, while the AI thinks about the next one.
//
// A plan is a list of steps named by what they act on ("Compose", "To"), not by reference, so it can be written
// without looking first. The whole plan is checked before anything runs. Its steps then go to the driver in as
// few calls as possible (a batch per stretch of steps), where each is tried as written: the driver matches text
// locally, for free, and paces itself (see the driver's batch). Only when a step's text matches several
// controls or none does the runner take over that one step: a rule the plan gave for a popup, then a decision
// model (Drex) choosing among a handful of candidates, and only a confident, harmless answer is acted on.
// Anything else stops the plan and hands back to the AI with what it needs to re-plan.
//
// While a plan runs, the driver also dismisses popups that turn up (the plan's `on` rules, or a button that
// only dismisses), in between steps.
//
// Plans queue: run returns at once, so the AI can write the next plan while this one runs. A plan that fails
// cancels the plans queued behind it (they were written assuming it would work), and the next run call gets
// the failure in its reply instead of being queued blind.

import { existsSync } from "node:fs";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import type { TechClient } from "./tech-client.ts";
import type { ToolName, ToolResult } from "../shared/protocol.ts";

export type Step = Record<string, unknown>;
/** A grid drawn on screen (a game board, a calendar): cells are named "board[row,col]", from 1 */
export interface Grid {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
  rows: number;
  cols: number;
}
export interface Plan {
  steps: Step[];
  /** One line on what the plan is for: the only context the decision model gets besides the candidates */
  goal?: string;
  /** Text that must be on screen before the plan starts (alternatives with |); waited for up to whenSec */
  when?: string;
  whenSec?: number;
  /** Known interruptions: { "text a popup shows": "what to press" } */
  on?: Record<string, string>;
  /** Window every step works in unless it names its own */
  window?: string | number;
  /** Run the steps once per row: [["7","3"], …] fills $1 $2 …, [{a: "7"}, …] fills ${a} */
  repeat?: Row[];
  /** true: inputs never wait for the screen to settle; false: they always do; unset: the driver decides */
  fast?: boolean;
  /** Grids on screen, in the latest screenshot's pixels */
  grids?: Record<string, Grid>;
}

type Cell = string | number | boolean;
type Row = Cell[] | Record<string, Cell>;

/** A step with $1 / ${name} filled in from a row; a value that is the whole placeholder keeps its type (true) */
function fill(v: unknown, row: Row): unknown {
  const get = (k: string): Cell | undefined => (Array.isArray(row) ? (/^\d+$/.test(k) ? row[Number(k) - 1] : undefined) : row[k]);
  if (typeof v === "string") {
    const whole = v.match(/^\$(?:\{(\w+)\}|(\d+))$/);
    if (whole) {
      const got = get(whole[1] ?? whole[2]);
      if (got !== undefined) return got;
    }
    return v.replace(/\$(?:\{(\w+)\}|(\d+))/g, (m, name, n) => {
      const got = get(name ?? n);
      return got === undefined ? m : String(got);
    });
  }
  if (Array.isArray(v)) return v.map((x) => fill(x, row));
  if (v && typeof v === "object") return Object.fromEntries(Object.entries(v).map(([k, x]) => [fill(k, row) as string, fill(x, row)]));
  return v;
}

// ---------- Steps

/** Tools a `{tool: …}` step may name: the ones the driver's batch runs */
export const STEP_TOOLS = [
  "click", "type_text", "press_keys", "scroll", "drag", "mouse", "set_value", "element_action", "focus_window", "open_app", "wait_for",
  "clipboard", "read_text", "observe", "read_grid",
] as const;

/** Every kind of step: the key that names it, and the other keys it takes (besides window) */
const KINDS: Record<string, string[]> = {
  open: ["args", "cwd", "timeoutSec"],
  click: ["role", "nth", "count", "button", "timeoutSec"],
  press: ["role", "nth", "timeoutSec"],
  fill: ["timeoutSec"],
  type: ["into", "enter", "replace", "method"],
  keys: ["repeat"],
  expect: ["gone", "timeoutSec"],
  check: ["timeoutSec"],
  await: ["gone", "timeoutSec"],
  scroll: ["target", "amount"],
  read: [],
};

const GRID_CELL = /^(\w+)\[\s*(\d+)\s*,\s*(\d+)\s*\]$/;
const placeholder = (v: unknown) => typeof v === "string" && /\$(\{\w+\}|\d+)/.test(v);

/** Everything wrong with a plan, before any of it runs (empty when nothing is) */
export function validate(plan: Plan): string[] {
  const errs: string[] = [];
  for (const [name, g] of Object.entries(plan.grids ?? {})) {
    const ok = [g.x0, g.y0, g.x1, g.y1].every((n) => typeof n === "number" && n >= 0) && g.x1 > g.x0 && g.y1 > g.y0 && Number.isInteger(g.rows) && Number.isInteger(g.cols) && g.rows >= 1 && g.cols >= 1;
    if (!ok) errs.push(`grid "${name}": give x0 < x1, y0 < y1 (screenshot pixels) and whole rows, cols ≥ 1`);
  }
  const gridRef = (where: string, v: unknown) => {
    if (typeof v !== "string" || placeholder(v)) return;
    const m = v.match(GRID_CELL);
    if (!m) return;
    const g = plan.grids?.[m[1]];
    if (!g) errs.push(`${where}: no grid named "${m[1]}" (declare it in grids)`);
    else if (+m[2] < 1 || +m[2] > g.rows || +m[3] < 1 || +m[3] > g.cols) errs.push(`${where}: ${v} is outside grid "${m[1]}" (${g.rows}×${g.cols})`);
  };
  plan.steps.forEach((s, i) => {
    const where = `step ${i + 1}`;
    if (!s || typeof s !== "object" || Array.isArray(s)) return void errs.push(`${where}: not an object`);
    if (typeof s.tool === "string") {
      if (!(STEP_TOOLS as readonly string[]).includes(s.tool)) errs.push(`${where}: tool "${s.tool}" can't be a step; allowed: ${STEP_TOOLS.join(", ")}`);
      return;
    }
    const kinds = Object.keys(KINDS).filter((k) => s[k] !== undefined);
    if (kinds.length !== 1) return void errs.push(`${where}: give exactly one of ${[...Object.keys(KINDS), "tool"].join(", ")} (got ${kinds.length ? kinds.join(", ") : "none"})`);
    const kind = kinds[0];
    const extra = Object.keys(s).filter((k) => k !== kind && k !== "window" && !KINDS[kind].includes(k));
    if (extra.length) errs.push(`${where} (${kind}): ${extra.map((k) => `"${k}"`).join(", ")} not understood; it takes ${["window", ...KINDS[kind]].join(", ")}`);
    const v = s[kind];
    if (kind === "fill") {
      if (!v || typeof v !== "object" || Array.isArray(v) || !Object.keys(v).length) errs.push(`${where}: fill takes {label: value, …}`);
    } else if (kind === "read") {
      if (typeof v !== "string" || (!placeholder(v) && !plan.grids?.[v])) errs.push(`${where}: read takes the name of a grid declared in grids`);
    } else if (kind === "scroll") {
      if (!["up", "down", "left", "right"].includes(String(v)) && !placeholder(v)) errs.push(`${where}: scroll takes up, down, left or right`);
    } else if (typeof v !== "string" && typeof v !== "number") {
      errs.push(`${where}: ${kind} takes text`);
    } else if (String(v).trim() === "") {
      errs.push(`${where}: ${kind} is empty`);
    }
    gridRef(where, kind === "type" ? s.into : kind === "scroll" ? s.target : v);
  });
  return errs;
}

/** The texts a plan works with: a window showing any of them is the plan's own, not a popup to dismiss */
function keepTexts(steps: Step[]): string[] {
  const out = new Set<string>();
  const add = (v: unknown) => {
    if (typeof v === "string" && v.trim() && !placeholder(v) && !GRID_CELL.test(v)) for (const alt of v.split("|")) if (alt.trim()) out.add(alt.trim());
  };
  for (const s of steps) {
    for (const k of ["click", "press", "expect", "into", "target", "await"]) add(s[k]);
    if (s.fill && typeof s.fill === "object") for (const k of Object.keys(s.fill)) add(k);
  }
  return [...out].slice(0, 200);
}

/** One step as the driver runs it */
interface Prepared {
  tool: ToolName;
  args: Record<string, unknown>;
  /** How it is named in the report */
  what: string;
  /** What kind of control a text target should be */
  kind: "click" | "field";
  /** A wording the decision model resolved earlier in this program: the target was swapped for its control */
  alias?: string;
  /** The target as the plan wrote it, before an alias */
  asWritten?: Record<string, unknown>;
}

/** One step of the plan as it runs: filled from its row; `k` is its place in the steps written */
interface Item {
  s: Step;
  row: number;
  k: number;
}

/** What an action's reply says it did to the screen: changed it, changed nothing, or (fast, no reading) unknown */
function effectOf(t: string): "changed" | "unchanged" | "" {
  if (/Screen changed around|is now in front/.test(t)) return "changed";
  if (/Nothing visibly changed|No visible change/.test(t)) return "unchanged";
  return "";
}
const effectText = (e: string) => (e === "changed" ? " → the screen changed" : e === "unchanged" ? " → nothing changed on screen" : "");

const CLICKISH = /^(Button|SplitButton|Link|MenuItem|TabItem|ListItem|TreeItem|DataItem|CheckBox|RadioButton|Hyperlink)$/;
const FIELDISH = /^(Edit|ComboBox|Document|Spinner|CheckBox|RadioButton|Slider)$/;
/** Acting on these is never left to the decision model's own judgment */
const DESTRUCTIVE = /delete|remove|erase|discard|send|submit|purchase|buy|pay|replace|overwrite|format|uninstall|sign out|log out|删除|移除|清空|丢弃|发送|提交|购买|支付|付款|替换|覆盖|格式化|卸载|退出登录/i;

interface Cand { ref: string; role: string; name: string; label: string }
/** What a control is, without what it holds or its state: stays the same as it is used */
const ident = (c: Cand) => `${c.role} "${c.name}"`;

/** Lines like `[e12] Button "Save" (disabled) = "x" @(1,2)` from observe or a miss message */
function parseControls(text: string): Cand[] {
  const out: Cand[] = [];
  for (const m of text.matchAll(/\[(e\d+)\] (\w+)(?: "((?:[^"\\]|\\.)*)")?([^@\n]*)/g)) {
    const [, ref, role, name = "", rest] = m;
    const label = `${role}${name ? ` "${name.slice(0, 80)}"` : ""}${rest.trimEnd().slice(0, 60)}`.replace(/[,:]/g, " ").replace(/\s+/g, " ").trim();
    out.push({ ref, role, name, label });
  }
  return out;
}

const norm = (s: string) => s.toLowerCase().replace(/[\s​-‏‪-‮]+/g, " ").trim();
function grams(s: string) {
  const t = norm(s);
  const g = new Set<string>();
  for (const w of t.split(/[^\p{L}\p{N}]+/u)) if (w.length > 1) g.add(w);
  for (let i = 0; i + 1 < t.length; i++) g.add(t.slice(i, i + 2));
  return g;
}
/** The candidates worth asking about: the closest by name, then controls of the right kind, at most 40 */
function shortlist(all: Cand[], want: string, kind: "click" | "field"): Cand[] {
  const fits = (c: Cand) => (kind === "click" ? CLICKISH : FIELDISH).test(c.role);
  const w = grams(want);
  const words = norm(want).split(/[^\p{L}\p{N}]+/u).filter(Boolean);
  const scored = all
    .filter((c) => c.name || fits(c))
    .map((c) => {
      let s = 0;
      for (const g of grams(`${c.name}`)) if (w.has(g)) s += g.length;
      // Whole words count for more than letters inside other words ("To" in "To recipients", not in "Toggle"),
      // and a name that starts with them most
      const name = norm(c.name);
      const own = new Set(name.split(/[^\p{L}\p{N}]+/u));
      for (const x of words) if (own.has(x)) s += 4;
      if (words.length && name.startsWith(words.join(" "))) s += 6;
      return { c, s: s + (fits(c) ? 0.5 : 0) };
    });
  const named = scored.filter((x) => x.s >= 1).sort((a, b) => b.s - a.s).slice(0, 8).map((x) => x.c);
  const seen = new Set(named.map((c) => c.ref));
  const kindOnly = all.filter((c) => fits(c) && c.name && !seen.has(c.ref)).slice(0, 40 - named.length);
  return [...named, ...kindOnly];
}

// ---------- Drex

interface DrexAnswer { choice: string; confidence: number; probabilities?: Record<string, number> }
class Drex {
  private key?: string;
  private model = process.env.DREX_MODEL ?? "drex-max-v1.1";
  private cache = new Map<string, string>();
  private cacheFile = process.env.ASCTRL_DREX_CACHE ?? path.join(os.homedir(), ".asctrl", "drex-cache.json");
  private loaded = false;

  constructor() {
    if (!process.env.NACE_API_KEY) {
      try {
        const envFile = path.resolve(import.meta.dirname, "../../.env");
        if (existsSync(envFile)) process.loadEnvFile(envFile);
      } catch {}
    }
    this.key = process.env.NACE_API_KEY;
  }

  get ready() {
    return !!this.key;
  }

  private async load() {
    if (this.loaded) return;
    this.loaded = true;
    try {
      for (const [k, v] of Object.entries(JSON.parse(await readFile(this.cacheFile, "utf8")))) this.cache.set(k, String(v));
    } catch {}
  }
  private async save() {
    try {
      await mkdir(path.dirname(this.cacheFile), { recursive: true });
      await writeFile(this.cacheFile, JSON.stringify(Object.fromEntries([...this.cache].slice(-2000))));
    } catch {}
  }

  /** Remembered choice for the same question over the same candidates (by label, since references change) */
  async recall(key: string) {
    await this.load();
    return this.cache.get(key);
  }
  async remember(key: string, label: string) {
    this.cache.set(key, label);
    await this.save();
  }

  /** The probability that a yes/no claim holds */
  async yes(state: string, claim: string): Promise<{ p: number; tokens: number; ms: number }> {
    const t = performance.now();
    const r = await fetch("https://console.nace.ai/v1/systemone", {
      method: "POST",
      signal: AbortSignal.timeout(8000),
      headers: { Authorization: `Bearer ${this.key}`, "Content-Type": "application/json", "X-Nace-Client": "asctrl-run/1" },
      body: JSON.stringify({ model: this.model, state, questions: { holds: { type: "noul", instructions: claim } } }),
    });
    const body = (await r.json()) as { answers?: { holds: { noul: number } }; usage?: { input_tokens: number } };
    if (!r.ok || !body.answers) throw new Error(`Drex ${r.status}: ${JSON.stringify(body).slice(0, 200)}`);
    return { p: body.answers.holds.noul, tokens: body.usage?.input_tokens ?? 0, ms: performance.now() - t };
  }

  async choose(state: string, question: string, options: Record<string, string>): Promise<{ a: DrexAnswer; tokens: number; ms: number }> {
    const t = performance.now();
    const ctl = AbortSignal.timeout(8000);
    const r = await fetch("https://console.nace.ai/v1/systemone", {
      method: "POST",
      signal: ctl,
      headers: { Authorization: `Bearer ${this.key}`, "Content-Type": "application/json", "X-Nace-Client": "asctrl-run/1" },
      body: JSON.stringify({ model: this.model, state, questions: { pick: { type: "choice", instructions: question, criteria: options } } }),
    });
    const body = (await r.json()) as { answers?: { pick: DrexAnswer }; usage?: { input_tokens: number }; error?: unknown };
    if (!r.ok || !body.answers) throw new Error(`Drex ${r.status}: ${JSON.stringify(body).slice(0, 200)}`);
    return { a: body.answers.pick, tokens: body.usage?.input_tokens ?? 0, ms: performance.now() - t };
  }
}

// ---------- Runner

type Status = "queued" | "running" | "done" | "failed" | "cancelled";
interface Job {
  id: string;
  plan: Plan;
  status: Status;
  lines: string[];
  /** Where things stand when it failed: a few lines for re-planning */
  tail?: string;
  ms: number;
  drex: { calls: number; tokens: number; ms: number };
  reported: boolean;
  done: Promise<void>;
  /** Choices the decision model made in this plan: remembered for good only once the whole plan has worked */
  pending: Map<string, string>;
  /** The program each window of the plan belongs to, the scope of what is remembered */
  scopes: Map<string, string>;
  /** The plan's last steps and what each did to the screen, for the decision model */
  history: string[];
  /** Inputs the driver put in without waiting for the screen */
  fast: number;
}

/** Most steps sent to the driver in one call */
const SEGMENT = 500;

interface BatchStep {
  ok: boolean;
  text: string;
  fast: boolean;
}

export class Runner {
  private jobs: Job[] = [];
  private chain: Promise<void> = Promise.resolve();
  private n = 0;
  /** A failed plan nobody has been told about yet: new plans are refused until it is reported */
  private unreported?: Job;
  private drex = new Drex();
  private client: TechClient;

  constructor(client: TechClient) {
    this.client = client;
  }

  /** Queue a plan. wait: return when it has run, with its report; else return at once, with the reports of
   *  plans that finished since the AI was last told */
  async run(plan: Plan, wait: boolean): Promise<{ text: string; ok: boolean }> {
    const errs = validate(plan);
    if (errs.length) return { ok: false, text: `Not queued: the plan has mistakes, and nothing of it ran.\n${errs.map((e) => `- ${e}`).join("\n")}` };
    if (this.unreported) {
      const j = this.unreported;
      this.unreported = undefined;
      j.reported = true;
      const cancelled = this.jobs.filter((x) => x.status === "cancelled" && !x.reported);
      for (const c of cancelled) c.reported = true;
      return {
        ok: false,
        text: `Not queued: an earlier plan failed, and this one was likely written assuming it would work. Re-plan from here.\n\n${this.report(j)}${cancelled.length ? `\n(also cancelled: ${cancelled.map((c) => c.id).join(", ")})` : ""}`,
      };
    }
    const job: Job = { id: `r${++this.n}`, plan, status: "queued", lines: [], ms: 0, drex: { calls: 0, tokens: 0, ms: 0 }, reported: false, done: Promise.resolve(), pending: new Map(), scopes: new Map(), history: [], fast: 0 };
    this.jobs.push(job);
    job.done = this.chain = this.chain.then(() => this.exec(job));
    const earlier = this.finished(job);
    if (wait) {
      await job.done;
      job.reported = true;
      if (this.unreported === job) this.unreported = undefined;
      return { ok: job.status === "done", text: [...earlier, this.report(job)].join("\n\n") };
    }
    const ahead = this.jobs.filter((j) => j !== job && (j.status === "queued" || j.status === "running")).length;
    const head = `Queued as ${job.id}${ahead ? ` behind ${ahead} other plan(s)` : ", running now"}. Write the next plan, or call progress to see how it went.`;
    return { ok: true, text: [head, ...earlier].join("\n\n") };
  }

  /** Reports of plans that finished well and nobody has been told about (a failure is told by run itself) */
  private finished(except: Job): string[] {
    const out: string[] = [];
    for (const j of this.jobs) {
      if (j === except || j.reported || j.status !== "done") continue;
      j.reported = true;
      out.push(this.report(j));
    }
    this.jobs = this.jobs.filter((j) => !j.reported || j.status === "running" || j.status === "queued");
    return out;
  }

  /** Reports of plans that finished since last asked, and where the running one is. wait: until all queued are done (seconds) */
  async progress(waitSec: number): Promise<{ text: string; ok: boolean }> {
    if (waitSec > 0) await Promise.race([this.chain, new Promise((r) => setTimeout(r, waitSec * 1000))]);
    const parts: string[] = [];
    let ok = true;
    for (const j of this.jobs) {
      if (j.reported) continue;
      if (j.status === "done" || j.status === "failed" || j.status === "cancelled") {
        j.reported = true;
        if (j.status !== "done") ok = false;
        parts.push(this.report(j));
      } else {
        parts.push(`${j.id} ${j.status}${j.lines.length ? `, at step ${j.lines.length}: ${j.lines.at(-1)}` : ""}`);
      }
    }
    if (this.unreported?.reported) this.unreported = undefined;
    this.jobs = this.jobs.filter((j) => !j.reported || j.status === "running" || j.status === "queued");
    return { ok, text: parts.join("\n\n") || "Nothing queued or running." };
  }

  private report(j: Job) {
    const head =
      j.status === "done"
        ? `${j.id} ✓ done in ${(j.ms / 1000).toFixed(1)} s`
        : j.status === "cancelled"
          ? `${j.id} – cancelled (an earlier plan failed)`
          : `${j.id} ✗ failed after ${(j.ms / 1000).toFixed(1)} s`;
    const fast = j.fast ? ` · ${j.fast} step${j.fast === 1 ? "" : "s"} fast` : "";
    const dx = j.drex.calls ? ` · decision model: ${j.drex.calls} call(s), ${j.drex.tokens} tokens, ${(j.drex.ms / 1000).toFixed(1)} s` : "";
    return [head + fast + dx, ...j.lines.map((l, i) => `${i + 1}. ${l}`), ...(j.tail ? [j.tail] : [])].join("\n");
  }

  private async exec(job: Job) {
    if (this.unreported || job.status === "cancelled") {
      job.status = "cancelled";
      return;
    }
    job.status = "running";
    const t0 = performance.now();
    const p = job.plan;
    // To the overlay the whole plan is one piece of work; to the driver, the time to watch for popups
    await this.call("plan_mark", { plan: job.id, on: p.on ?? {}, keep: keepTexts(p.steps) }).catch(() => undefined);
    try {
      if (p.when) {
        const r = await this.call("wait_for", { text: p.when, timeoutSec: p.whenSec ?? 5, ...this.win(p.window), after: "none" });
        if (!r.ok) return await this.fail(job, `✗ not started: "${p.when}" did not show up`, p.window);
      }
      // Once, or once per row of data: the steps written once, $1 / ${name} filled in from each row; a fill
      // of several fields is one step per field, so each can be resolved on its own
      const rows: (Row | undefined)[] = p.repeat?.length ? p.repeat : [undefined];
      const items: Item[] = rows.flatMap((row, r) =>
        p.steps.flatMap((raw, k) => {
          const s = row === undefined ? raw : (fill(raw, row) as Step);
          if (s.fill && typeof s.fill === "object" && Object.keys(s.fill).length > 1) {
            const { fill: fields, ...rest } = s;
            return Object.entries(fields as Record<string, unknown>).map(([label, value], f) => ({ s: { ...rest, fill: { [label]: value } }, row: r, k: k * 64 + f }));
          }
          return [{ s, row: r, k: k * 64 }];
        }),
      );
      const many = rows.length > 1;
      // The first row's steps are reported one by one; later rows only where something was out of the way
      let firstRowEnd = -1;
      const said = (it: Item, line: string) => {
        if (it.row === 0) job.lines.push(line);
        else if (!line.startsWith("✓ ") || /\((decision model|showed up)|:\n/.test(line)) job.lines.push(`row ${it.row + 1}: ${line}`);
      };
      let i = 0;
      while (i < items.length) {
        if (many && firstRowEnd < 0 && items[i].row > 0) firstRowEnd = job.lines.length;
        const it = items[i];
        if (it.s.check !== undefined) {
          const window = (it.s.window as string | number | undefined) ?? p.window;
          if (!(await this.checkStep(job, it, String(it.s.check), window, (it.s.timeoutSec as number | undefined) ?? 5, said))) return await this.fail(job, undefined, window);
          i++;
          continue;
        }
        // A stretch of steps up to the next check, sent in one call
        const seg: Item[] = [];
        for (let j = i; j < items.length && items[j].s.check === undefined && seg.length < SEGMENT; j++) seg.push(items[j]);
        const prepared: Prepared[] = [];
        for (const x of seg) prepared.push(await this.prepare(job, x.s));
        const steps = prepared.map((x, m) => ({ tool: x.tool, ...x.args, ...(many ? { k: seg[m].k } : {}) }));
        const pace = p.fast === true ? "fast" : p.fast === false ? "normal" : "auto";
        const r = await this.call("batch", { steps, pace, after: "none" });
        const data = r.data as { steps?: BatchStep[]; failed?: number | null } | undefined;
        if (!data?.steps) throw new Error(firstLines(r.error ?? r.text ?? "the driver gave no step results (is it up to date?)", 6));
        data.steps.forEach((res, m) => {
          if (res.fast) job.fast++;
          if (!res.ok) return;
          const x = prepared[m];
          const what = x.alias ? `${x.what} → ${x.alias} (remembered)` : x.what;
          // What a read step read is the point of it
          said(seg[m], x.tool === "read_grid" || x.tool === "read_text" || x.tool === "observe" ? `✓ ${what}:\n${res.text}` : `✓ ${what}`);
          this.noteDone(job, what, res.text);
        });
        if (data.failed == null) {
          i += seg.length;
          continue;
        }
        const m = data.failed;
        // Settled before it: a wait came just before, or an input that waited for the screen
        const prev = m > 0 ? seg[m - 1] : i > 0 ? items[i - 1] : undefined;
        const prevFast = m > 0 ? data.steps[m - 1]?.fast : false;
        const settled = !prev || prev.s.expect !== undefined || prev.s.await !== undefined || prev.s.check !== undefined || prev.s.tool === "wait_for" || !prevFast;
        if (!(await this.recover(job, seg[m], prepared[m], data.steps[m].text, settled, said))) {
          if (seg[m].row > 0) job.lines.push(`✗ in row ${seg[m].row + 1} of ${rows.length} ${JSON.stringify(rows[seg[m].row]).slice(0, 80)}`);
          return await this.fail(job, undefined, (seg[m].s.window as string | number | undefined) ?? p.window);
        }
        i += m + 1;
      }
      if (many) {
        if (firstRowEnd < 0) firstRowEnd = job.lines.length;
        job.lines.splice(firstRowEnd, 0, `✓ row 1 of ${rows.length} ${JSON.stringify(rows[0]).slice(0, 80)}`);
        job.lines.push(`✓ rows 2–${rows.length} the same way`);
      }
      job.status = "done";
      // The plan worked, its checks included: what the decision model chose in it can be relied on next time
      for (const [k, v] of job.pending) await this.drex.remember(k, v);
    } catch (e) {
      job.lines.push(`✗ ${(e as Error).message}`);
      job.status = "failed";
      this.unreported = job;
    } finally {
      const end = await this.call("plan_mark", { plan: job.id, end: true }).catch(() => undefined);
      if (end?.text) job.lines.push(...end.text.split("\n").filter(Boolean));
      job.ms = performance.now() - t0;
      if (job.status === "failed") for (const j of this.jobs) if (j.status === "queued") j.status = "cancelled";
    }
  }

  private async fail(job: Job, line: string | undefined, window?: string | number) {
    if (line) job.lines.push(line);
    // Two entries per choice (the candidates it chose among, and the wording)
    const n = job.pending.size / 2;
    if (n) job.lines.push(`(${n === 1 ? "the decision model's choice in this plan was" : `the decision model's ${n} choices in this plan were`} not remembered, since the plan failed)`);
    job.status = "failed";
    this.unreported = job;
    // A short look at where things stand, so the AI can re-plan without a separate look
    const o = await this.call("observe", { silent: true, ...this.win(window), max: 40 }).catch(() => undefined);
    if (o?.text) job.tail = `Where things stand:\n${o.text.split("\n").slice(0, 45).join("\n")}`;
  }

  private call(tool: ToolName, args: Record<string, unknown>): Promise<ToolResult> {
    return this.client.call(tool, args, 120_000);
  }

  private win(window: string | number | undefined) {
    return window !== undefined ? { window } : {};
  }

  /** The program a window belongs to (its program and title, numbers left out), so what is learnt in one
   *  program is never used in another */
  private async scope(job: Job, window: string | number | undefined): Promise<string> {
    const k = String(window ?? "");
    const known = job.scopes.get(k);
    if (known) return known;
    // The windows, front to back, as observe lists them: [handle] program "title"
    const lw = (await this.call("observe", { silent: true, max: 1 }).catch(() => undefined))?.text ?? "";
    const wins = [...lw.matchAll(/\[(\d+)\] (\S+) "(.*?)"(?= \[| \(|\.|$)/gm)].map((m) => ({ h: Number(m[1]), program: m[2], title: m[3] }));
    const w =
      typeof window === "number"
        ? wins.find((x) => x.h === window)
        : typeof window === "string"
          ? wins.find((x) => `${x.program} ${x.title}`.toLowerCase().includes(window.toLowerCase()))
          : wins[0];
    const scope = w ? `${w.program.toLowerCase()}|${w.title.replace(/\d+/g, "#").replace(/\s+/g, " ").trim()}` : "?";
    job.scopes.set(k, scope);
    return scope;
  }

  /** A plan step → the driver call, with a wording the decision model resolved before swapped for its control */
  private async prepare(job: Job, s: Step): Promise<Prepared> {
    const window = (s.window as string | number | undefined) ?? job.plan.window;
    const x = translate(s, this.win(window), job.plan.grids ?? {});
    const target = x.args.target;
    if (typeof target !== "string") return x;
    const scope = await this.scope(job, window);
    const k = `alias|${scope}|${x.kind}|${norm(target)}`;
    const alias = job.pending.get(k) ?? (await this.drex.recall(k));
    const name = alias?.match(/^\w+ "(.*)"$/)?.[1];
    if (!name) return x;
    return { ...x, args: { ...x.args, target: name }, alias, asWritten: x.args };
  }

  /** A step whose target the driver couldn't tell: the plan's popup rules, then the decision model */
  private async recover(job: Job, it: Item, x: Prepared, err: string, settled: boolean, said: (it: Item, line: string) => void): Promise<boolean> {
    const window = (it.s.window as string | number | undefined) ?? job.plan.window;
    const w = this.win(window);
    const ok = (line: string, r: ToolResult, what = x.what) => {
      said(it, line);
      this.noteDone(job, what, r.text ?? "");
      return true;
    };
    let { tool, args } = x;
    // A remembered wording that no longer fits: the plan's own words, as written
    if (x.asWritten) {
      args = x.asWritten;
      const r = await this.call(tool, { ...args, probe: true });
      if (r.ok) return ok(`✓ ${x.what}`, r);
      err = (r.error ?? r.text ?? "").trim();
    }
    const { what, kind } = x;
    const target = args.target as string | undefined;
    const missed = target && /equally well|Nothing in .* shows|has no item|is on screen in \d+ places/.test(err);

    // A popup in the way that the plan said how to handle
    for (const [text, press] of Object.entries(job.plan.on ?? {})) {
      const seen = await this.call("observe", { silent: true, ...w, query: text, max: 3 });
      if (!seen.ok || !/\[e\d+\]/.test(seen.text ?? "")) continue;
      const c = await this.call("click", { ...w, target: press, after: "none" });
      if (!c.ok) continue;
      said(it, `↷ "${text}" was in the way: pressed "${press}"`);
      const r = await this.call(tool, args);
      if (r.ok) return ok(`✓ ${what}`, r);
      err = (r.error ?? r.text ?? "").trim();
    }
    if (!missed) {
      said(it, `✗ ${what}: ${firstLines(err, 6)}`);
      return false;
    }

    const why = /equally well|in \d+ places/.test(err) ? "several match" : "no exact match";
    // Wait first, judge after: nothing showing that name right after an action (or a fast input, which doesn't
    // wait for the screen) may only mean it hasn't come up yet. Asked now, the decision model would pick among
    // what is there and take "not there yet" for "called something else".
    if (why === "no exact match" && !settled && it.s.timeoutSec === undefined) {
      const r1 = await this.call(tool, { ...args, timeoutSec: 0.8, probe: true });
      if (r1.ok) return ok(`✓ ${what} (showed up after a moment)`, r1);
    }
    // The first try didn't wait: what the plan named may still be on its way (a dialog opening). Before giving
    // up, give it the usual moment to show up.
    const late = async () => {
      if (it.s.timeoutSec !== undefined || why !== "no exact match") return false;
      const r3 = await this.call(tool, { ...args, timeoutSec: 1.5 });
      return r3.ok && ok(`✓ ${what} (showed up late)`, r3);
    };
    const scope = await this.scope(job, window);
    const recall = async (k: string) => job.pending.get(k) ?? (await this.drex.recall(k));
    // Who decides: up to twice, since the decision model may say the screen isn't ready yet or a popup is in the
    // way, and once that is dealt with it looks again
    for (let round = 0; round < 2; round++) {
      // The equal matches the driver named first (the words may fit them only by chance: "To" in "Toggle"), then
      // the closest controls of the right kind in the window, as it is now
      const o = await this.call("observe", { silent: true, ...w, max: 1000 });
      const named = round === 0 && why === "several match" ? parseControls(err) : [];
      const seen = new Set(named.map((c) => c.ref));
      const cands = [...named, ...shortlist(parseControls(o.text ?? ""), target!, kind).filter((c) => !seen.has(c.ref))].slice(0, 40);
      if (!cands.length) {
        if (await late()) return true;
        said(it, `✗ ${what}: ${firstLines(err, 4)}`);
        return false;
      }
      const key = `${scope}|${kind}|${norm(target!)}|${cands.map(ident).sort().join("¦")}`;
      let ref: string | undefined;
      let how = "";
      let byModel = false;
      const remembered = await recall(key);
      if (remembered) {
        ref = cands.find((c) => ident(c) === remembered)?.ref;
        if (ref) how = "remembered";
      }
      const list = cands.slice(0, 8).map((c) => `   [${c.ref}] ${c.label}`).join("\n");
      if (!ref) {
        if (!this.drex.ready) {
          said(it, `✗ ${what}: ${why}, and no decision model is set up (NACE_API_KEY):\n${list}`);
          return false;
        }
        // Besides the controls: what else can be true of this screen (the harness of the model's makers offers the
        // same: wait, Escape, help)
        const options: Record<string, string> = Object.fromEntries(cands.map((c) => [c.ref, c.label]));
        options.wait = "The control meant is not on screen yet: the window is still loading or opening something";
        options.escape = "A popup, menu or dialog is in the way of the control meant: close it with Escape";
        options.none = "None of these: the control meant is not here";
        const done = job.history.slice(-6);
        const state = [
          `Window: ${String(window ?? "the one in front")}.`,
          job.plan.goal ? `Overall goal: ${job.plan.goal}.` : "",
          done.length ? `Steps already taken in this plan, oldest first:\n${done.map((h, k) => `${k + 1}. ${h}`).join("\n")}` : "No steps taken yet in this plan.",
          `Step now: ${what}.`,
        ]
          .filter(Boolean)
          .join("\n");
        let d;
        try {
          d = await this.drex.choose(state, `Which control is the one meant by the step now?`, options);
        } catch (e) {
          said(it, `✗ ${what}: ${why}; the decision model failed (${(e as Error).message})`);
          return false;
        }
        job.drex.calls++;
        job.drex.tokens += d.tokens;
        job.drex.ms += d.ms;
        const probs = Object.values(d.a.probabilities ?? {}).sort((a, b) => b - a);
        const margin = probs.length > 1 ? probs[0] - probs[1] : 1;
        const sure = d.a.confidence >= 0.7 && margin >= 0.3;
        // Not ready yet, or something in the way: deal with it and look again (once)
        if (round === 0 && sure && (d.a.choice === "wait" || d.a.choice === "escape")) {
          if (d.a.choice === "wait") {
            said(it, `… ${what}: the decision model says the window isn't ready yet (${d.a.confidence.toFixed(2)}); waiting`);
            await this.call("wait_for", { stable: true, timeoutSec: 4, ...w, after: "none", silent: true });
          } else {
            said(it, `↷ ${what}: the decision model says something is in the way (${d.a.confidence.toFixed(2)}); pressed Escape`);
            await this.call("press_keys", { keys: "escape", ...w, after: "none" });
          }
          const again = await this.call(tool, { ...args, timeoutSec: 0.3, probe: true });
          if (again.ok) return ok(`✓ ${what}`, again);
          continue;
        }
        const pick = cands.find((c) => c.ref === d.a.choice);
        if ((!pick || !sure) && round === 0) {
          // Unsure the first time: the screen may still be on its way somewhere. Let it settle, look again, ask again.
          await this.call("wait_for", { stable: true, timeoutSec: 1.5, ...w, after: "none", silent: true });
          const again = await this.call(tool, { ...args, timeoutSec: 0.2, probe: true });
          if (again.ok) return ok(`✓ ${what} (showed up after a moment)`, again);
          continue;
        }
        if (!pick || !sure) {
          if (await late()) return true;
          const best = pick ? `best ${pick.label}` : d.a.choice === "wait" ? "the window isn't ready" : d.a.choice === "escape" ? "something is in the way" : "none fits";
          said(it, `✗ ${what}: ${why}; the decision model wasn't sure (${best}, ${d.a.confidence.toFixed(2)}). Candidates:\n${list}`);
          return false;
        }
        // Something that can't be undone is done only when the plan named it, not on the model's guess
        if (DESTRUCTIVE.test(pick.name) && !norm(pick.name).includes(norm(target!)) && !DESTRUCTIVE.test(target!)) {
          said(it, `✗ ${what}: the decision model picked ${pick.label}, which can't be undone and isn't what the plan named. Candidates:\n${list}`);
          return false;
        }
        ref = pick.ref;
        byModel = true;
        how = `decision model ${d.a.confidence.toFixed(2)}, ${d.tokens} tokens, ${Math.round(d.ms)} ms`;
      }
      const r2 = await this.call(tool, { ...args, target: ref, nth: undefined, role: undefined });
      const label = cands.find((c) => c.ref === ref)?.label ?? ref;
      if (!r2.ok) {
        said(it, `✗ ${what} → ${label} (${how}): ${firstLines(r2.error ?? "", 4)}`);
        return false;
      }
      // A click the model chose that visibly did nothing was most likely the wrong control: say so rather than go on
      // (a field is checked by reading it back, so this is for clicks)
      if (byModel && kind === "click" && effectOf(r2.text ?? "") === "unchanged") {
        said(it, `✗ ${what} → ${label} (${how}): clicked, but nothing changed on screen, so it was likely the wrong control. Candidates:\n${list}`);
        return false;
      }
      if (byModel) {
        // Held for this plan only: kept for good once the whole plan has worked (an action that ran is not yet
        // proof the choice was right; the plan's checks are)
        const id = ident(cands.find((c) => c.ref === ref)!);
        job.pending.set(key, id);
        job.pending.set(`alias|${scope}|${kind}|${norm(target!)}`, id);
      }
      return ok(`✓ ${what} → ${label} (${how})`, r2, `${what} → ${label}`);
    }
    said(it, `✗ ${what}: still not there after waiting and looking again`);
    return false;
  }

  /** check: the decision model says whether a claim holds for the window as it is now, asked again until it does
   *  or time runs out. For what a text search can't settle ("the email was sent", "the dialog shows an error"). */
  private async checkStep(job: Job, it: Item, claim: string, window: string | number | undefined, timeoutSec: number, said: (it: Item, line: string) => void): Promise<boolean> {
    const what = `check "${claim}"`;
    if (!this.drex.ready) {
      said(it, `✗ ${what}: no decision model is set up (NACE_API_KEY)`);
      return false;
    }
    const until = performance.now() + timeoutSec * 1000;
    let p = 0;
    for (let n = 0; ; n++) {
      const o = await this.call("observe", { silent: true, ...this.win(window), max: 150 });
      // The windows and the controls of the one looked at; the coordinates mean nothing to the model
      const state = (o.text ?? "").replace(/ @\(\d+,\d+\)/g, "").replace(/Pass a reference such as[^\n]*/, "");
      let d;
      try {
        d = await this.drex.yes(state, `${claim} Answer from the window as it is now.`);
      } catch (e) {
        said(it, `✗ ${what}: the decision model failed (${(e as Error).message})`);
        return false;
      }
      job.drex.calls++;
      job.drex.tokens += d.tokens;
      job.drex.ms += d.ms;
      p = d.p;
      if (p >= 0.85) {
        said(it, `✓ ${what} (${p.toFixed(2)}${n ? `, after ${n + 1} looks` : ""}; decision model, ${d.tokens} tokens)`);
        return true;
      }
      if (performance.now() + 500 > until) break;
      await new Promise((r) => setTimeout(r, 500));
    }
    said(it, `✗ ${what}: not so after ${timeoutSec} s (the decision model gives it ${p.toFixed(2)})`);
    return false;
  }

  /** A step that went through, and what it did to the screen, for the decision model's view of the plan */
  private noteDone(job: Job, what: string, text: string) {
    job.history.push(`${what}${effectText(effectOf(text))}`);
    if (job.history.length > 12) job.history.splice(0, job.history.length - 12);
  }
}

const firstLines = (s: string, n: number) => s.split("\n").slice(0, n).join("\n").slice(0, 900);

/** Where a grid cell's centre is, in screenshot pixels; undefined for anything that isn't a cell */
function cell(v: unknown, grids: Record<string, Grid>): { x: number; y: number } | undefined {
  const m = typeof v === "string" ? v.match(GRID_CELL) : null;
  const g = m && grids[m[1]];
  if (!m || !g) return undefined;
  const [r, c] = [Number(m[2]), Number(m[3])];
  if (r < 1 || r > g.rows || c < 1 || c > g.cols) throw new Error(`${v} is outside grid "${m[1]}" (${g.rows}×${g.cols})`);
  return { x: Math.round(g.x0 + ((c - 0.5) * (g.x1 - g.x0)) / g.cols), y: Math.round(g.y0 + ((r - 0.5) * (g.y1 - g.y0)) / g.rows) };
}

/** A plan step → the driver call that does it. `w` is the window it works in ({} for the one in front) */
export function translate(s: Step, w: Record<string, unknown>, grids: Record<string, Grid>): Prepared {
  const pass = (keys: string[]) => Object.fromEntries(keys.filter((k) => s[k] !== undefined).map((k) => [k, s[k]]));
  const quiet = { after: "none" };
  // A text target is tried at once (every action already waits for the screen to settle, so what is named is
  // normally there) and as a probe: a miss goes to the runner, which waits or decides (see recover)
  const aim = (v: unknown, key = "target") => cell(v, grids) ?? { [key]: v, probe: true, timeoutSec: s.timeoutSec ?? 0 };
  if (typeof s.tool === "string") {
    const { tool, ...rest } = s;
    return { tool: tool as ToolName, args: { ...w, ...quiet, ...rest }, what: `${tool} ${JSON.stringify(rest).slice(0, 80)}`, kind: "click" };
  }
  if (s.open !== undefined) return { tool: "open_app", args: { target: s.open, ...pass(["args", "cwd", "timeoutSec"]), ...quiet }, what: `open ${s.open}`, kind: "click" };
  if (s.click !== undefined) return { tool: "click", args: { ...w, ...aim(s.click), ...pass(["role", "nth", "count", "button"]), ...quiet }, what: `click "${s.click}"`, kind: "click" };
  if (s.press !== undefined) return { tool: "element_action", args: { ...w, ...aim(s.press), action: "press", ...pass(["role", "nth"]), ...quiet }, what: `press "${s.press}"`, kind: "click" };
  if (s.fill !== undefined) {
    const [label, value] = Object.entries(s.fill as Record<string, unknown>)[0];
    return { tool: "set_value", args: { ...w, ...aim(label), value, ...quiet }, what: `fill "${label}" with ${JSON.stringify(value).slice(0, 60)}`, kind: "field" };
  }
  if (s.type !== undefined) {
    const into = s.into !== undefined ? aim(s.into) : {};
    return { tool: "type_text", args: { ...w, ...into, text: String(s.type), ...pass(["enter", "replace", "method"]), ...quiet }, what: `type ${JSON.stringify(s.type).slice(0, 60)}${s.into ? ` into "${s.into}"` : ""}`, kind: "field" };
  }
  if (s.keys !== undefined) return { tool: "press_keys", args: { ...w, keys: String(s.keys), ...pass(["repeat"]), ...quiet }, what: `keys ${s.keys}`, kind: "click" };
  if (s.expect !== undefined) return { tool: "wait_for", args: { ...w, text: String(s.expect), timeoutSec: s.timeoutSec ?? 8, ...pass(["gone"]), ...quiet }, what: `expect "${s.expect}"${s.gone ? " gone" : ""}`, kind: "click" };
  if (s.await !== undefined) return { tool: "wait_for", args: { appears: String(s.await), timeoutSec: s.timeoutSec ?? 10, ...pass(["gone"]), ...quiet }, what: `await window "${s.await}"`, kind: "click" };
  if (s.scroll !== undefined) {
    const at = s.target !== undefined ? aim(s.target) : {};
    return { tool: "scroll", args: { ...w, ...at, direction: s.scroll, ...pass(["amount"]), ...quiet }, what: `scroll ${s.scroll}`, kind: "click" };
  }
  if (s.read !== undefined) {
    const g = grids[String(s.read)];
    if (!g) throw new Error(`read: no grid named "${s.read}"`);
    return { tool: "read_grid", args: { ...w, ...g }, what: `read ${s.read}`, kind: "click" };
  }
  throw new Error(`not a step: ${JSON.stringify(s).slice(0, 120)}`);
}
