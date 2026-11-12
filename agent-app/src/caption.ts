// 小精灵头顶的一行说明文字

import { orb } from "./orb";
import { primary } from "./geo";

const box = document.getElementById("caption") as HTMLDivElement;
const textEl = document.getElementById("caption-text") as HTMLSpanElement;
let hideTimer: number | undefined;
let sticky = false;

/** ms = 0 表示一直显示，直到下一次 caption */
export function caption(text: string, ms = 2800) {
  window.clearTimeout(hideTimer);
  sticky = ms === 0;
  if (!text) {
    box.classList.remove("on");
    return;
  }
  if (textEl.textContent !== text) {
    textEl.textContent = text;
    box.animate([{ filter: "blur(6px)", opacity: 0.3 }, { filter: "blur(0)", opacity: 1 }], { duration: 260, easing: "ease-out" });
  }
  box.classList.add("on");
  if (!sticky) hideTimer = window.setTimeout(() => box.classList.remove("on"), ms);
}

/** 每帧跟着小精灵走，靠近屏幕边时往里收 */
export function followCaption() {
  if (!box.classList.contains("on")) return;
  const w = box.offsetWidth;
  const work = primary().work;
  const p = orb.pos;
  let x = p.x - w / 2;
  x = Math.min(Math.max(x, work.x + 12), work.x + work.w - w - 12);
  const above = p.y - 74 - box.offsetHeight > work.y + 8;
  const y = above ? p.y - 70 - box.offsetHeight : p.y + 64;
  box.style.transform = `translate3d(${x}px, ${y}px, 0)`;
}
