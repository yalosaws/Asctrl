// Work panel: unfolds from the orb when the AI runs commands or reads/writes files.
// The terminal and file cards share one shell. Across consecutive actions the shell stays put and only
// its content swaps, so there is never a "one closes, another opens" gap between them.
// All text goes in via textContent, never as HTML (command output and file contents may contain angle brackets).

import { dock, home, primary, snap, type Pt, type R } from "./geo";
import { orb } from "./orb";
import { bloom } from "./light";
import { done, EASE, el, play, playOut, sleep, SPRING } from "./motion";

const layer = document.getElementById("cards") as HTMLDivElement;

const W = 560;
const H = 340;

type View = "term" | "file";

let root: HTMLDivElement | null = null;
let views: Record<View, HTMLDivElement> | null = null;
let termBody: HTMLDivElement | null = null;
let active: View | null = null;
let closeTimer: number | undefined;
let holds = 0;

// ---- Icons (fixed built-in strings, no external content) ----

const svg = (d: string) => `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">${d}</svg>`;
const DOC = '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/>';
const ICON: Record<string, string> = {
  read: svg(`${DOC}<path d="M9 13h6M9 17h4"/>`),
  write: svg('<path d="M12 20h8"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L8 18l-4 1 1-4z"/>'),
  edit: svg('<path d="M12 20h8"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L8 18l-4 1 1-4z"/>'),
  dir: svg('<path d="M3 7a2 2 0 0 1 2-2h4l2 2.5h8a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>'),
  find: svg('<circle cx="11" cy="11" r="6.5"/><path d="m20 20-4.2-4.2"/>'),
  search: svg('<circle cx="11" cy="11" r="6.5"/><path d="m20 20-4.2-4.2"/>'),
  upload: svg('<path d="M12 4v11"/><path d="m7 11 5 5 5-5"/><path d="M5 20h14"/>'),
  download: svg('<path d="M12 16V5"/><path d="m7 9 5-5 5 5"/><path d="M5 20h14"/>'),
  clip: svg('<rect x="6" y="5" width="12" height="16" rx="2"/><path d="M9 5a3 3 0 0 1 6 0"/>'),
};
const MARK_OK = svg('<path d="m5 12.5 4.5 4.5L19 7.5" pathLength="1"/>');
const MARK_BAD = svg('<path d="M6 6l12 12" pathLength="1"/><path d="M18 6 6 18" pathLength="1"/>');

function glyph(parent: HTMLElement, kind: string) {
  const g = el("span", "deck-glyph", parent);
  g.innerHTML = ICON[kind] ?? ICON.read;
  return g;
}

// ---- Shell ----

function place(): Pt {
  const d = dock();
  const work = primary().work;
  const x = snap(Math.min(Math.max(work.x + 16, d.x - W - 48), work.x + work.w - W - 16));
  const y = snap(Math.min(Math.max(work.y + 16, d.y - H + 40), work.y + work.h - H - 16));
  return { x, y };
}

/** Panel scale origin: the orb's home (relative to the panel's top-left) */
function origin(p: Pt) {
  const h = home();
  return `${h.x - p.x}px ${h.y - p.y}px`;
}

function open() {
  window.clearTimeout(closeTimer);
  orb.goTo(home(), "fast");
  if (root) return;
  const p = place();
  root = el("div", "deck", layer);
  Object.assign(root.style, { left: `${p.x}px`, top: `${p.y}px`, width: `${W}px`, height: `${H}px`, transformOrigin: origin(p) });

  // The terminal has no title bar or icon; the first line is the command
  const term = el("div", "deck-view term", root);
  termBody = el("div", "term-body", term);
  const file = el("div", "deck-view file", root);
  views = { term, file };
  term.style.opacity = file.style.opacity = "0";
  active = null;

  play(root, [{ transform: "scale(.05)", opacity: 0 }, { transform: "scale(1)", opacity: 1 }], { spring: SPRING.pop });
  orb.bump(0.7);
}

function show(v: View) {
  open();
  if (active === v || !views) return;
  if (active) play(views[active], [{ opacity: 1, transform: "none" }, { opacity: 0, transform: "translateY(-12px) scale(.985)" }], { duration: 200, easing: EASE.inOut });
  play(views[v], [{ opacity: 0, transform: "translateY(16px)" }, { opacity: 1, transform: "none" }], { spring: SPRING.snap, delay: active ? 90 : 120 });
  views[v].style.zIndex = "1";
  if (active) views[active].style.zIndex = "0";
  active = v;
}

