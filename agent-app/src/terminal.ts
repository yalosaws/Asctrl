// 内置终端：AI 跑命令时从小精灵身边弹出，把命令一个字一个字敲出来，再把输出流出来。

import { dock, home, primary } from "./geo";
import { orb } from "./orb";
import { absorb, beam, burst } from "./particles";
import { done, el, sleep } from "./ease";

const layer = document.getElementById("cards") as HTMLDivElement;

let win: HTMLDivElement | null = null;
let body: HTMLDivElement | null = null;
let queue: Promise<void> = Promise.resolve();
let closeTimer: number | undefined;
const pending = new Map<string, HTMLSpanElement>();

const W = 600;
const H = 360;

function place(w: HTMLDivElement) {
  const d = dock();
  const work = primary().work;
  const x = Math.max(work.x + 16, d.x - W - 70);
  const y = Math.max(work.y + 16, d.y - H + 30);
  w.style.left = `${x}px`;
  w.style.top = `${y}px`;
}

function open() {
  window.clearTimeout(closeTimer);
  if (win) return;
  win = el("div", "term", layer);
  win.style.width = `${W}px`;
  win.style.height = `${H}px`;
  place(win);
  const bar = el("div", "term-bar", win);
  el("i", "dot r", bar);
  el("i", "dot y", bar);
  el("i", "dot g", bar);
  el("span", "term-title", bar, "PowerShell");
  body = el("div", "term-body", win);
  win.animate(
    [
      { transform: "translate(80px, 60px) scale(0.12)", opacity: 0, filter: "blur(10px)" },
      { transform: "translate(0,0) scale(1.02)", opacity: 1, filter: "blur(0)", offset: 0.7 },
      { transform: "translate(0,0) scale(1)", opacity: 1, filter: "blur(0)" },
    ],
    { duration: 520, easing: "cubic-bezier(.2,.9,.3,1)" },
  );
  const b = win.getBoundingClientRect();
  beam(() => orb.pos, () => ({ x: b.right - 30, y: b.bottom - 30 }), { ms: 500, hue: 200 });
}

function scheduleClose(ms = 4200) {
  window.clearTimeout(closeTimer);
  closeTimer = window.setTimeout(() => {
    if (pending.size) return scheduleClose(2000);
    const w = win;
    if (!w) return;
    win = null;
    body = null;
    const b = w.getBoundingClientRect();
    absorb({ x: b.left, y: b.top, w: b.width, h: b.height }, () => orb.pos, { n: 26, hue: 200 });
    const a = w.animate(
      [
        { transform: "translate(0,0) scale(1)", opacity: 1, filter: "blur(0)" },
        { transform: "translate(90px, 70px) scale(0.1)", opacity: 0, filter: "blur(8px)" },
      ],
      { duration: 480, easing: "cubic-bezier(.6,0,.4,1)", fill: "forwards" },
    );
    done(a).then(() => {
      w.remove();
      orb.bump(0.8);
    });
  }, ms);
}

function line(cls = "") {
  const l = el("div", `term-line ${cls}`, body!);
  body!.scrollTop = body!.scrollHeight;
  return l;
}

async function typeInto(target: HTMLElement, text: string) {
  const chars = [...text];
  const total = Math.min(1500, Math.max(250, chars.length * 28));
  const per = total / Math.max(1, chars.length);
  const caret = el("span", "caret", target);
  // 每次至少等一帧，太快的话一次多打几个字
  const chunk = Math.max(1, Math.ceil(16 / per));
  for (let i = 0; i < chars.length; i += chunk) {
    caret.before(chars.slice(i, i + chunk).join(""));
    body!.scrollTop = body!.scrollHeight;
    await sleep(per * chunk);
  }
  caret.remove();
}

/** 开始一条命令：打出命令，显示等待中的转圈 */
export function termStart(id: string, command: string) {
  // 终端挨着小精灵的"家"，先飞回去再敲命令
  orb.goTo(home(), "fast");
  orb.lookAt(null);
  queue = queue.then(async () => {
    open();
    orb.setBusy(true);
    const lines = command.replace(/\r/g, "").split("\n");
    for (let i = 0; i < lines.length; i++) {
      const l = line("cmd");
      el("span", "prompt", l, i === 0 ? "PS ~> " : ">> ");
      const span = el("span", "", l);
      await typeInto(span, lines[i]);
    }
    const spin = el("span", "spin", line("wait"));
    pending.set(id, spin);
  });
}

/** 命令结束：把输出流出来 */
export function termEnd(id: string, ok: boolean, text: string) {
  queue = queue.then(async () => {
    const spin = pending.get(id);
    pending.delete(id);
    spin?.parentElement?.remove();
    if (!body) open();
    const all = text.replace(/\r/g, "").replace(/\n+$/, "").split("\n");
    const MAX = 80;
    const shown = all.length > MAX ? [...all.slice(0, MAX - 1)] : all;
    const per = Math.max(4, Math.min(28, 700 / Math.max(1, shown.length)));
    for (const s of shown) {
      const l = line(ok ? "out" : "err");
      l.textContent = s || " ";
      await sleep(per);
    }
    if (all.length > MAX) line("more").textContent = `… 还有 ${all.length - MAX + 1} 行`;
    const status = line(ok ? "ok" : "err");
    status.textContent = ok ? "✔" : "✖";
    if (win) {
      const b = win.getBoundingClientRect();
      burst({ x: b.left + 40, y: b.bottom - 24 }, { n: 12, hue: ok ? 150 : 0, speed: 180 });
    }
    if (pending.size === 0) orb.setBusy(false);
    scheduleClose();
  });
}

/** 终端当前占的区域（文件卡片要避开它） */
export const termBox = () => win?.getBoundingClientRect() ?? null;
