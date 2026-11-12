// One animation per tool call. `start` fires when the tool begins, `end` when its result arrives.
// The orb ties consecutive actions together: it is one continuously moving object that flies from
// the last spot straight to the next, never teleporting.
// If the result arrives before the start animation lands, the end animation waits for it (see `ready`).

import { center, home, monitorAt, monitors, primary, pt, rect, type Pt, type R } from "./geo";
import { orb } from "./orb";
import { bloom, capture, dart, hold, Outline, ripple, streaks, strand, sweep, type Tone } from "./light";
import { breathe, pulse } from "./edge";
import { closeCard, deckRect, fillEntries, fillLines, openCard, progress, scan, skeleton, termEnd, termStart, type Card } from "./panel";
import { easeInOutCubic, el, fadeOut, play, sleep, SPRING, tween } from "./motion";

export interface FxEvent {
  phase: "start" | "end";
  id: string;
  tool: string;
  args?: Record<string, unknown>;
  geo?: Record<string, unknown>;
  ok?: boolean;
  text?: string;
}

const marks = document.getElementById("marks") as HTMLDivElement;

const str = (v: unknown) => (typeof v === "string" ? v : v === undefined || v === null ? "" : String(v));
const num = (v: unknown, d = 0) => (typeof v === "number" && Number.isFinite(v) ? v : d);
const baseName = (p: string) => p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || p;
const dirName = (p: string) => {
  const i = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
  return i > 0 ? p.slice(0, i) : "";
};

// ---- Orb attention: drift back to the corner a while after the work is done ----

let homeTimer: number | undefined;
let active = 0;

function attend() {
  active++;
  window.clearTimeout(homeTimer);
  orb.setMood("work");
}

function release(ok = true, delay = 1500) {
  active = Math.max(0, active - 1);
  if (!ok) orb.setMood("error");
  window.clearTimeout(homeTimer);
  homeTimer = window.setTimeout(() => {
    if (active > 0) return;
    orb.setMood("idle");
    orb.goTo(home(), "slow");
  }, ok ? delay : delay + 1200);
}

/** Fly next to a target point (without covering it) */
function near(p: Pt, speed: "normal" | "fast" = "fast") {
  const work = monitorAt(p).work;
  const dx = p.x + 80 > work.x + work.w ? -52 : 52;
  const dy = p.y + 80 > work.y + work.h ? -46 : 46;
  orb.goTo({ x: p.x + dx, y: p.y + dy }, speed);
}

/** When each start animation lands; the end animation waits for it */
const ready = new Map<string, Promise<void>>();
const whenReady = (id: string) => {
  const p = ready.get(id) ?? Promise.resolve();
  ready.delete(id);
  return p;
};

/** The orb takes something in */
function swallow() {
  orb.bump(1.2);
  bloom(orb.pos, { size: 150, ms: 620 });
  ripple(orb.pos, { size: 50, ms: 560 });
}

// ---- Screenshot ----

const shots = new Map<string, { stop: (() => void)[]; ol?: Outline }>();

function shotRegions(args: Record<string, unknown>): R[] {
  const d = num(args.display);
  if (d > 0 && monitors[d - 1]) return [monitors[d - 1].bounds];
  return monitors.map((m) => m.bounds);
}

/** The only screenshot effect: the edge ring of the captured screens breathes once */
function screenshotEnd(e: FxEvent) {
  if (!e.ok) return;
  const shot = rect(e.geo?.shot);
  breathe(shot ? [shot] : shotRegions(e.args ?? {}));
}

// ---- Zoom ----

function zoomStart(e: FxEvent) {
  const a = pt(e.geo?.from);
  const b = pt(e.geo?.to);
  if (!a || !b) return;
  const r: R = { x: Math.min(a.x, b.x), y: Math.min(a.y, b.y), w: Math.abs(a.x - b.x), h: Math.abs(a.y - b.y) };
  near({ x: r.x + r.w, y: r.y + r.h });
  const ol = new Outline(r).popIn().trace(620);
  shots.set(e.id, { stop: [sweep(r, { loop: true })], ol });
  orb.setMood("focus");
  ready.set(e.id, sleep(560));
}

async function zoomEnd(e: FxEvent) {
  const s = shots.get(e.id);
  shots.delete(e.id);
  await whenReady(e.id);
  s?.stop.forEach((f) => f());
  const ol = s?.ol;
  if (!ol) return;
  if (!e.ok) {
    ol.shake();
    void ol.out("fade", 400, 500);
    return;
  }
  void ol.out("sink", 420);
  await capture(ol.r, () => orb.pos, { ms: 640 });
  swallow();
}

// ---- Mouse ----

