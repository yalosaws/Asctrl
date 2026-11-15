// The orb's working body. While the AI works, the orb itself turns into the thing doing the work and turns
// back into the ball when it is done: it splits into pointers to click, stretches into a strip to type, a
// scrollbar to scroll, a frame round a window, a viewfinder to look closer, a bar to wait, and opens out into
// the work panel for commands and files.
//
// The body is a box on springs: centre, width, height and corner radius each have their own, so when it
// changes shape the axes arrive at different moments and it stretches like a liquid instead of scaling like
// a picture. It is also the orb you see at rest: orb.ts keeps the position, mood and timing, and this body
// draws it, so whatever the orb does (fly, stretch, bounce, turn red) the same body does, and when work
// starts there is nothing to swap: the ball you were looking at is the thing that changes shape.

import type { Pt, R } from "./geo";
import { orb } from "./orb";
import { clamp, onFrame, sleep } from "./motion";

export type Shape = { cx: number; cy: number; w: number; h: number; r: number };
export type Mode = "ball" | "panel" | "light" | "ring" | "capsule" | "bar";
type Key = keyof Shape;

const KEYS: Key[] = ["cx", "cy", "w", "h", "r"];
export const BALL = { w: 32, h: 32, r: 16 };

// ---- Elements ----

const root = document.createElement("div");
root.id = "body";
root.dataset.mode = "ball";
root.innerHTML =
  '<div class="bd-skin"><div class="bd-wheel"></div><div class="bd-gloss"></div></div>' +
  '<div class="bd-glass"></div><div class="bd-ring"></div><div class="bd-rim"></div><div class="bd-tint"></div><div class="bd-content"></div>';
root.hidden = true;
document.body.append(root);
const wheel = root.querySelector(".bd-wheel") as HTMLDivElement;
const contentEl = root.querySelector(".bd-content") as HTMLDivElement;

const NS = "http://www.w3.org/2000/svg";
const ptrLayer = document.createElementNS(NS, "svg");
ptrLayer.id = "ptrs";
ptrLayer.innerHTML =
  "<defs>" +
  '<filter id="goo-f"><feGaussianBlur in="SourceGraphic" stdDeviation="6"/><feColorMatrix values="1 0 0 0 0  0 1 0 0 0  0 0 1 0 0  0 0 0 20 -8"/></filter>' +
  '<path id="ptr-probe"/></defs>';
document.body.append(ptrLayer);

/** The splitting happens in a box just big enough for it, so the liquid filter never covers the whole screen */
const goo = document.createElement("div");
goo.id = "goo";
goo.hidden = true;
document.body.append(goo);

// ---- Tempo ----
// The actions set the pace, never the animation. One action now and then plays at the full, unhurried pace;
// actions coming fast play faster, up to three times. The moment a newer action is waiting, the one on screen
// speeds up (six times with one waiting, far more with several), and at that speed any step shorter than a
// frame simply lands where it ends: every action is still drawn, in order, but the body never trails behind
// what really happens. Timeouts that wait for the real outcome stay in real time.

const started: number[] = [];
let tempoNow = 1;
let tempoAt = performance.now();

/** How fast the body animates right now: 1 = full pace, larger = quicker */
export function tempo() {
  const now = performance.now();
  while (started.length && now - started[0] > 4000) started.shift();
  const want = queued > 2 ? Math.min(100, 6 + (queued - 2) * 20) : queued > 1 ? 6 : Math.min(3, 1 + Math.max(0, started.length - 2) * 0.3);
  const dt = (now - tempoAt) / 1000;
  tempoAt = now;
  // Up at once (falling behind is what looks bad), down gently
  tempoNow = want >= tempoNow ? want : want + (tempoNow - want) * Math.exp(-dt / 0.8);
  return tempoNow;
}

/** A pause that is part of the animation: shorter when the body has to hurry */
export const beat = (ms: number) => {
  const d = ms / tempo();
  return d < 4 ? Promise.resolve() : sleep(d);
};

