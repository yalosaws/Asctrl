// Virtual cursors: the orb spins up and flings a drop of one of its colours off its rim toward each spot it
// acts on. The drop becomes a pointer of that colour, so several actions can be told apart and seen at once
// while the user's own mouse pointer stays where it is. When the result comes back the pointer presses,
// then turns back into a drop and falls into the orb.

import type { Pt } from "./geo";
import { orb } from "./orb";
import { bloom, hold, ripple, strand } from "./light";
import { clamp, done, easeInOutCubic, el, fadeOut, onFrame, play, sleep, SPRING, tween } from "./motion";

const layer = document.getElementById("marks") as HTMLDivElement;

/** The orb's colours and where each sits on its wheel (degrees clockwise from the top, see --loop) */
const PALETTE = [
  { color: "#5b8cff", at: 0 },
  { color: "#b36bff", at: 72 },
  { color: "#ff7ab6", at: 144 },
  { color: "#ffb86b", at: 216 },
  { color: "#5be0c8", at: 288 },
];
const RIM = 16;
/** Release this many degrees before the colour's tangent points at the target, so the throw curves in */
const LEAD = 28;

/** Mix a #rrggbb colour toward another by t */
function mix(a: string, b: string, t: number) {
  const n = (h: string, i: number) => parseInt(h.slice(1 + i * 2, 3 + i * 2), 16);
  return "#" + [0, 1, 2].map((i) => Math.round(n(a, i) + (n(b, i) - n(a, i)) * t).toString(16).padStart(2, "0")).join("");
}

/** Light, base and deep shade of each colour, for the pointer's body and its tag */
const SHADES = PALETTE.map(({ color }) => ({ light: mix(color, "#ffffff", 0.5), base: color, deep: mix(color, "#000000", 0.22) }));

// Shared SVG paint: a body gradient per colour, and one gloss for all
const defs = document.createElementNS("http://www.w3.org/2000/svg", "svg");
defs.setAttribute("width", "0");
defs.setAttribute("height", "0");
defs.style.position = "absolute";
defs.innerHTML =
  "<defs>" +
  SHADES.map(
    (s, k) =>
      `<linearGradient id="vc-g${k}" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="${s.light}"/><stop offset=".5" stop-color="${s.base}"/><stop offset="1" stop-color="${s.deep}"/></linearGradient>`,
  ).join("") +
  '<linearGradient id="vc-gloss" x1="0" y1="0" x2=".75" y2=".75"><stop offset="0" stop-color="#fff" stop-opacity=".85"/><stop offset=".45" stop-color="#fff" stop-opacity=".18"/><stop offset=".62" stop-color="#fff" stop-opacity="0"/></linearGradient>' +
  "</defs>";
document.body.appendChild(defs);

/** A rounded arrowhead with its tip at the origin: dark hairline, white rim, gradient body, gloss on top */
const HEAD = "M0 0L19 7.2L9.4 9.4L7.2 19Z";
const arrow = (k: number) =>
  `<svg viewBox="-6 -6 34 34" width="34" height="34">` +
  `<path d="${HEAD}" class="vc-line"/><path d="${HEAD}" class="vc-rim"/>` +
  `<path d="${HEAD}" class="vc-fill" fill="url(#vc-g${k})" stroke="url(#vc-g${k})"/>` +
  `<path d="${HEAD}" class="vc-gloss" fill="url(#vc-gloss)"/></svg>`;

let launched = 0;

const wrap = (a: number) => ((((a + 180) % 360) + 360) % 360) - 180;
const rad = (d: number) => (d * Math.PI) / 180;
const lerp = (a: number, b: number, t: number) => a + (b - a) * clamp(t);
const easeOut = (t: number) => 1 - Math.pow(1 - t, 3);

function bez(p0: Pt, p1: Pt, p2: Pt, p3: Pt, t: number): Pt {
  const u = 1 - t;
  const a = u * u * u, b = 3 * u * u * t, c = 3 * u * t * t, d = t * t * t;
  return { x: a * p0.x + b * p1.x + c * p2.x + d * p3.x, y: a * p0.y + b * p1.y + c * p2.y + d * p3.y };
}

/** Pick a colour no cursor still out is using, if there is one */
const out = new Set<number>();
function nextColour(): number {
  for (let i = 0; i < PALETTE.length; i++) {
    const k = (launched + i) % PALETTE.length;
    if (!out.has(k)) return (launched = k + 1), k;
  }
  return launched++ % PALETTE.length;
}

export class Cursor {
  private root: HTMLDivElement;
  private drop: HTMLDivElement;
  private arrow: HTMLDivElement;
  private pos: Pt = { ...orb.pos };
  private aim?: HTMLElement;
  private k: number;
  readonly color: string;

  constructor() {
    this.k = nextColour();
    out.add(this.k);
    this.color = PALETTE[this.k].color;
    this.root = el("div", "vcursor", layer);
    this.root.style.setProperty("--c", this.color);
    this.drop = el("div", "vc-drop", this.root);
    this.arrow = el("div", "vc-body", this.root);
    this.arrow.innerHTML = arrow(this.k);
    this.root.style.opacity = "0";
  }

  private place(p: Pt) {
    this.pos = p;
    this.root.style.transform = `translate(${p.x}px, ${p.y}px)`;
  }

  /** 0 = a round drop, 1 = the pointer; `spin` turns the pointer while it forms */
  private morph(m: number, spin = 0) {
    this.drop.style.transform = `scale(${lerp(1, 0, (m - 0.15) / 0.55).toFixed(3)})`;
    this.drop.style.opacity = lerp(1, 0, (m - 0.3) / 0.5).toFixed(3);
    this.arrow.style.transform = `rotate(${spin.toFixed(1)}deg) scale(${lerp(0.2, 1, (m - 0.25) / 0.55).toFixed(3)})`;
    this.arrow.style.opacity = lerp(0, 1, (m - 0.2) / 0.3).toFixed(3);
  }

