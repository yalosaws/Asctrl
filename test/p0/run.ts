// P0 benchmark: a frontier model does real tasks through Asctrl's MCP tools, and this counts what it took.
// Each task runs in a fresh headless Claude Code (claude -p) that has only the asctrl tools, minus the ones that
// would let it skip the apps (PowerShell, file tools). Per task it records: success (checked by what the task left
// behind, not by the model's word), wall time, model calls, tokens and tool calls.
//
// Usage: node test/p0/run.ts [--only b01,office,...] [--model sonnet] [--root D:\other-checkout] [--turns 60] [--tools compact|full] [--popups 4] [--dry]
//   --tools  full for an older checkout (--root) whose MCP server lists every tool by name; otherwise the five tools
//   --root   another checkout (its MCP server and its release driver), e.g. main for the v0.2 numbers
//   --popups every Nth task gets an unexpected popup in the middle of its work (test/p0/popup.ps1, "Update
//            available"), the experiment of docs/architecture-v2.md section 7: the summary compares those tasks
//   --dry    no model: only sets up, checks and cleans up every task (all should fail, each for a clear reason)
// Takes the computer over for the whole run (about 40 tasks, a few minutes each): leave it alone.
// Results: test/out/p0-<time>.jsonl, and a summary per area at the end.

import { driverExecutable, fixtureDirectory, owned } from "../fixtures.ts";
import { spawn, execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, rmSync, writeFileSync, appendFileSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { site } from "./site.ts";
import { TASKS, prepare, ps, type Area } from "./tasks.ts";

const here = path.resolve(import.meta.dirname, "../..");
const arg = (name: string) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 ? process.argv[i + 1] : undefined;
};
const dry = process.argv.includes("--dry");
const root = path.resolve(arg("root") ?? here);
const model = arg("model");
const turns = arg("turns") ?? "60";
const only = arg("only")?.split(",");
const surface = arg("tools") ?? "compact";
const perTaskMs = 15 * 60_000;
const popupEvery = Number(arg("popups") ?? 0);

const NO = ["run_powershell", "job", "request_admin", "restart_computer", "read_file", "write_file", "edit_file", "list_files", "search_files", "disk_usage", "upload_file", "download_file"];
const SYSTEM = `You are doing a task on this Windows computer through the asctrl tools; connect to this computer first (connect with no arguments).
Work through the apps, the way a person would; tools that run commands or touch files directly are not available.
The task is real and runs unattended: nobody will answer questions, so make reasonable choices yourself.
When finished, end your last reply with a line "ANSWER: <the answer>", or "ANSWER: done" when the task asks for no answer.`;

function findClaude(): string {
  if (process.env.CLAUDE_EXE) return process.env.CLAUDE_EXE;
  try {
    return execFileSync("where.exe", ["claude"], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).split(/\r?\n/)[0].trim();
  } catch {}
  // The Claude desktop app keeps its own Claude Code: take the newest
  const base = path.join(process.env.LOCALAPPDATA ?? "", "Claude-Data");
  const found: string[] = [];
  const walk = (d: string, depth: number) => {
    if (depth > 4 || !existsSync(d)) return;
    for (const e of readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name);
      if (e.isDirectory()) walk(p, depth + 1);
      else if (e.name === "claude.exe") found.push(p);
    }
  };
  walk(base, 0);
  found.sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);
  if (!found[0]) throw new Error("claude.exe not found: set CLAUDE_EXE");
  return found[0];
}

interface Result {
  id: string; area: Area; ok: boolean; why: string; answer: string; ms: number;
  calls: number; turns: number; tokens: { input: number; cacheWrite: number; cacheRead: number; output: number };
  cost: number; tools: Record<string, number>; model: string; stop: string; reply: string;
  /** --popups: a popup came up during the task, and whether it was gone (dismissed) before the task ended */
  popup?: { shown: boolean; dismissed: boolean };
}