/** So fast that a spring would be done within a frame or two: it lands at once instead */
const snapping = () => tempo() >= 20;

// ---- Springs ----

const P = Object.fromEntries(KEYS.map((k) => [k, { x: 0, v: 0, t: 0, k: 170, z: 0.74 }])) as Record<Key, { x: number; v: number; t: number; k: number; z: number }>;
let waiters: { res: () => void; t0: number }[] = [];
let following: (() => R | null) | null = null;
let followPad = 0;
let running = false;

/** Stretch along the direction of flight while the orb flies (at rest only; a working body keeps its shape) */
let stretch = 0;
let heading = 0;

/** What was last written, so a frame that only moves the body touches nothing but its transform (a change of
 *  size means layout and repainting the colour wheel; a flight must not pay that every frame) */
let drawn = { w: -1, h: -1, r: -1 };

function apply() {
  const s = out ? 0 : stretch;
  // Half-pixel steps: smooth enough to the eye, and a body at rest stops writing its size altogether
  const w = Math.max(2, Math.round(P.w.x * 2) / 2);
  const h = Math.max(2, Math.round(P.h.x * 2) / 2);
  const r = Math.round(Math.max(0, Math.min(P.r.x, w / 2, h / 2)) * 2) / 2;
  if (w !== drawn.w || h !== drawn.h) {
    root.style.width = `${w}px`;
    root.style.height = `${h}px`;
    const d = Math.round(Math.hypot(w, h) * 1.05);
    wheel.style.width = wheel.style.height = `${d}px`;
  }
  if (r !== drawn.r) root.style.borderRadius = `${r}px`;
  drawn = { w, h, r };
  // The flight stretch is a transform: along the heading, longer and thinner, all on the compositor. Turned
  // back after the scale, so the colours inside keep their angle: without it they turn with the heading in
  // flight and snap back the moment the body lands.
  const a = heading.toFixed(3);
  root.style.transform =
    `translate3d(${(P.cx.x - w / 2).toFixed(2)}px, ${(P.cy.x - h / 2).toFixed(2)}px, 0)` +
    (s > 0.004 ? ` rotate(${a}rad) scale(${(1 + s).toFixed(3)}, ${(1 - s * 0.55).toFixed(3)}) rotate(${-a}rad)` : "");
}

function wake() {
  if (running) return;
  running = true;
  onFrame((dt, now) => {
    // Spring time runs faster when the body hurries; small steps keep the stiff springs stable
    const sim = dt * tempo();
    const n = Math.max(2, Math.ceil(sim / 0.008));
    const f = following?.();
    if (f) {
      P.cx.t = f.x + f.w / 2;
      P.cy.t = f.y + f.h / 2;
      P.w.t = f.w + followPad;
      P.h.t = f.h + followPad;
    }
    for (let s = 0; s < n; s++) {
      for (const k of KEYS) {
        const p = P[k];
        const c = 2 * Math.sqrt(p.k) * p.z;
        p.v += (p.k * (p.t - p.x) - c * p.v) * (sim / n);
        p.x += p.v * (sim / n);
      }
    }
    const moving = KEYS.some((k) => Math.abs(P[k].t - P[k].x) > 0.5 || Math.abs(P[k].v) > 8);
    if (!moving) for (const k of KEYS) (P[k].x = P[k].t), (P[k].v = 0);
    apply();
    // Close enough to go on with: the last pixel of the settle carries on underneath whatever comes next
    const there = !KEYS.some((k) => Math.abs(P[k].t - P[k].x) > 2.5 || Math.abs(P[k].v) > 60);
    waiters = waiters.filter((w) => {
      if (there || now - w.t0 > 2600) {
        w.res();
        return false;
      }
      return true;
    });
    if (!moving && !following) {
      running = false;
      return false;
    }
  });
}