  /** Wait until this cursor's colour comes round to the release point for `p`, then throw it there */
  async go(p: Pt) {
    const want = (o: Pt) => (Math.atan2(p.y - o.y, p.x - o.x) * 180) / Math.PI;
    // Screen angle (clockwise from the top) where the colour is now
    const where = () => PALETTE[this.k].at + orb.turn;
    await new Promise<void>((resolve) => {
      let t = 0;
      onFrame((dt) => {
        t += dt;
        // A brief spin-up, then the next time its colour lines up; never more than 0.12 s, so clicks stay fast
        const lined = t > 0.03 && Math.abs(wrap(where() - (want(orb.pos) - LEAD))) < 20;
        if (lined || t > 0.12) {
          resolve();
          return false;
        }
      });
    });

    // Off the rim, along the rim's direction of travel at that point
    const phi = where();
    const c = { ...orb.pos };
    const from = { x: c.x + RIM * Math.sin(rad(phi)), y: c.y - RIM * Math.cos(rad(phi)) };
    const tangent = { x: Math.cos(rad(phi)), y: Math.sin(rad(phi)) };
    const dist = Math.hypot(p.x - from.x, p.y - from.y);
    const reach = Math.min(dist * 0.5, 190);
    const c1 = { x: from.x + tangent.x * reach, y: from.y + tangent.y * reach };
    const c2 = { x: lerp(from.x, p.x, 0.78), y: lerp(from.y, p.y, 0.78) };
    this.place(from);
    this.morph(0);
    this.root.style.opacity = "1";
    bloom(from, { size: 46, ms: 380, color: this.color });
    orb.bump(0.18);

    const ms = clamp(dist * 0.16, 100, 220);
    await tween(ms, (t) => {
      const e = easeOut(t);
      this.place(bez(from, c1, c2, p, e));
      this.morph(e, (1 - e) * -420);
    });
    this.morph(1);

    // A small glow of its colour under the tip marks the spot, beneath the pointer rather than over it
    const aim = el("div", "vc-contact");
    layer.insertBefore(aim, this.root);
    aim.style.setProperty("--c", this.color);
    aim.style.left = `${p.x}px`;
    aim.style.top = `${p.y}px`;
    this.aim = aim;
    play(aim, [{ transform: "scale(2.2)", opacity: 0 }, { transform: "scale(1)", opacity: 1 }], { spring: SPRING.snap });
  }

  /** Press where it stands, pull a line of its colour to `to` over `ms`, then let go there */
  async drag(to: Pt, ms: number) {
    const from = { ...this.pos };
    const aim = this.aim;
    this.aim = undefined;
    if (aim) void fadeOut(aim, 200, { transform: "scale(.5)" });
    const grip = hold(from, "aurora", this.color);
    // Held down for the whole pull
    play(this.arrow, [{ scale: "1" }, { scale: ".8" }], { duration: 120, fill: "forwards" });
    const line = strand(from, to, ms, this.color);
    await tween(ms, (t) => {
      const e = easeInOutCubic(t);
      this.place({ x: lerp(from.x, to.x, e), y: lerp(from.y, to.y, e) });
    });
    play(this.arrow, [{ scale: ".8" }, { scale: "1" }], { duration: 140, fill: "forwards" });
    ripple(to, { size: 34, color: this.color });
    void fadeOut(grip, 360, { transform: "scale(1.8)" });
    void fadeOut(line, 520);
  }

  /** Never ran (a sequence stopped before this step): fade away quietly, no press and no red */
  async cancel() {
    const aim = this.aim;
    this.aim = undefined;
    if (aim) void fadeOut(aim, 300, { transform: "scale(.5)" });
    await fadeOut(this.root, 320, { transform: `translate(${this.pos.x}px, ${this.pos.y}px) scale(.6)` });
    out.delete(this.k);
  }

  /** Press (count times), then fall back into the orb; on failure flash red and fade where it stands */
  async finish(ok: boolean, count = 1) {
    const p = { ...this.pos };
    const aim = this.aim;
    this.aim = undefined;
    if (!ok) {
      if (aim) void fadeOut(aim, 400, { transform: "scale(1.6)" });
      ripple(p, { size: 36, tone: "red" });
      await done(play(this.arrow, [{ translate: "0" }, { translate: "-5px" }, { translate: "5px" }, { translate: "-3px" }, { translate: "0" }], { duration: 360, fill: "none" }));
      await fadeOut(this.root, 380);
      out.delete(this.k);
      return;
    }
    if (aim) void fadeOut(aim, 260, { transform: "scale(.5)" });
    bloom(p, { size: 130, ms: 620, color: this.color });
    for (let i = 0; i < count; i++) {
      ripple(p, { size: 34, ms: 560, to: 1.9 + i * 0.35, color: this.color });
      await done(play(this.arrow, [{ scale: "1" }, { scale: ".72" }, { scale: "1" }], { duration: 170, fill: "none" }));
    }
    await sleep(240);

    // Home: turn back into a drop on the way and fall into the orb, which may have moved meanwhile
    const from = { ...this.pos };
    const dist = Math.hypot(orb.pos.x - from.x, orb.pos.y - from.y);
    await tween(clamp(dist * 0.36, 260, 560), (t) => {
      const e = easeInOutCubic(t);
      const o = orb.pos;
      this.place({ x: lerp(from.x, o.x, e), y: lerp(from.y, o.y, e) });
      this.morph(1 - e);
    });
    orb.bump(0.3);
    this.root.remove();
    out.delete(this.k);
  }
}