/** Runs one task in headless Claude Code and reads its stream of events */
function agentRun(claude: string, prompt: string, cwd: string, mcpConfig: string): Promise<Omit<Result, "id" | "area" | "ok" | "why">> {
  // The tools that let it skip the apps are not offered at all (ASCTRL_TOOLS_DENY in the MCP config); an older
  // checkout (--root) without that still has them refused here by name
  const tools = surface === "full"
    ? ["connect", "disconnect", "observe", "screenshot", "read_text", "wait_for", "click", "type_text", "press_keys", "set_value", "scroll", "drag", "mouse", "run", "progress", "exec", "resume", "skills", "open_app", "focus_window", "clipboard"]
    : ["connect", "query", "act", "exec", "resume"];
  const a = ["-p", prompt, "--output-format", "stream-json", "--verbose", "--no-session-persistence", "--strict-mcp-config", "--mcp-config", mcpConfig,
    "--tools", "", "--allowedTools", ...tools.map((n) => `mcp__asctrl__${n}`), "--disallowedTools", ...NO.map((n) => `mcp__asctrl__${n}`),
    "--append-system-prompt", SYSTEM, "--max-turns", turns, "--setting-sources", "user"];
  if (model) a.push("--model", model);
  return new Promise((resolve) => {
    const t0 = performance.now();
    const p = owned(() => spawn(claude, a, { cwd, stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, ENABLE_TOOL_SEARCH: "false" }, windowsHide: true }));
    const ids = new Set<string>();
    const toolCount: Record<string, number> = {};
    let buf = "", last = "", used = "", stop = "", err = "", cost = 0, nTurns = 0;
    let tokens = { input: 0, cacheWrite: 0, cacheRead: 0, output: 0 };
    const timer = setTimeout(() => { stop = "timeout"; p.kill(); }, perTaskMs);
    p.stderr.on("data", (d) => (err += d).length > 4000 && (err = err.slice(-4000)));
    p.stdout.on("data", (d) => {
      buf += d;
      let i: number;
      while ((i = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, i);
        buf = buf.slice(i + 1);
        let e: any;
        try { e = JSON.parse(line); } catch { continue; }
        if (e.type === "system" && e.subtype === "init") used = e.model ?? "";
        if (e.type === "assistant") {
          ids.add(e.message?.id);
          for (const c of e.message?.content ?? []) {
            if (c.type === "tool_use") {
              // act and query: by what they did
              const base = c.name.replace("mcp__asctrl__", "");
              const k = base === "act" ? `act:${c.input?.do}` : base === "query" ? `query:${c.input?.what}` : base;
              toolCount[k] = (toolCount[k] ?? 0) + 1;
            }
            if (c.type === "text") last = c.text;
          }
        }
        if (e.type === "result") {
          stop ||= e.subtype ?? "";
          nTurns = e.num_turns ?? 0;
          cost = e.total_cost_usd ?? 0;
          if (typeof e.result === "string" && e.result) last = e.result;
          const u = e.usage ?? {};
          tokens = { input: u.input_tokens ?? 0, cacheWrite: u.cache_creation_input_tokens ?? 0, cacheRead: u.cache_read_input_tokens ?? 0, output: u.output_tokens ?? 0 };
        }
      }
    });
    p.on("exit", () => {
      clearTimeout(timer);
      if (!ids.size && err) last = `claude failed: ${err.trim().split("\n").slice(-3).join(" ")}`;
      const answer = [...last.matchAll(/ANSWER:\s*(.+)/g)].pop()?.[1]?.trim() ?? "";
      resolve({ answer, reply: last.slice(-600), ms: Math.round(performance.now() - t0), calls: ids.size, turns: nTurns, tokens, cost, tools: toolCount, model: used, stop: stop || "?" });
    });
  });
}