// ---- Pointers ----

const N = 48;
const probe = ptrLayer.querySelector("#ptr-probe") as SVGPathElement;

/** N points round an outline, clockwise, starting at the top-left, so any two outlines can flow into each other */
function sample(d: string): [number, number][] {
  probe.setAttribute("d", d);
  const L = probe.getTotalLength();
  const pts: [number, number][] = [];
  for (let i = 0; i < N; i++) {
    const q = probe.getPointAtLength((L * i) / N);
    pts.push([q.x, q.y]);
  }
  let area = 0;
  for (let i = 0; i < N; i++) area += pts[i][0] * pts[(i + 1) % N][1] - pts[(i + 1) % N][0] * pts[i][1];
  if (area < 0) pts.reverse();
  const xs = pts.map((p) => p[0]);
  const ys = pts.map((p) => p[1]);
  const cx = (Math.min(...xs) + Math.max(...xs)) / 2;
  const cy = (Math.min(...ys) + Math.max(...ys)) / 2;
  const want = (-112 * Math.PI) / 180;
  let best = 0;
  let bd = 9;
  pts.forEach(([x, y], i) => {
    const a = Math.atan2(y - cy, x - cx);
    const dd = Math.abs(Math.atan2(Math.sin(a - want), Math.cos(a - want)));
    if (dd < bd) (bd = dd), (best = i);
  });
  return pts.slice(best).concat(pts.slice(0, best));
}

/** Where a drop's centre sits relative to the pointer's tip: over the arrow's body, so it pours in without a jump */
export const DROP_AT = { x: 9.5, y: 9.5 };
const CIRCLE = sample("M 9.5 -0.5 A 10 10 0 1 1 9.5 19.5 A 10 10 0 1 1 9.5 -0.5 Z");
/** A navigation arrow: tip at the top left, two wings out to the right and down, a notch between them */
const ARROW = sample("M 0 0 L 21 7.5 L 11.5 11.5 L 7.5 21 Z");
const toD = (pts: [number, number][]) => "M" + pts.map((p) => `${p[0].toFixed(2)} ${p[1].toFixed(2)}`).join("L") + "Z";
const easeBack = (t: number) => 1 + 2.2 * Math.pow(t - 1, 3) + 1.2 * Math.pow(t - 1, 2);
const easeIO = (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);

/** The shortest a pointer's moves, shape changes and presses get (ms) when the body hurries: a backlog speeds
 *  everything else up until it lands at once, but a pointer is always seen going to where it clicks */
export const PTR_MIN = { move: 180, morph: 120, press: 110 };

/** Squash and stretch: a pointer going faster than STRETCH_FROM (px/ms) is drawn longer along its way and
 *  narrower across it, by STRETCH_PER for each px/ms over, up to STRETCH_MAX; it springs back as it lands.
 *  A glide of a few frames reads as a quick dart instead of a jump, and the arrow itself stays sharp. */
const STRETCH_FROM = 1.5;
const STRETCH_PER = 0.07;
const STRETCH_MAX = 0.75;
/** The spring the stretch follows: stiff enough to keep up with a dart, a little bounce when it lands */
const STRETCH_K = 900;
const STRETCH_D = 34;
/** The stretch is about the middle of the arrow's body, not its tip */
const STRETCH_AT = 9;

/** A pointer split off the orb: its tip is at (x, y) */
export class Pointer {
  x: number;
  y: number;
  s = 1;
  private g = document.createElementNS(NS, "g");
  private paths: SVGPathElement[];
  private pts = CIRCLE.map((q) => [...q] as [number, number]);
  private m: { from: [number, number][]; to: [number, number][]; t: number; at: number; ms: number; res: () => void } | null = null;
  private live = true;
  /** Squash and stretch (see STRETCH_*): how much, how fast that is changing, the way it points, last frame */
  private st = 0;
  private stV = 0;
  private dir = 0;
  private last = { x: 0, y: 0, t: 0 };

