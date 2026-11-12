// 小精灵：一个会飞、会眨眼、会看方向的发光球。弹簧物理驱动位置，挤压拉伸跟随速度。

import type { Pt } from "./geo";
import { trail } from "./particles";
import { clamp, lerp } from "./ease";

export type Mood = "idle" | "ready" | "work" | "focus" | "ask" | "happy" | "error" | "offline";

const HUE: Record<Mood, [number, number]> = {
  idle: [212, 1],
  ready: [196, 1],
  work: [228, 1],
  focus: [190, 1],
  ask: [38, 1],
  happy: [158, 1],
  error: [356, 1],
  offline: [220, 0.18],
};

const SIZE = 220; // 画布边长（CSS 像素）
const R = 30; // 球半径

class Orb {
  readonly el = document.getElementById("orb") as HTMLDivElement;
  private canvas = document.getElementById("orb-canvas") as HTMLCanvasElement;
  private ctx = this.canvas.getContext("2d")!;
  private dpr = 1;

  pos: Pt = { x: -200, y: -200 };
  vel: Pt = { x: 0, y: 0 };
  private target: Pt = { x: -200, y: -200 };
  private stiffness = 70;
  private damping = 0.78;
  /** 被别的动画直接摆位置（例如推边框时） */
  private pinned = false;

  private mood: Mood = "idle";
  private hue = 212;
  private sat = 1;
  private t = 0;
  /** 出场缩放 0..1 */
  private presence = 0;
  private presenceTarget = 0;
  /** 吞东西/受惊时的一下弹跳 */
  private pulse = 0;
  private pulseV = 0;
  private busy = 0;
  private busyTarget = 0;
  private blink = 0;
  private nextBlink = 2;
  private look: Pt = { x: 0, y: 0 };
  private lookTarget: Pt | null = null;
  private wander = 0;
  private wanderLook: Pt = { x: 0, y: 0 };
  private spin = 0;

  constructor() {
    this.resize();
  }

  resize() {
    this.dpr = Math.min(window.devicePixelRatio || 1, 2);
    this.canvas.width = SIZE * this.dpr;
    this.canvas.height = SIZE * this.dpr;
    this.canvas.style.width = this.canvas.style.height = `${SIZE}px`;
  }

  get hueNow() {
    return this.hue;
  }

  setMood(m: Mood) {
    this.mood = m;
  }

  getMood() {
    return this.mood;
  }

  show(on = true) {
    this.presenceTarget = on ? 1 : 0;
  }

  /** 瞬移（不播放飞行） */
  place(p: Pt) {
    this.pos = { ...p };
    this.target = { ...p };
    this.vel = { x: 0, y: 0 };
  }

  goTo(p: Pt, speed: "slow" | "normal" | "fast" = "normal") {
    this.pinned = false;
    this.target = { ...p };
    this.stiffness = speed === "fast" ? 150 : speed === "slow" ? 28 : 70;
    this.damping = speed === "slow" ? 0.9 : 0.75;
  }

  /** 由外部逐帧设置位置（保留速度用于挤压拉伸） */
  pin(p: Pt, dt: number) {
    this.pinned = true;
    if (dt > 0) this.vel = { x: (p.x - this.pos.x) / dt, y: (p.y - this.pos.y) / dt };
    this.pos = { ...p };
    this.target = { ...p };
  }

  unpin() {
    this.pinned = false;
  }

  lookAt(p: Pt | null) {
    this.lookTarget = p;
  }

  bump(strength = 1) {
    this.pulseV += 9 * strength;
  }

  setBusy(on: boolean) {
    this.busyTarget = on ? 1 : 0;
  }

