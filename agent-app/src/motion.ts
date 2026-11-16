// Motion basics: spring curves, a shared frame loop, and small Web Animations helpers.
// Rule: every animation moves only transform / opacity and runs on the compositor; anything computed
// per frame (the orb's flight) shares one frame loop, which stops itself when nobody needs it.

export const clamp = (v: number, a = 0, b = 1) => Math.min(b, Math.max(a, v));
export const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

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

/** Sample a real spring (0 -> 1) into a CSS linear() curve; the compositor still runs the animation */
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
  /** Crisp, no overshoot */
  snap: spring(420, 40),
  /** Slight overshoot, for things popping in */
  pop: spring(300, 21),
  /** Strong overshoot, for an impact bounce */
  bounce: spring(380, 15),
  /** Slow and soft */
  soft: spring(140, 22),
};

export function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls = "", parent?: HTMLElement, text?: string) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  parent?.append(e);
  return e;
}

/** Wait for a Web Animation to finish (cancelled counts as finished) */
export const done = (a: Animation) => a.finished.then(() => {}, () => {});

type Timing = Omit<KeyframeAnimationOptions, "easing" | "duration"> & { easing?: string; duration?: number; spring?: Spring };

/** Play an animation, holding the last frame by default */
export function play(e: Element, frames: Keyframe[], o: Timing = {}) {
  const { spring: s, ...rest } = o;
  return e.animate(frames, { duration: s?.duration ?? 400, easing: s?.easing ?? EASE.out, fill: "both", ...rest });
}

/** Remove the element when the animation ends */
export function playOut(e: Element, frames: Keyframe[], o: Timing = {}) {
  return done(play(e, frames, o)).then(() => e.remove());
}

export function fadeOut(e: Element, ms = 300, extra: Keyframe = {}) {
  return playOut(e, [{}, { opacity: 0, ...extra }], { duration: ms, easing: EASE.inOut });
}

// ---- Frame loop ----

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

/** Call fn every frame until it returns false */
export function onFrame(fn: Tick) {
  ticks.add(fn);
  if (!raf) {
    last = performance.now();
    raf = requestAnimationFrame(loop);
  }
}

/** A per-frame animation driven by elapsed time */
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
