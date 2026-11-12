// 光效图元：绽放、涟漪、光迹、扫光、描边框。全部是普通 DOM 元素，只动 transform 和 opacity，
// 播完自己移除。没有全屏画布。

import { center, grow, snapRect, type Pt, type R } from "./geo";
import { clamp, done, EASE, el, fadeOut, play, playOut, SPRING } from "./motion";

const layer = document.getElementById("marks") as HTMLDivElement;

export type Tone = "aurora" | "warm" | "mint" | "red";

function at(cls: string, p: Pt, tone: Tone = "aurora") {
  const m = el("div", `${cls} t-${tone}`, layer);
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

/** 一团柔光从中心绽开 */
export function bloom(p: Pt, o: { size?: number; ms?: number; tone?: Tone; delay?: number } = {}) {
  const b = at("bloom", p, o.tone);
  b.style.setProperty("--s", `${o.size ?? 150}px`);
  void playOut(b, [{ transform: "scale(.12)", opacity: 1 }, { transform: "scale(1)", opacity: 0 }], { duration: o.ms ?? 680, delay: o.delay ?? 0 });
}

/** 一圈细线向外扩散（from > to 时向内收） */
export function ripple(p: Pt, o: { size?: number; ms?: number; from?: number; to?: number; delay?: number; tone?: Tone } = {}) {
  const r = at("ripple", p, o.tone);
  r.style.setProperty("--s", `${o.size ?? 40}px`);
  const from = o.from ?? 0.35;
  const to = o.to ?? 1.9;
  void playOut(r, [{ transform: `scale(${from})`, opacity: from > to ? 0 : 1 }, { transform: `scale(${to})`, opacity: from > to ? 1 : 0 }], {
    duration: o.ms ?? 600,
    delay: o.delay ?? 0,
  });
}

/** 一道光从 a 射到 b，到达时 resolve */
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
    // 尾巴收进落点
    void playOut(d, [{ opacity: 1 }, { opacity: 0 }], { duration: 160, easing: "linear" });
  });
}

/** 一条斜向的渐变光带扫过区域；返回停止函数 */
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

/** 一张"底片"从区域缩进目标点（截图被小精灵收走） */
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

/** 按住不放的标记 */
export function hold(p: Pt, tone: Tone = "aurora") {
  const h = at("hold", p, tone);
  play(h, [{ transform: "scale(2)", opacity: 0 }, { transform: "scale(1)", opacity: 1 }], { spring: SPRING.pop });
  return h;
}

/** 朝某个方向流过去的几道短光（滚动） */
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

/** 从 a 画到 b 的一条细光线（拖动的轨迹）；返回元素，由调用方淡出 */
export function strand(a: Pt, b: Pt, ms: number) {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const s = at("strand", a);
  s.style.width = `${Math.hypot(dx, dy)}px`;
  s.style.rotate = `${Math.atan2(dy, dx)}rad`;
  play(s, [{ transform: "scaleX(0)" }, { transform: "scaleX(1)" }], { duration: ms, easing: EASE.inOut });
  return s;
}

// ---- 描边框 ----

const RADIUS = 10;

function roundRect(w: number, h: number, r: number) {
  return `path("M ${r} 0 H ${w - r} A ${r} ${r} 0 0 1 ${w} ${r} V ${h - r} A ${r} ${r} 0 0 1 ${w - r} ${h} H ${r} A ${r} ${r} 0 0 1 0 ${h - r} V ${r} A ${r} ${r} 0 0 1 ${r} 0 Z")`;
}

/**
 * 沿着一个矩形的渐变描边（窗口、放大区域）。位置和尺寸只在创建和落定时写一次，
 * 中间的移动、缩放全是 transform。
 */
export class Outline {
  readonly el: HTMLDivElement;
  r: R;

  constructor(r: R, cls = "") {
    this.r = snapRect(grow(r, 3));
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

  /** 两颗光点各跑半圈，像是把框描出来 */
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

  /** 光晕亮一下（落定、锁定） */
  flash() {
    const g = this.el.querySelector(".ol-glow")!;
    play(g, [{ opacity: 1, transform: "scale(1.012)" }, { opacity: 0.6, transform: "scale(1)" }], { duration: 700, fill: "none" });
    return this;
  }

  /** 整个框移动并变形到新的矩形 */
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

  /** fade 淡出；burst 向外散开；sink 向内收拢 */
  out(how: "fade" | "burst" | "sink" = "fade", ms = 380, delay = 0) {
    this.el.style.transformOrigin = "50% 50%";
    const k = this.popScale() - 1;
    const end = how === "burst" ? `scale(${1 + k * 2.2})` : how === "sink" ? `scale(${1 - k * 1.6})` : "scale(1)";
    return playOut(this.el, [{ opacity: 1, transform: "scale(1)" }, { opacity: 0, transform: end }], { duration: ms, delay, easing: how === "fade" ? EASE.inOut : EASE.out });
  }
}
