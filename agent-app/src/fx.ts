// 每个工具调用对应的动画。start 在工具开始执行时触发，end 在结果出来后触发。
// 所有文字都用 textContent 写入，不拼 HTML（命令输出、文件内容都可能带尖括号）。

import { center, dock, home, monitorAt, monitors, primary, pt, rect, type Pt, type R } from "./geo";
import { orb } from "./orb";
import { absorb, beam, burst, ring, shatterRect, stream } from "./particles";
import { pulse } from "./frame";
import { caption } from "./caption";
import { termBox, termEnd, termStart } from "./terminal";
import { done, el, sleep, tween } from "./ease";

export interface FxEvent {
  phase: "start" | "end";
  id: string;
  tool: string;
  desc?: string;
  args?: Record<string, unknown>;
  geo?: Record<string, unknown>;
  ok?: boolean;
  text?: string;
}

const layer = document.getElementById("cards") as HTMLDivElement;
const marks = document.getElementById("marks") as HTMLDivElement;

const str = (v: unknown) => (typeof v === "string" ? v : v === undefined || v === null ? "" : String(v));
const num = (v: unknown, d = 0) => (typeof v === "number" && Number.isFinite(v) ? v : d);
const baseName = (p: string) => p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || p;
const dirName = (p: string) => {
  const i = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
  return i > 0 ? p.slice(0, i) : "";
};

// ---- 小精灵的注意力：干完活过一会儿回到角落 ----

let homeTimer: number | undefined;
let active = 0;

function attend() {
  active++;
  window.clearTimeout(homeTimer);
  orb.setMood("work");
}

function release(ok = true, delay = 1500) {
  active = Math.max(0, active - 1);
  if (!ok) {
    orb.setMood("error");
    orb.bump(0.8);
  }
  window.clearTimeout(homeTimer);
  homeTimer = window.setTimeout(() => {
    if (active > 0) return;
    orb.setMood("idle");
    orb.lookAt(null);
    orb.goTo(home(), "slow");
  }, ok ? delay : delay + 1200);
}

/** 飞到目标点旁边（不挡住目标），眼睛盯着它 */
function near(p: Pt, speed: "normal" | "fast" = "fast") {
  const work = monitorAt(p).work;
  const dx = p.x + 90 > work.x + work.w ? -78 : 78;
  const dy = p.y + 90 > work.y + work.h ? -70 : 70;
  orb.goTo({ x: p.x + dx, y: p.y + dy }, speed);
  orb.lookAt(p);
}

// ---- 标记层上的小元素 ----

function mark(cls: string, p: Pt) {
  const m = el("div", `mk ${cls}`, marks);
  m.style.transform = `translate3d(${p.x}px, ${p.y}px, 0)`;
  return m;
}

function box(cls: string, r: R) {
  const b = el("div", `bx ${cls}`, marks);
  setRect(b, r);
  return b;
}

function setRect(b: HTMLElement, r: R) {
  b.style.left = `${r.x}px`;
  b.style.top = `${r.y}px`;
  b.style.width = `${r.w}px`;
  b.style.height = `${r.h}px`;
}

function fadeOut(e: HTMLElement, ms = 400, extra: Keyframe = {}) {
  const a = e.animate([{ opacity: 1 }, { opacity: 0, ...extra }], { duration: ms, easing: "ease-in", fill: "forwards" });
  done(a).then(() => e.remove());
}

// ---- 截图 ----

const SHOT_HUE = 190;
const shots = new Map<string, HTMLElement[]>();

function shotRegions(args: Record<string, unknown>): R[] {
  const d = num(args.display);
  if (d > 0 && monitors[d - 1]) return [monitors[d - 1].bounds];
  return monitors.map((m) => m.bounds);
}

function viewfinder(r: R) {
  const v = box("vf", r);
  for (const c of ["tl", "tr", "bl", "br"]) el("i", `vf-c ${c}`, v);
  const scan = el("i", "vf-scan", v);
  v.animate([{ opacity: 0, transform: "scale(1.06)" }, { opacity: 1, transform: "scale(1)" }], {
    duration: 420,
    easing: "cubic-bezier(.2,.9,.3,1)",
  });
  scan.animate([{ transform: "translateY(-10vh)" }, { transform: `translateY(${r.h}px)` }], {
    duration: 900,
    easing: "cubic-bezier(.5,0,.5,1)",
    iterations: Infinity,
  });
  return v;
}

function screenshotStart(e: FxEvent) {
  const regions = shotRegions(e.args ?? {});
  shots.set(e.id, regions.map(viewfinder));
  orb.setMood("focus");
  orb.lookAt(center(primary().bounds));
  orb.bump(0.6);
}

