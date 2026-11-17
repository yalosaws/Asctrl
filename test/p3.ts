// P3, on a real desktop: the page channel in Asctrl's own browser.
// Usage: node test/p3.ts   (opens Asctrl's browser and Calculator for about 20 s; leave the computer alone)
//
// Opens test/web/p3.html in Asctrl's browser, covers the browser with Calculator (maximized), and then works the
// page without bringing it forward: controls named by their text, label, placeholder or aria-label, one that
// only takes real (trusted) input, one far below the fold, one that changes the page a moment later, a list,
// the whole page's text, two controls with the same name (left to the usual lookup), and a link to another
// page (waited for until it has loaded). The mouse must not move and Calculator must stay in front.

import { execFileSync, spawn } from "node:child_process";
import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { TechClient } from "../src/console/tech-client.ts";
import type { ObserveData } from "../src/shared/protocol.ts";

const root = path.resolve(import.meta.dirname, "..");
const exe = path.join(root, "agent-app/src-tauri/target/release/asctrl-agent.exe");
const localFile = path.join(tmpdir(), `asctrl-p3-local-${process.pid}.json`);
process.env.ASCTRL_LOCAL_FILE = localFile;
const agent = spawn(exe, ["--relay", "ws://127.0.0.1:1"], { stdio: ["ignore", "ignore", "inherit"] });
const tech = new TechClient("ws://127.0.0.1:1", "P3Test");
let failures = 0;
const say = (ok: boolean, line: string) => {
  if (!ok) failures++;
  console.log(`${ok ? "✓" : "✗"} ${line}`);
};
const first = (s?: string) => (s ?? "").split("\n")[0];
const cursor = () => execFileSync("powershell.exe", ["-NoProfile", "-Command", "Add-Type -AssemblyName System.Windows.Forms; $p = [System.Windows.Forms.Cursor]::Position; \"$($p.X),$($p.Y)\""]).toString().trim();

let C: number | undefined;
let W: number | undefined;
try {
  await tech.connectLocalDriver({ launch: false, waitMs: 30_000 });
  const url = pathToFileURL(path.join(root, "test/web/p3.html")).href;
  const t0 = performance.now();
  const o = await tech.call("open_app", { target: url, browser: "asctrl", after: "none" });
  W = (o.data as { handle?: number } | undefined)?.handle;
  say(o.ok && !!W, `opened in Asctrl's browser in ${Math.round(performance.now() - t0)} ms: ${first(o.text ?? o.error)}`);

  await tech.call("open_app", { target: "calc", after: "none" });
  await tech.call("wait_for", { appears: "计算器|Calculator", timeoutSec: 10, after: "none" });
  C = ((await tech.call("observe", { max: 1 })).data as ObserveData).windows.find((x) => /^(计算器|Calculator)$/.test(x.title))?.handle;
  await tech.call("focus_window", { window: C, action: "maximize", after: "none" });
  await tech.call("wait_for", { stable: true, timeoutSec: 3, after: "none" });
  const mouse = cursor();

  const page = async () => (await tech.call("read_text", { window: W })).text ?? "";
  const act = async (label: string, tool: string, args: Record<string, unknown>, shows: RegExp) => {
    const t = performance.now();
    const r = await tech.call(tool as "click", { window: W, after: "none", ...args });
    const ms = Math.round(performance.now() - t);
    const via = (r.data as { channel?: string } | undefined)?.channel === "page";
    const text = await page();
    say(r.ok && via && shows.test(text), `${label} (${ms} ms): ${first(r.text ?? r.error)}`);
  };
  await act("a button by its text", "click", { target: "Save" }, /saved 1/);
  await act("text that only has an onclick", "click", { target: "Fancy action" }, /fancy done/);
  await act("a button that only takes real input", "click", { target: "Trusted only" }, /trusted click/);
  await act("a button far below the fold", "click", { target: "Deep button" }, /deep pressed/);
  await act("a field by its placeholder", "type_text", { target: "Search here", text: "hello" }, /Asctrl Page Test/);
  await act("a field by its label", "set_value", { target: "Email address", value: "a@b.c" }, /Asctrl Page Test/);
  await act("a list by its aria-label", "set_value", { target: "Size", value: "Large" }, /size Large/);
  await act("a change that comes a moment later", "click", { target: "Late change" }, /late done/);

  const all = await page();
  say(/END-OF-PAGE-MARKER/.test(all), `read_text read the whole page, below the fold too (${all.length} characters)`);

  const d = await tech.call("click", { window: W, target: "Duplicate", after: "none" });
  say((d.data as { channel?: string } | undefined)?.channel !== "page", `two buttons with the same name are left to the usual lookup: ${first(d.text ?? d.error)}`);

  const front = ((await tech.call("observe", { max: 1 })).data as ObserveData).windows.find((x) => x.front)?.handle;
  const moved = cursor();
  say(front === C && moved === mouse, `Calculator stayed in front and the mouse stayed at ${mouse} (now ${moved})`);

  const n = await tech.call("click", { window: W, target: "Next page", after: "none" });
  say(n.ok && /It loaded "Second Page"/.test(n.text ?? ""), `a link, waited for until the next page loaded: ${first(n.text ?? n.error)}`);
} catch (e) {
  console.error(e);
  failures++;
} finally {
  // Restored before it closes: Calculator opens next time the way it was closed, and other tests expect its usual size
  if (C) await tech.call("focus_window", { window: C, action: "restore", after: "none" }).catch(() => undefined);
  if (C) await tech.call("focus_window", { window: C, action: "close", after: "none" }).catch(() => undefined);
  if (W) await tech.call("focus_window", { window: W, action: "close", after: "none" }).catch(() => undefined);
  console.log(failures ? `\n${failures} check(s) failed` : "\nAll checks passed");
  process.exitCode = failures ? 1 : 0;
  tech.disconnect();
  agent.kill();
  rmSync(localFile, { force: true });
  setTimeout(() => process.exit(), 300);
}