  /** `k` picks its colour: the pointers take the four in turn */
  constructor(x: number, y: number, k = 0) {
    this.x = x;
    this.y = y;
    const under = document.createElementNS(NS, "path");
    under.setAttribute("class", "ptr-under");
    const body = document.createElementNS(NS, "path");
    body.setAttribute("class", `ptr-body c${k % 4}`);
    this.paths = [under, body];
    this.g.append(under, body);
    ptrLayer.append(this.g);
    this.draw();
    onFrame(() => {
      if (!this.live) return false;
      this.draw();
    });
  }

  private draw() {
    if (this.m) {
      const now = performance.now();
      // Added up frame by frame at the pointer's current pace, so a shape change hurries along with it
      const t = (this.m.t = Math.min(1, this.m.t + ((now - this.m.at) * this.rush) / this.m.ms));
      this.m.at = now;
      const k = easeBack(t);
      const { from, to } = this.m;
      this.pts = from.map((a, i) => [a[0] + (to[i][0] - a[0]) * k, a[1] + (to[i][1] - a[1]) * k]);
      if (t >= 1) {
        const r = this.m.res;
        this.m = null;
        r();
      }
    }
    const d = toD(this.pts);
    for (const p of this.paths) p.setAttribute("d", d);
    this.g.setAttribute("transform", `translate(${this.x.toFixed(2)} ${this.y.toFixed(2)})${this.stretch()} scale(${this.s.toFixed(3)})`);
  }

  /** The squash and stretch for this frame, as a transform about the middle of the arrow ("" when there is none) */
  private stretch(): string {
    const now = performance.now();
    const l = this.last;
    const dt = l.t ? Math.min(50, now - l.t) : 0;
    const dx = this.x - l.x;
    const dy = this.y - l.y;
    this.last = { x: this.x, y: this.y, t: now };
    if (dt <= 0) return "";
    const v = Math.hypot(dx, dy) / dt;
    if (v > 0.3) this.dir = (Math.atan2(dy, dx) * 180) / Math.PI;
    const want = Math.min(STRETCH_MAX, Math.max(0, (v - STRETCH_FROM) * STRETCH_PER));
    // A damped spring towards it, a few small steps per frame so a slow frame cannot make it blow up
    const n = Math.ceil(dt / 4);
    const h = dt / 1000 / n;
    for (let i = 0; i < n; i++) {
      this.stV += (STRETCH_K * (want - this.st) - STRETCH_D * this.stV) * h;
      this.st += this.stV * h;
    }
    if (Math.abs(this.st) < 0.004 && Math.abs(this.stV) < 0.05) {
      this.st = this.stV = 0;
      return "";
    }
    // Longer along the way it goes, narrower across, keeping about the same area; a bounce past zero squashes
    const along = Math.max(0.6, 1 + this.st);
    const across = 1 / Math.sqrt(along);
    const c = (STRETCH_AT * this.s).toFixed(2);
    const a = this.dir.toFixed(1);
    return ` translate(${c} ${c}) rotate(${a}) scale(${along.toFixed(3)} ${across.toFixed(3)}) rotate(${-a}) translate(-${c} -${c})`;
  }

  /** How far behind its clicks this pointer is: 1 on time, more when late or when clicks are waiting for it.
   *  Everything it draws is that many times quicker, minimums included, and a change takes effect mid-glide,
   *  so a busy pointer catches up instead of trailing further behind. */
  rush = 1;

  private morph(to: [number, number][], ms: number) {
    // A pointer is always seen changing shape, however much the body hurries
    const d = Math.max(PTR_MIN.morph, ms / tempo());
    return new Promise<void>((res) => (this.m = { from: this.pts.map((q) => [...q] as [number, number]), to, t: 0, at: performance.now(), ms: d, res }));
  }
  /** Pour into the arrow shape */
  arrow(ms = 360) {
    return this.morph(ARROW, ms);
  }
  /** Back into a round drop */
  drop(ms = 280) {
    return this.morph(CIRCLE, ms);
  }