async function flyInto(card: HTMLElement, from: R, opts: { rotate?: number; ms?: number } = {}) {
  const to = orb.pos;
  const c = center(from);
  const s = Math.max(0.02, 46 / Math.max(from.w, from.h));
  const a = card.animate(
    [
      { transform: "translate(0,0) scale(1) rotate(0)", opacity: 1 },
      { transform: `translate(${(to.x - c.x) * 0.15}px, ${(to.y - c.y) * 0.15 - 20}px) scale(0.82) rotate(${(opts.rotate ?? -4) / 2}deg)`, opacity: 1, offset: 0.35 },
      { transform: `translate(${to.x - c.x}px, ${to.y - c.y}px) scale(${s}) rotate(${opts.rotate ?? -4}deg)`, opacity: 0.2 },
    ],
    { duration: opts.ms ?? 720, easing: "cubic-bezier(.55,0,.25,1)", fill: "forwards" },
  );
  await done(a);
  card.remove();
  orb.bump(1.3);
  ring(orb.pos, { r1: 64, hue: orb.hueNow, ms: 520 });
  burst(orb.pos, { n: 16, hue: orb.hueNow, speed: 200 });
}

function screenshotEnd(e: FxEvent) {
  const vfs = shots.get(e.id) ?? [];
  shots.delete(e.id);
  vfs.forEach((v) => {
    v.querySelector(".vf-scan")?.remove();
    v.animate([{ transform: "scale(1)" }, { transform: "scale(0.97)" }, { transform: "scale(1)" }], { duration: 260 });
    setTimeout(() => fadeOut(v, 300), 180);
  });
  if (!e.ok) return;
  const shot = rect(e.geo?.shot);
  const regions = shot ? [shot] : shotRegions(e.args ?? {});
  for (const r of regions) {
    const f = box("flash", r);
    done(f.animate([{ opacity: 0 }, { opacity: 0.55, offset: 0.15 }, { opacity: 0 }], { duration: 480, easing: "ease-out" })).then(() => f.remove());
    const photo = box("photo", r);
    el("i", "photo-sheen", photo);
    setTimeout(() => flyInto(photo, r), 220);
  }
  orb.setMood("focus");
}

// ---- 放大查看 ----

function zoomStart(e: FxEvent) {
  const a = pt(e.geo?.from);
  const b = pt(e.geo?.to);
  if (!a || !b) return;
  const r: R = { x: Math.min(a.x, b.x), y: Math.min(a.y, b.y), w: Math.abs(a.x - b.x), h: Math.abs(a.y - b.y) };
  const v = viewfinder(r);
  v.classList.add("small");
  const lens = mark("lens", center(r));
  lens.animate([{ scale: "0.2", opacity: 0 }, { scale: "1", opacity: 1 }], { duration: 420, easing: "cubic-bezier(.2,1.4,.4,1)", fill: "forwards" });
  near(center(r));
  shots.set(e.id, [v, lens]);
}

function zoomEnd(e: FxEvent) {
  const els = shots.get(e.id) ?? [];
  shots.delete(e.id);
  const [v, lens] = els;
  lens && fadeOut(lens, 300, { scale: "1.6" });
  if (!v) return;
  v.querySelector(".vf-scan")?.remove();
  if (e.ok) {
    const r = v.getBoundingClientRect();
    v.classList.add("photo");
    flyInto(v, { x: r.left, y: r.top, w: r.width, h: r.height }, { ms: 600 });
  } else fadeOut(v);
}

// ---- 鼠标 ----

let holdMark: HTMLElement | null = null;
const BTN_HUE = (b: string) => (b === "right" ? 38 : b === "middle" ? 280 : 200);

function clickStart(e: FxEvent) {
  const p = pt(e.geo?.at);
  if (!p) return;
  near(p);
  const hue = BTN_HUE(str(e.args?.button));
  ring(p, { r0: 56, r1: 9, ms: 320, hue, width: 2 });
  beam(() => orb.pos, () => p, { ms: 650, hue, width: 2 });
  const m = mark("reticle", p);
  m.style.setProperty("--h", String(hue));
  m.animate([{ scale: "2.2", opacity: 0, rotate: "-90deg" }, { scale: "1", opacity: 1, rotate: "0deg" }], {
    duration: 300,
    easing: "cubic-bezier(.2,.9,.3,1)",
    fill: "forwards",
  });
  setTimeout(() => fadeOut(m, 350, { scale: "0.6" }), 700);
}

