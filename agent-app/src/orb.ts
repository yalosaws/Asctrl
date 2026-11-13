// The orb: a ball of flowing gradient light. Its position is driven by spring physics, with a light trail while flying.
// At rest no per-frame script runs: spin, breathing and floating are all compositor CSS animations.

import type { Pt } from "./geo";
import { clamp, EASE, onFrame, play, SPRING } from "./motion";

export type Mood = "idle" | "ready" | "work" | "focus" | "ask" | "happy" | "error" | "offline";

/** Spin speed of the orb in each mood */
const SPIN: Record<Mood, number> = { idle: 0.6, ready: 1, work: 2.8, focus: 1.7, ask: 1.1, happy: 1.6, error: 0.5, offline: 0 };

/** One turn of the colour wheel at speed 1 */
const SPIN_MS = 5200;
/** Spin speed while flinging cursors out (about 0.4 s a turn) */
const WHIRL = 12;
/** Spin speed while thinking: the colours slow to a drift and the ring carries the motion */
const MUSE = 0.45;

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
  /** Spin speed right now; eases toward the mood's speed, or toward WHIRL while whirling */
  private rate = SPIN.idle;
  private whirlUntil = 0;
  private ramping = false;
  private thinking = false;

  pos: Pt = { x: -200, y: -200 };
  vel: Pt = { x: 0, y: 0 };
  private target: Pt = { x: -200, y: -200 };
  private stiffness = 70;
  private damping = 0.78;
  /** Positioned directly by another animation (e.g. while dragging) */
  private pinned = false;
  private running = false;
  private shown = false;
  private mood: Mood = "idle";
  private moved: (() => void)[] = [];

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
    this.pinned = false;
    this.target = { ...p };
    this.stiffness = speed === "fast" ? 170 : speed === "slow" ? 34 : 80;
    this.damping = speed === "slow" ? 0.92 : 0.8;
    this.wake();
  }

  /** Position set externally per frame (velocity kept, trail and stretch still work) */
  pin(p: Pt, dt: number) {
    this.pinned = true;
    if (dt > 0) this.vel = { x: (p.x - this.pos.x) / dt, y: (p.y - this.pos.y) / dt };
    this.pos = { ...p };
    this.target = { ...p };
    this.apply();
  }

  unpin() {
    if (!this.pinned) return;
    this.pinned = false;
    this.wake();
  }

  /** A bounce when it swallows something or takes a hit */
  bump(strength = 1) {
    play(this.bumper, [{ transform: `scale(${1 + 0.3 * strength})` }, { transform: "scale(1)" }], { spring: SPRING.bounce, fill: "none" });
    play(this.halo, [{ opacity: 1, transform: `scale(${1 + 0.45 * strength})` }, { opacity: 0.55, transform: "scale(1)" }], { duration: 620, fill: "none" });
  }

  /** Spin up hard for a while (to fling something out), then wind back down to the mood's speed */
  whirl(ms = 900) {
    this.whirlUntil = Math.max(this.whirlUntil, performance.now() + ms);
    this.ramp();
  }

  /** Where the colour wheel is turned to, in degrees clockwise */
  get turn(): number {
    const t = Number(this.spin.currentTime ?? 0);
    return (((t / SPIN_MS) % 1) + 1) % 1 * 360;
  }

  /** Ease the spin speed toward its goal; only runs while the speed is changing */
  private ramp() {
    if (this.ramping) return;
    this.ramping = true;
    onFrame((dt) => {
      const whirling = performance.now() < this.whirlUntil;
      const goal = whirling ? WHIRL : this.thinking ? Math.min(MUSE, SPIN[this.mood]) : SPIN[this.mood];
      // Quick to spin up, slower to wind down
      this.rate += (goal - this.rate) * Math.min(1, dt * (whirling ? 9 : 3.5));
      if (!whirling && Math.abs(goal - this.rate) < 0.02) this.rate = goal;
      this.spin.updatePlaybackRate(this.rate);
      if (!whirling && this.rate === goal) {
        this.ramping = false;
        return false;
      }
    });
  }

  setBusy(on: boolean) {
    this.el.classList.toggle("busy", on);
  }

  /** Circling arcs for a tool that only waits or scans (kept apart from the terminal's busy flag) */
  setWaiting(on: boolean) {
    this.el.classList.toggle("waiting", on);
  }

  get isThinking() {
    return this.thinking;
  }

  /** A ring of light is drawn in toward the orb while the next step is being decided */
  setThinking(on: boolean) {
    if (on === this.thinking) return;
    this.thinking = on;
    this.el.classList.toggle("thinking", on);
    this.ramp();
  }

  private wake() {
    if (this.running) return;
    this.running = true;
    onFrame((dt) => this.tick(dt));
  }

  private tick(dt: number) {
    if (this.pinned) return true;
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
