// What the person at the screen would see of a recorded run, measured: plays an animation log (ASCTRL_FX_LOG,
// written by the driver) through the real overlay code in a headless Chrome at the times it was recorded, and
// counts what the eye notices: how often the orb moves while it works, how often the work frame is redrawn,
// pointers that vanish and come back, how far each drawn press trails its real action, and frames left up after
// their window is gone. No desktop is touched.
//
// Usage: node test/fxcheck.ts <log>... [--json] [--check] [--keep] [--squeeze]
//   --check: fail (exit 1) when a log breaks the limits below (the overlay's regression test)
//   --keep: reuse the overlay already built in test/out/fxcheck-dist
//   --squeeze: replay pauses over 10 s as 10 s (long runs with an AI thinking between calls)
// A log without an "info" line (older drivers) is replayed on this computer's primary screen.

import { owned, fixtureDirectory } from "./fixtures.ts";
import { spawn, execFileSync } from "node:child_process";
import { createServer } from "node:http";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const out = path.join(root, "test/out/fxcheck-dist");
const argv = process.argv.slice(2);
const logs = argv.filter((a) => !a.startsWith("--"));
const asJson = argv.includes("--json");
const check = argv.includes("--check");
if (!logs.length) {
  console.error("usage: node test/fxcheck.ts <log>... [--json] [--check] [--keep]");
  process.exit(2);
}

/** The limits a run must keep to (per log), with --check */
const LIMITS = {
  /** Moves of the orb while it works in one window: it should stay put (a change of window may move it) */
  strayHops: 0,
  /** Work frames redrawn while the work stays in one window: one per stretch of work there, not one per action */
  reframes: 0,
  /** Pointers that vanish and come back within 1.5 s */
  blinks: 0,
  /** How long after its real action a press is drawn, at most (ms, the slowest one) */
  lagMax: 900,
  /** Actions that happened at a known point with no press drawn for them */
  undrawn: 0,
  /** Frames left up more than 400 ms after their window closed or moved */
  leftovers: 0,
};

type R = { x: number; y: number; w: number; h: number };
type Mark = { t: number; k: string; [k: string]: unknown };

// ---- The overlay, built for the replay page ----
if (!argv.includes("--keep") || !existsSync(path.join(out, "index.html"))) {
  execFileSync(process.execPath, [path.join(root, "agent-app/node_modules/vite/bin/vite.js"), "build", "--outDir", out, "--emptyOutDir", "--logLevel", "warn"], { cwd: path.join(root, "agent-app"), stdio: "inherit" });
}
const types: Record<string, string> = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml", ".png": "image/png" };
const server = createServer((req, res) => {
  const p = path.join(out, decodeURIComponent((req.url ?? "/").split("?")[0]).replace(/^\/$/, "/index.html"));
  if (!p.startsWith(out) || !existsSync(p)) return void res.writeHead(404).end();
  res.writeHead(200, { "content-type": types[path.extname(p)] ?? "application/octet-stream" }).end(readFileSync(p));
});
await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
const port = (server.address() as { port: number }).port;

// ---- A headless Chrome (Edge when there is none) with a profile of its own ----
const browsers = [`${process.env.ProgramFiles}\\Google\\Chrome\\Application\\chrome.exe`, `${process.env["ProgramFiles(x86)"]}\\Microsoft\\Edge\\Application\\msedge.exe`];
const exe = browsers.find(existsSync);
if (!exe) throw new Error("no Chrome or Edge to replay in");
const profile = fixtureDirectory(path.join(tmpdir(), "asctrl-fxcheck-"));