function clickEnd(e: FxEvent) {
  const p = pt(e.geo?.at);
  if (!p) return;
  const hue = BTN_HUE(str(e.args?.button));
  if (!e.ok) return;
  const n = Math.max(1, Math.min(3, num(e.args?.count, 1)));
  for (let i = 0; i < n; i++) ring(p, { r1: 40 + i * 6, ms: 560, hue, width: 3, delay: i * 120 });
  burst(p, { n: 12, hue, speed: 220, size: 11 });
}

function moveStart(e: FxEvent) {
  const p = pt(e.geo?.at);
  if (!p) return;
  near(p);
  ring(p, { r0: 30, r1: 6, ms: 380, hue: 200, width: 2 });
}

function downStart(e: FxEvent) {
  const p = pt(e.geo?.at);
  if (!p) return;
  near(p);
  holdMark?.remove();
  holdMark = mark("hold", p);
  holdMark.style.setProperty("--h", String(BTN_HUE(str(e.args?.button))));
}

function upStart(e: FxEvent) {
  const p = pt(e.geo?.at);
  if (holdMark) fadeOut(holdMark, 300, { scale: "1.8" });
  holdMark = null;
  if (p) ring(p, { r1: 36, hue: 200 });
}

function scrollStart(e: FxEvent) {
  const p = pt(e.geo?.at);
  if (!p) return;
  near(p);
  const amount = num(e.args?.amount, 3);
  const horiz = e.args?.horizontal === true;
  const dir = horiz ? (amount >= 0 ? "right" : "left") : amount >= 0 ? "down" : "up";
  const m = mark(`scroll ${dir}`, p);
  for (let i = 0; i < 3; i++) {
    const c = el("i", "chev", m);
    c.style.animationDelay = `${i * 110}ms`;
  }
  setTimeout(() => fadeOut(m, 300), 1100);
}

async function dragStart(e: FxEvent) {
  const a = pt(e.geo?.from);
  const b = pt(e.geo?.to);
  if (!a || !b) return;
  near(a);
  ring(a, { r0: 40, r1: 8, ms: 300, hue: 200 });
  const grip = mark("hold", a);
  grip.style.setProperty("--h", "200");
  await sleep(320);
  let last = performance.now();
  beam(() => a, () => orb.pos, { ms: 1400, hue: 205, width: 2, bend: 0 });
  await tween(760, (t) => {
    const now = performance.now();
    const k = t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2;
    const p = { x: a.x + (b.x - a.x) * k, y: a.y + (b.y - a.y) * k };
    grip.style.transform = `translate3d(${p.x}px, ${p.y}px, 0)`;
    orb.pin({ x: p.x + 60, y: p.y + 56 }, (now - last) / 1000);
    last = now;
    if (Math.random() < 0.7) burst(p, { n: 1, hue: 205, speed: 40, life: 0.7, size: 12 });
  });
  orb.unpin();
  orb.lookAt(b);
  ring(b, { r1: 44, hue: 200, width: 3 });
  fadeOut(grip, 400, { scale: "1.6" });
}

// ---- 键盘 ----

let typingPill: HTMLElement | null = null;

async function typeStart(e: FxEvent) {
  const text = str(e.args?.text);
  const at = pt(e.geo?.at) ?? dock();
  typingPill?.remove();
  const work = monitorAt(at).work;
  const pill = el("div", "typing", layer);
  typingPill = pill;
  const x = Math.min(at.x + 18, work.x + work.w - 380);
  const y = Math.min(at.y + 30, work.y + work.h - 70);
  pill.style.left = `${Math.max(work.x + 12, x)}px`;
  pill.style.top = `${y}px`;
  el("span", "kbd-ico", pill, "⌨");
  const line = el("span", "typing-text", pill);
  const caret = el("i", "caret", pill);
  pill.animate([{ opacity: 0, transform: "translateY(10px) scale(.9)" }, { opacity: 1, transform: "none" }], { duration: 260, easing: "cubic-bezier(.2,.9,.3,1)" });
  orb.goTo({ x: Math.max(work.x + 12, x) - 50, y: y + 22 }, "fast");
  orb.lookAt({ x: Math.max(work.x + 12, x) + 80, y: y + 22 });

  const chars = [...text.replace(/\r?\n/g, "⏎")];
  const per = Math.min(60, Math.max(14, 1400 / Math.max(1, chars.length)));
  for (const ch of chars) {
    if (typingPill !== pill) return;
    const s = el("span", "ch", line, ch);
    s.animate([{ transform: "translateY(-12px) scale(1.6)", opacity: 0 }, { transform: "none", opacity: 1 }], { duration: 220, easing: "cubic-bezier(.2,1.3,.4,1)" });
    // 只保留最后一截，长文本不撑爆
    while (line.childElementCount > 42) line.firstElementChild?.remove();
    await sleep(per);
  }
  caret.remove();
}

