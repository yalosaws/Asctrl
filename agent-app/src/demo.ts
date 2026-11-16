// Browser preview: a fake desktop plus every animation played in sequence.
// Add ?state=ready|admin|reboot|connected|offline|connecting to the URL to stop at one state;
// in the console, demoFx("click", {...}) triggers a single effect.

import { fxEnd, fxStart, fxStep, type FxEvent } from "./fx";
import { sleep } from "./motion";

interface Hooks {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  render: (s: any) => void;
}

let seq = 0;

async function sim(tool: string, args: Record<string, unknown>, geo: Record<string, unknown>, ms: number, ok = true, text = "", geoEnd: Record<string, unknown> = {}) {
  const id = `d${++seq}`;
  const start: FxEvent = { phase: "start", id, tool, args, geo };
  fxStart(start);
  // The driver sends what scrolls there as a step, once it has found out
  if (tool === "scroll") fxStep({ phase: "step", id, tool, index: 0, ok: true, geo });
  await sleep(ms);
  fxEnd({ phase: "end", id, tool, ok, text, geo: geoEnd });
}

function fakeDesktop() {
  const W = window.innerWidth;
  const H = window.innerHeight;
  const mk = (title: string, x: number, y: number, w: number, h: number) => {
    const d = document.createElement("div");
    d.className = "fake-win";
    d.dataset.title = title;
    Object.assign(d.style, { left: `${x}px`, top: `${y}px`, width: `${w}px`, height: `${h}px` });
    document.body.prepend(d);
    return [x, y, w, h];
  };
  const tb = document.createElement("div");
  tb.className = "fake-taskbar";
  document.body.prepend(tb);
  const a = mk("Notepad", W * 0.08, H * 0.1, W * 0.42, H * 0.5);
  const b = mk("Network & Internet settings", W * 0.45, H * 0.22, W * 0.4, H * 0.45);
  return { a, b, W, H };
}

const HOSTS = `C:\\Windows\\System32\\drivers\\etc\\hosts (UTF-8, 24 lines)
     1\t# Copyright (c) 1993-2009 Microsoft Corp.
     2\t#
     3\t# This is a sample HOSTS file used by Microsoft TCP/IP for Windows.
     4\t#
     5\t# localhost name resolution is handled within DNS itself.
     6\t#\t127.0.0.1       localhost
     7\t#\t::1             localhost
     8\t127.0.0.1 ads.example.com
     9\t127.0.0.1 tracker.example.net`;

const NET = `Name                      Status       LinkSpeed
----                      ------       ---------
Ethernet                  Disconnected 0 bps
WLAN                      Up           866.7 Mbps
Bluetooth Network         Disconnected 3 Mbps`;

