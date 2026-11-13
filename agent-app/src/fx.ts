// One animation per tool call. `start` fires when the tool begins, `end` when its result arrives.
// The orb ties consecutive actions together: it is one continuously moving object that flies from
// the last spot straight to the next, never teleporting.
// If the result arrives before the start animation lands, the end animation waits for it (see `ready`).

import { center, home, monitorAt, monitors, primary, pt, rect, type Pt, type R } from "./geo";
import { orb } from "./orb";
import { Cursor } from "./cursor";
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
    // While a work area is framed the orb waits over it; it goes home when the frame goes
    if (active > 0 || zone) return;
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

/** Virtual cursors out for each call, by action index (an action with nothing to point at has none) */
const sent = new Map<string, (Cursor | null)[]>();

// ---- Work zone: the area being operated on, framed while the orb works there ----

/** Room left around the controls inside the frame, and the smallest frame drawn */
const ZONE_PAD = 14;
const ZONE_MIN = { w: 120, h: 64 };

let zone: { r: R; ol: Outline; timer?: number } | null = null;

function union(a: R, b: R): R {
  const x = Math.min(a.x, b.x);
  const y = Math.min(a.y, b.y);
  return { x, y, w: Math.max(a.x + a.w, b.x + b.w) - x, h: Math.max(a.y + a.h, b.y + b.h) - y };
}

/** Rectangles together, padded, at least ZONE_MIN, and inside their monitor */
function zoneOf(rs: R[]): R {
  let r = rs.reduce(union);
  r = { x: r.x - ZONE_PAD, y: r.y - ZONE_PAD, w: r.w + ZONE_PAD * 2, h: r.h + ZONE_PAD * 2 };
  if (r.w < ZONE_MIN.w) r = { ...r, x: r.x - (ZONE_MIN.w - r.w) / 2, w: ZONE_MIN.w };
  if (r.h < ZONE_MIN.h) r = { ...r, y: r.y - (ZONE_MIN.h - r.h) / 2, h: ZONE_MIN.h };
  const m = monitorAt(center(r)).bounds;
  const x = Math.max(m.x + 2, r.x);
  const y = Math.max(m.y + 2, r.y);
  return { x, y, w: Math.min(m.x + m.w - 2, r.x + r.w) - x, h: Math.min(m.y + m.h - 2, r.y + r.h) - y };
}

/** Where the orb waits: centred above the zone, or below it when there is no room above, or inside its top edge */
function zoneSpot(r: R): Pt {
  const work = monitorAt(center(r)).work;
  const x = Math.min(work.x + work.w - 30, Math.max(work.x + 30, r.x + r.w / 2));
  if (r.y - 34 >= work.y + 24) return { x, y: r.y - 34 };
  if (r.y + r.h + 34 <= work.y + work.h - 24) return { x, y: r.y + r.h + 34 };
  return { x, y: r.y + 30 };
}

/**
 * Frame the area being worked on (where all the window's operable controls are) and bring the orb over it.
 * The frame is drawn once and stays put while actions keep landing in it; it is only redrawn when the work
 * moves to a different area. Resolves once the orb is in place.
 */
function frameZone(r: R, hug = false): Promise<void> {
  const same = (a: R, b: R) => Math.abs(a.x - b.x) + Math.abs(a.y - b.y) + Math.abs(a.w - b.w) + Math.abs(a.h - b.h) < 8;
  if (zone) window.clearTimeout(zone.timer);
  if (zone && !same(zone.r, r)) {
    void zone.ol.out("fade", 300);
    zone = null;
  }
  // Around a whole window the ring overlaps its edge by 1.5 px (the ring is 2.5 wide), so rounding to device
  // pixels can never open a gap between them; around a group of controls it has room
  if (!zone) zone = { r, ol: new Outline(r, "", hug ? 1 : 3).popIn().trace(560) };
  const spot = zoneSpot(zone.r);
  const far = Math.hypot(orb.pos.x - spot.x, orb.pos.y - spot.y) > 24;
  orb.goTo(spot, "fast");
  return far ? sleep(360) : Promise.resolve();
}