function typeEnd(e: FxEvent) {
  const pill = typingPill;
  if (!pill) return;
  pill.classList.add(e.ok ? "ok" : "bad");
  setTimeout(() => {
    if (typingPill === pill) typingPill = null;
    fadeOut(pill, 380, { transform: "translateY(-10px) scale(.95)" });
  }, 1300);
}

const KEY_LABEL: Record<string, string> = {
  ctrl: "Ctrl", control: "Ctrl", shift: "Shift", alt: "Alt", win: "Win", meta: "Win", cmd: "Win", super: "Win",
  enter: "Enter ⏎", return: "Enter ⏎", tab: "Tab ⇥", esc: "Esc", escape: "Esc", space: "Space", backspace: "⌫",
  delete: "Del", del: "Del", up: "↑", down: "↓", left: "←", right: "→", pageup: "PgUp", pagedown: "PgDn", home: "Home", end: "End",
};

async function keysStart(e: FxEvent) {
  const raw = str(e.args?.keys).trim();
  if (!raw) return;
  const combos = raw.split(/\s+/).slice(0, 6);
  const wrap = el("div", "keys", layer);
  const p = orb.pos;
  wrap.style.left = `${p.x}px`;
  wrap.style.top = `${p.y - 110}px`;
  orb.lookAt({ x: p.x, y: p.y - 100 });
  for (const combo of combos) {
    const g = el("div", "combo", wrap);
    combo.split("+").forEach((k, i) => {
      if (i) el("span", "plus", g, "+");
      const cap = el("span", "cap", g, KEY_LABEL[k.toLowerCase()] ?? (k.length === 1 ? k.toUpperCase() : k));
      cap.animate([{ transform: "translateY(-16px) scale(.6)", opacity: 0 }, { transform: "none", opacity: 1 }], {
        duration: 280,
        delay: i * 60,
        easing: "cubic-bezier(.2,1.4,.4,1)",
        fill: "backwards",
      });
    });
    await sleep(260);
    for (const cap of g.querySelectorAll<HTMLElement>(".cap")) cap.classList.add("down");
    burst({ x: p.x, y: p.y - 100 }, { n: 8, hue: 210, speed: 160, size: 9 });
    await sleep(e.tool === "hold_key" ? 700 : 220);
    for (const cap of g.querySelectorAll<HTMLElement>(".cap")) cap.classList.remove("down");
    await sleep(160);
  }
  setTimeout(() => fadeOut(wrap, 400, { transform: "translateY(-14px)" }), 600);
}

// ---- 窗口 ----

const winCtx = new Map<string, HTMLElement>();

function spotlight(r: R, ms = 1300) {
  const s = box("spot", r);
  done(s.animate([{ opacity: 0 }, { opacity: 1, offset: 0.2 }, { opacity: 1, offset: 0.7 }, { opacity: 0 }], { duration: ms, easing: "ease-in-out" })).then(() => s.remove());
}

function winStart(e: FxEvent) {
  const r0 = rect(e.geo?.rect);
  if (!r0) return;
  const o = box("winline", r0);
  o.animate([{ opacity: 0, transform: "scale(1.04)" }, { opacity: 1, transform: "scale(1)" }], { duration: 300, easing: "cubic-bezier(.2,.9,.3,1)" });
  winCtx.set(e.id, o);
  orb.goTo({ x: r0.x + r0.w / 2, y: Math.max(primary().work.y + 40, r0.y - 34) }, "fast");
  orb.lookAt(center(r0));
  beam(() => orb.pos, () => ({ x: r0.x + r0.w / 2, y: r0.y + 8 }), { ms: 600, hue: 205 });
}

