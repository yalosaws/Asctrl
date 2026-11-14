// Light primitives: bloom, ripple, dart, sweep, outline. All plain DOM elements that only animate
// transform and opacity and remove themselves when done. No fullscreen canvas.

import { center, grow, snapRect, type Pt, type R } from "./geo";
import { clamp, done, EASE, el, fadeOut, play, playOut, SPRING } from "./motion";

const layer = document.getElementById("marks") as HTMLDivElement;

export type Tone = "aurora" | "warm" | "mint" | "red";

/** A mark at p, coloured by a tone, or by any CSS colour when given */
function at(cls: string, p: Pt, tone: Tone = "aurora", color?: string) {
  const m = el("div", `${cls} t-${tone}`, layer);
  if (color) {
    m.style.setProperty("--b1", `color-mix(in srgb, ${color} 85%, transparent)`);
    m.style.setProperty("--b2", `color-mix(in srgb, ${color} 38%, transparent)`);
  }
  m.style.left = `${p.x}px`;
  m.style.top = `${p.y}px`;
  return m;
}

function setRect(b: HTMLElement, r: R) {
  b.style.left = `${r.x}px`;
  b.style.top = `${r.y}px`;
  b.style.width = `${r.w}px`;
  b.style.height = `${r.h}px`;
}

/** A soft glow blooms from the centre */
export function bloom(p: Pt, o: { size?: number; ms?: number; tone?: Tone; color?: string; delay?: number } = {}) {
  const b = at("bloom", p, o.tone, o.color);
  b.style.setProperty("--s", `${o.size ?? 150}px`);
  void playOut(b, [{ transform: "scale(.12)", opacity: 1 }, { transform: "scale(1)", opacity: 0 }], { duration: o.ms ?? 680, delay: o.delay ?? 0 });
}

/** A thin ring spreads outward (contracts when from > to) */
export function ripple(p: Pt, o: { size?: number; ms?: number; from?: number; to?: number; delay?: number; tone?: Tone; color?: string } = {}) {
  const r = at("ripple", p, o.tone, o.color);
  r.style.setProperty("--s", `${o.size ?? 40}px`);
  const from = o.from ?? 0.35;
  const to = o.to ?? 1.9;
  void playOut(r, [{ transform: `scale(${from})`, opacity: from > to ? 0 : 1 }, { transform: `scale(${to})`, opacity: from > to ? 1 : 0 }], {
    duration: o.ms ?? 600,
    delay: o.delay ?? 0,
  });
}

/** A beam shoots from a to b; resolves on arrival */
export function dart(a: Pt, b: Pt, o: { tone?: Tone } = {}): Promise<void> {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const dist = Math.hypot(dx, dy);
  if (dist < 6) return Promise.resolve();
  const d = at("dart", a, o.tone);
  d.style.transform = `rotate(${Math.atan2(dy, dx)}rad)`;
  const tail = el("i", "", d);
  const ms = clamp(dist * 0.5, 200, 460);
  const arrive = play(tail, [{ transform: "translateX(0)", opacity: 0 }, { transform: `translateX(${dist * 0.25}px)`, opacity: 1, offset: 0.25 }, { transform: `translateX(${dist}px)`, opacity: 1 }], {
    duration: ms,
    easing: "cubic-bezier(.5,0,.15,1)",
  });
  return done(arrive).then(() => {
    // The tail retracts into the landing point
    void playOut(d, [{ opacity: 1 }, { opacity: 0 }], { duration: 160, easing: "linear" });
  });
}

/** A diagonal gradient band sweeps across the area; returns a stop function */
export function sweep(r: R, o: { ms?: number; loop?: boolean } = {}) {
  const box = el("div", "sweep", layer);
  setRect(box, snapRect(r));
  const band = el("i", "", box);
  const bw = Math.max(180, r.w * 0.28);
  band.style.width = `${bw}px`;
  const ms = o.ms ?? clamp(r.w * 0.55, 620, 1100);
  const frames: Keyframe[] = [{ transform: `translateX(${-bw * 1.5}px) skewX(-16deg)` }, { transform: `translateX(${r.w + bw * 0.5}px) skewX(-16deg)`, offset: o.loop ? 0.72 : 1 }];
  if (o.loop) frames.push({ transform: `translateX(${r.w + bw * 0.5}px) skewX(-16deg)` });
  const a = play(band, frames, { duration: o.loop ? ms / 0.72 : ms, easing: EASE.inOut, iterations: o.loop ? Infinity : 1 });
  if (!o.loop) void done(a).then(() => box.remove());
  return () => void fadeOut(box, 220);
}

