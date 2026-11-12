// 小精灵：一颗流动的渐变光球。位置由弹簧物理驱动，飞行时身后拖一道光迹。
// 静止时没有任何逐帧脚本：自转、呼吸、悬浮都是合成器上的 CSS 动画。

import type { Pt } from "./geo";
import { clamp, EASE, onFrame, play, SPRING } from "./motion";

export type Mood = "idle" | "ready" | "work" | "focus" | "ask" | "happy" | "error" | "offline";

/** 各状态下光球自转的快慢 */
const SPIN: Record<Mood, number> = { idle: 0.6, ready: 1, work: 2.8, focus: 1.7, ask: 1.1, happy: 1.6, error: 0.5, offline: 0 };

const q = <T extends HTMLElement>(sel: string) => document.querySelector(sel) as T;

class Orb {
  readonly el = q<HTMLDivElement>("#orb");
  private pres = q<HTMLDivElement>(".orb-pres");
  private bumper = q<HTMLDivElement>(".orb-bump");
  private body = q<HTMLDivElement>(".orb-body");
  private halo = q<HTMLDivElement>(".orb-halo");
  private tail = q<HTMLDivElement>(".orb-tail");
  private spin = q<HTMLDivElement>(".orb-core").animate([{ transform: "rotate(0turn)" }, { transform: "rotate(1turn)" }], {
    duration: 5200,
    iterations: Infinity,
  });

  pos: Pt = { x: -200, y: -200 };
  vel: Pt = { x: 0, y: 0 };
  private target: Pt = { x: -200, y: -200 };
  private stiffness = 70;
  private damping = 0.78;
  /** 被别的动画直接摆位置（例如拖动时） */
  private pinned = false;
  private running = false;
  private shown = false;
  private mood: Mood = "idle";
  private moved: (() => void)[] = [];

  constructor() {
    this.setMood("idle");
  }

  setMood(m: Mood) {
    if (m === this.mood && this.el.dataset.mood === m) return;
    const was = this.mood;
    this.mood = m;
    this.el.dataset.mood = m;
    this.spin.updatePlaybackRate(SPIN[m]);
    if (m === "error" && was !== "error") {
      play(this.bumper, [{ transform: "translateX(0)" }, { transform: "translateX(-7px)" }, { transform: "translateX(6px)" }, { transform: "translateX(-3px)" }, { transform: "translateX(0)" }], {
        duration: 380,
        easing: EASE.inOut,
        fill: "none",
      });
    }
  }

  getMood() {
    return this.mood;
  }

  show(on = true) {
    if (on === this.shown) return;
    this.shown = on;
    if (on) play(this.pres, [{ transform: "scale(0)", opacity: 0 }, { transform: "scale(1)", opacity: 1 }], { spring: SPRING.pop });
    else play(this.pres, [{ transform: "scale(1)", opacity: 1 }, { transform: "scale(0)", opacity: 0 }], { duration: 300, easing: EASE.in });
  }

  /** 位置变化时通知（跟着小精灵走的面板用） */
  onMove(cb: () => void) {
    this.moved.push(cb);
  }

  /** 瞬移（不播放飞行） */
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

  /** 由外部逐帧设置位置（保留速度，光迹和拉伸照常） */
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

  /** 吞下东西、受到冲击时的一下弹跳 */
  bump(strength = 1) {
    play(this.bumper, [{ transform: `scale(${1 + 0.3 * strength})` }, { transform: "scale(1)" }], { spring: SPRING.bounce, fill: "none" });
    play(this.halo, [{ opacity: 1, transform: `scale(${1 + 0.45 * strength})` }, { opacity: 0.55, transform: "scale(1)" }], { duration: 620, fill: "none" });
  }

  setBusy(on: boolean) {
    this.el.classList.toggle("busy", on);
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
    // 沿飞行方向拉伸
    const s = clamp(speed / 2600, 0, 0.3);
    this.body.style.transform = s < 0.004 ? "" : `rotate(${ang}rad) scale(${(1 + s).toFixed(3)},${(1 - s * 0.55).toFixed(3)}) rotate(${-ang}rad)`;
    // 光迹：越快越长越亮
    const t = clamp((speed - 120) / 1000);
    this.tail.style.opacity = t.toFixed(3);
    if (t > 0) this.tail.style.transform = `rotate(${ang}rad) scaleX(${(0.2 + t * 1.15).toFixed(3)})`;
    for (const cb of this.moved) cb();
  }
}

export const orb = new Orb();