async function winEnd(e: FxEvent) {
  const action = str(e.args?.action) || "focus";
  const o = winCtx.get(e.id);
  winCtx.delete(e.id);
  const r1 = rect(e.geo?.rect);
  if (!e.ok) {
    if (o) {
      o.classList.add("bad");
      o.animate([{ translate: "0" }, { translate: "-8px" }, { translate: "8px" }, { translate: "0" }], { duration: 300 });
      setTimeout(() => fadeOut(o), 600);
    }
    return;
  }
  const from = o ? o.getBoundingClientRect() : null;
  const r0: R | null = from ? { x: from.left, y: from.top, w: from.width, h: from.height } : null;

  if (action === "close" && r0 && o) {
    o.remove();
    shatterRect(r0, 205);
    return;
  }
  if (action === "minimize" && r0 && o) {
    const w = monitorAt(center(r0)).work;
    const to: R = { x: w.x + w.w / 2 - 40, y: w.y + w.h - 10, w: 80, h: 10 };
    await morph(o, r0, to, 520);
    fadeOut(o, 200);
    ring({ x: to.x + 40, y: to.y + 5 }, { r1: 30, hue: 205 });
    return;
  }
  if (!r1) {
    if (o) fadeOut(o);
    return;
  }
  const line = o ?? box("winline", { x: center(r1).x - 40, y: primary().work.y + primary().work.h - 10, w: 80, h: 10 });
  const start = r0 ?? { x: center(r1).x - 40, y: primary().work.y + primary().work.h - 10, w: 80, h: 10 };
  orb.goTo({ x: r1.x + r1.w / 2, y: Math.max(primary().work.y + 40, r1.y - 34) }, "fast");
  await morph(line, start, r1, 520);
  spotlight(r1);
  line.classList.add("lock");
  burst({ x: r1.x, y: r1.y }, { n: 8, hue: 205, speed: 160 });
  burst({ x: r1.x + r1.w, y: r1.y + r1.h }, { n: 8, hue: 205, speed: 160 });
  setTimeout(() => fadeOut(line, 500), 1100);
}

function morph(b: HTMLElement, a: R, c: R, ms: number) {
  const k = (r: R): Keyframe => ({ left: `${r.x}px`, top: `${r.y}px`, width: `${r.w}px`, height: `${r.h}px` });
  setRect(b, c);
  return done(b.animate([k(a), k(c)], { duration: ms, easing: "cubic-bezier(.65,0,.25,1)" }));
}

function listWindowsEnd(e: FxEvent) {
  const ws = Array.isArray(e.geo?.windows) ? (e.geo!.windows as unknown[]) : [];
  ws.forEach((w, i) => {
    const r = rect(w);
    if (!r || r.w < 40 || r.h < 30) return;
    const b = box("winline thin", r);
    b.animate([{ opacity: 0 }, { opacity: 1, offset: 0.25 }, { opacity: 0 }], { duration: 900, delay: i * 70, fill: "both" });
    setTimeout(() => b.remove(), 900 + i * 70);
  });
  orb.setMood("focus");
}

function openAppStart() {
  orb.bump(1);
  burst(orb.pos, { n: 22, hue: 260, speed: 360, dir: -Math.PI / 2, spread: 0.9 });
  ring(orb.pos, { r1: 80, hue: 260, ms: 600 });
}

function openAppEnd(e: FxEvent) {
  if (!e.ok) return;
  const c = center(primary().work);
  ring(c, { r0: 20, r1: 260, hue: 260, ms: 900, width: 3 });
  burst(c, { n: 30, hue: 260, speed: 420 });
}

// ---- 文件卡片 ----

const ICON: Record<string, string> = {
  read: "📄",
  write: "✏️",
  edit: "🩹",
  dir: "📁",
  find: "🔎",
  search: "🔎",
  upload: "⬇️",
  download: "⬆️",
  clip: "📋",
};

interface Card {
  root: HTMLDivElement;
  body: HTMLDivElement;
  badge: HTMLSpanElement;
}

let current: Card | null = null;
const cards = new Map<string, Card>();

function card(id: string, kind: string, title: string, sub: string, label: string): Card {
  if (current) dismiss(current, true);
  const root = el("div", `card k-${kind}`, layer);
  const head = el("div", "card-head", root);
  el("span", "card-ico", head, ICON[kind] ?? "📄");
  const tt = el("div", "card-titles", head);
  el("div", "card-title", tt, title);
  if (sub) el("div", "card-sub", tt, sub);
  const badge = el("span", "card-badge", head, label);
  const body = el("div", "card-body", root);
  const d = dock();
  const work = primary().work;
  const tb = termBox();
  const bottom = tb ? tb.top - 18 : d.y - 64;
  root.style.left = `${Math.max(work.x + 16, d.x - 500)}px`;
  root.style.top = `${Math.max(work.y + 16, bottom - 300)}px`;
  root.animate(
    [
      { opacity: 0, transform: "translate(60px, 60px) scale(.3) rotate(6deg)", filter: "blur(8px)" },
      { opacity: 1, transform: "none", filter: "blur(0)" },
    ],
    { duration: 520, easing: "cubic-bezier(.2,1.1,.3,1)" },
  );
  const c = { root, body, badge };
  orb.goTo(home(), "fast");
  current = c;
  cards.set(id, c);
  setTimeout(() => {
    const b = root.getBoundingClientRect();
    orb.lookAt({ x: b.left + b.width / 2, y: b.top + b.height / 2 });
  }, 0);
  return c;
}