function screenOf(lines: { t: number; e: Record<string, unknown> }[]) {
  const info = lines.find((l) => l.e.phase === "info")?.e.info as { scale: number; monitors: { bounds: R; primary: boolean }[] } | undefined;
  if (info) return info;
  // An older log: this computer's primary screen, physical pixels
  const ps = `Add-Type -AssemblyName System.Windows.Forms; Add-Type 'using System; using System.Runtime.InteropServices; public class Dpi { [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v); [DllImport("user32.dll")] public static extern int GetDpiForSystem(); }'; [Dpi]::SetProcessDpiAwarenessContext([IntPtr]-4) | Out-Null; $s=[System.Windows.Forms.Screen]::PrimaryScreen; "$([Dpi]::GetDpiForSystem()) $($s.Bounds.Width) $($s.Bounds.Height) $($s.WorkingArea.Width) $($s.WorkingArea.Height)"`;
  const [dpi, bw, bh, ww, wh] = execFileSync("powershell.exe", ["-NoProfile", "-Command", ps], { encoding: "utf8" }).trim().split(/\s+/).map(Number);
  return { originX: 0, originY: 0, scale: dpi / 96, monitors: [{ bounds: { x: 0, y: 0, w: bw, h: bh }, work: { x: 0, y: 0, w: ww, h: wh }, primary: true }] };
}

async function openPage(scale: number, w: number, h: number) {
  const proc = owned(() => spawn(exe!, [`--user-data-dir=${profile}`, "--headless=new", "--remote-debugging-port=0", `--force-device-scale-factor=${scale}`, `--window-size=${w},${h}`, "--no-first-run", "--no-default-browser-check", "--disable-background-timer-throttling", "--disable-renderer-backgrounding", "about:blank"], { stdio: "ignore" }));
  const file = path.join(profile, "DevToolsActivePort");
  for (let i = 0; i < 100 && !existsSync(file); i++) await new Promise((r) => setTimeout(r, 100));
  const devPort = readFileSync(file, "utf8").split("\n")[0].trim();
  const list = (await (await fetch(`http://127.0.0.1:${devPort}/json/list`)).json()) as { type: string; webSocketDebuggerUrl: string }[];
  const ws = new WebSocket(list.find((t) => t.type === "page")!.webSocketDebuggerUrl);
  await new Promise((r, j) => ((ws.onopen = r), (ws.onerror = j)));
  let n = 0;
  const pending = new Map<number, (v: unknown) => void>();
  ws.onmessage = (m) => {
    const d = JSON.parse(String(m.data));
    if (d.id && pending.has(d.id)) pending.get(d.id)!(d), pending.delete(d.id);
    // The overlay's own errors would make the numbers lie: say them
    if (d.method === "Runtime.exceptionThrown") console.error("overlay error:", d.params.exceptionDetails?.exception?.description ?? d.params.exceptionDetails?.text);
    if (d.method === "Runtime.consoleAPICalled" && d.params.type === "error") console.error("overlay console:", d.params.args.map((a: { value?: unknown; description?: string }) => a.value ?? a.description).join(" "));
  };
  const send = (method: string, params: Record<string, unknown> = {}) =>
    new Promise<{ result?: { result?: { value?: unknown }; exceptionDetails?: unknown }; error?: unknown }>((r) => {
      pending.set(++n, r as (v: unknown) => void);
      ws.send(JSON.stringify({ id: n, method, params }));
    });
  await send("Runtime.enable");
  return { proc, ws, send };
}

// ---- Measuring ----

const overlaps = (a: R, b: R) => a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
const pct = (xs: number[], q: number) => (xs.length ? [...xs].sort((a, b) => a - b)[Math.min(xs.length - 1, Math.floor(q * xs.length))] : 0);

