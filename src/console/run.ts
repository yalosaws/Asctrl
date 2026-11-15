// run: plans the AI writes once and the driver carries out, while the AI thinks about the next one.
//
// A plan is a list of steps named by what they act on ("Compose", "To"), not by reference, so it can be written
// without looking first. Each step is tried as written (the driver matches text locally, for free). Only when
// that fails — the text matches several controls, or none — is a decision model (Drex) asked to choose among
// a handful of candidates, and only a confident, harmless answer is acted on. Anything else stops the plan and
// hands back to the AI with what it needs to re-plan.
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
export interface Plan {
  steps: Step[];
  /** One line on what the plan is for: the only context the decision model gets besides the candidates */
  goal?: string;
  /** Text that must be on screen before the plan starts (alternatives with |); waited for up to whenSec */
  when?: string;
  whenSec?: number;
  /** Known interruptions: { "text a dialog shows": "what to press" }, handled when a step fails */
  on?: Record<string, string>;
  /** Window every step works in unless it names its own */
  window?: string | number;
  /** Run the steps once per row: [["7","3"], …] fills $1 $2 …, [{a: "7"}, …] fills ${a} */
  repeat?: Row[];
  /** Inputs don't wait for the screen to settle, only for the program to take them (a screen that stays put) */
  fast?: boolean;
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
  /** The screen is known to have settled since the last action (an expect or a wait came after it) */
  settled: boolean;
  /** The program each window of the plan belongs to, the scope of what is remembered */
  scopes: Map<string, string>;
  /** The plan's last steps and what each did to the screen, for the decision model */
  history: string[];
}