  update(dt: number) {
    this.t += dt;

    if (!this.pinned) {
      const k = this.stiffness;
      const c = 2 * Math.sqrt(k) * this.damping;
      const ax = k * (this.target.x - this.pos.x) - c * this.vel.x;
      const ay = k * (this.target.y - this.pos.y) - c * this.vel.y;
      this.vel.x += ax * dt;
      this.vel.y += ay * dt;
      this.pos.x += this.vel.x * dt;
      this.pos.y += this.vel.y * dt;
    }

    const speed = Math.hypot(this.vel.x, this.vel.y);
    if (speed > 260 && this.presence > 0.5 && Math.random() < dt * 60) trail(this.pos, this.vel, this.hue);
    if (this.pinned) {
      this.vel.x *= 0.85;
      this.vel.y *= 0.85;
    }

    const [h, s] = HUE[this.mood];
    // 色相走最短路
    let dh = h - this.hue;
    if (dh > 180) dh -= 360;
    if (dh < -180) dh += 360;
    this.hue = (this.hue + dh * Math.min(1, dt * 4) + 360) % 360;
    this.sat = lerp(this.sat, s, Math.min(1, dt * 3));

    this.presence = lerp(this.presence, this.presenceTarget, Math.min(1, dt * (this.presenceTarget > this.presence ? 3.2 : 5)));
    this.busy = lerp(this.busy, this.busyTarget, Math.min(1, dt * 4));

    // 弹跳是一个小弹簧
    this.pulseV += (-180 * this.pulse - 12 * this.pulseV) * dt;
    this.pulse += this.pulseV * dt;

    // 眨眼
    this.nextBlink -= dt;
    if (this.nextBlink <= 0) {
      this.blink = 1;
      this.nextBlink = 2.2 + Math.random() * 3.5;
    }
    this.blink = Math.max(0, this.blink - dt * 7);

    // 眼睛看向：飞行方向 > 指定目标 > 四处张望
    let lx = 0;
    let ly = 0;
    if (speed > 80) {
      lx = this.vel.x / speed;
      ly = this.vel.y / speed;
    } else if (this.lookTarget) {
      const dx = this.lookTarget.x - this.pos.x;
      const dy = this.lookTarget.y - this.pos.y;
      const d = Math.hypot(dx, dy) || 1;
      const k = clamp(d / 160);
      lx = (dx / d) * k;
      ly = (dy / d) * k;
    } else {
      this.wander -= dt;
      if (this.wander <= 0) {
        this.wander = 1.5 + Math.random() * 2.5;
        this.wanderLook = Math.random() < 0.4 ? { x: 0, y: 0 } : { x: Math.random() * 1.4 - 0.7, y: Math.random() * 0.8 - 0.4 };
      }
      lx = this.wanderLook.x;
      ly = this.wanderLook.y;
    }
    this.look.x = lerp(this.look.x, lx, Math.min(1, dt * 10));
    this.look.y = lerp(this.look.y, ly, Math.min(1, dt * 10));

    this.spin += dt * (0.6 + this.busy * 2.4);

    const bob = Math.sin(this.t * 2.1) * 3.5 * (1 - clamp(speed / 300));
    this.el.style.transform = `translate3d(${this.pos.x - SIZE / 2}px, ${this.pos.y - SIZE / 2 + bob}px, 0)`;
    this.draw(speed);
  }

