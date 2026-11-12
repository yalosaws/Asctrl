// 全屏特效画布：粒子、光束、波纹。没有东西要画时整帧跳过，不占 GPU。

import type { Pt } from "./geo";

interface Particle {
  x: number;
  y: number;
  vx: number;
  vy: number;
  life: number;
  max: number;
  size: number;
  hue: number;
  drag: number;
  gravity: number;
  /** 朝某个点飞（被小精灵吸进去） */
  home?: () => Pt;
}

interface Beam {
  from: () => Pt;
  to: () => Pt;
  life: number;
  max: number;
  hue: number;
  width: number;
  bend: number;
}

interface Ring {
  x: number;
  y: number;
  r0: number;
  r1: number;
  life: number;
  max: number;
  hue: number;
  width: number;
  delay: number;
}

const canvas = document.getElementById("fx") as HTMLCanvasElement;
const ctx = canvas.getContext("2d")!;
let dpr = 1;
let W = 0;
let H = 0;

const parts: Particle[] = [];
const beams: Beam[] = [];
const rings: Ring[] = [];
let wasDirty = true;

export function resizeFx() {
  dpr = Math.min(window.devicePixelRatio || 1, 1.5);
  W = window.innerWidth;
  H = window.innerHeight;
  canvas.width = Math.round(W * dpr);
  canvas.height = Math.round(H * dpr);
  canvas.style.width = `${W}px`;
  canvas.style.height = `${H}px`;
  wasDirty = true;
}

// 预渲染的发光点，按色相缓存，drawImage 比每个粒子建渐变快得多
const sprites = new Map<number, HTMLCanvasElement>();
function sprite(hue: number) {
  const key = Math.round(hue / 6) * 6;
  let s = sprites.get(key);
  if (s) return s;
  s = document.createElement("canvas");
  s.width = s.height = 64;
  const c = s.getContext("2d")!;
  const g = c.createRadialGradient(32, 32, 0, 32, 32, 32);
  g.addColorStop(0, `hsla(${key},100%,97%,1)`);
  g.addColorStop(0.18, `hsla(${key},100%,75%,0.9)`);
  g.addColorStop(0.45, `hsla(${key},100%,60%,0.28)`);
  g.addColorStop(1, `hsla(${key},100%,50%,0)`);
  c.fillStyle = g;
  c.fillRect(0, 0, 64, 64);
  sprites.set(key, s);
  return s;
}

const rnd = (a: number, b: number) => a + Math.random() * (b - a);

export function burst(p: Pt, opts: { n?: number; hue?: number; speed?: number; size?: number; life?: number; spread?: number; dir?: number } = {}) {
  const n = opts.n ?? 18;
  for (let i = 0; i < n; i++) {
    const a = opts.dir !== undefined ? opts.dir + rnd(-(opts.spread ?? 0.6), opts.spread ?? 0.6) : rnd(0, Math.PI * 2);
    const sp = rnd(0.35, 1) * (opts.speed ?? 320);
    parts.push({
      x: p.x,
      y: p.y,
      vx: Math.cos(a) * sp,
      vy: Math.sin(a) * sp,
      life: 0,
      max: rnd(0.5, 1) * (opts.life ?? 0.9),
      size: rnd(0.6, 1.2) * (opts.size ?? 14),
      hue: (opts.hue ?? 210) + rnd(-12, 12),
      drag: 3.2,
      gravity: 0,
    });
  }
}

export function trail(p: Pt, v: Pt, hue: number) {
  parts.push({
    x: p.x + rnd(-6, 6),
    y: p.y + rnd(-6, 6),
    vx: -v.x * 0.08 + rnd(-20, 20),
    vy: -v.y * 0.08 + rnd(-20, 20),
    life: 0,
    max: rnd(0.35, 0.7),
    size: rnd(8, 16),
    hue: hue + rnd(-10, 10),
    drag: 2,
    gravity: 0,
  });
}

