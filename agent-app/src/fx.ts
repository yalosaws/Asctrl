// One animation per tool call. `start` fires when the tool begins, `end` when its result arrives.
// The orb ties consecutive actions together: it is one continuously moving object that flies from
// the last spot straight to the next, never teleporting, and while it works it becomes the thing doing
// the work (body.ts): pointers it splits into, a frame round a window, the panel.

import { invoke } from "@tauri-apps/api/core";
import { center, home, isTauri, monitorAt, monitors, pt, rect, type Pt, type R } from "./geo";
import { orb } from "./orb";
import { toolEnded, toolStarted } from "./think";
import { BALL, beat, body, busy, DROP_AT, holdUntil, job, keep, more, Pointer, tempo } from "./body";
import { bloom, dart, Outline, ripple, strand } from "./light";
import { breathe, pulse } from "./edge";
import { closeCard, deckRect, fillEntries, fillLines, openCard, progress, scan, setPanelHooks, skeleton, termEnd, termStart, type Card } from "./panel";
import { el, fadeOut, play, sleep, SPRING, tween } from "./motion";

export interface FxEvent {
  phase: "plan" | "start" | "step" | "end";
  id: string;
  tool: string;
  args?: Record<string, unknown>;
  geo?: Record<string, unknown>;
  ok?: boolean;
  text?: string;
  /** step: which action of the call just ran */
  index?: number;
  /** end: a probe that missed (the caller goes on another way), so no red */
  quiet?: boolean;
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

let errorTimer: number | undefined;
/** How long a failure keeps the orb red */
const ERROR_MS = 1600;

/** Red is a short flash, not a state */
function flashError() {
  orb.setMood("error");
  window.clearTimeout(errorTimer);
  errorTimer = window.setTimeout(() => {
    if (orb.currentMood === "error") orb.setMood(active > 0 ? "work" : "idle");
  }, ERROR_MS);
}

/** The work is done: the pointers still out come back into the ball, and only then does the orb go home */
async function goHomeWhenFree() {
  // While a work area is framed, or the body still has work queued, the orb stays
  if (active > 0 || zone) return;
  if (busy() || pressing > 0) return void (homeTimer = window.setTimeout(goHomeWhenFree, 600));
  await recallPointers();
  // New work came in while they were coming back: the orb stays with it
  if (active > 0 || zone || busy() || pressing > 0) return;
  orb.setMood("idle");
  orb.goTo(home(), "slow");
}

function release(ok = true, delay = 1500) {
  active = Math.max(0, active - 1);
  if (!ok) flashError();
  window.clearTimeout(homeTimer);
  homeTimer = window.setTimeout(goHomeWhenFree, delay);
}

/** The orb takes something in */
function swallow() {
  orb.bump(1.2);
  bloom(orb.pos, { size: 150, ms: 620 });
}

// ---- Results the body waits on ----

/** Each call's result, for jobs that wait on it */
const ends = new Map<string, { p: Promise<FxEvent>; r: (e: FxEvent) => void }>();
/** What a scroll's step brings: the area that scrolls, found by the driver while the scroll already goes ahead */
const scrollInfo = new Map<string, { p: Promise<FxEvent>; r: (e: FxEvent) => void }>();
function scrollInfoOf(id: string) {
  let d = scrollInfo.get(id);
  if (!d) {
    let r!: (e: FxEvent) => void;
    const p = new Promise<FxEvent>((res) => (r = res));
    scrollInfo.set(id, (d = { p, r }));
  }
  return d;
}

function endOf(id: string): Promise<FxEvent> {
  let d = ends.get(id);
  if (!d) {
    let r!: (e: FxEvent) => void;
    const p = new Promise<FxEvent>((res) => (r = res));
    ends.set(id, (d = { p, r }));
  }
  return d.p;
}

/** Keep a point inside its monitor's work area, `m` pixels in */
function inside(p: Pt, m = 30): Pt {
  const w = monitorAt(p).work;
  return { x: Math.min(w.x + w.w - m, Math.max(w.x + m, p.x)), y: Math.min(w.y + w.h - m, Math.max(w.y + m, p.y)) };
}

/** Fly the ball (as a ball) to p */
const fly = (p: Pt) => body.to({ cx: p.x, cy: p.y, ...BALL }, { cx: 170, cy: 170, w: 220, h: 220, r: 220 }, 0.82);

// ---- Screenshot ----

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

// ---- Zoom: the ball opens into a viewfinder round the area, flashes, and closes back round what it saw ----

function zoomStart(e: FxEvent) {
  const a = pt(e.geo?.from);
  const b = pt(e.geo?.to);
  if (!a || !b) return;
  const r: R = { x: Math.min(a.x, b.x), y: Math.min(a.y, b.y), w: Math.max(24, Math.abs(a.x - b.x)), h: Math.max(24, Math.abs(a.y - b.y)) };
  orb.setMood("focus");
  const end = endOf(e.id);
  void job(async () => {
    await fly({ x: r.x + r.w, y: r.y + r.h });
    body.content(r.w, r.h, '<div class="bd-corners"><i></i><i></i><i></i><i></i></div><div class="bd-flash"></div>');
    await body.open(r, "ring", 10);
    const res = await Promise.race([end, holdUntil(end, 15_000).then(() => null)]);
    if (res && !res.ok) return body.flash("err", 900);
    body.content(r.w, r.h).querySelector(".bd-flash")?.animate([{ opacity: 0.85 }, { opacity: 0 }], { duration: 420, easing: "ease-out" });
    await beat(380);
    await body.ball(center(r));
    body.kick("w", 320);
    body.kick("h", 320);
    await beat(200);
  });
}

// ---- Mouse ----

/** What each click call is waiting to hear, per action: true pressed, false failed, null never ran */
const sent = new Map<string, { outcome: { p: Promise<boolean | null>; r: (v: boolean | null) => void }[] }>();

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

/** Two frames this close are the same frame */
const sameRect = (a: R, b: R) => Math.abs(a.x - b.x) + Math.abs(a.y - b.y) + Math.abs(a.w - b.w) + Math.abs(a.h - b.h) < 8;

/**
 * Frame the area being worked on (where all the window's operable controls are). The frame is drawn once and
 * stays put while actions keep landing in it; it is only redrawn when the work moves to a different area.
 */
function frameZone(r: R, hug = false) {
  if (zone) window.clearTimeout(zone.timer);
  if (zone && !sameRect(zone.r, r)) {
    void zone.ol.out("fade", 300);
    zone = null;
  }
  // Around a whole window the ring overlaps its edge by 1.5 px (the ring is 2.5 wide), so rounding to device
  // pixels can never open a gap between them; around a group of controls it has room
  if (!zone) zone = { r, ol: new Outline(r, "", hug ? 1 : 3).popIn().trace(560) };
}

/** The frame the agent gave (the work area or the window), or one round the given points */
function frameFor(e: FxEvent, points: Pt[]): R | null {
  const given = rect(e.geo?.zone);
  if (given && e.geo?.zoneIsWindow === true) frameZone(given, true);
  else frameZone(given ? zoneOf([given]) : zoneOf(points.map((p) => ({ x: p.x, y: p.y, w: 0, h: 0 }))));
  return zone?.r ?? null;
}

/** Let the frame go a while after the last action in it (long enough to span the pause between steps), then send the orb home */
function unframeLater(ms = 8000, fadeMs = 420) {
  const z = zone;
  if (!z) return;
  window.clearTimeout(z.timer);
  z.timer = window.setTimeout(() => {
    if (zone !== z) return;
    void z.ol.out("fade", fadeMs);
    zone = null;
    goHomeWhenFree();
  }, ms);
}

/** The area a click framed has gone (the popup it was in closed): let the frame go now, not after the linger.
 *  Only the frame drawn round that area, in either form frameZone may have drawn it; a later action may already
 *  have framed somewhere else. */
function zoneGone(e: FxEvent) {
  const r = rect(e.geo?.zone);
  if (!zone || !r || !(sameRect(zone.r, r) || sameRect(zone.r, zoneOf([r])))) return;
  // A popup has already faded itself out by the time its window hides (Edge's take ~170 ms), so the frame
  // has nothing left to fade with: let it go at once. A window that moved or changed size is still there:
  // the frame fades off where it stood instead of being left round where the window used to be.
  unframeLater(0, e.geo?.moved === true ? 220 : 60);
}

/** The body is about to frame or work somewhere else (a window of its own, a scroll outside the frame):
 *  the work-area frame goes now rather than linger where nothing is happening any more */
function dropZone(unless?: Pt) {
  if (!zone) return;
  if (unless) {
    const r = zone.r;
    if (unless.x >= r.x && unless.x <= r.x + r.w && unless.y >= r.y && unless.y <= r.y + r.h) return;
  }
  window.clearTimeout(zone.timer);
  void zone.ol.out("fade", 220);
  zone = null;
}

/** The pointer presses (count times) once its action has happened, or shakes red if it failed */
async function pressAt(p: Pointer, outcome: Promise<boolean | null>, count: number, at: Pt = { x: p.x, y: p.y }) {
  // Normally the action has happened already; a slow one gets a few seconds before the press is drawn anyway
  // ... unless a newer action is already waiting: then the press is drawn now (the action happened, or the next
  // one would not have started)
  let got: boolean | null | undefined;
  void outcome.then((v) => (got = v));
  // Out on its own (not on the lane), so waiting here holds nothing else up
  await Promise.race([outcome, sleep(5000)]);
  await Promise.resolve();
  const r = got === undefined ? true : got;
  if (r === null) return;
  if (!r) {
    flashError();
    ripple(at, { size: 30, ms: 520, tone: "red" });
    return p.fail();
  }
  // The press plays on while the pointer already sets off for its next click (the press only scales it, the
  // glide only moves it): waiting for it here would add a press to every click of a busy column
  pressing++;
  void (async () => {
    try {
      for (let i = 0; i < count; i++) {
        ripple(at, { size: 28, ms: 480, to: 1.7 + i * 0.3 });
        await p.press();
      }
    } finally {
      pressing--;
    }
  })();
}

/** How many pointers share the work: the app being worked on is cut into this many columns, left to right, and
 *  each column has its own pointer, in its own colour (blue, violet, pink, teal from the left) */
const STRIPS = 4;
/** The pointer of each column, once it has been sent out; it stays out until the work is done */
const crew: (Pointer | null)[] = Array(STRIPS).fill(null);
/** What each column's pointer still has to do, in order: a click in its column waits for its earlier ones */
const crewQueue: Promise<unknown>[] = Array.from({ length: STRIPS }, () => Promise.resolve());
/** Pointers still on their way or pressing (waiting for their action to happen): the work is not done while any is */
let pressing = 0;
/** How many clicks each column has queued that its pointer has not set off for yet */
const waiting: number[] = Array(STRIPS).fill(0);

/** Whose column p is in: columns of the framed app, or of the screen when nothing is framed */
function stripOf(p: Pt): number {
  const r = zone?.r ?? monitorAt(p).work;
  return Math.min(STRIPS - 1, Math.max(0, Math.floor(((p.x - r.x) / Math.max(1, r.w)) * STRIPS)));
}

/** How late (ms) a pointer may start on a click before it hurries: past this it goes quicker the later it is */
const LATE_OK = 40;
/** How late it has to be to go at its quickest from lateness alone (4 times its usual pace) */
const LATE_MAX = 400;
/** The quickest a pointer ever goes: its minimums (PTR_MIN) divided by this still give a visible glide */
const RUSH_TOP = 6;
/** How much quicker a pointer goes for a click that started `late` ms ago with `behind` more queued after it.
 *  Clicks waiting count at once, before any of them is late: that is the backlog that grows otherwise. */
const rushFor = (late: number, behind = 0) =>
  Math.min(RUSH_TOP, Math.max(1 + 3 * Math.min(1, Math.max(0, late - LATE_OK) / (LATE_MAX - LATE_OK)), 1 + 1.25 * behind));

/** Column i's pointer goes to p in the arrow shape: from wherever it last worked, or, the first time, out of the
 *  still ball (once the ball is in place, `ready`) as a drop that takes the arrow shape there */
async function bring(i: number, p: Pt, rush: number, ready?: Promise<unknown>, near?: (ptr: Pointer) => void): Promise<Pointer> {
  const ptr = crew[i];
  if (ptr) {
    ptr.rush = rush;
    const d = Math.hypot(ptr.x - p.x, ptr.y - p.y);
    // Already there (sent ahead by fxPlan): nothing to glide, it presses at once
    if (d < 1) {
      near?.(ptr);
      return ptr;
    }
    await ptr.moveTo(p, Math.min(360, Math.max(150, d * 0.45)), Math.min(30, d * 0.1), near && (() => near(ptr)));
    return ptr;
  }
  await ready;
  const o = body.pos;
  const fresh = new Pointer(o.x - DROP_AT.x, o.y - DROP_AT.y, i);
  fresh.rush = rush;
  crew[i] = fresh;
  body.kick("w", -160);
  body.kick("h", 120);
  const far = Math.hypot(p.x - fresh.x, p.y - fresh.y);
  await fresh.moveTo(p, Math.min(560, Math.max(260, far * 0.75)), Math.min(40, far * 0.12));
  await fresh.arrow(260);
  return fresh;
}

/** The pointer whose column p is in goes there (after whatever it is still doing) and then does `work`.
 *  `arrived` resolves with it once it is at p; `done` once the work is finished too. `t0` is when the action
 *  really started: a pointer that only gets to it late goes quicker (rushFor), so it never trails far behind. */
function crewAt(p: Pt, work: (ptr: Pointer) => Promise<unknown> = async () => {}, t0 = performance.now(), ready?: Promise<unknown>) {
  // A pointer that is behind starts its work (the press) in the last stretch of its glide, not after it: the
  // two overlap, which wins back most of a press's time on every click it is late for
  const i = stripOf(p);
  let arrive!: (ptr: Pointer) => void;
  let fail!: (e: unknown) => void;
  const arrived = new Promise<Pointer>((res, rej) => ((arrive = res), (fail = rej)));
  pressing++;
  waiting[i]++;
  // One more click for this column: a pointer already gliding there hurries from now on, not only next time
  const out = crew[i];
  if (out) out.rush = Math.max(out.rush, rushFor(0, waiting[i]));
  const done = crewQueue[i]
    .then(async () => {
      waiting[i]--;
      const rush = rushFor(performance.now() - t0, waiting[i]);
      let working: Promise<unknown> | null = null;
      const ptr = await bring(i, p, rush, ready, rush > 1 ? (q) => void (working = work(q)) : undefined);
      arrive(ptr);
      await (working ?? work(ptr));
    })
    .catch((e) => {
      fail(e);
      crew[i]?.remove();
      crew[i] = null;
    })
    .finally(() => pressing--);
  crewQueue[i] = done;
  return { arrived, done };
}

/** One pointer goes back into the ball as a drop and is taken in */
async function comeBack(ptr: Pointer) {
  try {
    await ptr.drop(220);
    const home = body.pos;
    const back = { x: home.x - DROP_AT.x, y: home.y - DROP_AT.y };
    const dist = Math.hypot(back.x - ptr.x, back.y - ptr.y);
    await ptr.moveTo(back, Math.min(520, Math.max(240, dist * 0.7)), Math.min(30, dist * 0.08));
    body.kick("w", 200);
    body.kick("h", -150);
  } finally {
    ptr.remove();
  }
}

/** The work is done (or the link is gone): every pointer comes back into the ball, each once it has finished */
export function recallPointers(): Promise<void> {
  return Promise.all(
    crew.map((ptr, i) => {
      if (!ptr || ptr === held) return null;
      crew[i] = null;
      const back = crewQueue[i].then(() => beat(i * 70)).then(() => comeBack(ptr));
      crewQueue[i] = back.catch(() => {});
      return back;
    }),
  ).then(() => {});
}

/**
 * The pointer of the column p is in goes there and presses when its action really happens; then it waits there
 * for its column's next click, and is not called back until the work is done. Each pointer keeps to its own
 * column of the app, so the work is shared out, not taken by whichever is nearest. It sets off the moment the
 * action starts, not after the ball's own animations, and hurries when it is behind (rushFor). Only the drawing
 * overlaps; the actions run one after another as always.
 */
function sortie(p: Pt, outcome: Promise<boolean | null>, count: number, t0: number, ready: Promise<unknown>) {
  crewAt(p, (ptr) => pressAt(ptr, outcome, count, p), t0, ready);
}

/** Clicks and element actions: the ball waits still above the app and sends one pointer per target out of
 *  itself; each presses as its action happens and comes back on its own, while the next ones are already out. */
function dispatchStart(e: FxEvent) {
  const targets = Array.isArray(e.geo?.targets) ? (e.geo.targets as unknown[]).map(pt) : [pt(e.geo?.at)];
  const outcome = targets.map(() => {
    let r!: (v: boolean | null) => void;
    const p = new Promise<boolean | null>((res) => (r = res));
    return { p, r };
  });
  sent.set(e.id, { outcome });
  const aimed = targets.flatMap((p, i) => (p ? [{ p, i }] : []));
  if (!aimed.length) return;
  const framed = frameFor(
    e,
    aimed.map((a) => a.p),
  );
  const count = Math.max(1, Math.min(3, num(e.args?.count, 1)));
  const t0 = performance.now();
  // The ball takes its place over the app on the lane, as everything it does; the pointers do not wait for
  // the lane: each goes the moment its action starts (a new one only leaves the ball once the ball is there)
  const placed = job(async () => {
    try {
      const xs = aimed.map((a) => a.p.x);
      await fly(station({ x: (Math.min(...xs) + Math.max(...xs)) / 2, y: Math.min(...aimed.map((a) => a.p.y)) }, framed));
    } finally {
      // Whatever happened to the animation, the frame must not stay up for good
      unframeLater();
    }
  });
  aimed.forEach((a) => sortie(a.p, outcome[a.i].p, count, t0, placed));
}

/** The agent has just carried out step `index` */
function dispatchStep(e: FxEvent) {
  const i = num(e.index, -1);
  sent.get(e.id)?.outcome[i]?.r(!!e.ok);
}

function dispatchEnd(e: FxEvent) {
  const call = sent.get(e.id);
  sent.delete(e.id);
  if (!call) return;
  // Steps already reported their outcome as they happened. What is left never ran (skipped after a failure in a
  // sequence) or the whole call failed before acting. A batch reports each action on its own line.
  const marks = (re: RegExp) => new Set([...(e.text ?? "").matchAll(re)].map((m) => Number(m[1]) - 1));
  const failed = marks(/^(\d+)\. ✗/gm);
  const skipped = marks(/^(\d+)\. – skipped/gm);
  const perStep = Array.isArray(e.args?.actions) && (e.ok || failed.size + skipped.size > 0);
  call.outcome.forEach((o, i) => o.r(skipped.has(i) ? null : perStep ? !failed.has(i) : !!e.ok));
}

/** A pointer that stays out between mouse_down and mouse_up */
let held: Pointer | null = null;

/** Where the ball waits while pointers work at p: still, just above the top edge of the app being worked on
 *  (the framed area round p), so every action in that app is sent from the same spot and the ball never moves
 *  between them. With nothing framed round p, just above p. */
function station(p: Pt, r: R | null = zone?.r ?? null): Pt {
  const holds = r && p.x >= r.x && p.x <= r.x + r.w && p.y >= r.y && p.y <= r.y + r.h;
  return holds ? zoneSpot(r) : inside({ x: p.x, y: p.y - 64 });
}

/** The spot over a framed area: centred just above its top edge; below it when there is no room above (a
 *  maximized window), and just inside its top when there is room for neither */
function zoneSpot(r: R): Pt {
  const work = monitorAt(center(r)).work;
  const x = Math.min(work.x + work.w - 30, Math.max(work.x + 30, r.x + r.w / 2));
  if (r.y - 34 >= work.y + 24) return { x, y: r.y - 34 };
  if (r.y + r.h + 34 <= work.y + work.h - 24) return { x, y: r.y + r.h + 34 };
  return { x, y: r.y + 30 };
}
const above = (p: Pt) => station(p);

/** A pointer at p: the one held down goes there; otherwise the ball waits over the app and the pointer of
 *  p's column goes */
async function pointTo(p: Pt): Promise<Pointer> {
  if (held) {
    await held.moveTo(p, 300);
    return held;
  }
  await fly(above(p));
  return crewAt(p).arrived;
}

function moveStart(e: FxEvent) {
  const p = pt(e.geo?.at);
  if (!p) return;
  void job(async () => {
    await pointTo(p);
  });
}

/** Button held: the pointer stays out, pressed, until the button comes up */
function downStart(e: FxEvent) {
  const p = pt(e.geo?.at);
  if (!p) return;
  keep(true);
  void job(async () => {
    const ptr = await pointTo(p);
    ripple(p, { size: 26, ms: 420, from: 1.6, to: 0.6 });
    await ptr.hold(true);
    held = ptr;
  });
}

function upStart(e: FxEvent) {
  const p = pt(e.geo?.at);
  void job(async () => {
    const ptr = held ?? (p ? await pointTo(p) : null);
    held = null;
    keep(false);
    if (!ptr) return;
    if (p) await ptr.moveTo(p, 200);
    await ptr.hold(false);
    ripple({ x: ptr.x, y: ptr.y }, { size: 30, ms: 480 });
  });
}

/** Scroll: the ball lies down along the edge of what scrolls, where its own scrollbar is, as a scrollbar the
 *  same length; its thumb sits where the page is and runs to where the page went */
function scrollStart(e: FxEvent) {
  const p = pt(e.geo?.at);
  if (!p) return;
  dropZone(p);
  const end = endOf(e.id);
  const info = scrollInfoOf(e.id);
  const amount = num(e.args?.amount, 3);
  const horiz = e.args?.horizontal === true;
  const sign = amount >= 0 ? 1 : -1;
  const T = 10;
  void job(async () => {
    // The driver finds out what scrolls there on the side (the scroll itself never waits for it): the
    // animation waits a moment for that, and failing it draws the bar at the spot
    const got = await Promise.race([info.p, end.then(() => null), sleep(700).then(() => null)]);
    scrollInfo.delete(e.id);
    const area = rect(got?.geo?.area);
    const pos0 = typeof got?.geo?.pos === "number" ? (got.geo.pos as number) / 100 : null;
    const view = typeof got?.geo?.view === "number" ? (got.geo.view as number) / 100 : null;
    // The track: along the right (or bottom) edge of the scrolling area, inset a little; without one, at the spot
    const track: R = area
      ? horiz
        ? { x: area.x + 6, y: area.y + area.h - T - 3, w: Math.max(80, area.w - 12), h: T }
        : { x: area.x + area.w - T - 3, y: area.y + 6, w: T, h: Math.max(80, area.h - 12) }
      : (() => {
          const c = inside(p, 95);
          return horiz ? { x: c.x - 85, y: c.y - T / 2, w: 170, h: T } : { x: c.x - T / 2, y: c.y - 85, w: T, h: 170 };
        })();
    const L = horiz ? track.w : track.h;
    const len = Math.max(26, Math.min(L * 0.6, view !== null ? L * view : L * 0.3));
    const room = L - len - 4;
    const box = horiz ? { w: L, h: T } : { w: T, h: L };
    await fly({ x: track.x + track.w / 2, y: track.y + track.h / 2 });
    const inner = body.content(box.w, box.h, '<div class="bd-thumb"></div>');
    const thumb = inner.querySelector(".bd-thumb") as HTMLDivElement;
    const set = (f: number) => {
      const at = 2 + room * Math.min(1, Math.max(0, f));
      Object.assign(thumb.style, horiz ? { left: `${at}px`, top: "2px", width: `${len}px`, height: `${T - 4}px` } : { top: `${at}px`, left: "2px", height: `${len}px`, width: `${T - 4}px` });
    };
    const guess = (f: number, dir: number) => Math.min(1, Math.max(0, f + dir * sign * Math.min(0.35, Math.abs(amount) * 0.06)));
    body.mode("capsule");
    const shaped = body.to({ cx: track.x + track.w / 2, cy: track.y + track.h / 2, ...box, r: T / 2 }, horiz ? { w: 110, h: 240, r: 240 } : { w: 240, h: 110, r: 240 });
    // Where the page was: known when this area was scrolled last; otherwise worked back from where it went
    let from = pos0;
    if (from !== null) set(from);
    await shaped;
    const r = await Promise.race([end, sleep(2500).then(() => null)]);
    const now = typeof r?.geo?.pos === "number" ? (r.geo.pos as number) / 100 : null;
    if (from === null) from = now !== null ? guess(now, -1) : sign > 0 ? 0.1 : 0.9;
    // Where it went: the program says, or a guess in the right direction
    const to = now ?? guess(from, 1);
    set(from);
    if (Math.abs(to - from) < 0.002) {
      // Already at the end: the bar bumps against it
      body.kick(horiz ? "cx" : "cy", sign * 160);
    } else {
      await tween(Math.min(520, 240 + Math.abs(to - from) * 900) / tempo(), (t) => set(from! + (to - from!) * (1 - (1 - t) ** 3)));
    }
    await beat(more() ? 120 : 360);
  });
}

/** Drag: the ball pours into a pointer, which grabs at the start, carries along a line of light to the end and
 *  lets go there */
function dragStart(e: FxEvent) {
  const a = pt(e.geo?.from);
  const b = pt(e.geo?.to);
  if (!a || !b) return;
  frameFor(e, [a, b]);
  const end = endOf(e.id);
  const ms = Math.min(800, Math.max(320, Math.hypot(b.x - a.x, b.y - a.y) * 0.9));
  void job(async () => {
    try {
      const ptr = await pointTo(a);
      ripple(a, { size: 26, ms: 420, from: 1.6, to: 0.6 });
      await ptr.hold(true);
      const line = strand(a, b, ms);
      await ptr.moveTo(b, ms, Math.min(30, Math.hypot(b.x - a.x, b.y - a.y) * 0.08));
      await ptr.hold(false);
      ripple(b, { size: 30, ms: 480 });
      void fadeOut(line, 520);
      const r = await Promise.race([end, sleep(3000).then(() => null)]);
      if (r && !r.ok) {
        flashError();
        await ptr.fail();
      }
    } finally {
      unframeLater();
    }
  });
}

// ---- Waiting: the ball draws out into a bar that light runs through ----

/** A wait that is over this soon draws nothing: a plan checks each result with one, and a bar drawn out and
 *  pulled back for each would make the ball stretch and shrink all through it */
const WAIT_SHOWS_AFTER = 400;

function waitStart(e: FxEvent) {
  const end = endOf(e.id);
  void (async () => {
    const quick = await Promise.race([end.then(() => true), sleep(WAIT_SHOWS_AFTER).then(() => false)]);
    if (!quick) waitBar(end);
  })();
}

function waitBar(end: Promise<FxEvent>) {
  void job(async () => {
    const W = 200;
    const c = inside(body.pos, W / 2 + 10);
    await fly(c);
    body.content(W, 10, '<div class="bd-sheen"></div>');
    body.mode("bar");
    await body.to({ w: W, h: 10, r: 5 }, { w: 110, h: 260, r: 260 });
    const r = await Promise.race([end, holdUntil(end, 60_000).then(() => null)]);
    body.content(W, 10);
    if (r?.ok) body.flash("ok", 900);
  });
}

// ---- Windows: the ball opens into a frame round the window and moves with it ----

const wins = new Map<string, { r: R; on: boolean; opening: Promise<void>; settle: () => void; settled: Promise<void> }>();

const winTop = (r: R): Pt => inside({ x: r.x + r.w / 2, y: r.y + 14 });
/** The frame lies on the window's visible edge: its outer line 1px outside, so the ring covers the edge itself
 *  and no sliver of desktop shows between them */
const around = (r: R): R => ({ x: r.x - 1, y: r.y - 1, w: r.w + 2, h: r.h + 2 });
/** Windows 11 rounds window corners by 8px, except a maximized window, which fills its screen's work area */
function cornerOf(r: R) {
  const m = monitorAt(center(r)).work;
  const fills = r.x <= m.x + 2 && r.y <= m.y + 2 && r.x + r.w >= m.x + m.w - 2 && r.y + r.h >= m.y + m.h - 2;
  return fills ? 0 : 9;
}

function winStart(e: FxEvent) {
  // The body is about to become the frame round the window itself, which then moves with it: two frames,
  // one of them left behind when the window moves, is the leftover this prevents
  dropZone();
  const r0 = rect(e.geo?.rect);
  if (!r0) return void landed(e.id, 0);
  let settle!: () => void;
  const settled = new Promise<void>((res) => (settle = res));
  const w = { r: r0, on: false, opening: Promise.resolve(), settle, settled };
  wins.set(e.id, w);
  void job(async () => {
    await fly(winTop(w.r));
    body.content(0, 0);
    // From here a change of the window steers the frame, even while it is still opening out
    w.on = true;
    w.opening = body.open(around(w.r), "ring", cornerOf(w.r));
    await w.opening;
    // Even when the window already changed before the frame got there, the frame stays long enough to be seen
    await Promise.all([holdUntil(w.settled, 6000), beat(700)]);
    wins.delete(e.id);
  });
  landed(e.id, 0);
}

function landed(id: string, index: number) {
  if (isTauri) void invoke("fx_landed", { id, index }).catch(() => {});
}

/** Stiffness of the frame following the window: it keeps up with a maximize or restore without snapping */
const WIN_FOLLOW = 460;
/** Calls whose window change was already drawn the moment it happened (step event) */
const winDone = new Set<string>();

/** The window has just changed: the frame follows it now, not when the result comes back */
function winStep(e: FxEvent) {
  winDone.add(e.id);
  void winApply(e.id, str(started.get(e.id)?.args?.action) || "focus", rect(e.geo?.rect));
}

async function winEnd(e: FxEvent) {
  // Already drawn when it happened
  if (winDone.delete(e.id)) return;
  const w = wins.get(e.id);
  if (!e.ok) {
    if (w?.on) body.flash("err", 900);
    await sleep(500);
    return w?.settle();
  }
  // An agent that sends no step event: follow once the result is in
  await winApply(e.id, str(e.args?.action) || "focus", rect(e.geo?.rect));
}

/** Move the frame to where the window went (r1; null when minimized or closed) */
async function winApply(id: string, action: string, r1: R | null) {
  const w = wins.get(id);
  if (!w) return;
  // The body has not reached this window yet (other work first): it will frame where the window is now
  if (!w.on) {
    if (r1) w.r = r1;
    return w.settle();
  }
  await w.opening;
  if (action === "close") {
    // The frame closes in on the window's middle and becomes the ball again there
    bloom(center(w.r), { size: Math.min(420, Math.max(w.r.w, w.r.h) * 0.5), ms: 700 });
    await body.ball(center(w.r));
  } else if (action === "minimize") {
    const m = monitorAt(center(w.r)).work;
    await body.to({ cx: m.x + m.w / 2, cy: m.y + m.h - 8, w: 90, h: 8, r: 4 }, { all: 150 });
  } else if (r1) {
    const g = around(r1);
    await body.to({ cx: g.x + g.w / 2, cy: g.y + g.h / 2, w: g.w, h: g.h, r: cornerOf(r1) }, { all: WIN_FOLLOW }, 0.95);
    await beat(600);
  }
  w.settle();
}

/** Open an app: the ball waits; once the window is up it flies onto it and opens into a frame round it */
function openAppStart(e: FxEvent) {
  dropZone();
  const end = endOf(e.id);
  void job(async () => {
    body.kick("w", 200);
    body.kick("h", 200);
    const r = await Promise.race([end, holdUntil(end, 20_000).then(() => null)]);
    if (!r?.ok) return;
    const w = rect(r.geo?.rect);
    if (!w) return;
    await fly(center(w));
    body.content(0, 0);
    await body.open(around(w), "ring", cornerOf(w));
    body.flash("ok", 700);
    await beat(650);
    await body.ball(center(w));
  });
}

// ---- The work panel opens out of the ball and closes back into it ----

/** The body is standing in the panel's place under it, until the panel closes or other work needs the body */
let underDeck = false;
let deckClosed: (() => void) | null = null;

setPanelHooks({
  open(r: R) {
    return new Promise<boolean>((resolve) => {
      let answered = false;
      const answer = (v: boolean) => {
        if (!answered) (answered = true), resolve(v);
      };
      // Other work in front that the body can't leave yet: the panel opens on its own rather than wait
      let begun = false;
      window.setTimeout(() => begun || answer(false), 700);
      void job(async () => {
        if (answered) return;
        begun = true;
        await body.to({ cx: r.x + r.w - 40, cy: r.y + r.h - 40, ...BALL }, { all: 140 }, 0.8);
        if (answered) return;
        body.content(0, 0);
        await body.open(r, "light", 18);
        if (answered) return;
        underDeck = true;
        body.veil(true);
        answer(true);
        await holdUntil(new Promise<void>((res) => (deckClosed = res)), 180_000);
        deckClosed = null;
        // Other work took the body away while the panel stays: it comes out from under it
        if (underDeck) {
          underDeck = false;
          body.veil(false);
          return;
        }
        await body.ball({ x: r.x + r.w - 40, y: r.y + r.h - 40 });
      });
    });
  },
  close() {
    if (!underDeck) return false;
    underDeck = false;
    body.veil(false);
    deckClosed?.();
    return true;
  },
});

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

/** Tools that only look: they draw nothing and never wake the orb (a screenshot breathes the edge once, at the end) */
const LOOKS = new Set(["screenshot", "observe", "find", "read_text", "list_windows"]);

const TERM = new Set(["run_powershell", "system_info", "list_processes", "event_log", "job_output", "stop_job", "reset_shell"]);
const FILE = new Set(["read_file", "write_file", "edit_file", "list_dir", "find_files", "search_files", "disk_usage", "upload_file", "download_file", "read_clipboard", "write_clipboard"]);

/** End events carry only the result; args and start geometry are filled in from here */
const started = new Map<string, Pick<FxEvent, "args" | "geo">>();

export function fxStart(e: FxEvent) {
  // A plan running: one piece of work, so the pauses between its steps are not the AI thinking. It draws
  // nothing itself; its steps do.
  if (e.tool === "plan") return void toolStarted("plan");
  started.set(e.id, { args: e.args, geo: e.geo });
  begun.add(e.id);
  if (begun.size > 400) for (const id of [...begun].slice(0, 200)) begun.delete(id);
  toolStarted(e.tool);
  // Looking changes nothing and draws nothing
  if (LOOKS.has(e.tool)) return;
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
      return dragStart(e);
    case "wait_for":
      return waitStart(e);
    case "mouse_look":
    case "mouse":
    case "set_value":
    case "batch":
    case "type_text":
    case "press_keys":
      // Camera turns, typing and key presses show nothing on screen; actions that point at something send
      // their own animation (a click, a drag…) once they know where it is
      return;
    case "focus_window":
      return winStart(e);
    case "open_app":
      return openAppStart(e);
  }
  if (TERM.has(e.tool)) return termStart(e.id, termCommand(e));
  if (FILE.has(e.tool)) return fileStart(e);
}