let holdMark: HTMLElement | null = null;
const TONE = (b: string): Tone => (b === "right" ? "warm" : b === "middle" ? "mint" : "aurora");
const aims = new Map<string, HTMLElement>();

function clickStart(e: FxEvent) {
  const p = pt(e.geo?.at);
  if (!p) return;
  const tone = TONE(str(e.args?.button));
  const from = { ...orb.pos };
  near(p);
  // A beam shoots from the orb to the target, then settles into an aim ring
  ready.set(
    e.id,
    dart(from, p, { tone }).then(() => {
      const aim = el("div", `aim t-${tone}`, marks);
      aim.style.left = `${p.x}px`;
      aim.style.top = `${p.y}px`;
      aims.set(e.id, aim);
      play(aim, [{ transform: "scale(2.4)", opacity: 0 }, { transform: "scale(1)", opacity: 1 }], { spring: SPRING.snap });
    }),
  );
}

async function clickEnd(e: FxEvent) {
  const p = pt(e.geo?.at);
  await whenReady(e.id);
  const aim = aims.get(e.id);
  aims.delete(e.id);
  if (!p) return void aim?.remove();
  const tone = TONE(str(e.args?.button));
  if (!e.ok) {
    if (aim) void fadeOut(aim, 400, { transform: "scale(1.6)" });
    ripple(p, { size: 36, tone: "red" });
    return;
  }
  if (aim) void fadeOut(aim, 260, { transform: "scale(.5)" });
  const n = Math.max(1, Math.min(3, num(e.args?.count, 1)));
  bloom(p, { size: 130, ms: 620, tone });
  for (let i = 0; i < n; i++) ripple(p, { size: 34, ms: 560, to: 1.9 + i * 0.35, tone, delay: i * 130 });
}

function moveStart(e: FxEvent) {
  const p = pt(e.geo?.at);
  if (!p) return;
  near(p);
  ripple(p, { size: 26, ms: 520, from: 1.8, to: 0.5 });
}

function downStart(e: FxEvent) {
  const p = pt(e.geo?.at);
  if (!p) return;
  near(p);
  holdMark?.remove();
  holdMark = hold(p, TONE(str(e.args?.button)));
}

function upStart(e: FxEvent) {
  const p = pt(e.geo?.at);
  if (holdMark) void fadeOut(holdMark, 320, { transform: "scale(1.9)" });
  holdMark = null;
  if (p) ripple(p, { size: 34 });
}

function scrollStart(e: FxEvent) {
  const p = pt(e.geo?.at);
  if (!p) return;
  near(p);
  const amount = num(e.args?.amount, 3);
  const horiz = e.args?.horizontal === true;
  streaks(p, horiz ? (amount >= 0 ? "right" : "left") : amount >= 0 ? "down" : "up");
}

async function dragStart(e: FxEvent) {
  const a = pt(e.geo?.from);
  const b = pt(e.geo?.to);
  if (!a || !b) return;
  orb.goTo(a, "fast");
  const grip = hold(a);
  await sleep(340);
  const ms = 780;
  const line = strand(a, b, ms);
  // The orb itself pulls the line to the end point
  await tween(ms, (t, dt) => {
    const k = easeInOutCubic(t);
    orb.pin({ x: a.x + (b.x - a.x) * k, y: a.y + (b.y - a.y) * k }, dt);
  });
  orb.unpin();
  near(b, "normal");
  bloom(b, { size: 120, ms: 600 });
  ripple(b, { size: 34 });
  void fadeOut(grip, 360, { transform: "scale(1.8)" });
  void fadeOut(line, 520);
}

// ---- Windows ----

const winCtx = new Map<string, Outline>();

const aboveWindow = (r: R): Pt => ({ x: r.x + r.w / 2, y: Math.max(monitorAt(center(r)).work.y + 34, r.y - 30) });

function winStart(e: FxEvent) {
  const r0 = rect(e.geo?.rect);
  if (!r0) return;
  orb.goTo(aboveWindow(r0), "fast");
  winCtx.set(e.id, new Outline(r0).popIn().trace());
  ready.set(e.id, sleep(420));
}