  /** Fly the tip to p; `near` is called once it is most of the way there */
  async moveTo(p: Pt, ms: number, lift = 0, near?: () => void) {
    const a = { x: this.x, y: this.y };
    let told = !near;
    // Even in a hurry it glides there: it never jumps
    await tween(
      ms,
      (e) => {
        this.x = a.x + (p.x - a.x) * e;
        this.y = a.y + (p.y - a.y) * e - Math.sin(Math.PI * e) * lift;
        if (!told && e >= 0.7) {
          told = true;
          near!();
        }
      },
      easeIO,
      PTR_MIN.move,
      () => this.rush,
    );
  }

  /** Press down and up (the tip stays put) */
  async press() {
    await tween(200, (_, t) => (this.s = 1 - 0.22 * Math.sin(Math.PI * t)), (t) => t, PTR_MIN.press, () => this.rush);
  }

  /** Held down: a little smaller until let go */
  async hold(on: boolean) {
    const from = this.s;
    const to = on ? 0.84 : 1;
    await tween(140, (e) => (this.s = from + (to - from) * e), easeIO, PTR_MIN.press, () => this.rush);
  }

  /** On failure: a quick shake, red */
  async fail() {
    this.paths[1].style.fill = "var(--red)";
    const x = this.x;
    await tween(360, (_, t) => (this.x = x + Math.sin(t * Math.PI * 4) * 5 * (1 - t)), (t) => t);
    // It stays out for the next target, so the red is only for the shake; it can set off again meanwhile
    void beat(400).then(() => (this.paths[1].style.fill = ""));
  }

  remove() {
    this.live = false;
    this.g.remove();
  }
}

/** A per-frame animation driven by elapsed time; `floor` is the shortest it may get when the body hurries */
function tween(ms: number, step: (e: number, t: number) => void, ease = easeIO, floor = 0, rush: number | (() => number) = 1): Promise<void> {
  const base = ms / tempo();
  const len = () => {
    const r = typeof rush === "function" ? rush() : rush;
    return Math.max(floor / r, base / r);
  };
  // Shorter than a frame: it lands at once
  if (len() < 12) {
    step(ease(1), 1);
    return Promise.resolve();
  }
  // Progress is added up frame by frame, so a pointer told to hurry while on its way speeds up from there on
  return new Promise((resolve) => {
    let last = performance.now();
    let t = 0;
    onFrame((_, now) => {
      t = Math.min(1, t + (now - last) / len());
      last = now;
      step(ease(t), t);
      if (t >= 1) {
        resolve();
        return false;
      }
    });
  });
}

// ---- The body ----

/** Working: the body has its own shape and place; otherwise it is the orb and goes where the orb goes */
let out = false;

