// 边缘流光：AI 接管期间，每块屏幕四周有一圈流动的渐变光。
// 整圈光是同一张锥形渐变贴图在一个环形遮罩后面旋转，所以颜色沿着四条边和四个拐角
// 天然是连续的，没有拼接。贴图只画一次（一张小画布，由显卡拉伸），之后只有旋转这一个
// transform 在动；空闲几秒后旋转会停下来，不再产生帧。

import { center, home, monitors, type R } from "./geo";
import { orb } from "./orb";
import { bloom, ripple } from "./light";
import { done, EASE, el, play, sleep } from "./motion";

const stage = document.getElementById("frames") as HTMLDivElement;

interface Edge {
  root: HTMLDivElement;
  /** 常亮的一圈：3 像素实线 + 朝内淡出的柔光 */
  ring: HTMLDivElement;
  /** 更宽更亮的一圈，只在有动作时闪一下 */
  flare: HTMLDivElement;
  spins: Animation[];
}

let edges: Edge[] = [];
let shown = false;
let busy: Promise<void> = Promise.resolve();
let restTimer: number | undefined;

const LOOP = ["#5b8cff", "#b36bff", "#ff7ab6", "#ffb86b", "#5be0c8", "#5b8cff"];
/** 转一圈的时间 */
const TURN_MS = 16000;

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
  // 一个能盖住整块屏幕对角线的正方形，绕屏幕中心转
  const d = Math.ceil(Math.hypot(mon.w, mon.h)) + 4;
  Object.assign(cv.style, { width: `${d}px`, height: `${d}px`, left: `${(mon.w - d) / 2}px`, top: `${(mon.h - d) / 2}px` });
  return cv.animate([{ transform: "rotate(0turn)" }, { transform: "rotate(1turn)" }], { duration: TURN_MS, iterations: Infinity });
}

function make(mon: R): Edge {
  const root = el("div", "edge", stage);
  Object.assign(root.style, { left: `${mon.x}px`, top: `${mon.y}px`, width: `${mon.w}px`, height: `${mon.h}px` });
  const ring = el("div", "edge-ring", root);
  const flare = el("div", "edge-flare", root);
  return { root, ring, flare, spins: [spinner(ring, mon), spinner(flare, mon)] };
}

function eachSpin(fn: (a: Animation) => void) {
  for (const e of edges) e.spins.forEach(fn);
}

/** 有动作时转得快一些，停下来几秒后暂停 */
export function setActive(on: boolean) {
  window.clearTimeout(restTimer);
  if (!shown) return;
  if (on) {
    eachSpin((a) => {
      a.updatePlaybackRate(2.2);
      a.play();
    });
  } else {
    eachSpin((a) => a.updatePlaybackRate(0.8));
    restTimer = window.setTimeout(() => eachSpin((a) => a.pause()), 9000);
  }
}

/** AI 开始控制：小精灵飞到主屏中间绽开，一圈涟漪向外扩散，扩到屏幕边缘时整圈光亮起 */
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
    setActive(false);
    await sleep(500);
  });
  return busy;
}

/** 结束控制：整圈光向外退出屏幕，小精灵亮一下 */
export function collapse(): Promise<void> {
  busy = busy.then(async () => {
    if (!shown) return;
    shown = false;
    window.clearTimeout(restTimer);
    await Promise.all(edges.map((e) => done(play(e.root, [{ opacity: 1, transform: "scale(1)" }, { opacity: 0, transform: "scale(1.05)" }], { duration: 600, easing: EASE.inOut }))));
    clear();
    orb.bump(1.1);
    bloom(orb.pos, { size: 220, ms: 760 });
    ripple(orb.pos, { size: 60, ms: 700 });
  });
  return busy;
}

/** 有工具开始执行时，整圈光亮一下、变厚一下 */
export function pulse() {
  for (const e of edges) play(e.flare, [{ opacity: 0.9 }, { opacity: 0 }], { duration: 900, easing: EASE.out });
}

export const isShown = () => shown;

/** 显示器布局变了：按新的尺寸把流光重新铺一遍 */
export function relayout() {
  if (!shown) return;
  clear();
  edges = monitors.map((m) => make(m.bounds));
  for (const e of edges) e.ring.style.opacity = "1";
  setActive(false);
}

function clear() {
  stage.replaceChildren();
  edges = [];
}