async function winEnd(e: FxEvent) {
  const action = str(e.args?.action) || "focus";
  await whenReady(e.id);
  const o = winCtx.get(e.id);
  winCtx.delete(e.id);
  const r1 = rect(e.geo?.rect);
  if (!e.ok) {
    o?.shake();
    void o?.out("fade", 400, 600);
    return;
  }
  if (action === "close" && o) {
    bloom(center(o.r), { size: Math.min(520, Math.max(o.r.w, o.r.h) * 0.6), ms: 700 });
    void o.out("burst", 520);
    return;
  }
  if (action === "minimize" && o) {
    const w = monitorAt(center(o.r)).work;
    const to: R = { x: w.x + w.w / 2 - 40, y: w.y + w.h - 6, w: 80, h: 8 };
    await o.moveTo(to, 520);
    ripple(center(to), { size: 40 });
    void o.out("fade", 220);
    return;
  }
  if (!r1) return void o?.out("fade");
  // Move / maximize / restore: the same outline morphs to the new spot; if there was none (the window was minimized), pop one in
  orb.goTo(aboveWindow(r1), "fast");
  let ol = o;
  if (ol) await ol.moveTo(r1, 500);
  else {
    ol = new Outline(r1).popIn().trace();
    await sleep(360);
  }
  ol.flash();
  for (const p of [
    { x: r1.x, y: r1.y },
    { x: r1.x + r1.w, y: r1.y + r1.h },
  ])
    ripple(p, { size: 30, ms: 620 });
  void ol.out("fade", 480, 900);
}

function openAppStart() {
  orb.bump(1);
  bloom(orb.pos, { size: 180, ms: 700 });
  ripple(orb.pos, { size: 60, ms: 700 });
}

function openAppEnd(e: FxEvent) {
  if (!e.ok) return;
  const c = center(primary().work);
  void dart(orb.pos, c).then(() => {
    bloom(c, { size: 520, ms: 900 });
    ripple(c, { size: 120, ms: 900, to: 3.2 });
  });
}

// ---- Files ----

const cards = new Map<string, { card: Card; finish?: () => void }>();
const lines = (t: string) => t.replace(/\r/g, "").replace(/\n+$/, "").split("\n");

function fileStart(e: FxEvent) {
  const a = e.args ?? {};
  const path = str(a.path);
  const put = (card: Card, finish?: () => void) => {
    cards.set(e.id, { card, finish });
    return card;
  };
  switch (e.tool) {
    case "read_file":
      return void skeleton(put(openCard("read", baseName(path), dirName(path))));
    case "write_file":
      return void fillLines(put(openCard("write", baseName(path), dirName(path))), lines(str(a.content)), "add");
    case "edit_file": {
      const c = put(openCard("edit", baseName(path), dirName(path)));
      const oldL = lines(str(a.oldText)).slice(0, 5);
      const newL = lines(str(a.newText)).slice(0, 5);
      void (async () => {
        const olds = oldL.map((t) => el("div", "ln del", c.body, t || " "));
        olds.forEach((o, i) => play(o, [{ opacity: 0, transform: "translateX(-16px)" }, { opacity: 1, transform: "none" }], { spring: SPRING.snap, delay: i * 38, fill: "backwards" }));
        await sleep(520);
        olds.forEach((o, i) => play(o, [{ opacity: 1 }, { opacity: 0.35 }], { duration: 300, delay: i * 50 }));
        for (const [i, t] of newL.entries()) {
          const n = el("div", "ln add", c.body, t || " ");
          play(n, [{ opacity: 0, transform: "translateX(18px)" }, { opacity: 1, transform: "none" }], { spring: SPRING.snap, delay: i * 60, fill: "backwards" });
        }
      })();
      return;
    }
    case "list_dir":
      return void skeleton(put(openCard("dir", baseName(path || "~"), path)), 7);
    case "find_files":
    case "search_files": {
      const c = put(openCard(e.tool === "find_files" ? "find" : "search", str(a.pattern), path));
      skeleton(c, 5);
      scan(c, true);
      return;
    }
    case "upload_file": {
      const c = openCard("upload", baseName(path));
      put(c, progress(c, 0.9, 1400));
      return;
    }
    case "download_file": {
      const c = openCard("download", baseName(path));
      put(c, progress(c, 0.7, 1000));
      return;
    }
    case "read_clipboard":
      return void skeleton(put(openCard("clip", "")), 3);
    case "write_clipboard":
      return void fillLines(put(openCard("clip", "")), lines(str(a.text)), "add");
  }
}

