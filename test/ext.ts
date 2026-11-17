// The person's own browser, through Asctrl's extension, on a real desktop.
// Usage: node test/ext.ts --edge   (about 90 s; leave the computer alone; see below for why Edge, and why only when asked)
//
// Plays the person's browser with a Chrome (else Edge) of its own: a fresh profile with the extension loaded from
// browser-extension/ on the command line, and none of the flags Asctrl's own browser starts with (so a covered
// window is throttled as in anyone's everyday browser). Pages come from a local web server: the extension may not
// touch file:// pages unless the person allows it.
//
// 1. The extension reaches the driver (the browser starts the relay, the relay finds the driver's pipe).
// 2. open_app browser: "user" opens a page in a new tab there.
// 3. Clicking, typing, choosing, inside an iframe too, through the page: the mouse does not move.
// 4. The same while Calculator covers the browser, and how long that takes.
// 5. A tab the person already had open is worked too (found by its window).
// 6. After Asctrl let go of a tab it stopped working (the debugging bar goes away), it takes it up again.
// 7. A form in an iframe from another site (run by the browser in a process of its own): in this browser and in
//    Asctrl's own, through the page, the mouse not moving.

import { execFileSync, spawn } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import http from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import path from "node:path";
import { TechClient } from "../src/console/tech-client.ts";
import type { ObserveData } from "../src/shared/protocol.ts";

const root = path.resolve(import.meta.dirname, "..");
const exe = path.join(root, "agent-app/src-tauri/target/release/asctrl-agent.exe");
const localFile = path.join(tmpdir(), `asctrl-ext-local-${process.pid}.json`);
process.env.ASCTRL_LOCAL_FILE = localFile;
// Programs that work are kept as skills by themselves: in a file of the test's own, not the computer's
process.env.ASCTRL_SKILLS_FILE ??= path.join(tmpdir(), `asctrl-test-skills-${process.pid}.json`);
// Its driver acts without asking the person first (L5's check has a test of its own: test/l5.ts)
process.env.ASCTRL_CONFIRM ??= "off";
// A pipe of its own, so a driver already running keeps the person's; the browser passes it on to the relay
process.env.ASCTRL_BROWSER_PIPE = `asctrl-browser-test-${process.pid}`;
let failures = 0;
const say = (ok: boolean, line: string) => {
  if (!ok) failures++;
  console.log(`${ok ? "✓" : "✗"} ${line}`);
};
const first = (s?: string) => (s ?? "").split("\n")[0];
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const cursor = () => execFileSync("powershell.exe", ["-NoProfile", "-Command", "Add-Type -AssemblyName System.Windows.Forms; $p = [System.Windows.Forms.Cursor]::Position; \"$($p.X),$($p.Y)\""]).toString().trim();

// The test pages, over http
const web = http.createServer((req, res) => {
  const f = path.join(root, "test/web", path.basename(new URL(req.url ?? "/", "http://x").pathname) || "p3.html");
  if (!existsSync(f)) return void res.writeHead(404).end();
  res.writeHead(200, { "content-type": "text/html; charset=utf-8" }).end(readFileSync(f));
});
await new Promise<void>((r) => web.listen(0, "127.0.0.1", r));
const base = `http://127.0.0.1:${(web.address() as AddressInfo).port}`;