/** Let the frame go a while after the last action in it (long enough to span the pause between steps), then send the orb home */
function unframeLater(ms = 8000) {
  const z = zone;
  if (!z) return;
  window.clearTimeout(z.timer);
  z.timer = window.setTimeout(() => {
    if (zone !== z) return;
    void z.ol.out("fade", 420);
    zone = null;
    if (active > 0) return;
    orb.setMood("idle");
    orb.goTo(home(), "slow");
  }, ms);
}

/** Clicks and element actions: the orb frames the area, flies over it, then sends a virtual cursor to each target */
function dispatchStart(e: FxEvent) {
  const targets = Array.isArray(e.geo?.targets) ? (e.geo.targets as unknown[]).map(pt) : [pt(e.geo?.at)];
  if (!targets.some(Boolean)) return;
  // The agent says where the work area is (all operable controls, or the window a click lands in);
  // failing that, frame just the targets
  const given = rect(e.geo?.zone);
  const inPlace = given && e.geo?.zoneIsWindow === true ? frameZone(given, true) : frameZone(given ? zoneOf([given]) : zoneOf(targets.flatMap((p) => (p ? [{ x: p.x, y: p.y, w: 0, h: 0 }] : []))));
  // The orb spins up on the way and flings each cursor off its rim as its colour comes round
  orb.whirl(1000 + targets.length * 120);
  const cursors = targets.map((p) => (p ? new Cursor() : null));
  sent.set(e.id, cursors);
  const labels = Array.isArray(e.geo?.labels) ? (e.geo.labels as unknown[]) : [];
  ready.set(e.id, inPlace.then(() => Promise.all(cursors.map((c, i) => c?.go(targets[i]!, str(labels[i]))))).then(() => {}));
}

async function dispatchEnd(e: FxEvent) {
  await whenReady(e.id);
  const cursors = sent.get(e.id) ?? [];
  sent.delete(e.id);
  // A batch reports each action on its own line ("2. ✗ …"), so only the ones that failed flash red
  const failed = new Set<number>();
  for (const m of (e.text ?? "").matchAll(/^(\d+)\. ✗/gm)) failed.add(Number(m[1]) - 1);
  const batch = Array.isArray(e.args?.actions);
  const count = Math.max(1, Math.min(3, num(e.args?.count, 1)));
  const whole = !e.ok && failed.size === 0; // the call failed before any action ran
  cursors.forEach((c, i) => c && void c.finish(batch && !whole ? !failed.has(i) : !!e.ok, count));
  unframeLater();
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
    case "disk_usage": {
      const c = put(openCard("disk", "Disk usage", path || "C:\\"));
      skeleton(c, 5);
      scan(c, true);
      return;
    }
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
    case "disk_usage":
      // Header, then the largest folders and files as written by the agent
      await fillLines(c, lines(text).filter((l) => l.trim()).slice(0, 14), "mono");
      linger = 2200;
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
const FILE = new Set(["read_file", "write_file", "edit_file", "list_dir", "find_files", "search_files", "disk_usage", "upload_file", "download_file", "read_clipboard", "write_clipboard"]);

/** End events carry only the result; args and start geometry are filled in from here */
const started = new Map<string, Pick<FxEvent, "args" | "geo">>();

export function fxStart(e: FxEvent) {
  started.set(e.id, { args: e.args, geo: e.geo });
  // Screenshots only breathe the edge at the end; listing windows changes nothing and draws nothing
  if (e.tool === "screenshot" || e.tool === "list_windows" || e.tool === "list_elements") return;
  attend();
  pulse();
  switch (e.tool) {
    case "zoom":
      return zoomStart(e);
    case "click":
    case "element_action":
      return dispatchStart(e);
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
    case "mouse_look":
      // Typing, key presses and camera turns show nothing on screen
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
  if (e.tool === "list_windows" || e.tool === "list_elements") return;
  switch (e.tool) {
    case "screenshot":
      // Screenshots only breathe the edge; they never woke the orb, so there is nothing to release
      return screenshotEnd(e);
    case "zoom":
      void zoomEnd(e);
      break;
    case "click":
    case "element_action":
      void dispatchEnd(e);
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