/** 从一块区域里冒出粒子，飞向 target（比如小精灵） */
export function absorb(area: { x: number; y: number; w: number; h: number }, target: () => Pt, opts: { n?: number; hue?: number } = {}) {
  const n = opts.n ?? 30;
  for (let i = 0; i < n; i++) {
    parts.push({
      x: area.x + Math.random() * area.w,
      y: area.y + Math.random() * area.h,
      vx: rnd(-60, 60),
      vy: rnd(-60, 60),
      life: -rnd(0, 0.6),
      max: 1.6,
      size: rnd(6, 13),
      hue: (opts.hue ?? 205) + rnd(-15, 15),
      drag: 1.5,
      gravity: 0,
      home: target,
    });
  }
}

/** 从 from 发射粒子到 target */
export function stream(from: () => Pt, target: () => Pt, opts: { n?: number; hue?: number } = {}) {
  const n = opts.n ?? 24;
  for (let i = 0; i < n; i++) {
    const p = from();
    const a = rnd(0, Math.PI * 2);
    parts.push({
      x: p.x,
      y: p.y,
      vx: Math.cos(a) * 140,
      vy: Math.sin(a) * 140,
      life: -i * 0.025,
      max: 1.5,
      size: rnd(6, 12),
      hue: (opts.hue ?? 205) + rnd(-10, 10),
      drag: 1.2,
      gravity: 0,
      home: target,
    });
  }
}

export function beam(from: () => Pt, to: () => Pt, opts: { ms?: number; hue?: number; width?: number; bend?: number } = {}) {
  beams.push({ from, to, life: 0, max: (opts.ms ?? 600) / 1000, hue: opts.hue ?? 205, width: opts.width ?? 3, bend: opts.bend ?? rnd(-0.25, 0.25) });
}

export function ring(p: Pt, opts: { r0?: number; r1?: number; ms?: number; hue?: number; width?: number; delay?: number } = {}) {
  rings.push({
    x: p.x,
    y: p.y,
    r0: opts.r0 ?? 4,
    r1: opts.r1 ?? 46,
    life: -(opts.delay ?? 0) / 1000,
    max: (opts.ms ?? 650) / 1000,
    hue: opts.hue ?? 205,
    width: opts.width ?? 3,
    delay: opts.delay ?? 0,
  });
}

/** 沿矩形边框撒粒子（窗口关闭时碎掉） */
export function shatterRect(r: { x: number; y: number; w: number; h: number }, hue: number) {
  const per = 2 * (r.w + r.h);
  const n = Math.min(140, Math.max(40, per / 18));
  for (let i = 0; i < n; i++) {
    let d = Math.random() * per;
    let x: number;
    let y: number;
    if (d < r.w) [x, y] = [r.x + d, r.y];
    else if ((d -= r.w) < r.h) [x, y] = [r.x + r.w, r.y + d];
    else if ((d -= r.h) < r.w) [x, y] = [r.x + r.w - d, r.y + r.h];
    else [x, y] = [r.x, r.y + r.h - (d - r.w)];
    const cx = r.x + r.w / 2;
    const cy = r.y + r.h / 2;
    const a = Math.atan2(y - cy, x - cx);
    const sp = rnd(60, 260);
    parts.push({ x, y, vx: Math.cos(a) * sp, vy: Math.sin(a) * sp, life: 0, max: rnd(0.6, 1.2), size: rnd(8, 16), hue: hue + rnd(-10, 10), drag: 2.4, gravity: 180 });
  }
}

