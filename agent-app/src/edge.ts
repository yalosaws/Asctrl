// Edge glow: while the AI is in control, a ring of flowing gradient light runs around every screen.
// The whole ring is one conic-gradient texture rotating behind a ring mask, so the colour is naturally
// continuous along all four edges and corners, with no seams. The texture is drawn once (a small canvas
// stretched by the GPU); after that only the rotation transform animates, run by the compositor at a
// constant speed for as long as control lasts: no speed changes, no pauses.

import { center, home, monitors, type R } from "./geo";
import { orb } from "./orb";
import { bloom, ripple } from "./light";
import { done, EASE, el, play, sleep } from "./motion";

const stage = document.getElementById("frames") as HTMLDivElement;

interface Edge {
  root: HTMLDivElement;
  /** Always-on ring: 3px solid line + soft glow fading inward */
  ring: HTMLDivElement;
  /** Wider, brighter ring that flashes only when an action happens */
  flare: HTMLDivElement;
  spins: Animation[];
}

let edges: Edge[] = [];
let shown = false;
let busy: Promise<void> = Promise.resolve();

const LOOP = ["#5b8cff", "#b36bff", "#ff7ab6", "#ffb86b", "#5be0c8", "#5b8cff"];
/** Time for one full turn (constant speed) */
const TURN_MS = 7000;

let texture: HTMLCanvasElement | null = null;
function gradientTexture() {
  if (texture) return texture;
  texture = document.createElement("canvas");
  texture.width = texture.height = 512;
  const c = texture.getContext("2d")!;
  const g = c.createConicGradient(0, 256, 256);
  LOOP.forEach((color, i) => g.addColorStop(i / (LOOP.length - 1), color));
  c.fillStyle = g;
  c.fillRect(0, 0, 512, 512);
  return texture;
}

function spinner(host: HTMLElement, mon: R): Animation {
  const cv = el("canvas", "edge-spin", host);
  cv.width = cv.height = 512;
  cv.getContext("2d")!.drawImage(gradientTexture(), 0, 0);
  // A square large enough to cover the screen's diagonal, rotating around the screen centre
  const d = Math.ceil(Math.hypot(mon.w, mon.h)) + 4;
  Object.assign(cv.style, { width: `${d}px`, height: `${d}px`, left: `${(mon.w - d) / 2}px`, top: `${(mon.h - d) / 2}px` });
  // Linear, constant speed; all screens and both layers share one start time so the colours always line up
  const a = cv.animate([{ transform: "rotate(0turn)" }, { transform: "rotate(1turn)" }], { duration: TURN_MS, iterations: Infinity, easing: "linear" });
  a.startTime = 0;
  return a;
}

function make(mon: R): Edge {
  const root = el("div", "edge", stage);
  Object.assign(root.style, { left: `${mon.x}px`, top: `${mon.y}px`, width: `${mon.w}px`, height: `${mon.h}px` });
  const ring = el("div", "edge-ring", root);
  const flare = el("div", "edge-flare", root);
  return { root, ring, flare, spins: [spinner(ring, mon), spinner(flare, mon)] };
}

/** AI takes control: the orb flies to the middle of the primary screen and blooms; a ripple spreads out and the ring lights up when it reaches the edges */
export function expand(): Promise<void> {
  busy = busy.then(async () => {
    if (shown) return;
    shown = true;
    clear();
    const prim = monitors.find((m) => m.primary) ?? monitors[0];
    const c = center(prim.bounds);

    orb.setMood("work");
    orb.goTo(c, "normal");
    await sleep(720);
    orb.bump(1.3);
    bloom(orb.pos, { size: 420, ms: 900 });
    ripple(orb.pos, { size: 120, ms: 1100, from: 0.3, to: Math.max(prim.bounds.w, prim.bounds.h) / 110 });
    await sleep(380);

    edges = monitors.map((m) => make(m.bounds));
    const ins: Promise<void>[] = [];
    for (const e of edges) {
      ins.push(done(play(e.ring, [{ opacity: 0, transform: "scale(1.06)" }, { opacity: 1, transform: "scale(1)" }], { duration: 900 })));
      play(e.flare, [{ opacity: 0 }, { opacity: 1, offset: 0.25 }, { opacity: 0 }], { duration: 1700, easing: EASE.soft });
    }
    await Promise.all(ins);
    orb.goTo(home(), "normal");
    await sleep(500);
  });
  return busy;
}

/** Control ends: the ring exits outward and the orb flashes */
export function collapse(): Promise<void> {
  busy = busy.then(async () => {
    if (!shown) return;
    shown = false;
    await Promise.all(edges.map((e) => done(play(e.root, [{ opacity: 1, transform: "scale(1)" }, { opacity: 0, transform: "scale(1.05)" }], { duration: 600, easing: EASE.inOut }))));
    clear();
    orb.bump(1.1);
    bloom(orb.pos, { size: 220, ms: 760 });
    ripple(orb.pos, { size: 60, ms: 700 });
  });
  return busy;
}

/** When a tool starts, the ring brightens and thickens briefly */
export function pulse() {
  for (const e of edges) play(e.flare, [{ opacity: 0.9 }, { opacity: 0 }], { duration: 900, easing: EASE.out });
}

export const isShown = () => shown;

/** Monitor layout changed: rebuild the glow at the new sizes */
export function relayout() {
  if (!shown) return;
  clear();
  edges = monitors.map((m) => make(m.bounds));
  for (const e of edges) e.ring.style.opacity = "1";
}

function clear() {
  stage.replaceChildren();
  edges = [];
}