function cardRect(c: Card): R {
  const b = c.root.getBoundingClientRect();
  return { x: b.left, y: b.top, w: b.width, h: b.height };
}

function dismiss(c: Card, quick = false) {
  if (current === c) current = null;
  if (!c.root.isConnected) return;
  if (quick) return fadeOut(c.root, 220, { transform: "translateY(-20px) scale(.96)" });
  flyInto(c.root, cardRect(c), { rotate: 8, ms: 620 });
}

function skeleton(c: Card, n = 6) {
  for (let i = 0; i < n; i++) {
    const s = el("div", "sk", c.body);
    s.style.width = `${40 + Math.random() * 55}%`;
    s.style.animationDelay = `${i * 80}ms`;
  }
}

async function fillLines(c: Card, lines: string[], cls = "", highlight?: RegExp) {
  c.body.replaceChildren();
  const max = 11;
  for (const [i, text] of lines.slice(0, max).entries()) {
    const l = el("div", `ln ${cls}`, c.body);
    if (highlight) {
      let last = 0;
      for (const m of text.matchAll(highlight)) {
        if (m.index === undefined || !m[0]) continue;
        l.append(text.slice(last, m.index));
        el("mark", "", l, m[0]);
        last = m.index + m[0].length;
      }
      l.append(text.slice(last));
    } else l.textContent = text || " ";
    l.animate([{ opacity: 0, transform: "translateX(-14px)" }, { opacity: 1, transform: "none" }], {
      duration: 260,
      delay: i * 45,
      easing: "cubic-bezier(.2,.9,.3,1)",
      fill: "backwards",
    });
  }
  if (lines.length > max) el("div", "ln more", c.body, `… 共 ${lines.length} 行`);
  await sleep(Math.min(lines.length, max) * 45 + 260);
}

function finishBadge(c: Card, ok: boolean, text?: string) {
  c.badge.textContent = ok ? "✔ 完成" : "✖ 失败";
  c.badge.classList.add(ok ? "ok" : "bad");
  c.root.classList.add(ok ? "ok" : "bad");
  if (!ok && text) {
    const er = el("div", "ln err", c.body, text.split("\n")[0]);
    er.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 200 });
    c.root.animate([{ translate: "0" }, { translate: "-8px" }, { translate: "8px" }, { translate: "0" }], { duration: 320 });
  }
}

function scanBar(c: Card) {
  const s = el("i", "card-scan", c.root);
  done(s.animate([{ top: "40px", opacity: 1 }, { top: "100%", opacity: 0.2 }], { duration: 900, easing: "ease-in-out" })).then(() => s.remove());
}

const lines = (t: string) => t.replace(/\r/g, "").replace(/\n+$/, "").split("\n");

