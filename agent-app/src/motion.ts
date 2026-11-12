// 动效基础：弹簧曲线、统一帧循环、Web Animations 的小封装。
// 原则：所有动画只动 transform / opacity，交给合成器去跑；需要逐帧算的东西（小精灵的飞行）
// 走同一个帧循环，没人需要时循环自己停掉。

export const clamp = (v: number, a = 0, b = 1) => Math.min(b, Math.max(a, v));
export const lerp = (a: number, b: number, t: number) => a + (b - a) * t;
export const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));
export const easeInOutCubic = (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);

export const EASE = {
  out: "cubic-bezier(.16,1,.3,1)",
  inOut: "cubic-bezier(.65,0,.35,1)",
  in: "cubic-bezier(.5,0,.85,.35)",
  soft: "cubic-bezier(.3,.7,.3,1)",
} as const;

export interface Spring {
  easing: string;
  duration: number;
}

/** 把一根真实弹簧（0 → 1）采样成 CSS linear() 曲线，动画仍由合成器执行 */
export function spring(stiffness: number, damping: number, mass = 1): Spring {
  const dt = 1 / 240;
  let x = 0;
  let v = 0;
  let t = 0;
  const pts: number[] = [0];
  let rest = 0;
  while (t < 3) {
    for (let i = 0; i < 4; i++) {
      const a = (stiffness * (1 - x) - damping * v) / mass;
      v += a * dt;
      x += v * dt;
      t += dt;
    }
    pts.push(Math.round(x * 1000) / 1000);
    rest = Math.abs(1 - x) < 0.002 && Math.abs(v) < 0.02 ? rest + 1 : 0;
    if (rest >= 3) break;
  }
  pts[pts.length - 1] = 1;
  return { easing: `linear(${pts.join(",")})`, duration: Math.round(t * 1000) };
}

export const SPRING = {
  /** 干脆利落，不回弹 */
  snap: spring(420, 40),
  /** 轻微回弹，用于"冒出来" */
  pop: spring(300, 21),
  /** 明显回弹，用于受到冲击的一下 */
  bounce: spring(380, 15),
  /** 慢而柔和 */
  soft: spring(140, 22),
};

export function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls = "", parent?: HTMLElement, text?: string) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  parent?.append(e);
  return e;
}

/** 等一个 Web Animation 播完（被取消也算完） */
export const done = (a: Animation) => a.finished.then(() => {}, () => {});

type Timing = Omit<KeyframeAnimationOptions, "easing" | "duration"> & { easing?: string; duration?: number; spring?: Spring };

/** 播放一段动画，默认停在最后一帧 */
export function play(e: Element, frames: Keyframe[], o: Timing = {}) {
  const { spring: s, ...rest } = o;
  return e.animate(frames, { duration: s?.duration ?? 400, easing: s?.easing ?? EASE.out, fill: "both", ...rest });
}

/** 播完就把元素移除 */
export function playOut(e: Element, frames: Keyframe[], o: Timing = {}) {
  return done(play(e, frames, o)).then(() => e.remove());
}

export function fadeOut(e: Element, ms = 300, extra: Keyframe = {}) {
  return playOut(e, [{}, { opacity: 0, ...extra }], { duration: ms, easing: EASE.inOut });
}

// ---- 帧循环 ----

type Tick = (dt: number, now: number) => boolean | void;
const ticks = new Set<Tick>();
let raf = 0;
let last = 0;

function loop(now: number) {
  const dt = Math.min(0.05, (now - last) / 1000);
  last = now;
  for (const f of [...ticks]) if (f(dt, now) === false) ticks.delete(f);
  raf = ticks.size ? requestAnimationFrame(loop) : 0;
}

/** 每帧调用 fn，直到它返回 false */
export function onFrame(fn: Tick) {
  ticks.add(fn);
  if (!raf) {
    last = performance.now();
    raf = requestAnimationFrame(loop);
  }
}

/** 按时间推进的一段逐帧动画 */
export function tween(ms: number, step: (t: number, dt: number) => void): Promise<void> {
  return new Promise((resolve) => {
    let t = 0;
    onFrame((dt) => {
      t = Math.min(1, t + (dt * 1000) / ms);
      step(t, dt);
      if (t >= 1) {
        resolve();
        return false;
      }
    });
  });
}
