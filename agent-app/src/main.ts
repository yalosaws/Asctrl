import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type Status =
  | { state: "connecting" }
  | { state: "offline"; message?: string }
  | { state: "ready"; code: string }
  | { state: "consent"; techName: string; timeoutSec: number }
  | { state: "admin"; techName: string; timeoutSec: number }
  | { state: "reboot"; techName: string; timeoutSec: number }
  | { state: "connected"; techName: string; code: string };

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

const PILL: Record<Status["state"], string> = {
  connecting: "连接中",
  offline: "离线",
  ready: "等待连接",
  consent: "有人请求连接",
  admin: "协助中",
  reboot: "协助中",
  connected: "协助中",
};

const isTauri = "__TAURI_INTERNALS__" in window;
let consentTimer: number | undefined;
let lastCode = "";

function initial(name: string) {
  return (name.trim()[0] ?? "?").toUpperCase();
}

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
    el.style.animationDelay = `${i * 40}ms`;
    box.append(el);
  });
}

type Ask = "consent" | "admin" | "reboot";

const ASK_TEXT: Record<Ask, [string, string]> = {
  consent: ["想远程协助你的电脑", "允许后，对方可以："],
  admin: ["需要管理员权限", "允许后，对方还可以："],
  reboot: ["需要重启你的电脑", "点允许之前请注意："],
};

function showConsent(techName: string, timeoutSec: number, kind: Ask) {
  $("consent-name").textContent = techName;
  [$("consent-ask").textContent, $("consent-sub").textContent] = ASK_TEXT[kind];
  $("perm-consent").hidden = kind !== "consent";
  $("perm-admin").hidden = kind !== "admin";
  $("admin-note").hidden = kind !== "admin";
  $("perm-reboot").hidden = kind !== "reboot";
  $("reboot-note").hidden = kind !== "reboot";
  $("consent-avatar").textContent = initial(techName);
  $("consent").hidden = false;
  $("btn-allow").focus();

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
  $("consent").hidden = true;
}

function render(s: Status) {
  document.body.dataset.state = s.state;
  $("pill-text").textContent = PILL[s.state];

  if (s.state === "consent" || s.state === "admin" || s.state === "reboot") showConsent(s.techName, s.timeoutSec, s.state);
  else hideConsent();

  if (s.state === "ready") renderCode(s.code);
  if (s.state === "offline") $("offline-detail").textContent = s.message ?? "";
  if (s.state === "connected") {
    $("tech-name").textContent = s.techName;
    $("avatar").textContent = initial(s.techName);
  }
}

function addActivity(text: string) {
  const li = document.createElement("li");
  const time = document.createElement("time");
  time.textContent = new Date().toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
  const body = document.createElement("div");

  const cmdPrefix = "执行命令：";
  if (text.startsWith(cmdPrefix)) {
    body.textContent = "执行了一条命令";
    const code = document.createElement("code");
    code.className = "cmd";
    code.textContent = text.slice(cmdPrefix.length);
    body.append(code);
  } else {
    body.textContent = text;
  }

  li.append(time, body);
  const log = $("log");
  log.append(li);
  log.scrollTop = log.scrollHeight;
  $("log-empty").hidden = true;
}

async function respond(allow: boolean) {
  const wasAdmin = document.body.dataset.state === "admin" || document.body.dataset.state === "reboot";
  hideConsent();
  if (isTauri) await invoke("respond", { allow });
  else if (allow || wasAdmin) render({ state: "connected", techName: $("consent-name").textContent ?? "", code: lastCode });
  else render({ state: "ready", code: lastCode });
}

$("btn-allow").addEventListener("click", () => respond(true));
$("btn-deny").addEventListener("click", () => respond(false));
$("btn-end").addEventListener("click", () => {
  if (isTauri) invoke("disconnect");
  else render({ state: "ready", code: lastCode });
});
document.addEventListener("keydown", (e) => {
  if ($("consent").hidden) return;
  if (e.key === "Escape") respond(false);
});

if (isTauri) {
  await listen<Status>("status", (e) => render(e.payload));
  await listen<{ text: string }>("activity", (e) => addActivity(e.payload.text));
  $("admin-tag").hidden = !(await invoke<boolean>("is_admin"));
  render(await invoke<Status>("get_status"));
} else {
  // 浏览器预览：?state=ready|consent|admin|reboot|connected|offline|connecting，方便调界面
  const state = (new URLSearchParams(location.search).get("state") ?? "ready") as Status["state"];
  const demo: Record<Status["state"], Status> = {
    connecting: { state: "connecting" },
    offline: { state: "offline", message: "IO error: 由于目标计算机积极拒绝，无法连接。 (os error 10061)" },
    ready: { state: "ready", code: "482913" },
    consent: { state: "consent", techName: "小王", timeoutSec: 120 },
    admin: { state: "admin", techName: "小王", timeoutSec: 120 },
    reboot: { state: "reboot", techName: "小王", timeoutSec: 120 },
    connected: { state: "connected", techName: "小王", code: "482913" },
  };
  renderCode("482913");
  render(demo[state]);
  if (state === "connected" || state === "admin" || state === "reboot") {
    ["你允许了 小王 连接", "查看了电脑基本信息", "查看了你的屏幕", "点击了屏幕", "输入了文字：notepad",
      "按下了 enter", "执行命令：Get-Process chrome | Stop-Process", "请求管理员权限"].forEach(addActivity);
  }
}
