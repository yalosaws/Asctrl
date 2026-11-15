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
}

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
    const job: Job = { id: `r${++this.n}`, plan, status: "queued", lines: [], ms: 0, drex: { calls: 0, tokens: 0, ms: 0 }, reported: false, done: Promise.resolve() };
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
      }
      for (const step of p.steps) {
        const ok = await this.step(job, step);
        if (!ok) return await this.fail(job, undefined, (step.window as string | number | undefined) ?? p.window);
      }
      job.status = "done";
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
    job.status = "failed";
    this.unreported = job;
    // A short look at where things stand, so the AI can re-plan without a separate look
    const o = await this.call("observe", { silent: true, ...(window !== undefined ? { window } : {}), max: 40 }).catch(() => undefined);
    if (o?.text) job.tail = `Where things stand:\n${o.text.split("\n").slice(0, 45).join("\n")}`;
  }

  private call(tool: ToolName, args: Record<string, unknown>): Promise<ToolResult> {
    return this.client.call(tool, args, 120_000);
  }

  /** One step: run it as written, and when its target can't be told, try the known interruptions, then the decision model */
  private async step(job: Job, s: Step): Promise<boolean> {
    const window = (s.window as string | number | undefined) ?? job.plan.window;
    const w = window !== undefined ? { window } : {};
    const [tool, args, what, kind] = translate(s, w);
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
    // A wording the decision model already resolved: go straight to the control it chose
    const alias = typeof args.target === "string" ? await this.drex.recall(`alias|${kind}|${norm(args.target)}`) : undefined;
    const aliasName = alias?.match(/^\w+ "(.*)"$/)?.[1];
    if (aliasName) {
      const r0 = await this.call(tool, { ...args, target: aliasName, timeoutSec: 0.3, probe: true });
      if (r0.ok) {
        job.lines.push(`✓ ${what} → ${alias} (remembered)`);
        return true;
      }
    }
    // Tried as written first; a miss here is no failure yet (probe: the overlay doesn't flash red for it)
    let r = await this.call(tool, args.target !== undefined ? { ...args, probe: true } : args);
    if (r.ok) {
      job.lines.push(`✓ ${what}`);
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

    // The target couldn't be told: who should decide?
    // The equal matches the driver named first (the words may fit them only by chance: "To" in "Toggle"), then
    // the closest controls of the right kind in the window
    const o = await this.call("observe", { silent: true, ...w, max: 1000 });
    const named = parseControls(err).filter((c) => /equally well|in \d+ places/.test(err));
    const seen = new Set(named.map((c) => c.ref));
    const cands = [...named, ...shortlist(parseControls(o.text ?? ""), target!, kind).filter((c) => !seen.has(c.ref))].slice(0, 40);
    const why = /equally well|in \d+ places/.test(err) ? "several match" : "no exact match";
    // The first try didn't wait: what the plan named may still be on its way (a dialog opening). Before giving
    // up, give it the usual moment to show up.
    const late = async () => {
      if (s.timeoutSec !== undefined || why !== "no exact match") return false;
      const r3 = await this.call(tool, { ...args, timeoutSec: 1.5 });
      if (r3.ok) job.lines.push(`✓ ${what} (showed up late)`);
      return r3.ok;
    };
    if (!cands.length) {
      if (await late()) return true;
      job.lines.push(`✗ ${what}: ${firstLines(err, 4)}`);
      return false;
    }
    const key = `${kind}|${norm(target!)}|${cands.map(ident).sort().join("¦")}`;
    let ref: string | undefined;
    let how = "";
    const remembered = await this.drex.recall(key);
    if (remembered) {
      ref = cands.find((c) => ident(c) === remembered)?.ref;
      if (ref) how = "remembered";
    }
    if (!ref) {
      if (!this.drex.ready) {
        job.lines.push(`✗ ${what}: ${why}, and no decision model is set up (NACE_API_KEY):\n${cands.slice(0, 8).map((c) => `   [${c.ref}] ${c.label}`).join("\n")}`);
        return false;
      }
      const options: Record<string, string> = Object.fromEntries(cands.map((c) => [c.ref, c.label]));
      options.none = "None of these";
      const state = `Window: ${String(window ?? "the one in front")}. ${job.plan.goal ? `Overall goal: ${job.plan.goal}.` : ""}`;
      const question = `Which control is the one meant by: ${what}?`;
      let d;
      try {
        d = await this.drex.choose(state, question, options);
      } catch (e) {
        job.lines.push(`✗ ${what}: ${why}; the decision model failed (${(e as Error).message})`);
        return false;
      }
      job.drex.calls++;
      job.drex.tokens += d.tokens;
      job.drex.ms += d.ms;
      const pick = cands.find((c) => c.ref === d.a.choice);
      const probs = Object.values(d.a.probabilities ?? {}).sort((a, b) => b - a);
      const margin = probs.length > 1 ? probs[0] - probs[1] : 1;
      const list = cands.slice(0, 8).map((c) => `   [${c.ref}] ${c.label}`).join("\n");
      if (!pick || d.a.confidence < 0.7 || margin < 0.3) {
        if (await late()) return true;
        job.lines.push(`✗ ${what}: ${why}; the decision model wasn't sure (${pick ? `best ${pick.label}` : "none fits"}, ${d.a.confidence.toFixed(2)}). Candidates:\n${list}`);
        return false;
      }
      // Something that can't be undone is done only when the plan named it, not on the model's guess
      if (DESTRUCTIVE.test(pick.name) && !norm(pick.name).includes(norm(target!)) && !DESTRUCTIVE.test(target!)) {
        job.lines.push(`✗ ${what}: the decision model picked ${pick.label}, which can't be undone and isn't what the plan named. Candidates:\n${list}`);
        return false;
      }
      ref = pick.ref;
      how = `decision model ${d.a.confidence.toFixed(2)}, ${d.tokens} tokens, ${Math.round(d.ms)} ms`;
      await this.drex.remember(key, ident(pick));
      await this.drex.remember(`alias|${kind}|${norm(target!)}`, ident(pick));
    }
    const r2 = await this.call(tool, { ...args, target: ref, nth: undefined, role: undefined });
    const label = cands.find((c) => c.ref === ref)?.label ?? ref;
    if (r2.ok) {
      job.lines.push(`✓ ${what} → ${label} (${how})`);
      return true;
    }
    job.lines.push(`✗ ${what} → ${label} (${how}): ${firstLines(r2.error ?? "", 4)}`);
    return false;
  }
}

const firstLines = (s: string, n: number) => s.split("\n").slice(0, n).join("\n").slice(0, 900);

/** A plan step → the driver tool, its arguments, a short description, and what kind of control it targets */
function translate(s: Step, w: Record<string, unknown>): [ToolName | undefined, Record<string, unknown>, string, "click" | "field"] {
  const pass = (keys: string[]) => Object.fromEntries(keys.filter((k) => s[k] !== undefined).map((k) => [k, s[k]]));
  const quick = { after: "none" };
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