function fileStart(e: FxEvent) {
  const a = e.args ?? {};
  const path = str(a.path);
  switch (e.tool) {
    case "read_file": {
      const c = card(e.id, "read", baseName(path), dirName(path), "读取中");
      skeleton(c);
      setTimeout(() => beam(() => orb.pos, () => center(cardRect(c)), { ms: 700, hue: 200 }), 200);
      return;
    }
    case "write_file": {
      const c = card(e.id, "write", baseName(path), dirName(path), "写入中");
      stream(() => orb.pos, () => center(cardRect(c)), { n: 20, hue: 150 });
      fillLines(c, lines(str(a.content)), "add");
      return;
    }
    case "edit_file": {
      const c = card(e.id, "edit", baseName(path), dirName(path), "修改中");
      const oldL = lines(str(a.oldText)).slice(0, 5);
      const newL = lines(str(a.newText)).slice(0, 5);
      (async () => {
        c.body.replaceChildren();
        const olds = oldL.map((t) => el("div", "ln del", c.body, t || " "));
        await sleep(450);
        stream(() => orb.pos, () => center(cardRect(c)), { n: 14, hue: 150 });
        olds.forEach((o, i) =>
          o.animate([{ opacity: 1 }, { opacity: 0.35, textDecorationColor: "rgba(255,120,120,1)" }], { duration: 300, delay: i * 50, fill: "forwards" }),
        );
        for (const [i, t] of newL.entries()) {
          const n = el("div", "ln add", c.body, t || " ");
          n.animate([{ opacity: 0, transform: "translateX(18px)" }, { opacity: 1, transform: "none" }], { duration: 300, delay: i * 60, fill: "backwards" });
        }
      })();
      return;
    }
    case "list_dir": {
      const c = card(e.id, "dir", baseName(path || "~") || "用户目录", path, "打开中");
      skeleton(c, 7);
      return;
    }
    case "find_files": {
      const c = card(e.id, "find", `找 ${str(a.pattern)}`, path, "查找中");
      skeleton(c, 5);
      scanBar(c);
      return;
    }
    case "search_files": {
      const c = card(e.id, "search", `搜 “${str(a.pattern)}”`, path, "搜索中");
      skeleton(c, 5);
      scanBar(c);
      const lens = el("i", "card-lens", c.root);
      lens.animate([{ left: "10%", top: "45%" }, { left: "80%", top: "60%" }, { left: "30%", top: "80%" }, { left: "10%", top: "45%" }], {
        duration: 1400,
        iterations: Infinity,
        easing: "ease-in-out",
      });
      return;
    }
    case "upload_file": {
      const c = card(e.id, "upload", baseName(path), "对方传来一个文件", "接收中");
      const bar = el("div", "prog", c.body);
      el("i", "", bar).animate([{ width: "0%" }, { width: "92%" }], { duration: 1200, easing: "cubic-bezier(.3,.6,.4,1)", fill: "forwards" });
      const top = primary().bounds;
      stream(() => ({ x: top.x + top.w * Math.random(), y: top.y - 10 }), () => center(cardRect(c)), { n: 26, hue: 170 });
      return;
    }
    case "download_file": {
      const c = card(e.id, "download", baseName(path), "发送给帮你的人", "读取中");
      const bar = el("div", "prog", c.body);
      el("i", "", bar).animate([{ width: "0%" }, { width: "70%" }], { duration: 900, fill: "forwards" });
      return;
    }
    case "read_clipboard": {
      const c = card(e.id, "clip", "剪贴板", "", "查看中");
      skeleton(c, 3);
      return;
    }
    case "write_clipboard": {
      const c = card(e.id, "clip", "剪贴板", "放入内容", "写入中");
      fillLines(c, lines(str(a.text)), "add");
      stream(() => orb.pos, () => center(cardRect(c)), { n: 12, hue: 150 });
      return;
    }
  }
}

async function fileEnd(e: FxEvent) {
  const c = cards.get(e.id);
  cards.delete(e.id);
  if (!c || !c.root.isConnected) return;
  const ok = !!e.ok;
  const text = e.text ?? "";
  if (!ok) {
    c.body.querySelectorAll(".sk").forEach((s) => s.remove());
    finishBadge(c, false, text);
    setTimeout(() => dismiss(c, true), 2200);
    return;
  }
  switch (e.tool) {
    case "read_file": {
      const ls = lines(text).slice(1); // 第一行是路径和编码
      await fillLines(c, ls, "mono");
      scanBar(c);
      absorb(cardRect(c), () => orb.pos, { n: 34, hue: 200 });
      break;
    }
    case "list_dir": {
      const ls = lines(text)
        .slice(1)
        .map((l) => {
          const name = l.split(/\s{2,}/).pop() ?? l;
          return (name.endsWith("\\") ? "📁 " + name.slice(0, -1) : "📄 " + name).trim();
        });
      await fillLines(c, ls);
      break;
    }
    case "find_files":
      await fillLines(c, lines(text).map((l) => baseName(l) || l));
      break;
    case "search_files": {
      c.root.querySelector(".card-lens")?.remove();
      let re: RegExp | undefined;
      try {
        re = new RegExp(str(e.args?.pattern), e.args?.ignoreCase === false ? "g" : "gi");
      } catch {
        re = undefined;
      }
      await fillLines(c, lines(text), "mono", re);
      break;
    }
    case "upload_file":
    case "download_file": {
      const bar = c.body.querySelector<HTMLElement>(".prog i");
      bar?.getAnimations().forEach((a) => a.cancel());
      if (bar) bar.style.width = "100%";
      if (e.tool === "download_file") {
        // 文件飞向小精灵，再往上飞走（送给对方）
        absorb(cardRect(c), () => orb.pos, { n: 26, hue: 170 });
        setTimeout(() => burst(orb.pos, { n: 26, hue: 170, speed: 520, dir: -Math.PI / 2, spread: 0.35 }), 900);
      }
      break;
    }
    case "read_clipboard":
      await fillLines(c, lines(text));
      absorb(cardRect(c), () => orb.pos, { n: 18, hue: 200 });
      break;
  }
  finishBadge(c, true);
  orb.setMood("happy");
  setTimeout(() => dismiss(c), e.tool === "read_file" || e.tool === "search_files" ? 2200 : 1500);
}