// ---------- Leaving the desktop as it was between tasks
// Only the browser launched under this run-specific profile belongs wholly to the benchmark.
function closeOwnedBrowser(profile: string, parentPid: number) {
  const expected = profile.replaceAll("'", "''");
  ps(`$profileArg = '--user-data-dir=${expected}'; Get-CimInstance Win32_Process -Filter "ParentProcessId=${parentPid}" |
    Where-Object { $_.Name -in 'chrome.exe','msedge.exe' -and $_.CommandLine -match ('(?:^|\\s)"?' + [regex]::Escape($profileArg) + '"?(?:\\s|$)') } |
    ForEach-Object {
      $p = Get-Process -Id $_.ProcessId -ErrorAction SilentlyContinue
      if ($p -and [math]::Abs(($p.StartTime.ToUniversalTime() - $_.CreationDate.ToUniversalTime()).TotalMilliseconds) -lt 1) {
        if ($p.CloseMainWindow()) { [void]$p.WaitForExit(3000) }
      }
    }`);
}

// ---------- Run
const exe = driverExecutable(root);
if (!existsSync(exe)) throw new Error(`no release driver at ${exe}`);
const stamp = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
const base = fixtureDirectory(path.join(tmpdir(), `asctrl-p0-${stamp}-`));
const localFile = path.join(base, "local.json");
process.env.ASCTRL_LOCAL_FILE = localFile;
// Programs that work are kept as skills by themselves: in a file of the test's own, not the computer's
process.env.ASCTRL_SKILLS_FILE ??= path.join(tmpdir(), `asctrl-test-skills-${process.pid}.json`);
const mcpConfig = path.join(base, "mcp.json");
writeFileSync(mcpConfig, JSON.stringify({ mcpServers: { asctrl: { command: "node", args: [path.join(root, "src/console/mcp-server.ts")], env: { RELAY_URL: "ws://127.0.0.1:1", ASCTRL_LOCAL_FILE: localFile, TECH_NAME: "P0", ASCTRL_TOOLS: surface, ASCTRL_TOOLS_DENY: NO.join(","), ASCTRL_SKILLS_FILE: process.env.ASCTRL_SKILLS_FILE ?? "" } } } }));
mkdirSync(path.join(here, "test/out"), { recursive: true });
const out = path.join(here, `test/out/p0-${stamp}.jsonl`);
const browserProfile = path.join(base, "browser-profile");
const driver = owned(() => spawn(exe, ["--relay", "ws://127.0.0.1:1"], { stdio: ["ignore", "ignore", "inherit"], env: { ...process.env, ASCTRL_BROWSER_PROFILE: browserProfile } }));
const web = await site();
const claude = dry ? "" : findClaude();
const tasks = TASKS.filter((t) => !only || only.some((o) => t.id.startsWith(o) || t.area === o));
console.log(`${tasks.length} task(s), driver ${path.relative(here, exe) || exe}${dry ? ", dry run" : `, ${claude}`}\nresults: ${out}\n`);