/** A "snapshot" shrinks from the area into a target point (the orb takes the screenshot) */
export function capture(r: R, to: () => Pt, o: { ms?: number } = {}): Promise<void> {
  const s = snapRect(r);
  const sheet = el("div", "photo", layer);
  setRect(sheet, s);
  const c = center(s);
  const t = to();
  const k = Math.max(0.012, 34 / Math.max(s.w, s.h));
  const dx = t.x - c.x;
  const dy = t.y - c.y;
  return playOut(
    sheet,
    [
      { transform: "translate(0,0) scale(1)", opacity: 0 },
      { transform: "translate(0,0) scale(.985)", opacity: 1, offset: 0.18 },
      { transform: `translate(${dx * 0.12}px,${dy * 0.12}px) scale(${Math.max(k, 0.8)})`, opacity: 1, offset: 0.4 },
      { transform: `translate(${dx}px,${dy}px) scale(${k})`, opacity: 1, offset: 0.94 },
      { transform: `translate(${dx}px,${dy}px) scale(${k * 0.4})`, opacity: 0 },
    ],
    { duration: o.ms ?? 760, easing: "cubic-bezier(.6,0,.3,1)" },
  );
}

/** Press-and-hold marker */
export function hold(p: Pt, tone: Tone = "aurora", color?: string) {
  const h = at("hold", p, tone, color);
  play(h, [{ transform: "scale(2)", opacity: 0 }, { transform: "scale(1)", opacity: 1 }], { spring: SPRING.pop });
  return h;
}

/** A few short streaks flowing in one direction (scroll) */
export function streaks(p: Pt, dir: "up" | "down" | "left" | "right") {
  const ang = { right: 0, down: 90, left: 180, up: 270 }[dir];
  const g = at("streaks", p);
  g.style.transform = `rotate(${ang}deg)`;
  for (let i = 0; i < 3; i++) {
    const s = el("i", "", g);
    s.style.top = `${(i - 1) * 12}px`;
    play(s, [{ transform: "translateX(-34px) scaleX(.3)", opacity: 0 }, { transform: "translateX(0) scaleX(1)", opacity: 1, offset: 0.4 }, { transform: "translateX(46px) scaleX(.3)", opacity: 0 }], {
      duration: 560,
      delay: 90 * Math.abs(i - 1) + 60 * i,
      iterations: 2,
      easing: EASE.inOut,
    });
  }
  setTimeout(() => g.remove(), 1500);
}

/** A thin line of light drawn from a to b (drag path), in a cursor's colour when given; returns the element for the caller to fade out */
export function strand(a: Pt, b: Pt, ms: number, color?: string) {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const s = at("strand", a);
  if (color) s.style.background = `linear-gradient(90deg, color-mix(in srgb, ${color} 30%, transparent), ${color})`;
  s.style.width = `${Math.hypot(dx, dy)}px`;
  s.style.rotate = `${Math.atan2(dy, dx)}rad`;
  play(s, [{ transform: "scaleX(0)" }, { transform: "scaleX(1)" }], { duration: ms, easing: EASE.inOut });
  return s;
}

/** A band of light crosses a panel once (the panel clips it) */
export function sheen(host: HTMLElement, delay = 140) {
  const s = el("i", "sheen", host);
  void playOut(s, [{ transform: "translateX(-130%) skewX(-18deg)" }, { transform: "translateX(330%) skewX(-18deg)" }], { duration: 900, delay, easing: EASE.inOut });
}

// ---- Outline ----

const RADIUS = 10;

