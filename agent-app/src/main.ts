// 被控端界面：没有窗口，只有一个会动的小精灵。所有界面元素都浮在透明的全屏悬浮层上。

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { center, dock, hit, home, initGeo, isTauri, primary, setHome, syncHits, type OverlayInfo } from "./geo";
import { orb } from "./orb";
import { resizeFx, stepFx, stream, burst, ring } from "./particles";
import { caption, followCaption } from "./caption";
import { collapse, expand, isShown } from "./frame";
import { fxEnd, fxStart, type FxEvent } from "./fx";
import { sleep } from "./ease";
import { runDemo } from "./demo";

type Status =
  | { state: "connecting" }
  | { state: "offline"; message?: string }
  | { state: "ready"; code: string }
  | { state: "consent"; techName: string; timeoutSec: number }
  | { state: "admin"; techName: string; timeoutSec: number }
  | { state: "reboot"; techName: string; timeoutSec: number }
  | { state: "connected"; techName: string; code: string };

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

const codeCard = $("code-card");
const consent = $("consent");
const menu = $("menu");
const orbHit = $<HTMLButtonElement>("orb-hit");

let state: Status["state"] = "connecting";
let lastCode = "";
let techName = "";
let entered = false;
let consentTimer: number | undefined;

// ---------------- 协助码 ----------------

function renderCode(code: string) {
  if (code === lastCode) return;
  lastCode = code;
  const box = $("code");
  box.replaceChildren();
  box.setAttribute("aria-label", `协助码 ${code.split("").join(" ")}`);
  [...code].forEach((d, i) => {
    if (i === 3) box.append(Object.assign(document.createElement("span"), { className: "gap" }));
    const el = document.createElement("span");
    el.className = "digit";
    el.textContent = d;
    el.style.animationDelay = `${300 + i * 70}ms`;
    box.append(el);
  });
}

// ---------------- 面板的显示与位置 ----------------

function showPanel(p: HTMLElement, on: boolean, interactive = false) {
  if (on === !p.hidden) return;
  if (on) {
    p.hidden = false;
    p.animate(
      [
        { opacity: 0, transform: "scale(.85)", filter: "blur(8px)" },
        { opacity: 1, transform: "scale(1)", filter: "blur(0)" },
      ],
      { duration: 380, easing: "cubic-bezier(.2,1.1,.3,1)" },
    );
  } else {
    const a = p.animate([{ opacity: 1 }, { opacity: 0, transform: "scale(.92)", filter: "blur(6px)" }], { duration: 220, easing: "ease-in" });
    a.finished.then(() => (p.hidden = true), () => (p.hidden = true));
  }
  if (interactive) hit(p, on);
}

/** 面板放在小精灵左边（放不下就放下面），每帧跟着走 */
function besideOrb(p: HTMLElement, prefer: "left" | "below") {
  if (p.hidden) return;
  const w = p.offsetWidth;
  const h = p.offsetHeight;
  const work = primary().work;
  const o = orb.pos;
  let x: number;
  let y: number;
  if (prefer === "left" && o.x - w - 60 > work.x) {
    x = o.x - w - 56;
    y = o.y - h / 2;
  } else {
    x = o.x - w / 2;
    y = o.y + 62;
  }
  x = Math.min(Math.max(x, work.x + 12), work.x + work.w - w - 12);
  y = Math.min(Math.max(y, work.y + 12), work.y + work.h - h - 12);
  p.style.left = `${x}px`;
  p.style.top = `${y}px`;
}

// ---------------- 悬停菜单 ----------------

let menuTimer: number | undefined;
function openMenu() {
  window.clearTimeout(menuTimer);
  if (state === "consent" || state === "admin" || state === "reboot") return;
  $("menu-status").textContent =
    state === "connected" ? `${techName} 正在帮你看电脑` : state === "ready" ? "等待对方连接" : state === "offline" ? "暂时连不上服务器" : "正在准备";
  resetQuit();
  showPanel(menu, true, true);
}
function closeMenuSoon() {
  window.clearTimeout(menuTimer);
  menuTimer = window.setTimeout(() => showPanel(menu, false, true), 500);
}
orbHit.addEventListener("mouseenter", openMenu);
orbHit.addEventListener("click", () => (menu.hidden ? openMenu() : showPanel(menu, false, true)));
orbHit.addEventListener("mouseleave", closeMenuSoon);
menu.addEventListener("mouseenter", () => window.clearTimeout(menuTimer));
menu.addEventListener("mouseleave", closeMenuSoon);