function measure(marks: Mark[]) {
  const fx = marks.filter((m) => m.k === "fx") as (Mark & { phase: string; tool: string; id: string; at?: { x: number; y: number } | null; zone?: R | null; win?: R | null; app?: R | null; gone?: boolean })[];
  const drawnActs = fx.filter((m) => m.phase === "start" && m.at && ["click", "element_action", "drag", "mouse_move", "mouse_down"].includes(m.tool));
  const works = fx.filter((m) => m.phase === "start" && !["observe", "read_text", "screenshot", "exec", "resume", "plan", "wait_for"].includes(m.tool));
  const t0 = works.length ? works[0].t : 0;
  const tEnd = fx.length ? fx[fx.length - 1].t : 0;

  // The windows the body worked in, in order. With the driver's `win` (the window) and `app` (its program's main
  // window) a window is its exact rect; older logs only give the frame, and a frame inside another one counts as
  // the same window. The orb may move when the program changes, the frame when the window does.
  const holds = (a: R, b: R) => b.x >= a.x - 6 && b.y >= a.y - 6 && b.x + b.w <= a.x + a.w + 6 && b.y + b.h <= a.y + a.h + 6;
  const same = (a: R, b: R) => Math.abs(a.x - b.x) + Math.abs(a.y - b.y) + Math.abs(a.w - b.w) + Math.abs(a.h - b.h) < 8;
  const wins: R[] = [];
  const idOf = (r: R, exact: boolean) => {
    let i = wins.findIndex((w) => (exact ? same(w, r) : holds(w, r) || holds(r, w)));
    if (i < 0) i = wins.push(r) - 1;
    else if (!exact && holds(r, wins[i])) wins[i] = r;
    return i;
  };
  let switches = 0;
  let frameSwitches = 0;
  let curApp = -1;
  let curWin = -1;
  for (const m of fx) {
    // A window closed or minimized: the frame closes in on it and the ball is left where it was
    // (or maximized, restored, moved: the frame follows the window, and the ball is left where the frame ends)
    if (m.phase === "start" && m.tool === "focus_window" && !["", "focus"].includes((m as { act?: string }).act ?? "")) {
      switches++, frameSwitches++, (curApp = curWin = -1);
      continue;
    }
    const acts = (m.phase === "start" && ["click", "element_action", "drag", "focus_window"].includes(m.tool)) || (m.phase === "end" && m.tool === "open_app");
    const w = acts ? (m.win ?? m.zone) : null;
    if (!w) continue;
    const exact = !!m.win;
    const a = idOf(m.app ?? w, exact);
    const f = idOf(w, exact);
    if (a !== curApp) switches++, (curApp = a);
    if (f !== curWin) frameSwitches++, (curWin = f);
  }

  // The orb's moves while it works: from rest to rest, more than 8 px apart, between the first action and 1.5 s
  // after the last (its flight home after that is not a hop)
  const body = marks.filter((m) => m.k === "body") as (Mark & { x: number; y: number; w: number; h: number; out: boolean })[];
  let hops = 0;
  let away = 0;
  const home = (marks.find((m) => m.k === "home") as { x: number; y: number } | undefined) ?? null;
  let travel = 0;
  let rest: { x: number; y: number } | null = null;
  const hopAt: number[] = [];
  for (let i = 0; i < body.length; i++) {
    const b = body[i];
    const nextT = body[i + 1]?.t ?? b.t + 1000;
    if (i > 0 && b.t >= t0 && b.t <= tEnd + 1500) travel += Math.hypot(b.x - body[i - 1].x, b.y - body[i - 1].y);
    // At rest: unchanged for 300 ms (a spring easing in the last pixel moves less than one per frame), and a ball (a body that is a frame round a window is not "the orb")
    if (nextT - b.t >= 300 && b.w < 60 && b.h < 60) {
      if (rest && Math.hypot(b.x - rest.x, b.y - rest.y) > 8 && b.t >= t0 && b.t <= tEnd + 1500) {
        hops++;
        hopAt.push(b.t);
        // Going home when the work is done, and out again, is not hopping about while working
        const atHome = (p: { x: number; y: number }) => !!home && Math.hypot(p.x - home.x, p.y - home.y) < 12;
        if (!atHome(rest) && !atHome(b)) away++;
      }
      rest = { x: b.x, y: b.y };
    }
  }

  // Work frames: drawn, per window (the distinct windows framed), and the ones still up after their window went
  const frames = marks.filter((m) => m.k === "frame+") as (Mark & { id: number; r: R })[];
  const gone = new Map((marks.filter((m) => m.k === "frame-") as (Mark & { id: number })[]).map((m) => [m.id, m.t]));
  const closings = [
    ...fx.filter((m) => m.gone && m.zone).map((m) => ({ t: m.t, r: m.zone! })),
    ...fx.filter((m) => m.tool === "focus_window" && m.phase === "end").flatMap((m) => {
      const st = fx.find((s) => s.id === m.id && s.phase === "start");
      return st?.zone && !m.zone ? [{ t: m.t, r: st.zone }] : [];
    }),
  ];
  let leftovers = 0;
  for (const c of closings) {
    for (const f of frames) {
      const end = gone.get(f.id) ?? Infinity;
      // A frame drawn for that window: round it, or round part of it (a window that only overlaps it is another one)
      if (f.t <= c.t && end > c.t + 400 && overlaps(f.r, c.r) && holds({ x: c.r.x - 16, y: c.r.y - 16, w: c.r.w + 32, h: c.r.h + 32 }, f.r)) leftovers++;
    }
  }
  const stillUp = frames.filter((f) => !gone.has(f.id)).length;

  // Pointers that vanish and come back in the same column within 1.5 s
  const ptrs = marks.filter((m) => m.k === "ptr+") as (Mark & { id: number; col: number })[];
  const colOf = new Map(ptrs.map((p) => [p.id, p.col]));
  const removed = marks.filter((m) => m.k === "ptr-") as (Mark & { id: number })[];
  let blinks = 0;
  for (const r of removed) if (ptrs.some((p) => p.col === colOf.get(r.id) && p.t >= r.t && p.t - r.t < 1500)) blinks++;

  // Lag: each action that happened at a known point (its step), and the press drawn for it
  const presses = marks.filter((m) => m.k === "press") as (Mark & { x: number; y: number })[];
  const used = new Set<Mark>();
  const lags: number[] = [];
  const lagAt: number[] = [];
  const undrawnAt: number[] = [];
  let undrawn = 0;
  // Actions that happened (a failed one shakes red instead of pressing)
  for (const s of fx.filter((m) => m.phase === "step" && ["click", "element_action"].includes(m.tool) && (m as { ok?: boolean }).ok !== false)) {
    const st = fx.find((m) => m.id === s.id && m.phase === "start");
    if (!st?.at) continue;
    const p = presses.find((q) => !used.has(q) && q.t >= st.t && q.t <= s.t + 4000 && Math.hypot(q.x - st.at!.x, q.y - st.at!.y) < 30);
    if (!p) {
      undrawn++;
      undrawnAt.push(s.t);
      continue;
    }
    used.add(p);
    lags.push(Math.max(0, p.t - s.t));
    lagAt.push(s.t);
  }
  // Clicks the driver carried out with no point to draw (the page channel): a gap the overlay can't fill
  const outerClicks = fx.filter((m) => m.phase === "start" && m.tool === "click" && !m.id.includes("."));
  const okEnd = (id: string) => fx.some((m) => m.phase === "end" && m.id === id && (m as { ok?: boolean }).ok !== false);
  const blind = outerClicks.filter((c) => okEnd(c.id) && !fx.some((m) => m.phase === "start" && m.id.startsWith(`${c.id}.`) && m.at)).length;

  return {
    actions: drawnActs.length,
    hops,
    switches,
    /** Moves of the orb, not to or from its home, that no change of program called for */
    strayHops: Math.max(0, away - Math.max(0, switches - 1)),
    travelPx: Math.round(travel),
    frames: frames.length,
    windows: wins.length,
    /** Frames drawn past one per stretch of work in a window */
    reframes: Math.max(0, frames.length - frameSwitches),
    pointers: ptrs.length,
    blinks,
    lagMedian: Math.round(pct(lags, 0.5)),
    lagMax: Math.round(Math.max(0, ...lags)),
    undrawn,
    blindClicks: blind,
    leftovers,
    stillUp,
    hopAt,
    /** When the action drawn latest happened */
    lagWorstAt: lagAt[lags.indexOf(Math.max(...lags))] ?? null,
    undrawnAt,
  };
}

