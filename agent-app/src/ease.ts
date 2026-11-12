export const clamp = (v: number, a = 0, b = 1) => Math.min(b, Math.max(a, v));
export const lerp = (a: number, b: number, t: number) => a + (b - a) * t;
export const easeInOutCubic = (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);
export const easeInOutQuart = (t: number) => (t < 0.5 ? 8 * t ** 4 : 1 - Math.pow(-2 * t + 2, 4) / 2);
export const easeOutCubic = (t: number) => 1 - Math.pow(1 - t, 3);
export const easeOutExpo = (t: number) => (t >= 1 ? 1 : 1 - Math.pow(2, -10 * t));
export const easeOutBack = (t: number) => {
  const c1 = 1.70158;
  const c3 = c1 + 1;
  return 1 + c3 * Math.pow(t - 1, 3) + c1 * Math.pow(t - 1, 2);
};
export const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** 按时间推进的一段动画，返回结束时 resolve 的 Promise */
export function tween(ms: number, step: (t: number) => void): Promise<void> {
  return new Promise((resolve) => {
    const t0 = performance.now();
    const f = (now: number) => {
      const t = clamp((now - t0) / ms);
      step(t);
      if (t < 1) requestAnimationFrame(f);
      else resolve();
    };
    requestAnimationFrame(f);
  });
}

export function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls = "", parent?: HTMLElement, text?: string) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  parent?.append(e);
  return e;
}

/** 等一个 Web Animation 播完（被取消也算完） */
export const done = (a: Animation) => a.finished.then(() => {}, () => {});
