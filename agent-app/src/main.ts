import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type Status =
  | { state: "connecting" }
  | { state: "offline"; message?: string }
  | { state: "ready"; code: string }
  | { state: "consent"; techName: string; timeoutSec: number }
  | { state: "connected"; techName: string; code: string };

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

const PILL: Record<Status["state"], string> = {
  connecting: "连接中",
  offline: "离线",
  ready: "等待连接",
  consent: "有人请求连接",
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

function showConsent(techName: string, timeoutSec: number) {
  $("consent-name").textContent = techName;
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

  if (s.state === "consent") showConsent(s.techName, s.timeoutSec);
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
  hideConsent();
  if (isTauri) await invoke("respond_consent", { allow });
  else render(allow ? { state: "connected", techName: $("consent-name").textContent ?? "", code: lastCode } : { state: "ready", code: lastCode });
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
  render(await invoke<Status>("get_status"));
} else {
  // 浏览器预览：?state=ready|consent|connected|offline|connecting，方便调界面
  const state = (new URLSearchParams(location.search).get("state") ?? "ready") as Status["state"];
  const demo: Record<Status["state"], Status> = {
    connecting: { state: "connecting" },
    offline: { state: "offline", message: "IO error: 由于目标计算机积极拒绝，无法连接。 (os error 10061)" },
    ready: { state: "ready", code: "482913" },
    consent: { state: "consent", techName: "小王", timeoutSec: 120 },
    connected: { state: "connected", techName: "小王", code: "482913" },
  };
  renderCode("482913");
  render(demo[state]);
  if (state === "connected") {
    ["你允许了 小王 连接", "查看了电脑基本信息", "查看了你的屏幕", "查看了系统错误日志",
      "执行命令：Get-Process chrome | Stop-Process", "查看了正在运行的程序"].forEach(addActivity);
  }
}