// ---- 命令行 ----

function termCommand(e: FxEvent): [string, string?] {
  const a = e.args ?? {};
  switch (e.tool) {
    case "run_powershell":
      return a.background === true ? [`Start-Job { ${str(a.script).trim()} }`, "后台运行"] : [str(a.script).trim()];
    case "system_info":
      return ["Get-ComputerInfo", "查看电脑基本信息"];
    case "list_processes":
      return [`Get-Process | Sort-Object WorkingSet64 -Descending | Select -First ${num(a.top, 30)}`, "看看哪些程序在运行"];
    case "event_log":
      return [`Get-WinEvent -LogName ${str(a.log) || "System"} -MaxEvents ${num(a.max, 30)}`, "查看系统错误日志"];
    case "job_output":
      return [a.jobId === undefined ? "Get-Job" : `Receive-Job -Id ${str(a.jobId)}`, "后台任务进度"];
    case "stop_job":
      return [`Stop-Job -Id ${str(a.jobId)}`];
    case "reset_shell":
      return ["exit; powershell", "重开一个命令窗口"];
    default:
      return [e.tool];
  }
}

// ---- 分发 ----

const TERM = new Set(["run_powershell", "system_info", "list_processes", "event_log", "job_output", "stop_job", "reset_shell"]);
const FILE = new Set(["read_file", "write_file", "edit_file", "list_dir", "find_files", "search_files", "upload_file", "download_file", "read_clipboard", "write_clipboard"]);
const HUE_OF = (tool: string) =>
  TERM.has(tool) ? 228 : FILE.has(tool) ? 170 : tool === "screenshot" || tool === "zoom" ? SHOT_HUE : tool === "focus_window" ? 260 : 205;

/** 结束事件只带结果，参数和开始时的位置从这里补上 */
const started = new Map<string, Pick<FxEvent, "args" | "geo">>();

export function fxStart(e: FxEvent) {
  started.set(e.id, { args: e.args, geo: e.geo });
  attend();
  if (e.desc) caption(e.desc);
  pulse(HUE_OF(e.tool));
  switch (e.tool) {
    case "screenshot":
      return screenshotStart(e);
    case "zoom":
      return zoomStart(e);
    case "click":
      return clickStart(e);
    case "mouse_move":
      return moveStart(e);
    case "mouse_down":
      return downStart(e);
    case "mouse_up":
      return upStart(e);
    case "scroll":
      return scrollStart(e);
    case "drag":
      return void dragStart(e);
    case "type_text":
      return void typeStart(e);
    case "press_keys":
    case "hold_key":
      return void keysStart(e);
    case "focus_window":
      return winStart(e);
    case "list_windows":
      orb.setMood("focus");
      return;
    case "open_app":
      return openAppStart();
    case "wait":
    case "cursor_position":
      orb.setBusy(true);
      return;
  }
  if (TERM.has(e.tool)) {
    const [cmd, note] = termCommand(e);
    return termStart(e.id, cmd, note);
  }
  if (FILE.has(e.tool)) return fileStart(e);
}

export function fxEnd(ev: FxEvent) {
  const st = started.get(ev.id);
  started.delete(ev.id);
  const e: FxEvent = { ...ev, args: ev.args ?? st?.args ?? {}, geo: { ...(st?.geo ?? {}), ...(ev.geo ?? {}) } };
  const ok = !!e.ok;
  switch (e.tool) {
    case "screenshot":
      screenshotEnd(e);
      break;
    case "zoom":
      zoomEnd(e);
      break;
    case "click":
      clickEnd(e);
      break;
    case "type_text":
      typeEnd(e);
      break;
    case "focus_window":
      void winEnd(e);
      break;
    case "list_windows":
      listWindowsEnd(e);
      break;
    case "open_app":
      openAppEnd(e);
      break;
    case "wait":
    case "cursor_position":
      orb.setBusy(false);
      break;
    default:
      if (TERM.has(e.tool)) termEnd(e.id, ok, e.text ?? "");
      else if (FILE.has(e.tool)) void fileEnd(e);
  }
  if (!ok && e.text) caption(`没成功：${e.text.split("\n")[0].slice(0, 60)}`, 3500);
  release(ok, TERM.has(e.tool) ? 2600 : 1500);
}
