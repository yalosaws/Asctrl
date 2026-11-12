// 受控边框：AI 接管时由小精灵推着从屏幕中间扩大到整屏，结束时收回小精灵身体里。

import { center, home, monitors, type Pt, type R } from "./geo";
import { orb } from "./orb";
import { burst, ring } from "./particles";
import { easeInOutQuart, easeInOutCubic, el, lerp, sleep, tween } from "./ease";

const stage = document.getElementById("frames") as HTMLDivElement;

interface Frame {
  root: HTMLDivElement;
  glow: HTMLDivElement;
  comets: HTMLDivElement[];
  mon: R;
}

let frames: Frame[] = [];
let shown = false;
let busy: Promise<void> = Promise.resolve();

const RADIUS = 16;

function roundRectPath(w: number, h: number, r: number) {
  return `path("M ${r} 0 H ${w - r} A ${r} ${r} 0 0 1 ${w} ${r} V ${h - r} A ${r} ${r} 0 0 1 ${w - r} ${h} H ${r} A ${r} ${r} 0 0 1 0 ${h - r} V ${r} A ${r} ${r} 0 0 1 ${r} 0 Z")`;
}

function setBox(f: Frame, r: R) {
  const s = f.root.style;
  s.left = `${r.x}px`;
  s.top = `${r.y}px`;
  s.width = `${Math.max(2, r.w)}px`;
  s.height = `${Math.max(2, r.h)}px`;
}

function make(mon: R): Frame {
  const root = el("div", "frame", stage);
  const glow = el("div", "frame-glow", root);
  return { root, glow, comets: [], mon };
}

function addComets(f: Frame) {
  const path = roundRectPath(f.mon.w, f.mon.h, RADIUS);
  for (let i = 0; i < 2; i++) {
    const c = el("div", "comet", f.root);
    c.style.offsetPath = path;
    c.animate([{ offsetDistance: "0%" }, { offsetDistance: "100%" }], {
      duration: 9000,
      iterations: Infinity,
      delay: -i * 4500,
    });
    c.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 900, fill: "forwards" });
    f.comets.push(c);
  }
}

const insetOf = (m: R): R => ({ x: m.x + 3, y: m.y + 3, w: m.w - 6, h: m.h - 6 });

/** AI 开始控制：小精灵飞到主屏中间，撑开一个小框，推着它的右下角扩到全屏 */
export function expand(): Promise<void> {
  busy = busy.then(async () => {
    if (shown) return;
    shown = true;
    clear();
    const mons = monitors.length ? monitors : [];
    const prim = mons.find((m) => m.primary) ?? mons[0];
    const primIdx = mons.indexOf(prim);
    const c = center(prim.bounds);

    orb.setMood("work");
    orb.goTo(c, "normal");
    await sleep(750);
    orb.bump(1.2);
    ring(orb.pos, { r1: 90, hue: 212, ms: 700, width: 4 });
    burst(orb.pos, { n: 26, hue: 212, speed: 260 });
    await sleep(260);

    frames = mons.map((m) => {
      const f = make(insetOf(m.bounds));
      f.root.classList.add("growing");
      return f;
    });

    const dur = 2000;
    let last = performance.now();
    await tween(dur, (t) => {
      const now = performance.now();
      const dt = (now - last) / 1000;
      last = now;
      const e = easeInOutQuart(t);
      frames.forEach((f, i) => {
        const m = f.mon;
        const mc = center(m);
        const sw = 70;
        const sh = 46;
        const w = lerp(sw, m.w, e);
        const h = lerp(sh, m.h, e);
        const r: R = { x: mc.x - w / 2, y: mc.y - h / 2, w, h };
        setBox(f, r);
        f.root.style.opacity = String(Math.min(1, t * 6));
        if (i === primIdx) {
          // 小精灵抵在右下角往外推
          const corner: Pt = { x: r.x + r.w - 6, y: r.y + r.h - 6 };
          orb.pin(corner, dt);
          if (Math.random() < 0.6) burst(corner, { n: 2, hue: 212, speed: 160, dir: Math.PI * 1.25, spread: 0.8, life: 0.6 });
        }
      });
    });

    for (const f of frames) {
      setBox(f, f.mon);
      f.root.classList.remove("growing");
      f.root.classList.add("settle");
      addComets(f);
    }
    const corner = orb.pos;
    ring(corner, { r1: 120, hue: 205, ms: 900, width: 5 });
    burst(corner, { n: 40, hue: 205, speed: 420 });
    orb.bump(1.5);
    orb.unpin();
    orb.goTo(home(), "slow");
    await sleep(600);
  });
  return busy;
}

/** 结束控制：边框收缩回小精灵 */
export function collapse(): Promise<void> {
  busy = busy.then(async () => {
    if (!shown) return;
    shown = false;
    const target = { ...orb.pos };
    const starts = frames.map((f) => ({ ...f.mon }));
    for (const f of frames) f.comets.forEach((c) => c.remove());
    await tween(900, (t) => {
      const e = easeInOutCubic(t);
      frames.forEach((f, i) => {
        const s = starts[i];
        const w = lerp(s.w, 20, e);
        const h = lerp(s.h, 20, e);
        const cx = lerp(s.x + s.w / 2, target.x, e);
        const cy = lerp(s.y + s.h / 2, target.y, e);
        setBox(f, { x: cx - w / 2, y: cy - h / 2, w, h });
        f.root.style.opacity = String(1 - e * 0.6);
      });
    });
    clear();
    orb.bump(1.3);
    ring(target, { r1: 70, hue: 212, ms: 600 });
    burst(target, { n: 24, hue: 212, speed: 220 });
  });
  return busy;
}

/** 有工具在执行时边框亮一下 */
export function pulse(hue?: number) {
  for (const f of frames) {
    if (hue !== undefined) f.glow.style.setProperty("--pulse-hue", String(hue));
    f.glow.animate([{ opacity: 0 }, { opacity: 1, offset: 0.25 }, { opacity: 0 }], { duration: 1100, easing: "ease-out" });
  }
}

export const isShown = () => shown;

function clear() {
  stage.replaceChildren();
  frames = [];
}
