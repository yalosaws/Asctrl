// The orb: a ball of flowing gradient light. Its position is driven by spring physics, with a light trail while flying.
// At rest no per-frame script runs: spin, breathing and floating are all compositor CSS animations.

import type { Pt } from "./geo";
import { clamp, EASE, onFrame, play, SPRING } from "./motion";

export type Mood = "idle" | "ready" | "work" | "focus" | "ask" | "happy" | "error" | "offline";

/** What the orb's visible body (body.ts) needs to hear to look and move like the orb */
export type OrbEvent = { kind: "show"; on: boolean } | { kind: "bump"; strength: number } | { kind: "mood"; mood: Mood; was: Mood } | { kind: "state" };

/** Spin speed of the orb in each mood */
const SPIN: Record<Mood, number> = { idle: 0.6, ready: 1, work: 2.8, focus: 1.7, ask: 1.1, happy: 1.6, error: 0.5, offline: 0 };

/** One turn of the colour wheel at speed 1 */
const SPIN_MS = 5200;
/** Spin speed while thinking: the colour wheel winds up as the next step is worked out */
const MUSE = 3.2;

const q = <T extends HTMLElement>(sel: string) => document.querySelector(sel) as T;

class Orb {
  readonly el = q<HTMLDivElement>("#orb");
  private pres = q<HTMLDivElement>(".orb-pres");
  private bumper = q<HTMLDivElement>(".orb-bump");
  private body = q<HTMLDivElement>(".orb-body");
  private halo = q<HTMLDivElement>(".orb-halo");
  private tail = q<HTMLDivElement>(".orb-tail");
  private spin = q<HTMLDivElement>(".orb-core").animate([{ transform: "rotate(0turn)" }, { transform: "rotate(1turn)" }], {
    duration: SPIN_MS,
    iterations: Infinity,
  });
  /** Spin speed right now; eases toward the mood's speed */
  private rate = SPIN.idle;
  private ramping = false;
  private thinking = false;

  pos: Pt = { x: -200, y: -200 };
  vel: Pt = { x: 0, y: 0 };
  private target: Pt = { x: -200, y: -200 };
  private stiffness = 70;
  private damping = 0.78;
  private running = false;
  private shown = false;
  private mood: Mood = "idle";
  private moved: (() => void)[] = [];
  private heard: ((e: OrbEvent) => void)[] = [];

  /** Notified when the orb is shown or hidden, bumps, changes mood, or starts or stops busy / waiting / thinking */
  onEvent(cb: (e: OrbEvent) => void) {
    this.heard.push(cb);
  }
  private tell(e: OrbEvent) {
    for (const cb of this.heard) cb(e);
  }

  constructor() {
    this.setMood("idle");
  }

  get currentMood(): Mood {
    return this.mood;
  }

  setMood(m: Mood) {
    if (m === this.mood && this.el.dataset.mood === m) return;
    const was = this.mood;
    this.mood = m;
    this.el.dataset.mood = m;
    this.ramp();
    this.tell({ kind: "mood", mood: m, was });
    if (m === "error" && was !== "error") {
      play(this.bumper, [{ transform: "translateX(0)" }, { transform: "translateX(-7px)" }, { transform: "translateX(6px)" }, { transform: "translateX(-3px)" }, { transform: "translateX(0)" }], {
        duration: 380,
        easing: EASE.inOut,
        fill: "none",
      });
    }
  }

  show(on = true) {
    if (on === this.shown) return;
    this.shown = on;
    this.tell({ kind: "show", on });
    if (on) play(this.pres, [{ transform: "scale(0)", opacity: 0 }, { transform: "scale(1)", opacity: 1 }], { spring: SPRING.pop });
    else play(this.pres, [{ transform: "scale(1)", opacity: 1 }, { transform: "scale(0)", opacity: 0 }], { duration: 300, easing: EASE.in });
  }

  /** Notified on position changes (for panels that follow the orb) */
  onMove(cb: () => void) {
    this.moved.push(cb);
  }

  /** Teleport (no flight) */
  place(p: Pt) {
    this.pos = { ...p };
    this.target = { ...p };
    this.vel = { x: 0, y: 0 };
    this.apply();
  }