const results: Result[] = [];
/** Puts the unexpected popup up `ms` after the task starts; says what became of it once the task is over */
function popupDuring(ms: number) {
  let proc: ReturnType<typeof spawn> | null = null;
  let shown = false;
  let gone = false;
  const timer = setTimeout(() => {
    shown = true;
    proc = owned(() => spawn("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(here, "test/p0/popup.ps1"), "-Seconds", "600"], { stdio: "ignore", windowsHide: true }));
    proc.on("exit", () => (gone = true));
  }, ms);
  return () => {
    clearTimeout(timer);
    const r = { shown, dismissed: shown && gone };
    if (proc && !gone) proc.kill();
    return r;
  };
}

try {
  for (const [n, t] of tasks.entries()) {
    const c = { work: path.join(base, t.id), site: web };
    web.got.clear();
    prepare(t, c);
    const prompt = t.prompt(c);
    // Every Nth task: a popup a few seconds into the work (at a different moment each time)
    const popup = popupEvery > 0 && !dry && n % popupEvery === popupEvery - 1 ? popupDuring(8000 + ((n * 2900) % 9000)) : null;
    const r = dry
      ? { answer: "", reply: "", ms: 0, calls: 0, turns: 0, tokens: { input: 0, cacheWrite: 0, cacheRead: 0, output: 0 }, cost: 0, tools: {}, model: "", stop: "dry" }
      : await agentRun(claude, prompt, c.work, mcpConfig);
    const pop = popup?.();
    // Checked once what the task opened is closed again: the programs let go of the files they saved
    if (driver.pid) closeOwnedBrowser(browserProfile, driver.pid);
    let why: string;
    try {
      const v = t.check(c, r.answer);
      why = v === true ? "" : v;
    } catch (e) {
      why = (e as Error).message;
    }
    if (/Not logged in|Invalid API key|run \/login/i.test(r.reply)) {
      throw new Error(`Claude Code is not signed in ("${r.reply.trim()}"). Sign in once in a terminal (run ${claude}, then /login), or set ANTHROPIC_API_KEY or CLAUDE_CODE_OAUTH_TOKEN (from: claude setup-token)`);
    }
    const res: Result = { id: t.id, area: t.area, ok: !why, why, ...r, ...(pop ? { popup: pop } : {}) };
    results.push(res);
    appendFileSync(out, JSON.stringify(res) + "\n");
    const tok = r.tokens.input + r.tokens.cacheWrite + r.tokens.cacheRead + r.tokens.output;
    console.log(`${res.ok ? "✓" : "✗"} ${(t.id + (pop?.shown ? (pop.dismissed ? " +popup" : " +POPUP") : "")).padEnd(20)} ${(r.ms / 1000).toFixed(0).padStart(4)} s ${String(r.calls).padStart(3)} calls ${String(Math.round(tok / 1000)).padStart(5)}k tok  ${res.ok ? r.answer.slice(0, 50) : why.slice(0, 90)}${r.stop !== "success" && r.stop !== "dry" ? ` [${r.stop}]` : ""}`);
  }
} finally {
  web.close();
  driver.kill();
  rmSync(localFile, { force: true });
}

// ---------- Summary
const mean = (xs: number[]) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : 0);
const rows = (["browser", "settings", "office", "vscode"] as Area[]).map((a) => [a, results.filter((r) => r.area === a)] as const).filter(([, rs]) => rs.length);
rows.push(["all" as Area, results]);
console.log(`\n${"area".padEnd(9)} ${"success".padStart(9)} ${"time".padStart(7)} ${"calls".padStart(6)} ${"tokens".padStart(8)} ${"cost".padStart(7)}   (means per task)`);
for (const [a, rs] of rows) {
  const ok = rs.filter((r) => r.ok).length;
  const tok = mean(rs.map((r) => r.tokens.input + r.tokens.cacheWrite + r.tokens.cacheRead + r.tokens.output));
  console.log(`${a.padEnd(9)} ${`${ok}/${rs.length}`.padStart(9)} ${`${(mean(rs.map((r) => r.ms)) / 1000).toFixed(0)} s`.padStart(7)} ${mean(rs.map((r) => r.calls)).toFixed(1).padStart(6)} ${`${(tok / 1000).toFixed(0)}k`.padStart(8)} ${`$${mean(rs.map((r) => r.cost)).toFixed(2)}`.padStart(7)}`);
}
// The popup experiment: the tasks that had one against the rest
const popped = results.filter((r) => r.popup?.shown);
if (popped.length) {
  const line = (name: string, rs: Result[]) =>
    `${name.padEnd(14)} ${`${rs.filter((r) => r.ok).length}/${rs.length}`.padStart(7)} ok, ${(mean(rs.map((r) => r.ms)) / 1000).toFixed(0)} s, ${mean(rs.map((r) => r.calls)).toFixed(1)} calls per task`;
  console.log(`
popups: ${popped.filter((r) => r.popup!.dismissed).length}/${popped.length} dismissed during the task`);
  console.log(line("with popup", popped));
  console.log(line("without", results.filter((r) => !r.popup?.shown)));
}
const toolTotals: Record<string, number> = {};
for (const r of results) for (const [k, v] of Object.entries(r.tools)) toolTotals[k] = (toolTotals[k] ?? 0) + v;
console.log(`\ntools: ${Object.entries(toolTotals).sort((a, b) => b[1] - a[1]).map(([k, v]) => `${k} ${v}`).join(", ") || "none"}`);
setTimeout(() => process.exit(), 300);
