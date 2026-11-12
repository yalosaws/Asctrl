// 被控端界面：没有窗口，只有一颗会动的光球。所有界面元素都浮在透明的全屏悬浮层上。

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { center, dock, hit, home, initGeo, isTauri, primary, setHome, snap, syncHits, type OverlayInfo } from "./geo";
import { orb } from "./orb";
import { bloom, ripple } from "./light";
import { collapse, expand, isShown, relayout as relayoutEdge } from "./edge";
import { fxEnd, fxStart, type FxEvent } from "./fx";
import { EASE, play, sleep, SPRING } from "./motion";
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
    el.style.animationDelay = `${260 + i * 60}ms`;
    box.append(el);
  });
}

// ---------------- 面板的显示与位置 ----------------

// 面板挨着小精灵：位置写在 translate 上，出现/消失的缩放写在 transform 上，两者互不干扰。
// 尺寸只在显示时量一次，小精灵移动时不再读布局。
const PANELS: [HTMLElement, "left" | "below"][] = [
  [codeCard, "left"],
  [menu, "left"],
  [consent, "below"],
];
const sizes = new Map<HTMLElement, { w: number; h: number }>();

function placePanel(p: HTMLElement, prefer: "left" | "below") {
  const size = sizes.get(p);
  if (p.hidden || !size) return;
  const { w, h } = size;
  const work = primary().work;
  const o = orb.pos;
  let x: number;
  let y: number;
  if (prefer === "left" && o.x - w - 52 > work.x) {
    x = o.x - w - 44;
    y = o.y - h / 2;
  } else {
    x = o.x - w / 2;
    y = o.y + 48;
  }
  x = snap(Math.min(Math.max(x, work.x + 12), work.x + work.w - w - 12));
  y = snap(Math.min(Math.max(y, work.y + 12), work.y + work.h - h - 12));
  p.style.translate = `${x}px ${y}px`;
  // 缩放的原点指向小精灵，面板看起来是从它身上展开的
  p.style.transformOrigin = `${o.x - x}px ${o.y - y}px`;
}

orb.onMove(() => {
  for (const [p, prefer] of PANELS) placePanel(p, prefer);
});

function showPanel(p: HTMLElement, on: boolean, interactive = false) {
  const prefer = PANELS.find(([q]) => q === p)?.[1] ?? "left";
  if (on === !p.hidden) {
    if (on) {
      sizes.set(p, { w: p.offsetWidth, h: p.offsetHeight });
      placePanel(p, prefer);
    }
    return;
  }
  if (on) {
    p.getAnimations().forEach((a) => a.cancel());
    p.hidden = false;
    sizes.set(p, { w: p.offsetWidth, h: p.offsetHeight });
    placePanel(p, prefer);
    play(p, [{ opacity: 0, transform: "scale(.6)" }, { opacity: 1, transform: "scale(1)" }], { spring: SPRING.pop, fill: "none" });
  } else {
    const a = play(p, [{ opacity: 1, transform: "scale(1)" }, { opacity: 0, transform: "scale(.7)" }], { duration: 220, easing: EASE.in });
    a.finished.then(() => (p.hidden = true), () => (p.hidden = true));
  }
  if (interactive) hit(p, on);
}

// ---------------- 悬停菜单 ----------------

let menuTimer: number | undefined;
function openMenu() {
  window.clearTimeout(menuTimer);
  if (state === "consent" || state === "admin" || state === "reboot") return;
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
  orb.setMood("happy");
  await sleep(450);
  ripple(orb.pos, { size: 60, ms: 520, from: 1.8, to: 0.3 });
  orb.show(false);
  await sleep(500);
  if (isTauri) invoke("quit");
});
$("btn-end").addEventListener("click", () => {
  showPanel(menu, false, true);
  if (isTauri) invoke("disconnect");
  else render({ state: "ready", code: lastCode });
});

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
    orb.bump(1);
    bloom(orb.pos, { size: 220, ms: 760, tone: "mint" });
    ripple(orb.pos, { size: 60, ms: 700, tone: "mint" });
  }
  if (isTauri) await invoke("respond", { allow });
  else if (allow || wasAsk !== "consent") render({ state: "connected", techName: $("consent-name").textContent ?? "", code: lastCode });
  else render({ state: "ready", code: lastCode });
}
$("btn-allow").addEventListener("click", () => respond(true));
$("btn-deny").addEventListener("click", () => respond(false));

// ---------------- 状态机 ----------------

function render(s: Status) {
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
    return;
  }
  setHome(null);
  hideConsent();

  switch (s.state) {
    case "connecting":
      orb.setMood("ready");
      showPanel(codeCard, false);
      break;
    case "offline":
      orb.setMood("offline");
      showPanel(codeCard, false);
      if (entered) orb.goTo(home(), "slow");
      break;
    case "ready":
      renderCode(s.code);
      if (isShown()) {
        void collapse().then(() => {
          orb.goTo(home(), "slow");
          if (state === "ready") showPanel(codeCard, true);
        });
      } else if (entered) {
        orb.goTo(home(), "slow");
        showPanel(codeCard, true);
      }
      orb.setMood("ready");
      break;
    case "connected":
      showPanel(codeCard, false);
      if (!isShown()) void expand().then(() => orb.setMood("idle"));
      else {
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
  await sleep(300);
  orb.show(true);
  bloom(c, { size: 380, ms: 950 });
  ripple(c, { size: 90, ms: 950, to: 3 });
  await sleep(1200);
  entered = true;
  // 出场期间状态可能已经变了，按当前状态归位
  if (state === "ready") {
    orb.goTo(home(), "slow");
    await sleep(650);
    if (state === "ready") showPanel(codeCard, true);
  } else if (state === "connecting" || state === "offline") orb.goTo(home(), "slow");
}

// ---------------- 启动 ----------------

async function fetchGeo() {
  let info: OverlayInfo | null = null;
  if (isTauri) {
    try {
      info = await invoke<OverlayInfo>("overlay_info");
    } catch {
      info = null;
    }
  }
  initGeo(info);
}

async function boot() {
  await fetchGeo();
  orb.place(center(primary().work));
  hit(orbHit, true);
  // 可交互区域：布局没变时这次查询几乎不花时间
  window.setInterval(syncHits, 100);

  // 显示器插拔、改分辨率、改缩放后重新铺一遍
  let relayout: number | undefined;
  let lastLayout = 0;
  const onGeoChange = () => {
    window.clearTimeout(relayout);
    relayout = window.setTimeout(async () => {
      if (Date.now() - lastLayout < 2000) return;
      lastLayout = Date.now();
      await fetchGeo();
      relayoutEdge();
      if (entered) orb.goTo(home(), "normal");
    }, 300);
  };
  window.addEventListener("resize", onGeoChange);

  if (isTauri) {
    await listen<Status>("status", (e) => render(e.payload));
    await listen<FxEvent>("fx", (e) => (e.payload.phase === "start" ? fxStart(e.payload) : fxEnd(e.payload)));
    $("admin-tag").hidden = !(await invoke<boolean>("is_admin"));
    render(await invoke<Status>("get_status"));
    void entrance();
  } else {
    document.body.classList.add("demo");
    void entrance();
    runDemo({ render, dock });
  }
}

void boot();