  goTo(p: Pt, speed: "slow" | "normal" | "fast" = "normal") {
    this.target = { ...p };
    this.stiffness = speed === "fast" ? 170 : speed === "slow" ? 34 : 80;
    this.damping = speed === "slow" ? 0.92 : 0.8;
    this.wake();
  }

  /** A bounce when it swallows something or takes a hit */
  bump(strength = 1) {
    this.tell({ kind: "bump", strength });
    play(this.bumper, [{ transform: `scale(${1 + 0.3 * strength})` }, { transform: "scale(1)" }], { spring: SPRING.bounce, fill: "none" });
    play(this.halo, [{ opacity: 1, transform: `scale(${1 + 0.45 * strength})` }, { opacity: 0.55, transform: "scale(1)" }], { duration: 620, fill: "none" });
  }

  /** Ease the spin speed toward its goal; only runs while the speed is changing */
  private ramp() {
    if (this.ramping) return;
    this.ramping = true;
    onFrame((dt) => {
      const goal = this.thinking ? Math.max(MUSE, SPIN[this.mood]) : SPIN[this.mood];
      this.rate += (goal - this.rate) * Math.min(1, dt * 3.5);
      if (Math.abs(goal - this.rate) < 0.02) this.rate = goal;
      this.spin.updatePlaybackRate(this.rate);
      if (this.rate === goal) {
        this.ramping = false;
        return false;
      }
    });
  }

  setBusy(on: boolean) {
    this.el.classList.toggle("busy", on);
    this.tell({ kind: "state" });
  }

  /** Circling arcs for a tool that only waits or scans (kept apart from the terminal's busy flag) */
  setWaiting(on: boolean) {
    this.el.classList.toggle("waiting", on);
    this.tell({ kind: "state" });
  }

  /** The arcs circle and the colour wheel spins up while the next step is being decided. `decided` adds
   *  a small bump as it lands, for when thinking ends because the next action has begun. */
  setThinking(on: boolean, decided = false) {
    if (on === this.thinking) return;
    this.thinking = on;
    this.el.classList.toggle("thinking", on);
    this.tell({ kind: "state" });
    if (!on && decided) this.bump(0.45);
    this.ramp();
  }

  get isThinking() {
    return this.thinking;
  }

  private wake() {
    if (this.running) return;
    this.running = true;
    onFrame((dt) => this.tick(dt));
  }

  private tick(dt: number) {
    const k = this.stiffness;
    const c = 2 * Math.sqrt(k) * this.damping;
    this.vel.x += (k * (this.target.x - this.pos.x) - c * this.vel.x) * dt;
    this.vel.y += (k * (this.target.y - this.pos.y) - c * this.vel.y) * dt;
    this.pos.x += this.vel.x * dt;
    this.pos.y += this.vel.y * dt;
    const rest = Math.hypot(this.target.x - this.pos.x, this.target.y - this.pos.y) < 0.3 && Math.hypot(this.vel.x, this.vel.y) < 4;
    if (rest) {
      this.pos = { ...this.target };
      this.vel = { x: 0, y: 0 };
    }
    this.apply();
    if (rest) this.running = false;
    return !rest;
  }

  private apply() {
    const { x, y } = this.pos;
    this.el.style.transform = `translate3d(${x.toFixed(2)}px,${y.toFixed(2)}px,0)`;
    const speed = Math.hypot(this.vel.x, this.vel.y);
    const ang = Math.atan2(this.vel.y, this.vel.x).toFixed(3);
    // Stretch along the direction of flight
    const s = clamp(speed / 2600, 0, 0.3);
    this.body.style.transform = s < 0.004 ? "" : `rotate(${ang}rad) scale(${(1 + s).toFixed(3)},${(1 - s * 0.55).toFixed(3)}) rotate(${-ang}rad)`;
    // Trail: longer and brighter the faster it goes
    const t = clamp((speed - 120) / 1000);
    this.tail.style.opacity = t.toFixed(3);
    if (t > 0) this.tail.style.transform = `rotate(${ang}rad) scaleX(${(0.2 + t * 1.15).toFixed(3)})`;
    for (const cb of this.moved) cb();
  }
}

export const orb = new Orb();