function scheduleClose(ms: number) {
  window.clearTimeout(closeTimer);
  closeTimer = window.setTimeout(() => {
    if (holds > 0) return scheduleClose(1500);
    const w = root;
    if (!w) return;
    root = views = termBody = null;
    active = current = null;
    void playOut(w, [{ transform: "scale(1)", opacity: 1 }, { transform: "scale(.05)", opacity: 0 }], { duration: 420, easing: "cubic-bezier(.6,0,.7,.2)" }).then(() => {
      orb.bump(0.9);
      bloom(orb.pos, { size: 130, ms: 560 });
    });
  }, ms);
}

/** Area the panel currently covers */
export function deckRect(): R | null {
  if (!root) return null;
  const p = place();
  return { x: p.x, y: p.y, w: W, h: H };
}

// ---- Terminal ----

let queue: Promise<void> = Promise.resolve();
const pending = new Map<string, HTMLElement>();

function line(cls = "") {
  const l = el("div", `term-line ${cls}`, termBody!);
  play(l, [{ opacity: 0, transform: "translateY(4px)" }, { opacity: 1, transform: "none" }], { duration: 140 });
  termBody!.scrollTop = termBody!.scrollHeight;
  return l;
}

async function typeInto(target: HTMLElement, text: string) {
  const chars = [...text];
  // About 6 ms per character; even long commands finish within 0.45 s
  const total = Math.min(450, Math.max(80, chars.length * 6));
  const per = total / Math.max(1, chars.length);
  const caret = el("span", "caret", target);
  // Wait at least one frame per step; if that is too slow, type several characters at once
  const chunk = Math.max(1, Math.ceil(16 / per));
  for (let i = 0; i < chars.length; i += chunk) {
    caret.before(chars.slice(i, i + chunk).join(""));
    if (termBody) termBody.scrollTop = termBody.scrollHeight;
    await sleep(per * chunk);
  }
  caret.remove();
}

/** Start a command: type it out, then show a waiting indicator */
export function termStart(id: string, command: string) {
  holds++;
  queue = queue.then(async () => {
    show("term");
    orb.setBusy(true);
    const lines = command.replace(/\r/g, "").split("\n");
    for (let i = 0; i < lines.length; i++) {
      const l = line("cmd");
      el("span", "prompt", l, i === 0 ? "PS ~> " : ">> ");
      await typeInto(el("span", "", l), lines[i]);
    }
    pending.set(id, el("span", "spin", line("wait")));
  });
}

/** Command finished: stream the output in */
export function termEnd(id: string, ok: boolean, text: string) {
  queue = queue.then(async () => {
    const spin = pending.get(id);
    pending.delete(id);
    spin?.parentElement?.remove();
    show("term");
    const all = text.replace(/\r/g, "").replace(/\n+$/, "").split("\n");
    const MAX = 80;
    const shown = all.length > MAX ? all.slice(0, MAX - 1) : all;
    const per = Math.max(2, Math.min(14, 350 / Math.max(1, shown.length)));
    for (const s of shown) {
      line(ok ? "out" : "err").textContent = s || " ";
      await sleep(per);
    }
    if (all.length > MAX) line("more").textContent = `… ${all.length - MAX + 1}`;
    // Success prints nothing extra; only failures get a cross
    if (!ok) mark(line("status"), false);
    if (pending.size === 0) orb.setBusy(false);
    holds = Math.max(0, holds - 1);
    scheduleClose(4200);
  });
}

// ---- File cards ----

export interface Card {
  root: HTMLDivElement;
  body: HTMLDivElement;
  slot: HTMLSpanElement;
}

let current: Card | null = null;

/** Draw a check or a cross, stroke by stroke */
function mark(parent: HTMLElement, ok: boolean) {
  const m = el("span", `mark ${ok ? "ok" : "bad"}`, parent);
  m.innerHTML = ok ? MARK_OK : MARK_BAD;
  m.querySelectorAll("path").forEach((p, i) => play(p, [{ strokeDashoffset: 1 }, { strokeDashoffset: 0 }], { duration: 320, delay: i * 140, easing: EASE.inOut }));
  play(m, [{ transform: "scale(.4)", opacity: 0 }, { transform: "scale(1)", opacity: 1 }], { spring: SPRING.pop });
  return m;
}