const quitBtn = $<HTMLButtonElement>("btn-quit");
let quitArmed = false;
function resetQuit() {
  quitArmed = false;
  quitBtn.textContent = "退出程序";
}
quitBtn.addEventListener("click", async () => {
  if (!quitArmed) {
    quitArmed = true;
    quitBtn.textContent = "再点一次退出";
    return;
  }
  showPanel(menu, false, true);
  caption("再见", 0);
  orb.setMood("happy");
  await sleep(500);
  orb.show(false);
  await sleep(500);
  if (isTauri) invoke("quit");
});
$("btn-end").addEventListener("click", () => {
  showPanel(menu, false, true);
  if (isTauri) invoke("disconnect");
  else render({ state: "ready", code: lastCode });
});

function addActivity(text: string) {
  const li = document.createElement("li");
  const time = document.createElement("time");
  time.textContent = new Date().toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
  const span = document.createElement("span");
  span.textContent = text;
  span.title = text;
  li.append(time, span);
  const log = $("menu-log");
  log.prepend(li);
  while (log.childElementCount > 8) log.lastElementChild?.remove();
}

// ---------------- 同意 ----------------

type Ask = "consent" | "admin" | "reboot";
const ASK_TEXT: Record<Ask, [string, string]> = {
  consent: ["想远程协助你的电脑", "允许后，对方可以："],
  admin: ["需要管理员权限", "允许后，对方还可以："],
  reboot: ["需要重启你的电脑", "点允许之前请注意："],
};

function showConsent(name: string, timeoutSec: number, kind: Ask) {
  $("consent-name").textContent = name;
  [$("consent-ask").textContent, $("consent-sub").textContent] = ASK_TEXT[kind];
  $("perm-consent").hidden = kind !== "consent";
  $("perm-admin").hidden = kind !== "admin";
  $("admin-note").hidden = kind !== "admin";
  $("perm-reboot").hidden = kind !== "reboot";
  $("reboot-note").hidden = kind !== "reboot";
  showPanel(menu, false, true);
  showPanel(consent, true, true);

  const bar = $("timer-bar");
  const started = Date.now();
  window.clearInterval(consentTimer);
  const tick = () => {
    const left = Math.max(0, timeoutSec - (Date.now() - started) / 1000);
    bar.style.transform = `scaleX(${left / timeoutSec})`;
    $("timer-sec").textContent = String(Math.ceil(left));
    if (left <= 0) window.clearInterval(consentTimer);
  };
  tick();
  consentTimer = window.setInterval(tick, 200);
}

function hideConsent() {
  window.clearInterval(consentTimer);
  showPanel(consent, false, true);
}

async function respond(allow: boolean) {
  const wasAsk = state;
  hideConsent();
  if (allow) {
    orb.setMood("happy");
    burst(orb.pos, { n: 30, hue: 150, speed: 320 });
  }
  if (isTauri) await invoke("respond", { allow });
  else if (allow || wasAsk !== "consent") render({ state: "connected", techName: $("consent-name").textContent ?? "", code: lastCode });
  else render({ state: "ready", code: lastCode });
}
$("btn-allow").addEventListener("click", () => respond(true));
$("btn-deny").addEventListener("click", () => respond(false));

// ---------------- 状态机 ----------------