// Chrome (since 137) no longer loads an extension from the command line, so this test needs Edge, which does. But
// a fresh Edge profile signs itself in to the person's Microsoft account (Windows passes it on) and warns about
// extensions in developer mode: it is used only when asked for (node test/ext.ts --edge)
const useEdge = process.argv.includes("--edge");
const browsers = [
  [`${process.env.ProgramFiles}\\Google\\Chrome\\Application\\chrome.exe`, "Chrome"],
  [`${process.env.LOCALAPPDATA}\\Google\\Chrome\\Application\\chrome.exe`, "Chrome"],
  ...(useEdge ? [[`${process.env["ProgramFiles(x86)"]}\\Microsoft\\Edge\\Application\\msedge.exe`, "Edge"]] : []),
].filter(([p]) => existsSync(p));
const profile = mkdtempSync(path.join(tmpdir(), "asctrl-ext-profile-"));
const agent = spawn(exe, ["--relay", "ws://127.0.0.1:1"], { stdio: ["ignore", "ignore", "inherit"] });
const tech = new TechClient("ws://127.0.0.1:1", "ExtTest");
let browser: ReturnType<typeof spawn> | undefined;
let C: number | undefined;
try {
  await tech.connectLocalDriver({ launch: false, waitMs: 30_000 });
  // 1. Start a browser with the extension; the driver registered the relay when it started
  let opened: Awaited<ReturnType<typeof tech.call>> | undefined;
  let name = "";
  for (const [bin, n] of browsers) {
    browser = spawn(bin, [`--user-data-dir=${profile}`, `--load-extension=${path.join(root, "browser-extension")}`, "--disable-features=DisableLoadExtensionCommandLineSwitch",
      "--no-first-run", "--no-default-browser-check", "--new-window", `${base}/p3b.html`], { stdio: "ignore" });
    const t0 = performance.now();
    while (performance.now() - t0 < 20_000) {
      opened = await tech.call("open_app", { target: `${base}/p3.html`, browser: "user", after: "none" });
      if (opened.ok) break;
      await sleep(1000);
    }
    name = n;
    if (opened?.ok) {
      say(true, `the extension in ${n} reached the driver within ${Math.round(performance.now() - t0)} ms`);
      // A fresh profile opens a tab of its own some seconds in (Edge: a welcome page) and puts it in front:
      // let that happen, then open the test page again, in front
      await sleep(Math.max(0, 18_000 - (performance.now() - t0)));
      opened = await tech.call("open_app", { target: `${base}/p3.html`, browser: "user", after: "none" });
      break;
    }
    browser.kill();
  }
  if (!opened?.ok) throw new Error(`no browser connected through the extension: ${opened?.error}`);
  // 2.
  const W = (opened.data as { handle?: number } | undefined)?.handle;
  say(!!W && /person's own browser/.test(opened.text ?? ""), `browser: "user" opened the page in a new tab of ${name}: ${first(opened.text)}`);

  // 3. Through the page, in front
  const page = async () => (await tech.call("read_text", { window: W })).text ?? "";
  const mouse = cursor();
  const act = async (label: string, tool: string, args: Record<string, unknown>, shows: RegExp) => {
    const t = performance.now();
    const r = await tech.call(tool as "click", { window: W, after: "none", ...args });
    const ms = Math.round(performance.now() - t);
    const via = (r.data as { channel?: string } | undefined)?.channel === "page";
    say(r.ok && via && shows.test(await page()), `${label} (${ms} ms): ${first(r.text ?? r.error)}`);
    return ms;
  };
  await act("a button", "click", { target: "Save" }, /saved 1/);
  await act("a field by its label", "set_value", { target: "Email address", value: "a@b.c" }, /Asctrl Page Test/);
  await act("a list", "set_value", { target: "Size", value: "Large" }, /size Large/);
  await act("a field inside an iframe", "set_value", { target: "Ticket subject", value: "Printer" }, /Asctrl Page Test/);
  await act("a button inside an iframe", "click", { target: "Open ticket" }, /ticket Printer Low/);
  say(cursor() === mouse, `the mouse stayed at ${mouse}`);

  // 4. Covered by Calculator
  await tech.call("open_app", { target: "calc", after: "none" });
  await tech.call("wait_for", { appears: "计算器|Calculator", timeoutSec: 10, after: "none" });
  C = ((await tech.call("observe", { max: 1 })).data as ObserveData).windows.find((x) => /^(计算器|Calculator)$/.test(x.title))?.handle;
  await tech.call("focus_window", { window: C, action: "maximize", after: "none" });
  await sleep(1500);
  const covered: number[] = [];
  for (let i = 2; i <= 4; i++) covered.push(await act(`a button while Calculator covers the browser (${i})`, "click", { target: "Save" }, new RegExp(`saved ${i}`)));
  const front = ((await tech.call("observe", { max: 1 })).data as ObserveData).windows.find((x) => x.front)?.handle;
  say(front === C && Math.max(...covered) < 1500, `Calculator stayed in front, and covered clicks took ${covered.join(", ")} ms (each under 1.5 s: the tab kept drawing)`);
  await tech.call("focus_window", { window: C, action: "restore", after: "none" });
  await tech.call("focus_window", { window: C, action: "close", after: "none" });
  C = undefined;

  // 5. The person switches to the tab the browser started with: the window now shows it, and that is the one worked
  await tech.call("focus_window", { window: W, after: "none" });
  await tech.call("press_keys", { keys: "ctrl+1", window: W, after: "none" });
  await sleep(800);
  const r = await tech.call("read_text", { window: W });
  say(r.ok && /Second Page/.test(r.text ?? "") && (r.data as { channel?: string } | undefined)?.channel === "page", `the tab the person switched to (one Asctrl did not open) is read through the page: ${first(r.text ?? r.error)}`);
  await tech.call("press_keys", { keys: "ctrl+2", window: W, after: "none" });
  await sleep(800);

  // 6. Let go after 15 s without work: the extension says which tabs it holds
  await sleep(22_000);
  const held = await tech.call("read_text", { window: W });
  say(held.ok, `after a pause the tab is worked again (attached anew): ${first(held.text ?? held.error)}`);

  // 7. A form in an iframe from another site (its own process), in this browser and in Asctrl's own
  for (const which of ["user", "asctrl"] as const) {
    const o = await tech.call("open_app", { target: `${base}/cross.html`, browser: which, after: "none" });
    const X = (o.data as { handle?: number } | undefined)?.handle;
    await sleep(1500);
    const where = which === "user" ? `${name} (the extension)` : "Asctrl's own browser";
    const steps: [string, string, Record<string, unknown>][] = [
      ["a field", "set_value", { target: "Card holder", value: "Kim Lee" }],
      ["a list", "set_value", { target: "Plan", value: "Premium" }],
      ["a button", "click", { target: "Confirm order" }],
    ];
    let all = true;
    const ms: number[] = [];
    for (const [label, tool, args] of steps) {
      const t = performance.now();
      const r = await tech.call(tool as "click", { window: X, after: "none", ...args });
      ms.push(Math.round(performance.now() - t));
      const via = (r.data as { channel?: string } | undefined)?.channel === "page";
      if (!r.ok || !via) {
        all = false;
        say(false, `${where}: ${label} in a frame of another site: ${first(r.text ?? r.error)}`);
      }
    }
    const shown = (await tech.call("read_text", { window: X })).text ?? "";
    say(all && /Order for Kim Lee \(Premium\)/.test(shown) && !/untrusted/.test(shown), `${where}: a field, a list and a button in a frame of another site, through the page (${ms.join(", ")} ms); the frame's text is read with the page's: ${/Order for[^\n]*/.exec(shown)?.[0] ?? "no order shown"}`);
    say(cursor() === mouse, `${where}: the mouse stayed put`);
    if (X && which === "asctrl") await tech.call("focus_window", { window: X, action: "close", after: "none" });
  }
} catch (e) {
  console.error(e);
  failures++;
} finally {
  if (C) await tech.call("focus_window", { window: C, action: "close", after: "none" }).catch(() => undefined);
  console.log(failures ? `\n${failures} check(s) failed` : "\nAll checks passed");
  process.exitCode = failures ? 1 : 0;
  tech.disconnect();
  browser?.kill();
  agent.kill();
  web.close();
  await sleep(1500);
  rmSync(profile, { recursive: true, force: true });
  rmSync(localFile, { force: true });
  setTimeout(() => process.exit(), 300);
}