export const body = {
  get pos(): Pt {
    return { x: P.cx.x, y: P.cy.x };
  },
  get out() {
    return out;
  },

  /** Start working: the ball stops following the orb and goes where the work is */
  takeOver() {
    if (out) return;
    out = true;
    stretch = 0;
    this.place({ cx: P.cx.x, cy: P.cy.x });
    this.mode("ball");
  },

  /** Done: the body is a ball again, and the orb takes up from where it stands */
  handBack() {
    if (!out) return;
    out = false;
    following = null;
    contentEl.replaceChildren();
    orb.place(this.pos);
  },

  /** Spring toward a shape. `ks` sets the stiffness per key (or `all`); lower damping `z` overshoots more. */
  to(t: Partial<Shape>, ks: Partial<Record<Key | "all", number>> = {}, z = 0.74): Promise<void> {
    if (snapping()) {
      this.place(t);
      return Promise.resolve();
    }
    for (const k of Object.keys(t) as Key[]) {
      P[k].t = t[k]!;
      P[k].k = ks[k] ?? ks.all ?? 170;
      P[k].z = z;
    }
    wake();
    return new Promise((res) => waiters.push({ res, t0: performance.now() }));
  },

  place(t: Partial<Shape>) {
    for (const k of Object.keys(t) as Key[]) Object.assign(P[k], { x: t[k], t: t[k], v: 0 });
    apply();
  },

  kick(k: Key, v: number) {
    P[k].v += v;
    wake();
  },

  mode(m: Mode) {
    root.dataset.mode = m;
  },

  /** What the body carries inside it (terminal text, a typed line, a scrollbar thumb...), laid out at its final size */
  content(w: number, h: number, html = ""): HTMLDivElement {
    Object.assign(contentEl.style, { width: `${w}px`, height: `${h}px` });
    contentEl.innerHTML = html;
    return contentEl;
  },

  /** Keep the shape fitted round a rectangle as it moves (a window being maximized) */
  follow(f: (() => R | null) | null, pad = 0) {
    following = f;
    followPad = pad;
    for (const k of KEYS) P[k].k = 260;
    if (f) wake();
  },

  /** Back to a ball where it stands: the long side gives way last, so it pours back together */
  async ball(at?: Pt) {
    const wide = P.w.x >= P.h.x;
    const c = at ?? this.pos;
    // The short side gives way first in its current look (a frame stays a frame); only then does the colour pour in
    const big = Math.min(P.w.x, P.h.x) > 60;
    if (!big) this.mode("ball");
    void this.to(wide ? { cy: c.y, h: BALL.h } : { cx: c.x, w: BALL.w }, { all: 210 });
    await beat(big ? 170 : 110);
    this.mode("ball");
    await this.to(wide ? { cx: c.x, w: BALL.w, r: BALL.r } : { cy: c.y, h: BALL.h, r: BALL.r }, { all: 200 });
    await this.to(BALL, { all: 220 });
  },

  /** Open out into a rectangle: across first, then down, like a liquid filling it */
  async open(r: R, m: Mode, radius = 14) {
    this.mode(m);
    const wide = r.w >= r.h;
    const c = { x: r.x + r.w / 2, y: r.y + r.h / 2 };
    void this.to(wide ? { cx: c.x, w: r.w, r: radius } : { cy: c.y, h: r.h, r: radius }, wide ? { cx: 130, w: 130, r: 130 } : { cy: 130, h: 130, r: 130 });
    await beat(110);
    await this.to(wide ? { cy: c.y, h: r.h } : { cx: c.x, w: r.w }, { all: 110 });
  },

  flash(cls: "ok" | "err", ms = 1200) {
    root.classList.add(cls);
    window.setTimeout(() => root.classList.remove(cls), ms);
  },

  /** Hide while something else stands exactly where the body is (the work panel), and come back */
  veil(on: boolean) {
    root.style.visibility = on ? "hidden" : "";
  },

  /** The ball divides into one drop per target; each drop flies to its target and pours into a pointer there */
  async split(targets: Pt[]): Promise<Pointer[]> {
    const o = this.pos;
    const box = gooBox([o, ...targets.map((t) => ({ x: t.x + DROP_AT.x, y: t.y + DROP_AT.y }))]);
    root.style.visibility = "hidden";
    const blob = gooDot("goo-blob", o, box);
    blob.animate([{ scale: 1 }, { scale: 0.55, offset: 0.35 }, { scale: 0 }], { duration: (640 + targets.length * 80) / tempo(), easing: "cubic-bezier(.5,0,.3,1)", fill: "forwards" });
    const list = await Promise.all(
      targets.map(async (t, i) => {
        const d = gooDot("goo-drop", o, box, `var(--ptr-${(i % 4) + 1})`);
        const end = { x: t.x + DROP_AT.x, y: t.y + DROP_AT.y };
        // Each drop curves out its own way, so several never travel on top of each other
        const bend = (i - (targets.length - 1) / 2) * 40;
        const nx = -(end.y - o.y);
        const ny = end.x - o.x;
        const nl = Math.hypot(nx, ny) || 1;
        const c = { x: (o.x + end.x) / 2 + (nx / nl) * bend, y: (o.y + end.y) / 2 + (ny / nl) * bend - 20 };
        await beat(i * 90);
        const ms = Math.min(620, Math.max(340, Math.hypot(end.x - o.x, end.y - o.y) * 0.7));
        await tween(ms, (e) => {
          const u = 1 - e;
          moveDot(d, { x: u * u * o.x + 2 * u * e * c.x + e * e * end.x, y: u * u * o.y + 2 * u * e * c.y + e * e * end.y }, box);
        });
        const p = new Pointer(t.x, t.y, i);
        d.remove();
        await p.arrow();
        return p;
      }),
    );
    blob.remove();
    goo.hidden = true;
    return list;
  },

  /** Every pointer turns back into a drop and they all run together into one ball at `at` */
  async merge(list: Pointer[], at: Pt) {
    await Promise.all(list.map((p) => p.drop()));
    const box = gooBox([at, ...list.map((p) => ({ x: p.x + DROP_AT.x, y: p.y + DROP_AT.y }))]);
    const blob = gooDot("goo-blob", at, box);
    blob.style.scale = "0";
    await Promise.all(
      list.map(async (p, i) => {
        const s = { x: p.x + DROP_AT.x, y: p.y + DROP_AT.y };
        const d = gooDot("goo-drop", s, box, `var(--ptr-${(i % 4) + 1})`);
        p.remove();
        await beat(i * 70);
        const ms = Math.min(560, Math.max(320, Math.hypot(at.x - s.x, at.y - s.y) * 0.6));
        await tween(ms, (e) => {
          moveDot(d, { x: s.x + (at.x - s.x) * e, y: s.y + (at.y - s.y) * e }, box);
          blob.style.scale = String(Math.max(Number(blob.style.scale) || 0, Math.min(1, (i + e) / list.length)));
        });
        d.remove();
      }),
    );
    this.place({ cx: at.x, cy: at.y, ...BALL });
    this.mode("ball");
    root.style.visibility = "";
    blob.remove();
    goo.hidden = true;
    this.kick("w", 260);
    this.kick("h", -200);
  },
};