/** Swap in a new card; the previous one slides up and away */
export function openCard(kind: string, title: string, sub = ""): Card {
  holds++;
  show("file");
  const prev = current;
  if (prev) void playOut(prev.root, [{ opacity: 1, transform: "none" }, { opacity: 0, transform: "translateY(-18px)" }], { duration: 220, easing: EASE.inOut });
  const c = el("div", `card k-${kind}`, views!.file);
  const head = el("div", "deck-head", c);
  glyph(head, kind);
  const tt = el("div", "card-titles", head);
  el("div", "deck-title", tt, title);
  if (sub) el("div", "card-sub", tt, sub);
  const slot = el("span", "card-slot", head);
  const body = el("div", "card-body", c);
  play(c, [{ opacity: 0, transform: "translateY(20px)" }, { opacity: 1, transform: "none" }], { spring: SPRING.snap, delay: prev ? 80 : 0 });
  current = { root: c, body, slot };
  return current;
}

/** The card's operation finished */
export function closeCard(c: Card, ok: boolean, text = "", linger = 1600) {
  holds = Math.max(0, holds - 1);
  if (!c.root.isConnected) return;
  c.root.querySelectorAll(".sk, .card-scan.loop").forEach((s) => s.remove());
  mark(c.slot, ok);
  c.root.classList.add(ok ? "ok" : "bad");
  if (!ok) {
    if (text) {
      const er = el("div", "ln err", c.body, text.split("\n")[0]);
      play(er, [{ opacity: 0, transform: "translateY(6px)" }, { opacity: 1, transform: "none" }], { duration: 240 });
    }
    play(c.root, [{ translate: "0" }, { translate: "-8px" }, { translate: "7px" }, { translate: "-3px" }, { translate: "0" }], { duration: 360, easing: EASE.inOut, fill: "none" });
  }
  scheduleClose(ok ? linger : linger + 900);
}

/** Placeholder bars until content arrives */
export function skeleton(c: Card, n = 6) {
  for (let i = 0; i < n; i++) {
    const s = el("div", "sk", c.body);
    s.style.width = `${38 + ((i * 37) % 50)}%`;
    el("i", "", s).style.animationDelay = `${i * 90}ms`;
    play(s, [{ opacity: 0 }, { opacity: 1 }], { duration: 260, delay: i * 50 });
  }
}

/** A gradient light sweeps top to bottom (search, read) */
export function scan(c: Card, loop = false) {
  const s = el("i", loop ? "card-scan loop" : "card-scan", c.root);
  const a = play(s, [{ transform: "translateY(0)", opacity: 0 }, { opacity: 1, offset: 0.15 }, { opacity: 1, offset: 0.85 }, { transform: `translateY(${H - 60}px)`, opacity: 0 }], {
    duration: 1000,
    easing: EASE.inOut,
    iterations: loop ? Infinity : 1,
  });
  if (!loop) void done(a).then(() => s.remove());
}

/** Progress bar; returns a function that pushes it to the end */
export function progress(c: Card, toward: number, ms: number) {
  const bar = el("i", "", el("div", "prog", c.body));
  const a = play(bar, [{ transform: "scaleX(0)" }, { transform: `scaleX(${toward})` }], { duration: ms, easing: EASE.soft });
  return () => {
    const now = getComputedStyle(bar).transform;
    a.cancel();
    play(bar, [{ transform: now === "none" ? "scaleX(0)" : now }, { transform: "scaleX(1)" }], { duration: 320 });
  };
}

/** Push lines into the card one by one; `highlight` matches are marked */
export async function fillLines(c: Card, lines: string[], cls = "", highlight?: RegExp) {
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
    play(l, [{ opacity: 0, transform: "translateX(-16px)" }, { opacity: 1, transform: "none" }], { spring: SPRING.snap, delay: i * 38, fill: "backwards" });
  }
  if (lines.length > max) el("div", "ln more", c.body, `… ${lines.length}`);
  await sleep(Math.min(lines.length, max) * 38 + 260);
}

/** Directory entries: folders and files get different glyphs */
export async function fillEntries(c: Card, entries: { name: string; dir: boolean }[]) {
  c.body.replaceChildren();
  const max = 11;
  for (const [i, e] of entries.slice(0, max).entries()) {
    const l = el("div", `ln ent ${e.dir ? "dir" : "file"}`, c.body);
    el("i", "", l);
    el("span", "", l, e.name);
    play(l, [{ opacity: 0, transform: "translateX(-16px)" }, { opacity: 1, transform: "none" }], { spring: SPRING.snap, delay: i * 38, fill: "backwards" });
  }
  if (entries.length > max) el("div", "ln more", c.body, `… ${entries.length}`);
  await sleep(Math.min(entries.length, max) * 38 + 260);
}