function roundRect(w: number, h: number, r: number) {
  return `path("M ${r} 0 H ${w - r} A ${r} ${r} 0 0 1 ${w} ${r} V ${h - r} A ${r} ${r} 0 0 1 ${w - r} ${h} H ${r} A ${r} ${r} 0 0 1 0 ${h - r} V ${r} A ${r} ${r} 0 0 1 ${r} 0 Z")`;
}

/**
 * A gradient outline around a rectangle (window, zoom area). Position and size are written only on
 * creation and when it settles; all movement and scaling in between is transform.
 */
export class Outline {
  readonly el: HTMLDivElement;
  r: R;

  /** pad: how far outside r the ring box starts; the ring is 2.5 wide, so less than that overlaps r's edge */
  constructor(r: R, cls = "", pad = 3) {
    this.r = snapRect(grow(r, pad));
    this.el = el("div", `ol ${cls}`, layer);
    setRect(this.el, this.r);
    el("i", "ol-glow", this.el);
    el("i", "ol-ring", this.el);
  }

  private popScale() {
    return 1 + 14 / Math.max(this.r.w, this.r.h, 60);
  }

  popIn(delay = 0) {
    this.el.style.transformOrigin = "50% 50%";
    play(this.el, [{ opacity: 0, transform: `scale(${this.popScale()})` }, { opacity: 1, transform: "scale(1)" }], { spring: SPRING.pop, delay });
    return this;
  }

  /** Two sparks each run half the perimeter, as if tracing the outline */
  trace(ms = 720) {
    const path = roundRect(this.r.w, this.r.h, RADIUS);
    for (const [from, to] of [
      ["0%", "50%"],
      ["50%", "100%"],
    ]) {
      const c = el("i", "ol-comet", this.el);
      c.style.offsetPath = path;
      void playOut(c, [{ offsetDistance: from, opacity: 0 }, { opacity: 1, offset: 0.15 }, { opacity: 1, offset: 0.8 }, { offsetDistance: to, opacity: 0 }], { duration: ms, easing: EASE.inOut });
    }
    return this;
  }

  /** Glow flashes (settled / locked on) */
  flash() {
    const g = this.el.querySelector(".ol-glow")!;
    play(g, [{ opacity: 1, transform: "scale(1.012)" }, { opacity: 0.6, transform: "scale(1)" }], { duration: 700, fill: "none" });
    return this;
  }

  /** Move and morph the whole outline to a new rectangle */
  async moveTo(r: R, ms = 480) {
    const to = snapRect(grow(r, 3));
    const a = this.r;
    this.el.style.transformOrigin = "0 0";
    const anim = play(this.el, [{ transform: "translate(0,0) scale(1,1)" }, { transform: `translate(${to.x - a.x}px,${to.y - a.y}px) scale(${to.w / a.w},${to.h / a.h})` }], {
      duration: ms,
      easing: EASE.inOut,
    });
    await done(anim);
    this.r = to;
    setRect(this.el, to);
    anim.cancel();
  }

  shake() {
    this.el.classList.add("bad");
    play(this.el, [{ translate: "0" }, { translate: "-8px" }, { translate: "7px" }, { translate: "-3px" }, { translate: "0" }], { duration: 360, easing: EASE.inOut, fill: "none" });
    return this;
  }

  /** fade: fade out; burst: scatter outward; sink: collapse inward */
  out(how: "fade" | "burst" | "sink" = "fade", ms = 380, delay = 0) {
    this.el.style.transformOrigin = "50% 50%";
    const k = this.popScale() - 1;
    const end = how === "burst" ? `scale(${1 + k * 2.2})` : how === "sink" ? `scale(${1 - k * 1.6})` : "scale(1)";
    // fill "forwards", not the default "both": a backwards fill would hold opacity 1 during `delay`
    // and, being the newer animation, override a still-pending popIn, so everything showed at once.
    return playOut(this.el, [{ opacity: 1, transform: "scale(1)" }, { opacity: 0, transform: end }], { duration: ms, delay, fill: "forwards", easing: how === "fade" ? EASE.inOut : EASE.out });
  }
}