export function stepFx(dt: number) {
  const dirty = parts.length > 0 || beams.length > 0 || rings.length > 0;
  if (!dirty && !wasDirty) return;
  wasDirty = dirty;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, W, H);
  if (!dirty) return;
  ctx.globalCompositeOperation = "lighter";

  for (let i = rings.length - 1; i >= 0; i--) {
    const r = rings[i];
    r.life += dt;
    if (r.life >= r.max) {
      rings.splice(i, 1);
      continue;
    }
    if (r.life < 0) continue;
    const t = r.life / r.max;
    const e = 1 - Math.pow(1 - t, 3);
    const rad = r.r0 + (r.r1 - r.r0) * e;
    const a = (1 - t) * (1 - t);
    ctx.lineWidth = r.width * (1 - t * 0.6);
    ctx.strokeStyle = `hsla(${r.hue},100%,70%,${a * 0.9})`;
    ctx.beginPath();
    ctx.arc(r.x, r.y, rad, 0, Math.PI * 2);
    ctx.stroke();
    ctx.lineWidth = r.width * 4;
    ctx.strokeStyle = `hsla(${r.hue},100%,60%,${a * 0.18})`;
    ctx.stroke();
  }

  for (let i = beams.length - 1; i >= 0; i--) {
    const b = beams[i];
    b.life += dt;
    if (b.life >= b.max) {
      beams.splice(i, 1);
      continue;
    }
    const t = b.life / b.max;
    const a = Math.sin(t * Math.PI);
    const p = b.from();
    const q = b.to();
    const mx = (p.x + q.x) / 2 - (q.y - p.y) * b.bend;
    const my = (p.y + q.y) / 2 + (q.x - p.x) * b.bend;
    ctx.lineCap = "round";
    for (const [w, al, l] of [
      [b.width * 5, 0.12, 60],
      [b.width * 2, 0.35, 68],
      [b.width * 0.7, 0.95, 92],
    ] as const) {
      ctx.lineWidth = w;
      ctx.strokeStyle = `hsla(${b.hue},100%,${l}%,${al * a})`;
      ctx.beginPath();
      ctx.moveTo(p.x, p.y);
      ctx.quadraticCurveTo(mx, my, q.x, q.y);
      ctx.stroke();
    }
    // 光束上跑的亮点
    const k = (b.life * 2.2) % 1;
    const bx = (1 - k) * (1 - k) * p.x + 2 * (1 - k) * k * mx + k * k * q.x;
    const by = (1 - k) * (1 - k) * p.y + 2 * (1 - k) * k * my + k * k * q.y;
    const s = sprite(b.hue);
    ctx.globalAlpha = a;
    ctx.drawImage(s, bx - 14, by - 14, 28, 28);
    ctx.globalAlpha = 1;
  }

  for (let i = parts.length - 1; i >= 0; i--) {
    const p = parts[i];
    p.life += dt;
    if (p.life >= p.max) {
      parts.splice(i, 1);
      continue;
    }
    if (p.life < 0) continue;
    if (p.home) {
      const h = p.home();
      const dx = h.x - p.x;
      const dy = h.y - p.y;
      const d = Math.hypot(dx, dy);
      if (d < 18) {
        parts.splice(i, 1);
        continue;
      }
      const acc = 2600;
      p.vx += (dx / d) * acc * dt;
      p.vy += (dy / d) * acc * dt;
      // 限速 + 往目标方向收拢，轨迹是平滑的弧线
      const sp = Math.hypot(p.vx, p.vy);
      const max = 900;
      if (sp > max) {
        p.vx *= max / sp;
        p.vy *= max / sp;
      }
      p.vx *= 1 - Math.min(1, p.drag * dt);
      p.vy *= 1 - Math.min(1, p.drag * dt);
    } else {
      p.vx *= 1 - Math.min(1, p.drag * dt);
      p.vy *= 1 - Math.min(1, p.drag * dt);
      p.vy += p.gravity * dt;
    }
    p.x += p.vx * dt;
    p.y += p.vy * dt;
    const t = p.life / p.max;
    const a = p.home ? Math.min(1, p.life * 4) : 1 - t;
    const s = p.size * (p.home ? 1 : 1 - t * 0.5);
    ctx.globalAlpha = a;
    ctx.drawImage(sprite(p.hue), p.x - s, p.y - s, s * 2, s * 2);
  }
  ctx.globalAlpha = 1;
  ctx.globalCompositeOperation = "source-over";
}