function render(s: Status) {
  const prev = state;
  state = s.state;
  document.body.dataset.state = s.state;
  const asking = s.state === "consent" || s.state === "admin" || s.state === "reboot";

  if (asking) {
    const c = center(primary().work);
    setHome({ x: c.x, y: c.y - 190 });
    orb.goTo(home(), "normal");
    orb.setMood("ask");
    orb.bump(1);
    showPanel(codeCard, false);
    showConsent(s.techName, s.timeoutSec, s.state as Ask);
    caption("");
    return;
  }
  setHome(null);
  hideConsent();

  switch (s.state) {
    case "connecting":
      orb.setMood("ready");
      showPanel(codeCard, false);
      caption("正在准备…", 0);
      break;
    case "offline":
      orb.setMood("offline");
      showPanel(codeCard, false);
      caption(`暂时连不上服务器，正在重试${s.message ? "" : "…"}`, 0);
      if (entered) orb.goTo(home(), "slow");
      break;
    case "ready":
      renderCode(s.code);
      techName = "";
      if (isShown()) {
        caption("协助已结束", 3000);
        void collapse().then(() => {
          orb.goTo(home(), "slow");
          if (state === "ready") showPanel(codeCard, true);
        });
      } else {
        if (prev !== "ready") caption("");
        if (entered) {
          orb.goTo(home(), "slow");
          showPanel(codeCard, true);
        }
      }
      orb.setMood("ready");
      break;
    case "connected":
      techName = s.techName;
      showPanel(codeCard, false);
      if (!isShown()) {
        caption(`${s.techName} 开始帮你看电脑`, 0);
        void expand().then(() => {
          orb.setMood("idle");
          caption(`${s.techName} 正在帮你看电脑，每一步都会显示在这里`, 4000);
        });
      } else {
        orb.goTo(home(), "slow");
        orb.setMood("idle");
      }
      break;
  }
}

// ---------------- 出场 ----------------

async function entrance() {
  const c = center(primary().work);
  orb.place(c);
  // 粒子从四周汇聚成小精灵
  stream(
    () => {
      const a = Math.random() * Math.PI * 2;
      const r = 220 + Math.random() * 160;
      return { x: c.x + Math.cos(a) * r, y: c.y + Math.sin(a) * r };
    },
    () => c,
    { n: 70, hue: 205 },
  );
  await sleep(700);
  orb.show(true);
  orb.bump(1.6);
  ring(c, { r1: 120, hue: 205, ms: 800, width: 4 });
  burst(c, { n: 30, hue: 205, speed: 300 });
  await sleep(1300);
  entered = true;
  // 出场期间状态可能已经变了，按当前状态归位
  if (state === "ready") {
    orb.goTo(home(), "slow");
    await sleep(500);
    if (state === "ready") showPanel(codeCard, true);
  } else if (state === "connecting" || state === "offline") orb.goTo(home(), "slow");
}

// ---------------- 主循环 ----------------

let last = performance.now();
let hitClock = 0;
function frame(now: number) {
  const dt = Math.min(0.05, (now - last) / 1000);
  last = now;
  orb.update(dt);
  stepFx(dt);
  followCaption();
  besideOrb(codeCard, "left");
  besideOrb(menu, "left");
  besideOrb(consent, "below");
  hitClock += dt;
  if (hitClock > 0.08) {
    hitClock = 0;
    syncHits();
  }
  requestAnimationFrame(frame);
}

async function boot() {
  let info: OverlayInfo | null = null;
  if (isTauri) {
    try {
      info = await invoke<OverlayInfo>("overlay_info");
    } catch {
      info = null;
    }
  }
  initGeo(info);
  resizeFx();
  orb.resize();
  orb.place(center(primary().work));
  hit(orbHit, true);
  requestAnimationFrame(frame);

  window.addEventListener("resize", () => {
    if (!isTauri) initGeo(null);
    resizeFx();
  });

  if (isTauri) {
    await listen<Status>("status", (e) => render(e.payload));
    await listen<{ text: string }>("activity", (e) => addActivity(e.payload.text));
    await listen<FxEvent>("fx", (e) => (e.payload.phase === "start" ? fxStart(e.payload) : fxEnd(e.payload)));
    $("admin-tag").hidden = !(await invoke<boolean>("is_admin"));
    render(await invoke<Status>("get_status"));
    void entrance();
  } else {
    document.body.classList.add("demo");
    void entrance();
    runDemo({ render, addActivity, dock });
  }
}

void boot();
