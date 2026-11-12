// 坐标换算：后台给的都是屏幕物理像素，界面用的是相对悬浮层左上角的 CSS 像素。

import { invoke } from "@tauri-apps/api/core";

export interface Pt {
  x: number;
  y: number;
}
export interface R {
  x: number;
  y: number;
  w: number;
  h: number;
}
export interface Mon {
  bounds: R;
  work: R;
  primary: boolean;
}

interface RawRect {
  x: number;
  y: number;
  w: number;
  h: number;
}
export interface OverlayInfo {
  originX: number;
  originY: number;
  scale: number;
  monitors: { bounds: RawRect; work: RawRect; primary: boolean }[];
}

export const isTauri = "__TAURI_INTERNALS__" in window;

let originX = 0;
let originY = 0;
let scale = 1;
export let monitors: Mon[] = [];

export function initGeo(info: OverlayInfo | null) {
  if (!info || info.monitors.length === 0) {
    const w = window.innerWidth;
    const h = window.innerHeight;
    monitors = [{ bounds: { x: 0, y: 0, w, h }, work: { x: 0, y: 0, w, h: h - 48 }, primary: true }];
    return;
  }
  originX = info.originX;
  originY = info.originY;
  // 以网页自己实际的缩放为准：后台读到的值在窗口刚跨屏移动后可能还是旧的
  scale = window.devicePixelRatio || info.scale || 1;
  const conv = (r: RawRect): R => ({ x: (r.x - originX) / scale, y: (r.y - originY) / scale, w: r.w / scale, h: r.h / scale });
  monitors = info.monitors.map((m) => ({ bounds: conv(m.bounds), work: conv(m.work), primary: m.primary }));
  if (!monitors.some((m) => m.primary)) monitors[0].primary = true;
}

/** 屏幕物理像素 → 界面坐标。物理像素是整数，除以缩放后正好落在设备像素上，线条不会发虚 */
export function pt(p: unknown): Pt | null {
  if (!Array.isArray(p) || p.length < 2) return null;
  const x = Number(p[0]);
  const y = Number(p[1]);
  if (!Number.isFinite(x) || !Number.isFinite(y)) return null;
  return { x: (Math.round(x) - originX) / scale, y: (Math.round(y) - originY) / scale };
}

export function rect(r: unknown): R | null {
  if (!Array.isArray(r) || r.length < 4) return null;
  const [x, y, w, h] = r.slice(0, 4).map((v) => Math.round(Number(v)));
  if (![x, y, w, h].every(Number.isFinite) || w <= 0 || h <= 0) return null;
  return { x: (x - originX) / scale, y: (y - originY) / scale, w: w / scale, h: h / scale };
}

/** 把界面坐标对齐到设备像素 */
export const snap = (v: number) => Math.round(v * scale) / scale;
export const snapRect = (r: R): R => {
  const x = snap(r.x);
  const y = snap(r.y);
  return { x, y, w: snap(r.x + r.w) - x, h: snap(r.y + r.h) - y };
};
/** 向外扩一圈（负数向内缩） */
export const grow = (r: R, d: number): R => ({ x: r.x - d, y: r.y - d, w: r.w + d * 2, h: r.h + d * 2 });

export function primary(): Mon {
  return monitors.find((m) => m.primary) ?? monitors[0];
}

export function monitorAt(p: Pt): Mon {
  return (
    monitors.find((m) => p.x >= m.bounds.x && p.y >= m.bounds.y && p.x < m.bounds.x + m.bounds.w && p.y < m.bounds.y + m.bounds.h) ??
    primary()
  );
}

export const center = (r: R): Pt => ({ x: r.x + r.w / 2, y: r.y + r.h / 2 });

/** 小精灵平时待的位置：主屏右下角，任务栏上面 */
export function dock(): Pt {
  const w = primary().work;
  return { x: snap(w.x + w.w - 84), y: snap(w.y + w.h - 84) };
}

// ---- 可交互区域：只有这些元素底下的鼠标会被悬浮层接住 ----

const hitEls = new Set<HTMLElement>();
let lastSent = "";

export function hit(el: HTMLElement, on: boolean) {
  if (on) hitEls.add(el);
  else hitEls.delete(el);
}

export function syncHits() {
  const rects: RawRect[] = [];
  for (const el of hitEls) {
    if (!el.isConnected || el.hidden) continue;
    const b = el.getBoundingClientRect();
    if (b.width < 1 || b.height < 1) continue;
    const pad = 6;
    rects.push({ x: Math.round(b.left - pad), y: Math.round(b.top - pad), w: Math.round(b.width + pad * 2), h: Math.round(b.height + pad * 2) });
  }
  const s = JSON.stringify(rects);
  if (s === lastSent) return;
  lastSent = s;
  if (isTauri) invoke("set_hit_regions", { rects, scale }).catch(() => {});
}

// ---- 小精灵的"家"：平时在角落，弹出确认框时在屏幕中间 ----

let homeOverride: Pt | null = null;
export const home = (): Pt => homeOverride ?? dock();
export function setHome(p: Pt | null) {
  homeOverride = p;
}