function gooBox(pts: Pt[]): R {
  const pad = 40;
  const x = Math.min(...pts.map((p) => p.x)) - pad;
  const y = Math.min(...pts.map((p) => p.y)) - pad;
  const r = { x, y, w: Math.max(...pts.map((p) => p.x)) + pad - x, h: Math.max(...pts.map((p) => p.y)) + pad - y };
  Object.assign(goo.style, { left: `${r.x}px`, top: `${r.y}px`, width: `${r.w}px`, height: `${r.h}px` });
  goo.hidden = false;
  return r;
}

function gooDot(cls: string, p: Pt, box: R, color?: string) {
  const d = document.createElement("i");
  d.className = cls;
  if (color) d.style.background = color;
  goo.append(d);
  moveDot(d, p, box);
  return d;
}

function moveDot(d: HTMLElement, p: Pt, box: R) {
  d.style.transform = `translate(${(p.x - box.x).toFixed(1)}px, ${(p.y - box.y).toFixed(1)}px)`;
}

// ---- The lane: one body, so one job at a time ----
// Jobs run in the order the tools ran. A job that holds its shape until a result comes back gives way as soon
// as another job is queued, so a long command never makes the next click wait. When the last job ends the
// body pours back into a ball and hands back to the orb.

let lane: Promise<void> = Promise.resolve();
let queued = 0;
let giveWay: (() => void) | null = null;
/** Something has to keep the body out after its job (a mouse button still held down) */
let kept = false;