let bad = 0;
const results: Record<string, unknown> = {};
let page: Awaited<ReturnType<typeof openPage>> | null = null;
let pageScreen = "";
try {
  for (const log of logs) {
    const lines = readFileSync(log, "utf8")
      .split(/\r?\n/)
      .filter(Boolean)
      .map((l) => {
        const i = l.indexOf(" ");
        return { t: Number(l.slice(0, i)), e: JSON.parse(l.slice(i + 1)) as Record<string, unknown> };
      });
    const info = screenOf(lines);
    const prim = info.monitors.find((m) => m.primary) ?? info.monitors[0];
    const key = `${info.scale} ${prim.bounds.w} ${prim.bounds.h}`;
    if (key !== pageScreen) {
      if (page) page.ws.close(), page.proc.kill();
      page = await openPage(info.scale, Math.round(prim.bounds.w / info.scale), Math.round(prim.bounds.h / info.scale));
      pageScreen = key;
    }
    const pg = page!;
    await pg.send("Page.navigate", { url: `http://127.0.0.1:${port}/?replay` });
    for (let i = 0; i < 50; i++) {
      const r = await pg.send("Runtime.evaluate", { expression: "typeof fxReplay", returnByValue: true });
      if (r.result?.result?.value === "function") break;
      await new Promise((r) => setTimeout(r, 100));
    }
    const events = lines.filter((l) => ["plan", "start", "step", "end"].includes(l.e.phase as string));
    // --squeeze: pauses longer than 10 s (the AI thinking) are cut to 10 s, past every linger of the overlay
    // (a frame stays 8 s), so a long run replays in minutes and draws the same
    if (argv.includes("--squeeze")) {
      let cut = 0;
      for (let i = 1; i < events.length; i++) {
        const gap = events[i].t - cut - events[i - 1].t;
        if (gap > 10_000) cut += gap - 10_000;
        events[i] = { ...events[i], t: events[i].t - cut };
      }
    }
    const r = await pg.send("Runtime.evaluate", {
      expression: `fxReplay(${JSON.stringify(info)}, ${JSON.stringify(events)}, 9000)`,
      awaitPromise: true,
      returnByValue: true,
    });
    if (r.result?.exceptionDetails || !Array.isArray(r.result?.result?.value)) throw new Error(`replay of ${log} failed: ${JSON.stringify(r.result?.exceptionDetails ?? r.error)}`);
    const all = r.result!.result!.value as Mark[];
    // --dump a-b: the marks between a and b ms, to see what drew what
    const dump = argv.find((a) => a.startsWith("--dump="));
    if (dump) {
      const [a, b] = dump.slice(7).split("-").map(Number);
      for (const x of all.filter((x) => x.t >= a && x.t <= b)) console.log(JSON.stringify(x));
    }
    const m = measure(all);
    results[path.basename(log)] = m;
    const broken = Object.entries(LIMITS).filter(([k, lim]) => (m as Record<string, unknown>)[k] as number > lim).map(([k, lim]) => `${k} ${(m as Record<string, unknown>)[k]} > ${lim}`);
    if (check && broken.length) bad++;
    if (!asJson) {
      const { hopAt: _h, lagWorstAt: _l, undrawnAt: _u, ...shown } = m;
      console.log(`${check ? (broken.length ? "✗" : "✓") + " " : ""}${path.basename(log)}: ${Object.entries(shown).map(([k, v]) => `${k} ${v}`).join(", ")}${broken.length ? `\n   over the limit: ${broken.join("; ")}` : ""}`);
    }
  }
} finally {
  if (page) page.ws.close(), page.proc.kill();
  server.close();
  await new Promise((r) => setTimeout(r, 300));
  try {
    rmSync(profile, { recursive: true, force: true });
  } catch {}
}
if (asJson) console.log(JSON.stringify(results, null, 2));
if (check) {
  console.log(bad ? `\n${bad} log(s) over the limits` : "\nAll logs within the limits");
  process.exit(bad ? 1 : 0);
}