  private draw(speed: number) {
    const c = this.ctx;
    const d = this.dpr;
    c.setTransform(d, 0, 0, d, 0, 0);
    c.clearRect(0, 0, SIZE, SIZE);
    const pr = this.presence;
    if (pr < 0.01) return;

    const cx = SIZE / 2;
    const cy = SIZE / 2;
    const h = this.hue;
    const s = Math.round(this.sat * 100);
    const t = this.t;
    const breathe = 1 + Math.sin(t * 2.4) * 0.025;
    const scale = pr * breathe * (1 + this.pulse * 0.12);

    // 速度方向上的拉伸
    const ang = Math.atan2(this.vel.y, this.vel.x);
    const stretch = clamp(speed / 2400, 0, 0.32);

    c.save();
    c.translate(cx, cy);

    // 外层光晕
    c.globalCompositeOperation = "lighter";
    const halo = c.createRadialGradient(0, 0, R * 0.4 * scale, 0, 0, R * 3.2 * scale);
    halo.addColorStop(0, `hsla(${h},${s}%,60%,${0.42 * pr})`);
    halo.addColorStop(0.35, `hsla(${h},${s}%,55%,${0.16 * pr})`);
    halo.addColorStop(1, `hsla(${h},${s}%,50%,0)`);
    c.fillStyle = halo;
    c.beginPath();
    c.arc(0, 0, R * 3.2 * scale, 0, Math.PI * 2);
    c.fill();

    // 轨道粒子（3D 椭圆，先画后面一半）
    const orbit = (front: boolean) => {
      const n = 14;
      for (let i = 0; i < n; i++) {
        const ring = i % 2;
        const a = this.spin * (ring ? 1.3 : -1) + (i / n) * Math.PI * 2;
        const tilt = ring ? 0.32 : -0.42;
        const rx = R * (1.75 + ring * 0.25 + this.busy * 0.25);
        const z = Math.sin(a);
        if (z > 0 !== front) continue;
        const x0 = Math.cos(a) * rx;
        const y0 = Math.sin(a) * rx * 0.28;
        const x = x0 * Math.cos(tilt) - y0 * Math.sin(tilt);
        const y = x0 * Math.sin(tilt) + y0 * Math.cos(tilt);
        const sz = (1.4 + (z + 1) * 0.9) * scale;
        c.fillStyle = `hsla(${h + 20},${s}%,${80 + z * 12}%,${(0.35 + (z + 1) * 0.3) * pr})`;
        c.beginPath();
        c.arc(x * scale, y * scale, sz, 0, Math.PI * 2);
        c.fill();
      }
    };
    orbit(false);

    // 忙碌时的旋转光弧
    if (this.busy > 0.02) {
      c.lineCap = "round";
      for (let k = 0; k < 2; k++) {
        const a0 = this.spin * 2.2 + k * Math.PI;
        c.strokeStyle = `hsla(${h + 30},100%,72%,${0.75 * this.busy * pr})`;
        c.lineWidth = 2.2 * scale;
        c.beginPath();
        c.arc(0, 0, R * 1.42 * scale, a0, a0 + 1.1);
        c.stroke();
      }
    }

    // 球体
    c.globalCompositeOperation = "source-over";
    c.save();
    c.rotate(ang);
    c.scale((1 + stretch) * scale, (1 - stretch * 0.7) * scale);
    c.rotate(-ang);

    const body = c.createRadialGradient(-R * 0.35, -R * 0.4, R * 0.1, 0, 0, R);
    body.addColorStop(0, `hsla(${h - 10},${s}%,92%,1)`);
    body.addColorStop(0.35, `hsla(${h},${s}%,62%,1)`);
    body.addColorStop(0.8, `hsla(${h + 18},${s}%,38%,1)`);
    body.addColorStop(1, `hsla(${h + 30},${s}%,24%,1)`);
    c.fillStyle = body;
    c.beginPath();
    c.arc(0, 0, R, 0, Math.PI * 2);
    c.fill();

    // 内部流动的等离子
    c.save();
    c.clip();
    c.globalCompositeOperation = "lighter";
    for (let k = 0; k < 3; k++) {
      const a = t * (0.7 + k * 0.35) + k * 2.1;
      const bx = Math.cos(a) * R * 0.45;
      const by = Math.sin(a * 1.3) * R * 0.4;
      const g = c.createRadialGradient(bx, by, 0, bx, by, R * 0.75);
      g.addColorStop(0, `hsla(${h + 40 - k * 30},100%,70%,${0.4 + this.busy * 0.2})`);
      g.addColorStop(1, `hsla(${h},100%,50%,0)`);
      c.fillStyle = g;
      c.fillRect(-R, -R, R * 2, R * 2);
    }
    c.restore();

    // 边缘光
    c.globalCompositeOperation = "lighter";
    const rim = c.createRadialGradient(0, 0, R * 0.78, 0, 0, R * 1.05);
    rim.addColorStop(0, `hsla(${h},100%,70%,0)`);
    rim.addColorStop(0.85, `hsla(${h - 10},100%,80%,0.55)`);
    rim.addColorStop(1, `hsla(${h},100%,70%,0)`);
    c.fillStyle = rim;
    c.beginPath();
    c.arc(0, 0, R * 1.05, 0, Math.PI * 2);
    c.fill();
    // 高光
    c.fillStyle = "rgba(255,255,255,0.55)";
    c.beginPath();
    c.ellipse(-R * 0.38, -R * 0.52, R * 0.26, R * 0.13, -0.5, 0, Math.PI * 2);
    c.fill();
    c.globalCompositeOperation = "source-over";

    this.drawEyes(c);
    c.restore();

    c.globalCompositeOperation = "lighter";
    orbit(true);
    c.restore();
  }

  private drawEyes(c: CanvasRenderingContext2D) {
    const m = this.mood;
    const lx = this.look.x * 7;
    const ly = this.look.y * 5;
    const gap = 10;
    let w = 5.2;
    let hgt = 9.5;
    if (m === "focus") {
      w = 6.4;
      hgt = 11.5;
    }
    if (m === "offline") hgt = 3;
    if (m === "work") hgt = 8;
    const open = 1 - Math.sin(Math.min(1, this.blink) * Math.PI);
    c.fillStyle = m === "offline" ? "rgba(255,255,255,0.45)" : "rgba(255,255,255,0.97)";
    c.shadowColor = `hsla(${this.hue},100%,85%,0.9)`;
    c.shadowBlur = 8;
    for (const side of [-1, 1]) {
      const ex = side * gap + lx;
      const ey = -2 + ly;
      if (m === "happy") {
        // ^ ^
        c.strokeStyle = "rgba(255,255,255,0.97)";
        c.lineWidth = 3.2;
        c.lineCap = "round";
        c.beginPath();
        c.arc(ex, ey + 3, 5, Math.PI * 1.15, Math.PI * 1.85);
        c.stroke();
        continue;
      }
      const hh = Math.max(1.2, hgt * open);
      c.beginPath();
      c.ellipse(ex, ey, w / 2, hh / 2, 0, 0, Math.PI * 2);
      c.fill();
      if (m === "error") {
        // 皱眉
        c.strokeStyle = "rgba(255,255,255,0.9)";
        c.lineWidth = 2;
        c.beginPath();
        c.moveTo(ex - 5, ey - 9 + side * -1.5);
        c.lineTo(ex + 5, ey - 9 + side * 1.5);
        c.stroke();
      }
    }
    c.shadowBlur = 0;
  }
}

export const orb = new Orb();