/** Queue work for the body: every job runs, in order; a backlog makes them run faster (see tempo) */
export function job(fn: () => Promise<void>): Promise<void> {
  queued++;
  started.push(performance.now());
  giveWay?.();
  const run = lane
    .then(async () => {
      body.takeOver();
      await fn();
    })
    .catch((e) => console.error(e))
    .finally(async () => {
      queued--;
      if (queued === 0 && !kept && body.out) {
        await body.ball();
        if (queued === 0 && !kept) body.handBack();
      }
    });
  lane = run;
  return run;
}

export const busy = () => queued > 0;
/** More work is waiting behind the job running now */
export const more = () => queued > 1;

export function keep(on: boolean) {
  kept = on;
}

/** Keep the current shape until `until` settles, a later job is queued, or `max` ms pass */
export function holdUntil(until: Promise<unknown>, max = 30_000): Promise<void> {
  return new Promise((resolve) => {
    const finish = () => {
      window.clearTimeout(t);
      if (giveWay === finish) giveWay = null;
      resolve();
    };
    const t = window.setTimeout(finish, max);
    giveWay = finish;
    void until.then(finish, finish);
  });
}

// ---- Being the orb ----

orb.onMove(() => {
  if (out) return;
  const o = orb.pos;
  P.cx.x = P.cx.t = o.x;
  P.cy.x = P.cy.t = o.y;
  P.cx.v = P.cy.v = 0;
  const speed = Math.hypot(orb.vel.x, orb.vel.y);
  stretch = clamp(speed / 2600, 0, 0.3);
  heading = Math.atan2(orb.vel.y, orb.vel.x);
  apply();
});

orb.onEvent((e) => {
  switch (e.kind) {
    case "show":
      root.getAnimations().forEach((a) => a.cancel());
      if (e.on) {
        root.hidden = false;
        root.animate([{ scale: "0", opacity: 0 }, { scale: "1", opacity: 1 }], { duration: 520, easing: "cubic-bezier(.2,1.5,.4,1)" });
      } else {
        const a = root.animate([{ scale: "1", opacity: 1 }, { scale: "0", opacity: 0 }], { duration: 300, easing: "cubic-bezier(.5,0,.85,.35)", fill: "forwards" });
        a.finished.then(() => (root.hidden = true), () => {});
      }
      break;
    case "bump":
      body.kick("w", 320 * e.strength);
      body.kick("h", 320 * e.strength);
      break;
    case "mood":
      root.dataset.mood = e.mood;
      if (e.mood === "error" && e.was !== "error") body.kick("cx", -420);
      break;
    case "state":
      root.classList.toggle("busy", ["busy", "waiting", "thinking"].some((c) => orb.el.classList.contains(c)));
      break;
  }
});
root.dataset.mood = orb.currentMood;
body.place({ cx: orb.pos.x, cy: orb.pos.y, ...BALL });

// ---- How fast the colours turn ----
// Busy, or a bar, turns them faster. The speed eases to the new rate and the colours keep their angle: changing
// the CSS animation's duration instead would make it jump to another angle (the colours "reset" for a frame).

let spinRate = 1;
let spinTo = 1;
function spin() {
  const want = root.dataset.mode === "bar" ? 3.4 : root.classList.contains("busy") ? 3.7 : 1;
  if (want === spinTo) return;
  spinTo = want;
  const a = wheel.getAnimations()[0];
  if (!a) return void (spinRate = want);
  const from = spinRate;
  const t0 = performance.now();
  onFrame((_dt, now) => {
    if (spinTo !== want) return false;
    const t = Math.min(1, (now - t0) / 500);
    spinRate = from + (want - from) * (1 - (1 - t) ** 2);
    a.playbackRate = spinRate;
    return t < 1;
  });
}
new MutationObserver(spin).observe(root, { attributes: true, attributeFilter: ["class", "data-mode"] });