/** What an action's reply says it did to the screen: changed it, changed nothing, or (fast, no reading) unknown */
function effectOf(r: ToolResult): "changed" | "unchanged" | "" {
  const t = r.text ?? "";
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

  /** Queue a plan. wait: return when it has run, with its report; else return at once */
  async run(plan: Plan, wait: boolean): Promise<{ text: string; ok: boolean }> {
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
    const job: Job = { id: `r${++this.n}`, plan, status: "queued", lines: [], ms: 0, drex: { calls: 0, tokens: 0, ms: 0 }, reported: false, done: Promise.resolve(), pending: new Map(), settled: false, scopes: new Map(), history: [] };
    this.jobs.push(job);
    job.done = this.chain = this.chain.then(() => this.exec(job));
    if (wait) {
      await job.done;
      job.reported = true;
      if (this.unreported === job) this.unreported = undefined;
      return { ok: job.status === "done", text: this.report(job) };
    }
    const ahead = this.jobs.filter((j) => j !== job && (j.status === "queued" || j.status === "running")).length;
    return { ok: true, text: `Queued as ${job.id}${ahead ? ` behind ${ahead} other plan(s)` : ", running now"}. Write the next plan, or call progress to see how it went.` };
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
    const dx = j.drex.calls ? ` · decision model: ${j.drex.calls} call(s), ${j.drex.tokens} tokens, ${(j.drex.ms / 1000).toFixed(1)} s` : "";
    return [head + dx, ...j.lines.map((l, i) => `${i + 1}. ${l}`), ...(j.tail ? [j.tail] : [])].join("\n");
  }

  private async exec(job: Job) {
    if (this.unreported || job.status === "cancelled") {
      job.status = "cancelled";
      return;
    }
    job.status = "running";
    const t0 = performance.now();
    // To the overlay the whole plan is one piece of work (an older driver doesn't know this: no matter)
    await this.call("plan_mark", { plan: job.id }).catch(() => undefined);
    try {
      const p = job.plan;
      if (p.when) {
        const r = await this.call("wait_for", { text: p.when, timeoutSec: p.whenSec ?? 5, ...(p.window !== undefined ? { within: p.window } : {}), after: "none" });
        if (!r.ok) return await this.fail(job, `✗ not started: "${p.when}" did not show up`, p.window);
        job.settled = true;
      }
      // Once, or once per row of data: the steps written once, $1 / ${name} filled in from each row
      const rows: (Row | undefined)[] = p.repeat?.length ? p.repeat : [undefined];
      for (let i = 0; i < rows.length; i++) {
        const row = rows[i];
        // The first row's steps are reported one by one; later rows in a line each only when one goes wrong
        const mark = job.lines.length;
        for (const raw of p.steps) {
          const step = row === undefined ? raw : (fill(raw, row) as Step);
          const ok = await this.step(job, step);
          if (!ok) {
            if (i > 0) job.lines.splice(mark, 0, `✗ row ${i + 1} of ${rows.length} ${JSON.stringify(row).slice(0, 80)}:`);
            return await this.fail(job, undefined, (step.window as string | number | undefined) ?? p.window);
          }
        }
        if (i > 0) {
          // What went right needs no line per step: only what the decision model had to settle
          const said = job.lines.splice(mark).filter((l) => l.includes("(decision model"));
          job.lines.push(...said);
        }
        if (i === 0 && rows.length > 1) job.lines.push(`✓ row 1 of ${rows.length} ${JSON.stringify(row).slice(0, 80)}`);
      }
      if (rows.length > 1) job.lines.push(`✓ rows 2–${rows.length} the same way`);
      job.status = "done";
      // The plan worked, its checks included: what the decision model chose in it can be relied on next time
      for (const [k, v] of job.pending) await this.drex.remember(k, v);
    } catch (e) {
      job.lines.push(`✗ ${(e as Error).message}`);
      job.status = "failed";
      this.unreported = job;
    } finally {
      await this.call("plan_mark", { plan: job.id, end: true }).catch(() => undefined);
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
    const o = await this.call("observe", { silent: true, ...(window !== undefined ? { window } : {}), max: 40 }).catch(() => undefined);
    if (o?.text) job.tail = `Where things stand:\n${o.text.split("\n").slice(0, 45).join("\n")}`;
  }

  private call(tool: ToolName, args: Record<string, unknown>): Promise<ToolResult> {
    return this.client.call(tool, args, 120_000);
  }

  /** The program a window belongs to (its program and title, numbers left out), so what is learnt in one
   *  program is never used in another */
  private async scope(job: Job, window: string | number | undefined): Promise<string> {
    const k = String(window ?? "");
    const known = job.scopes.get(k);
    if (known) return known;
    const lw = (await this.call("list_windows", { silent: true }).catch(() => undefined))?.text ?? "";
    const wins = [...lw.matchAll(/^\[(\d+)\] (\S+) "(.*)"/gm)].map((m) => ({ h: Number(m[1]), program: m[2], title: m[3] }));
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

  /** One step; afterwards, whether the screen is known to have settled */
  private async step(job: Job, s: Step): Promise<boolean> {
    const ok = await this.stepOnce(job, s);
    const fast = (s.fast as boolean | undefined) ?? job.plan.fast;
    job.settled = s.expect !== undefined || s.await !== undefined || s.check !== undefined || s.tool === "wait_for" || (s.fill !== undefined && !fast);
    return ok;
  }

  /** One step: run it as written, and when its target can't be told, try the known interruptions, then the decision model */
  private async stepOnce(job: Job, s: Step): Promise<boolean> {
    const window = (s.window as string | number | undefined) ?? job.plan.window;
    const w = window !== undefined ? { window } : {};
    if (typeof s.check === "string") return this.checkStep(job, s.check, window, (s.timeoutSec as number | undefined) ?? 5);
    const [tool, args, what, kind] = translate(s, w, (s.fast as boolean | undefined) ?? job.plan.fast);
    if (!tool) {
      job.lines.push(`✗ not a step: ${JSON.stringify(s).slice(0, 120)}`);
      return false;
    }
    if (tool === "set_value" && args.fields) {
      // One field at a time, so each can be resolved on its own
      for (const f of args.fields as { target: string; value: unknown }[]) {
        if (!(await this.step(job, { fill: { [f.target]: f.value }, ...w }))) return false;
      }
      return true;
    }
    // A wording the decision model already resolved in this program: go straight to the control it chose
    const scope = typeof args.target === "string" ? await this.scope(job, window) : "";
    const recall = async (k: string) => job.pending.get(k) ?? (await this.drex.recall(k));
    const alias = typeof args.target === "string" ? await recall(`alias|${scope}|${kind}|${norm(args.target)}`) : undefined;
    const aliasName = alias?.match(/^\w+ "(.*)"$/)?.[1];
    if (aliasName) {
      const r0 = await this.call(tool, { ...args, target: aliasName, timeoutSec: 0.3, probe: true });
      if (r0.ok) {
        job.lines.push(`✓ ${what} → ${alias} (remembered)`);
        this.noteDone(job, `${what} → ${alias}`, r0);
        return true;
      }
    }
    // Tried as written first; a miss here is no failure yet (probe: the overlay doesn't flash red for it)
    let r = await this.call(tool, args.target !== undefined ? { ...args, probe: true } : args);
    if (r.ok) {
      job.lines.push(`✓ ${what}`);
      this.noteDone(job, what, r);
      return true;
    }
    const err = (r.error ?? r.text ?? "").trim();
    const target = args.target as string | undefined;
    const missed = target && /equally well|Nothing in .* shows|has no item|is on screen in \d+ places/.test(err);

    // A dialog in the way that the plan said how to handle
    if (job.plan.on) {
      for (const [text, press] of Object.entries(job.plan.on)) {
        const seen = await this.call("observe", { silent: true, query: text, max: 3 });
        if (seen.ok && /\[e\d+\]/.test(seen.text ?? "")) {
          const c = await this.call("click", { target: press, after: "none" });
          if (c.ok) {
            job.lines.push(`↷ "${text}" was in the way: pressed "${press}"`);
            r = await this.call(tool, args);
            if (r.ok) {
              job.lines.push(`✓ ${what}`);
              this.noteDone(job, what, r);
              return true;
            }
          }
        }
      }
    }
    if (!missed) {
      job.lines.push(`✗ ${what}: ${firstLines(err, 6)}`);
      return false;
    }

    const why = /equally well|in \d+ places/.test(err) ? "several match" : "no exact match";
    // Wait first, judge after: nothing showing that name right after an action (or a fast input, which doesn't
    // wait for the screen) may only mean it hasn't come up yet. Asked now, the decision model would pick among
    // what is there and take "not there yet" for "called something else".
    if (why === "no exact match" && !job.settled && s.timeoutSec === undefined) {
      const r1 = await this.call(tool, { ...args, timeoutSec: 0.8, probe: true });
      if (r1.ok) {
        job.lines.push(`✓ ${what} (showed up after a moment)`);
        this.noteDone(job, what, r1);
        return true;
      }
    }
    // The first try didn't wait: what the plan named may still be on its way (a dialog opening). Before giving
    // up, give it the usual moment to show up.
    const late = async () => {
      if (s.timeoutSec !== undefined || why !== "no exact match") return false;
      const r3 = await this.call(tool, { ...args, timeoutSec: 1.5 });
      if (r3.ok) {
        job.lines.push(`✓ ${what} (showed up late)`);
        this.noteDone(job, what, r3);
      }
      return r3.ok;
    };
    // Who decides: up to twice, since the decision model may say the screen isn't ready yet or a popup is in the
    // way, and once that is dealt with it looks again
    for (let round = 0; round < 2; round++) {
      // The equal matches the driver named first (the words may fit them only by chance: "To" in "Toggle"), then
      // the closest controls of the right kind in the window, as it is now
      const o = await this.call("observe", { silent: true, ...w, max: 1000 });
      const named = round === 0 ? parseControls(err).filter(() => why === "several match") : [];
      const seen = new Set(named.map((c) => c.ref));
      const cands = [...named, ...shortlist(parseControls(o.text ?? ""), target!, kind).filter((c) => !seen.has(c.ref))].slice(0, 40);
      if (!cands.length) {
        if (await late()) return true;
        job.lines.push(`✗ ${what}: ${firstLines(err, 4)}`);
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
          job.lines.push(`✗ ${what}: ${why}, and no decision model is set up (NACE_API_KEY):\n${list}`);
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
          job.lines.push(`✗ ${what}: ${why}; the decision model failed (${(e as Error).message})`);
          return false;
        }
        job.drex.calls++;
        job.drex.tokens += d.tokens;
        job.drex.ms += d.ms;
        const probs = Object.values(d.a.probabilities ?? {}).sort((a, b) => b - a);
        const margin = probs.length > 1 ? probs[0] - probs[1] : 1;
        const sure = d.a.confidence >= 0.7 && margin >= 0.3;
        // Not ready yet, or something in the way: deal with it and look again (once)
        if (round === 0 && sure && d.a.choice === "wait") {
          job.lines.push(`… ${what}: the decision model says the window isn't ready yet (${d.a.confidence.toFixed(2)}); waiting`);
          await this.call("wait_for", { stable: true, timeoutSec: 4, ...(window !== undefined ? { within: window } : {}), after: "none", silent: true });
          const again = await this.call(tool, { ...args, timeoutSec: 0.3, probe: true });
          if (again.ok) {
            job.lines.push(`✓ ${what}`);
            this.noteDone(job, what, again);
            return true;
          }
          continue;
        }
        if (round === 0 && sure && d.a.choice === "escape") {
          job.lines.push(`↷ ${what}: the decision model says something is in the way (${d.a.confidence.toFixed(2)}); pressed Escape`);
          await this.call("press_keys", { keys: "escape", ...w, after: "none" });
          const again = await this.call(tool, { ...args, timeoutSec: 0.3, probe: true });
          if (again.ok) {
            job.lines.push(`✓ ${what}`);
            this.noteDone(job, what, again);
            return true;
          }
          continue;
        }
        const pick = cands.find((c) => c.ref === d.a.choice);
        if ((!pick || !sure) && round === 0) {
          // Unsure the first time: the screen may still be on its way somewhere. Let it settle, look again, ask again.
          await this.call("wait_for", { stable: true, timeoutSec: 1.5, ...(window !== undefined ? { within: window } : {}), after: "none", silent: true });
          const again = await this.call(tool, { ...args, timeoutSec: 0.2, probe: true });
          if (again.ok) {
            job.lines.push(`✓ ${what} (showed up after a moment)`);
            this.noteDone(job, what, again);
            return true;
          }
          continue;
        }
        if (!pick || !sure) {
          if (await late()) return true;
          const said = pick ? `best ${pick.label}` : d.a.choice === "wait" ? "the window isn't ready" : d.a.choice === "escape" ? "something is in the way" : "none fits";
          job.lines.push(`✗ ${what}: ${why}; the decision model wasn't sure (${said}, ${d.a.confidence.toFixed(2)}). Candidates:\n${list}`);
          return false;
        }
        // Something that can't be undone is done only when the plan named it, not on the model's guess
        if (DESTRUCTIVE.test(pick.name) && !norm(pick.name).includes(norm(target!)) && !DESTRUCTIVE.test(target!)) {
          job.lines.push(`✗ ${what}: the decision model picked ${pick.label}, which can't be undone and isn't what the plan named. Candidates:\n${list}`);
          return false;
        }
        ref = pick.ref;
        byModel = true;
        how = `decision model ${d.a.confidence.toFixed(2)}, ${d.tokens} tokens, ${Math.round(d.ms)} ms`;
      }
      const r2 = await this.call(tool, { ...args, target: ref, nth: undefined, role: undefined });
      const label = cands.find((c) => c.ref === ref)?.label ?? ref;
      if (!r2.ok) {
        job.lines.push(`✗ ${what} → ${label} (${how}): ${firstLines(r2.error ?? "", 4)}`);
        return false;
      }
      // A click the model chose that visibly did nothing was most likely the wrong control: say so rather than go on
      // (a field is checked by reading it back, so this is for clicks)
      if (byModel && kind === "click" && effectOf(r2) === "unchanged") {
        job.lines.push(`✗ ${what} → ${label} (${how}): clicked, but nothing changed on screen, so it was likely the wrong control. Candidates:\n${list}`);
        return false;
      }
      if (byModel) {
        // Held for this plan only: kept for good once the whole plan has worked (an action that ran is not yet
        // proof the choice was right; the plan's checks are)
        job.pending.set(key, ident(cands.find((c) => c.ref === ref)!));
        job.pending.set(`alias|${scope}|${kind}|${norm(target!)}`, ident(cands.find((c) => c.ref === ref)!));
      }
      job.lines.push(`✓ ${what} → ${label} (${how})`);
      this.noteDone(job, `${what} → ${label}`, r2);
      return true;
    }
    job.lines.push(`✗ ${what}: still not there after waiting and looking again`);
    return false;
  }

  /** check: the decision model says whether a claim holds for the window as it is now, asked again until it does
   *  or time runs out. For what a text search can't settle ("the email was sent", "the dialog shows an error"). */
  private async checkStep(job: Job, claim: string, window: string | number | undefined, timeoutSec: number): Promise<boolean> {
    const what = `check "${claim}"`;
    if (!this.drex.ready) {
      job.lines.push(`✗ ${what}: no decision model is set up (NACE_API_KEY)`);
      return false;
    }
    const until = performance.now() + timeoutSec * 1000;
    let p = 0;
    for (let n = 0; ; n++) {
      const o = await this.call("observe", { silent: true, ...(window !== undefined ? { window } : {}), max: 150 });
      const text = o.text ?? "";
      // The windows and the controls of the one looked at; the coordinates mean nothing to the model
      const state = text.replace(/ @\(\d+,\d+\)/g, "").replace(/Pass a reference such as[^\n]*/, "");
      let d;
      try {
        d = await this.drex.yes(state, `${claim} Answer from the window as it is now.`);
      } catch (e) {
        job.lines.push(`✗ ${what}: the decision model failed (${(e as Error).message})`);
        return false;
      }
      job.drex.calls++;
      job.drex.tokens += d.tokens;
      job.drex.ms += d.ms;
      p = d.p;
      if (p >= 0.85) {
        job.lines.push(`✓ ${what} (${p.toFixed(2)}${n ? `, after ${n + 1} looks` : ""}; decision model, ${d.tokens} tokens)`);
        job.settled = true;
        return true;
      }
      if (performance.now() + 500 > until) break;
      await new Promise((r) => setTimeout(r, 500));
    }
    job.lines.push(`✗ ${what}: not so after ${timeoutSec} s (the decision model gives it ${p.toFixed(2)})`);
    return false;
  }

  /** A step that went through, and what it did to the screen, for the decision model's view of the plan */
  private noteDone(job: Job, what: string, r: ToolResult) {
    job.history.push(`${what}${effectText(effectOf(r))}`);
    if (job.history.length > 12) job.history.splice(0, job.history.length - 12);
  }
}

const firstLines = (s: string, n: number) => s.split("\n").slice(0, n).join("\n").slice(0, 900);

/** A plan step → the driver tool, its arguments, a short description, and what kind of control it targets */
function translate(s: Step, w: Record<string, unknown>, fast?: boolean): [ToolName | undefined, Record<string, unknown>, string, "click" | "field"] {
  const pass = (keys: string[]) => Object.fromEntries(keys.filter((k) => s[k] !== undefined).map((k) => [k, s[k]]));
  const quick = { after: "none", ...(fast ? { fast: true } : {}) };
  // The first try doesn't wait: every action already waits for the screen to settle, so what is named is
  // normally there. A miss goes to the decision model at once, and only if that finds nothing is it given
  // a moment to show up (see late)
  const soon = { timeoutSec: s.timeoutSec ?? 0 };
  if (typeof s.tool === "string") {
    const { tool, ...rest } = s;
    return [tool as ToolName, { ...w, ...quick, ...rest }, `${tool} ${JSON.stringify(rest).slice(0, 80)}`, "click"];
  }
  if (s.open !== undefined) return ["open_app", { target: s.open, ...pass(["args", "cwd", "timeoutSec"]), ...quick }, `open ${s.open}`, "click"];
  if (s.click !== undefined) return ["click", { ...w, target: s.click, ...pass(["role", "nth", "count", "button"]), ...soon, ...quick }, `click "${s.click}"`, "click"];
  if (s.press !== undefined) return ["element_action", { ...w, target: s.press, action: "press", ...soon, ...quick }, `press "${s.press}"`, "click"];
  if (s.fill !== undefined && typeof s.fill === "object") {
    const entries = Object.entries(s.fill as Record<string, unknown>);
    if (entries.length === 1) {
      const [target, value] = entries[0];
      return ["set_value", { ...w, target, value, ...soon, ...quick }, `fill "${target}" with ${JSON.stringify(value).slice(0, 60)}`, "field"];
    }
    return ["set_value", { ...w, fields: entries.map(([target, value]) => ({ target, value })), ...quick }, "fill", "field"];
  }
  if (s.type !== undefined) {
    const into = s.into !== undefined ? { target: s.into } : {};
    return ["type_text", { ...w, ...into, text: s.type, ...pass(["enter", "replace", "method"]), ...quick }, `type ${JSON.stringify(s.type).slice(0, 60)}${s.into ? ` into "${s.into}"` : ""}`, "field"];
  }
  if (s.keys !== undefined) return ["press_keys", { ...w, keys: s.keys, ...pass(["repeat"]), ...quick }, `keys ${s.keys}`, "click"];
  if (s.expect !== undefined) {
    const within = w.window !== undefined ? { within: w.window } : {};
    return ["wait_for", { text: s.expect, timeoutSec: s.timeoutSec ?? 8, ...within, ...pass(["gone"]), ...quick }, `expect "${s.expect}"${s.gone ? " gone" : ""}`, "click"];
  }
  if (s.await !== undefined) return ["wait_for", { window: s.await, timeoutSec: s.timeoutSec ?? 10, ...pass(["gone"]), ...quick }, `await window "${s.await}"`, "click"];
  if (s.scroll !== undefined) return ["scroll", { ...w, direction: s.scroll, ...pass(["target", "amount"]), ...quick }, `scroll ${s.scroll}`, "click"];
  return [undefined, {}, "", "click"];
}