async function fileEnd(e: FxEvent) {
  const ctx = cards.get(e.id);
  cards.delete(e.id);
  if (!ctx) return;
  const c = ctx.card;
  const text = e.text ?? "";
  if (!e.ok) return closeCard(c, false, text, 2200);
  let linger = 1500;
  switch (e.tool) {
    case "read_file":
      await fillLines(c, lines(text).slice(1), "mono"); // first line is path and encoding
      scan(c);
      linger = 2200;
      break;
    case "list_dir":
      await fillEntries(
        c,
        lines(text)
          .slice(1)
          .map((l) => {
            const name = (l.split(/\s{2,}/).pop() ?? l).trim();
            return name.endsWith("\\") ? { name: name.slice(0, -1), dir: true } : { name, dir: false };
          }),
      );
      break;
    case "find_files":
      await fillLines(c, lines(text).map((l) => baseName(l) || l));
      break;
    case "search_files": {
      let re: RegExp | undefined;
      try {
        re = new RegExp(str(e.args?.pattern), e.args?.ignoreCase === false ? "g" : "gi");
      } catch {
        re = undefined;
      }
      await fillLines(c, lines(text), "mono", re);
      linger = 2200;
      break;
    }
    case "upload_file":
    case "download_file": {
      ctx.finish?.();
      if (e.tool === "download_file") {
        // The file flies from the panel into the orb, then up and away (to the technician)
        const d = deckRect();
        if (d) await dart({ x: d.x + d.w - 40, y: d.y + d.h - 40 }, orb.pos, { tone: "mint" });
        swallow();
        void sleep(360).then(() => dart(orb.pos, { x: orb.pos.x, y: orb.pos.y - 320 }, { tone: "mint" }));
      }
      break;
    }
    case "read_clipboard":
      await fillLines(c, lines(text));
      break;
  }
  orb.setMood("happy");
  closeCard(c, true, "", linger);
}

// ---- Command line ----

function termCommand(e: FxEvent): string {
  const a = e.args ?? {};
  switch (e.tool) {
    case "run_powershell":
      return a.background === true ? `Start-Job { ${str(a.script).trim()} }` : str(a.script).trim();
    case "system_info":
      return "Get-ComputerInfo";
    case "list_processes":
      return `Get-Process | Sort-Object WorkingSet64 -Descending | Select -First ${num(a.top, 30)}`;
    case "event_log":
      return `Get-WinEvent -LogName ${str(a.log) || "System"} -MaxEvents ${num(a.max, 30)}`;
    case "job_output":
      return a.jobId === undefined ? "Get-Job" : `Receive-Job -Id ${str(a.jobId)}`;
    case "stop_job":
      return `Stop-Job -Id ${str(a.jobId)}`;
    case "reset_shell":
      return "exit; powershell";
    default:
      return e.tool;
  }
}

// ---- Dispatch ----

const TERM = new Set(["run_powershell", "system_info", "list_processes", "event_log", "job_output", "stop_job", "reset_shell"]);
const FILE = new Set(["read_file", "write_file", "edit_file", "list_dir", "find_files", "search_files", "upload_file", "download_file", "read_clipboard", "write_clipboard"]);

/** End events carry only the result; args and start geometry are filled in from here */
const started = new Map<string, Pick<FxEvent, "args" | "geo">>();

export function fxStart(e: FxEvent) {
  started.set(e.id, { args: e.args, geo: e.geo });
  // Screenshots only breathe the edge at the end; listing windows changes nothing and draws nothing
  if (e.tool === "screenshot" || e.tool === "list_windows") return;
  attend();
  pulse();
  switch (e.tool) {
    case "zoom":
      return zoomStart(e);
    case "click":
      return clickStart(e);
    case "mouse_move":
      return moveStart(e);
    case "mouse_down":
      return downStart(e);
    case "mouse_up":
      return upStart(e);
    case "scroll":
      return scrollStart(e);
    case "drag":
      return void dragStart(e);
    case "type_text":
    case "press_keys":
    case "hold_key":
      // Typing and key presses show nothing on screen
      return;
    case "focus_window":
      return winStart(e);
    case "open_app":
      return openAppStart();
    case "cursor_position":
      orb.setBusy(true);
      return;
  }
  if (TERM.has(e.tool)) return termStart(e.id, termCommand(e));
  if (FILE.has(e.tool)) return fileStart(e);
}

export function fxEnd(ev: FxEvent) {
  const st = started.get(ev.id);
  started.delete(ev.id);
  const e: FxEvent = { ...ev, args: ev.args ?? st?.args ?? {}, geo: { ...(st?.geo ?? {}), ...(ev.geo ?? {}) } };
  const ok = !!e.ok;
  // Never woke the orb at start, so there is nothing to release
  if (e.tool === "list_windows") return;
  switch (e.tool) {
    case "screenshot":
      // Screenshots only breathe the edge; they never woke the orb, so there is nothing to release
      return screenshotEnd(e);
    case "zoom":
      void zoomEnd(e);
      break;
    case "click":
      void clickEnd(e);
      break;
    case "focus_window":
      void winEnd(e);
      break;
    case "open_app":
      openAppEnd(e);
      break;
    case "cursor_position":
      orb.setBusy(false);
      break;
    default:
      if (TERM.has(e.tool)) termEnd(e.id, ok, e.text ?? "");
      else if (FILE.has(e.tool)) void fileEnd(e);
  }
  release(ok, TERM.has(e.tool) ? 2600 : 1500);
}
