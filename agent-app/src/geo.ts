// Coordinate conversion: the backend speaks physical screen pixels; the UI uses CSS pixels relative to the overlay's top-left.

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
  // Trust the page's own scale: the backend's value may be stale right after the window moves across screens
  scale = window.devicePixelRatio || info.scale || 1;
  const conv = (r: RawRect): R => ({ x: (r.x - originX) / scale, y: (r.y - originY) / scale, w: r.w / scale, h: r.h / scale });
  monitors = info.monitors.map((m) => ({ bounds: conv(m.bounds), work: conv(m.work), primary: m.primary }));
  if (!monitors.some((m) => m.primary)) monitors[0].primary = true;
}

/** Physical screen pixels -> UI coordinates. Physical pixels are integers, so after dividing by the scale they land exactly on device pixels and lines stay crisp */
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

/** Snap a UI coordinate to device pixels */
export const snap = (v: number) => Math.round(v * scale) / scale;
export const snapRect = (r: R): R => {
  const x = snap(r.x);
  const y = snap(r.y);
  return { x, y, w: snap(r.x + r.w) - x, h: snap(r.y + r.h) - y };
};
/** Grow outward (negative shrinks inward) */
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

/** The orb's resting spot: bottom-right of the primary screen, above the taskbar */
export function dock(): Pt {
  const w = primary().work;
  return { x: snap(w.x + w.w - 84), y: snap(w.y + w.h - 84) };
}

// ---- Interactive regions: the overlay only catches the mouse over these elements ----

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

// ---- The orb's "home": where it rests between actions ----

export const home = (): Pt => dock();