export async function runDemo(h: Hooks) {
  const { a, b, W, H } = fakeDesktop();
  const q = new URLSearchParams(location.search);
  const only = q.get("state");
  // For debugging from the console
  (window as unknown as Record<string, unknown>).demoFx = (tool: string, args = {}, geo = {}, ms = 800, ok = true, text = "", geoEnd = {}) =>
    sim(tool, args, geo, ms, ok, text, geoEnd);

  const T = { techName: "Alex", timeoutSec: 120 };
  if (only) {
    const map: Record<string, unknown> = {
      connecting: { state: "connecting" },
      offline: { state: "offline", message: "IO error" },
      ready: { state: "ready", code: "482913" },
      admin: { state: "admin", ...T },
      reboot: { state: "reboot", ...T },
      connected: { state: "connected", techName: "Alex", code: "482913" },
    };
    h.render(map[only] ?? map.ready);
    return;
  }

  const s = sim;
  const work = { x: 0, y: 0, w: W, h: H - 48 };

  for (;;) {
    h.render({ state: "connecting" });
    await sleep(1600);
    h.render({ state: "ready", code: "482913" });
    await sleep(4500);
    h.render({ state: "connected", techName: "Alex", code: "482913" });
    await sleep(5200);

    await s("screenshot", {}, {}, 900, true, "Screenshot", { shot: [0, 0, W, H] });
    await sleep(1600);
    await s("click", { button: "left", count: 1 }, { at: [a[0] + a[2] * 0.5, a[1] + a[3] * 0.4] }, 450);
    await sleep(700);
    await s("type_text", { text: "Hi, I'm here to fix your network" }, { at: [a[0] + a[2] * 0.5, a[1] + a[3] * 0.4] }, 1300);
    await sleep(800);
    await s("press_keys", { keys: "ctrl+s" }, {}, 600);
    await sleep(1200);
    // Several controls at once: the orb splits into one pointer each, the third one fails
    const f = (x: number, y: number) => [b[0] + b[2] * x, b[1] + b[3] * y];
    await s(
      "element_action",
      { actions: [{ id: 3 }, { id: 4, action: "toggle" }, { id: 7 }, { id: 9, action: "set_value", value: "8.8.8.8" }] },
      { targets: [f(0.25, 0.3), f(0.7, 0.3), f(0.3, 0.7), f(0.75, 0.75)], labels: ["Wi-Fi", "Metered connection", "Advanced", "Preferred DNS"] },
      700,
      false,
      "1. ✓ Clicked [3]\n2. ✓ [4] is now checked\n3. ✗ [7] can't be clicked without the mouse\n4. ✓ Set [9] to \"8.8.8.8\"",
    );
    await sleep(1600);
    await s("run_powershell", { script: "Get-NetAdapter | Select Name, Status, LinkSpeed" }, {}, 1900, true, NET);
    await sleep(1400);
    await s("wait_for", { text: "Connected" }, {}, 4200, true, "Found \"Connected\" after 4.2 s");
    await sleep(7000);
    await s("run_powershell", { script: "ipconfig /flushdns" }, {}, 1200, true, "Windows IP Configuration\n\nSuccessfully flushed the DNS Resolver Cache.");
    await sleep(3600);
    await s("read_file", { path: "C:\\Windows\\System32\\drivers\\etc\\hosts" }, {}, 900, true, HOSTS);
    await sleep(3200);
    await s(
      "edit_file",
      { path: "C:\\Windows\\System32\\drivers\\etc\\hosts", oldText: "127.0.0.1 ads.example.com\n127.0.0.1 tracker.example.net", newText: "# cleaned up" },
      {},
      1300,
    );
    await sleep(2200);
    await s("search_files", { path: "~\\Documents", pattern: "network" }, {}, 1400, true,
      "report.txt:3: the network keeps dropping\nnotes.md:12: home network password is on the back of the router\ntodo.txt:1: call the ISP about the network");
    await sleep(3200);
    await s("list_files", { path: "~\\Downloads" }, {}, 700, true,
      "C:\\Users\\me\\Downloads (6 items)\n2026-11-01 10:00  <DIR>  installers\\\n2026-11-02 11:00  2.1 MB  driver.zip\n2026-11-03 09:00  80 KB  invoice.pdf\n2026-11-04 12:00  1.2 GB  system-image.iso\n2026-11-05 13:00  4 KB  readme.txt");
    await sleep(2400);
    await s("focus_window", { action: "maximize" }, { rect: [b[0], b[1], b[2], b[3]] }, 600, true, "", {
      rect: [work.x, work.y, work.w, work.h],
    });
    await sleep(2000);
    await s("drag", {}, { from: [W * 0.3, H * 0.3], to: [W * 0.6, H * 0.55] }, 1100);
    await sleep(1000);
    await s("scroll", { amount: 5 }, { at: [W * 0.5, H * 0.5] }, 500);
    await sleep(1200);
    await s("screenshot", { x0: 0 }, { from: [b[0] + 40, b[1] + 60], to: [b[0] + 360, b[1] + 220] }, 900);
    await sleep(1800);
    await s("download_file", { path: "~\\Downloads\\invoice.pdf" }, {}, 900);
    await sleep(2400);
    await s("focus_window", { action: "close" }, { rect: [a[0], a[1], a[2], a[3]] }, 400);
    await sleep(2400);
    h.render({ state: "ready", code: "482913" });
    await sleep(5000);
  }
}
