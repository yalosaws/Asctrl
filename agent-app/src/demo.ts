// 浏览器预览用：假桌面 + 按顺序演示所有动画。
// 地址加 ?state=ready|consent|admin|reboot|connected|offline|connecting 只停在某个状态；
// 控制台里可以用 demoFx("click", {...}) 单独触发。

import { fxEnd, fxStart, type FxEvent } from "./fx";
import type { Pt } from "./geo";
import { sleep } from "./ease";

interface Hooks {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  render: (s: any) => void;
  addActivity: (t: string) => void;
  dock: () => Pt;
}

let seq = 0;

async function sim(tool: string, desc: string, args: Record<string, unknown>, geo: Record<string, unknown>, ms: number, ok = true, text = "", geoEnd: Record<string, unknown> = {}, hooks?: Hooks) {
  const id = `d${++seq}`;
  hooks?.addActivity(desc);
  const start: FxEvent = { phase: "start", id, tool, desc, args, geo };
  fxStart(start);
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
  const a = mk("记事本", W * 0.08, H * 0.1, W * 0.42, H * 0.5);
  const b = mk("网络和 Internet 设置", W * 0.45, H * 0.22, W * 0.4, H * 0.45);
  return { a, b, W, H };
}

const HOSTS = `C:\\Windows\\System32\\drivers\\etc\\hosts（UTF-8，共 24 行）
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
以太网                    Disconnected 0 bps
WLAN                      Up           866.7 Mbps
蓝牙网络连接              Disconnected 3 Mbps`;

export async function runDemo(h: Hooks) {
  const { a, b, W, H } = fakeDesktop();
  const q = new URLSearchParams(location.search);
  const only = q.get("state");
  // 给控制台调试用
  (window as unknown as Record<string, unknown>).demoFx = (tool: string, args = {}, geo = {}, ms = 800, ok = true, text = "", geoEnd = {}) =>
    sim(tool, tool, args, geo, ms, ok, text, geoEnd);

  const T = { techName: "小王", timeoutSec: 120 };
  if (only) {
    const map: Record<string, unknown> = {
      connecting: { state: "connecting" },
      offline: { state: "offline", message: "IO error" },
      ready: { state: "ready", code: "482913" },
      consent: { state: "consent", ...T },
      admin: { state: "admin", ...T },
      reboot: { state: "reboot", ...T },
      connected: { state: "connected", techName: "小王", code: "482913" },
    };
    h.render(map[only] ?? map.ready);
    return;
  }

  const s = (...args: Parameters<typeof sim>) => sim(args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7], h);
  const work = { x: 0, y: 0, w: W, h: H - 48 };

  for (;;) {
    h.render({ state: "connecting" });
    await sleep(1600);
    h.render({ state: "ready", code: "482913" });
    await sleep(4500);
    h.render({ state: "consent", ...T });
    await sleep(3000);
    h.render({ state: "connected", techName: "小王", code: "482913" });
    await sleep(5200);

    await s("screenshot", "查看了你的屏幕", {}, {}, 900, true, "截图", { shot: [0, 0, W, H] });
    await sleep(1600);
    await s("click", "点击了屏幕", { button: "left", count: 1 }, { at: [a[0] + a[2] * 0.5, a[1] + a[3] * 0.4] }, 450);
    await sleep(700);
    await s("type_text", "输入了文字：你好，我来帮你修网络", { text: "你好，我来帮你修网络" }, { at: [a[0] + a[2] * 0.5, a[1] + a[3] * 0.4] }, 1300);
    await sleep(800);
    await s("press_keys", "按下了 ctrl+s", { keys: "ctrl+s" }, {}, 600);
    await sleep(1200);
    await s("run_powershell", "执行命令：Get-NetAdapter", { script: "Get-NetAdapter | Select Name, Status, LinkSpeed" }, {}, 1900, true, NET);
    await sleep(1400);
    await s("run_powershell", "执行命令：ipconfig /flushdns", { script: "ipconfig /flushdns" }, {}, 1200, true, "Windows IP 配置\n\n已成功刷新 DNS 解析缓存。");
    await sleep(3600);
    await s("read_file", "查看了文件 hosts", { path: "C:\\Windows\\System32\\drivers\\etc\\hosts" }, {}, 900, true, HOSTS);
    await sleep(3200);
    await s(
      "edit_file",
      "修改了文件 hosts",
      { path: "C:\\Windows\\System32\\drivers\\etc\\hosts", oldText: "127.0.0.1 ads.example.com\n127.0.0.1 tracker.example.net", newText: "# 已清理" },
      {},
      1300,
    );
    await sleep(2200);
    await s("search_files", "在 ~\\Documents 里搜索 网络", { path: "~\\Documents", pattern: "网络" }, {}, 1400, true,
      "报告.txt:3: 网络老是断开\n笔记.md:12: 家里网络密码在路由器背面\n待办.txt:1: 打电话问宽带网络");
    await sleep(3200);
    await s("list_dir", "查看了文件夹 下载", { path: "~\\Downloads" }, {}, 700, true,
      "C:\\Users\\me\\Downloads（6 项）\n2026-11-01 10:00  <文件夹>  安装包\\\n2026-11-02 11:00  2.1 MB  驱动.zip\n2026-11-03 09:00  80 KB  发票.pdf\n2026-11-04 12:00  1.2 GB  系统镜像.iso\n2026-11-05 13:00  4 KB  readme.txt");
    await sleep(2400);
    await s("focus_window", "最大化了一个窗口", { action: "maximize" }, { rect: [b[0], b[1], b[2], b[3]] }, 600, true, "", {
      rect: [work.x, work.y, work.w, work.h],
    });
    await sleep(2000);
    await s("drag", "拖动了鼠标", {}, { from: [W * 0.3, H * 0.3], to: [W * 0.6, H * 0.55] }, 1100);
    await sleep(1000);
    await s("scroll", "滚动了页面", { amount: 5 }, { at: [W * 0.5, H * 0.5] }, 500);
    await sleep(1200);
    await s("list_windows", "查看了打开的窗口", {}, {}, 300, true, "", { windows: [a, b] });
    await sleep(1400);
    await s("zoom", "放大查看了屏幕的一部分", {}, { from: [b[0] + 40, b[1] + 60], to: [b[0] + 360, b[1] + 220] }, 900);
    await sleep(1800);
    await s("download_file", "取走了文件 发票.pdf", { path: "~\\Downloads\\发票.pdf" }, {}, 900);
    await sleep(2400);
    await s("focus_window", "关闭了一个窗口", { action: "close" }, { rect: [a[0], a[1], a[2], a[3]] }, 400);
    await sleep(2400);
    h.render({ state: "ready", code: "482913" });
    await sleep(5000);
  }
}