/** A batch has found where its next click will land, while the step before it still runs: that column's pointer
 *  sets off now, so it is there (or nearly) when the click happens instead of only leaving then. Too late
 *  (the click has already started) and it is left out. */
export function fxPlan(e: FxEvent) {
  const p = pt(e.geo?.at);
  if (!p || begun.has(e.id)) return;
  crewAt(p);
}
/** Calls that have started lately, so a look-ahead that comes too late is left out even after they ended */
const begun = new Set<string>();

/** One action of a click or element_action call has just really happened */
export function fxStep(e: FxEvent) {
  if (e.tool === "focus_window") return winStep(e);
  if (e.tool === "zone") return zoneGone(e);
  if (e.tool === "scroll") return void scrollInfoOf(e.id).r(e);
  dispatchStep(e);
}

export function fxEnd(ev: FxEvent) {
  if (ev.tool === "plan") return void toolEnded("plan");
  const st = started.get(ev.id);
  started.delete(ev.id);
  const e: FxEvent = { ...ev, args: ev.args ?? st?.args ?? {}, geo: { ...(st?.geo ?? {}), ...(ev.geo ?? {}) } };
  const ok = !!e.ok;
  toolEnded(e.tool);
  // Whatever body was waiting on this result can move on
  const waiting = ends.get(e.id);
  ends.delete(e.id);
  waiting?.r(e);
  // Never woke the orb at start, so there is nothing to release. A look that took a picture breathes the edge.
  if (LOOKS.has(e.tool)) {
    if (e.tool === "screenshot" || e.geo?.shot) screenshotEnd(e);
    return;
  }
  switch (e.tool) {
    case "click":
    case "element_action":
      dispatchEnd(e);
      break;
    case "focus_window":
      void winEnd(e);
      break;
    default:
      if (TERM.has(e.tool)) termEnd(e.id, ok, e.text ?? "");
      else if (FILE.has(e.tool)) void fileEnd(e);
  }
  release(ok || !!ev.quiet, TERM.has(e.tool) ? 2600 : 1500);
}
