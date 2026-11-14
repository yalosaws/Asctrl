// One animation per tool call. `start` fires when the tool begins, `end` when its result arrives.
// The orb ties consecutive actions together: it is one continuously moving object that flies from
// the last spot straight to the next, never teleporting, and while it works it becomes the thing doing
// the work (body.ts): pointers it splits into, a strip it types in, a frame round a window, the panel.

import { invoke } from "@tauri-apps/api/core";
import { center, home, isTauri, monitorAt, monitors, pt, rect, type Pt, type R } from "./geo";
import { orb } from "./orb";
import { toolEnded, toolStarted } from "./think";
import { BALL, beat, body, busy, DROP_AT, holdUntil, job, keep, more, tempo, type Pointer } from "./body";
import { bloom, dart, Outline, ripple, strand } from "./light";
import { breathe, pulse } from "./edge";
import { closeCard, deckRect, fillEntries, fillLines, openCard, progress, scan, setPanelHooks, skeleton, termEnd, termStart, type Card } from "./panel";
import { el, fadeOut, play, sleep, SPRING, tween } from "./motion";

export interface FxEvent {
  phase: "start" | "step" | "end";
  id: string;
  tool: string;
  args?: Record<string, unknown>;
  geo?: Record<string, unknown>;
  ok?: boolean;
  text?: string;
  /** step: which action of the call just ran */
  index?: number;
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

function goHomeWhenFree() {
  // While a work area is framed, or the body still has work queued, the orb stays
  if (active > 0 || zone) return;
  if (busy()) return void (homeTimer = window.setTimeout(goHomeWhenFree, 600));
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

/** Where a pointer last clicked and when: typing that follows opens right below it */
let lastPoint: Pt | null = null;
let lastAt = 0;

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
function frameFor(e: FxEvent, points: Pt[]) {
  const given = rect(e.geo?.zone);
  if (given && e.geo?.zoneIsWindow === true) frameZone(given, true);
  else frameZone(given ? zoneOf([given]) : zoneOf(points.map((p) => ({ x: p.x, y: p.y, w: 0, h: 0 }))));
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
  // The popup has already faded itself out by the time its window hides (Edge's take ~170 ms), so the frame
  // has nothing left to fade with: let it go at once
  unframeLater(0, 60);
}

/** The pointer presses (count times) once its action has happened, or shakes red if it failed */
async function pressAt(p: Pointer, outcome: Promise<boolean | null>, count: number) {
  // Normally the action has happened already; a slow one gets a few seconds before the press is drawn anyway
  // ... unless a newer action is already waiting: then the press is drawn now (the action happened, or the next
  // one would not have started)
  let got: boolean | null | undefined;
  void outcome.then((v) => (got = v));
  await holdUntil(outcome, 5000);
  await Promise.resolve();
  const r = got === undefined ? true : got;
  if (r === null) return;
  lastPoint = { x: p.x, y: p.y };
  lastAt = performance.now();
  if (!r) {
    flashError();
    ripple({ x: p.x, y: p.y }, { size: 30, ms: 520, tone: "red" });
    return p.fail();
  }
  for (let i = 0; i < count; i++) {
    ripple({ x: p.x, y: p.y }, { size: 28, ms: 480, to: 1.7 + i * 0.3 });
    await p.press();
  }
}

/** Clicks and element actions: the ball flies over the targets, splits into one pointer per target, each
 *  presses as its action happens, and they all run back together into the ball. */
function dispatchStart(e: FxEvent) {
  const targets = Array.isArray(e.geo?.targets) ? (e.geo.targets as unknown[]).map(pt) : [pt(e.geo?.at)];
  const outcome = targets.map(() => {
    let r!: (v: boolean | null) => void;
    const p = new Promise<boolean | null>((res) => (r = res));
    return { p, r };
  });
  sent.set(e.id, { outcome });
  const aimed = targets.flatMap((p, i) => (p ? [{ p, i }] : []));
  // A typing call was waiting for this click to focus its field: it queues up right behind it
  const typing = e.id.endsWith(".f") ? focusing.get(e.id.slice(0, -2)) : undefined;
  if (!aimed.length) return typing?.();
  frameFor(
    e,
    aimed.map((a) => a.p),
  );
  const count = Math.max(1, Math.min(3, num(e.args?.count, 1)));
  void job(async () => {
    const xs = aimed.map((a) => a.p.x);
    const gather = inside({ x: (Math.min(...xs) + Math.max(...xs)) / 2, y: Math.min(...aimed.map((a) => a.p.y)) - 64 });
    await fly(gather);
    const ps = await body.split(aimed.map((a) => a.p));
    await Promise.all(ps.map((p, k) => pressAt(p, outcome[aimed[k].i].p, count)));
    await beat(more() ? 80 : 260);
    // More work waiting: the pointers run together where the last one is, so the next shape starts right there
    const last = ps[ps.length - 1];
    await body.merge(ps, more() ? { x: last.x + DROP_AT.x, y: last.y + DROP_AT.y } : gather);
    unframeLater();
  });
  typing?.();
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

async function pointTo(p: Pt): Promise<Pointer> {
  if (held) {
    await held.moveTo(p, 300);
    return held;
  }
  await fly({ x: p.x + 40, y: p.y + 40 });
  const ptr = await body.pointer();
  await ptr.moveTo(p, Math.min(520, Math.max(240, Math.hypot(ptr.x - p.x, ptr.y - p.y) * 1.2)));
  return ptr;
}

function moveStart(e: FxEvent) {
  const p = pt(e.geo?.at);
  if (!p) return;
  void job(async () => {
    const ptr = await pointTo(p);
    lastPoint = p;
    lastAt = performance.now();
    if (ptr === held) return;
    await beat(320);
    await body.unpointer(ptr);
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
    await beat(160);
    await body.unpointer(ptr);
  });
}

/** Scroll: the ball lies down along the edge of what scrolls, where its own scrollbar is, as a scrollbar the
 *  same length; its thumb sits where the page is and runs to where the page went */
function scrollStart(e: FxEvent) {
  const p = pt(e.geo?.at);
  if (!p) return;
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
    await beat(160);
    await body.unpointer(ptr);
    unframeLater();
  });
}

// ---- Typing: the ball opens into a strip right under the field, and the text types out in it ----

/** Typing calls waiting for the click that focuses their field (it is sent as "<call id>.f", after the
 *  typing call's own start), so the pointer clicks the field before the strip opens */
const focusing = new Map<string, () => void>();

function typeStart(e: FxEvent) {
  const end = endOf(e.id);
  const aimed = e.args?.target !== undefined || e.args?.x !== undefined;
  const focused = new Promise<void>((res) => {
    focusing.set(e.id, res);
    window.setTimeout(res, aimed ? 1500 : 0);
    void end.then(() => res());
  });
  void focused.then(() => {
    focusing.delete(e.id);
    void typeJob(e, end);
  });
}

function typeJob(e: FxEvent, end: Promise<FxEvent>) {
  const keys = e.tool === "press_keys";
  const text = [...str(keys ? e.args?.keys : e.args?.text)];
  const H = 44;
  const W = keys ? 240 : Math.min(440, Math.max(220, Math.min(text.length, 36) * 9.4 + 90));
  // The animation never outlasts the typing: once the call is done the rest of the text appears at once
  let done = false;
  void end.then(() => (done = true));
  return job(async () => {
    // Right below the field just clicked, or where the orb is
    const fresh = lastPoint && performance.now() - lastAt < 5000 ? lastPoint : null;
    const base = fresh ?? body.pos;
    const work = monitorAt(base).work;
    const below = base.y + 40 + H < work.y + work.h - 10;
    const c = inside({ x: base.x + (fresh ? W / 2 - 20 : 0), y: fresh ? base.y + (below ? 40 : -40) : base.y }, W / 2 + 10);
    await fly({ x: c.x - W / 2 + 22, y: c.y });
    const box = body.content(W, H, '<div class="bd-strip"><span class="txt"></span><span class="caps"></span></div>');
    const txt = box.querySelector(".txt") as HTMLSpanElement;
    const caps = box.querySelector(".caps") as HTMLSpanElement;
    await body.open({ x: c.x - W / 2, y: c.y - H / 2, w: W, h: H }, "panel", 12);
    const cap = (labels: string[]) => {
      caps.replaceChildren();
      labels.forEach((k, i) => {
        if (i) el("span", "plus", caps, "+");
        el("kbd", "", caps, k.length === 1 ? k.toUpperCase() : k[0].toUpperCase() + k.slice(1));
      });
      const ks = [...caps.querySelectorAll("kbd")];
      ks.forEach((k) => k.classList.add("dn"));
      window.setTimeout(() => ks.forEach((k) => k.classList.remove("dn")), 90);
      body.kick("h", -55);
    };
    if (keys) {
      const combos = text.join("").trim().split(/\s+/).filter(Boolean).slice(0, 6);
      for (const [i, combo] of combos.entries()) {
        cap(combo.split("+").filter(Boolean));
        // Keys go fast: each combination shows long enough to read, the last one until the call is done
        if (i < combos.length - 1) await beat(done ? 90 : 220);
      }
    } else {
      // Long text: the strip shows its tail, typed at about the speed the keys really go in
      const shown = text.length > 40 ? text.slice(-40) : text;
      const step = Math.min(45, Math.max(10, 700 / Math.max(1, shown.length)));
      const glyph = (ch: string) => (ch === "\n" ? "↵" : ch);
      for (const [i, ch] of shown.entries()) {
        if (done) {
          // The typing is finished: the rest goes in at once
          el("span", "", txt, shown.slice(i).map(glyph).join(""));
          cap([shown[shown.length - 1] === "\n" ? "Enter" : shown[shown.length - 1] === " " ? "Space" : shown[shown.length - 1]]);
          break;
        }
        el("span", "", txt, glyph(ch));
        cap([ch === " " ? "Space" : ch === "\n" ? "Enter" : ch]);
        await beat(step);
      }
    }
    await holdUntil(end, 4000);
    await beat(more() ? 60 : 200);
  });
}

// ---- Waiting: the ball draws out into a bar that light runs through ----

function waitStart(e: FxEvent) {
  const end = endOf(e.id);
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
  started.set(e.id, { args: e.args, geo: e.geo });
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
    case "type_text":
    case "press_keys":
      return typeStart(e);
    case "wait_for":
      return waitStart(e);
    case "mouse_look":
    case "mouse":
    case "set_value":
    case "batch":
      // Camera turns show nothing on screen; actions that point at something send their own animation
      // (a click, a drag, typing…) once they know where it is
      return;
    case "focus_window":
      return winStart(e);
    case "open_app":
      return openAppStart(e);
  }
  if (TERM.has(e.tool)) return termStart(e.id, termCommand(e));
  if (FILE.has(e.tool)) return fileStart(e);
}

/** One action of a click or element_action call has just really happened */
export function fxStep(e: FxEvent) {
  if (e.tool === "focus_window") return winStep(e);
  if (e.tool === "zone") return zoneGone(e);
  if (e.tool === "scroll") return void scrollInfoOf(e.id).r(e);
  dispatchStep(e);
}

export function fxEnd(ev: FxEvent) {
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
  release(ok, TERM.has(e.tool) ? 2600 : 1500);
}
