// The page channel in Asctrl's own browser, on a real desktop.
// Usage: node test/page.ts   (opens Asctrl's browser and Calculator for about 20 s; leave the computer alone)
//
// Opens test/web/p3.html in Asctrl's browser, covers the browser with Calculator (maximized), and then works the
// page without bringing it forward: controls named by their text, label, placeholder or aria-label, one that
// only takes real (trusted) input, one far below the fold, one that changes the page a moment later, a list,
// the whole page's text, two controls with the same name (left to the usual lookup), and a link to another
// page (waited for until it has loaded). The mouse must not move and Calculator must stay in front.

import { fixtureDirectory } from "./fixtures.ts";
import { execFileSync } from "node:child_process";
import { rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { Suite, root } from "./desk.ts";
import type { ObserveData } from "../src/shared/protocol.ts";

const pagesDir = fixtureDirectory(path.join(tmpdir(), "asctrl-duplicate-pages-"));
process.env.ASCTRL_BROWSER_PROFILE = path.join(pagesDir, "browser");
const s = new Suite("Page");
const { tech, say } = s;
const first = (s?: string) => (s ?? "").split("\n")[0];
const cursor = () => execFileSync("powershell.exe", ["-NoProfile", "-Command", "Add-Type -AssemblyName System.Windows.Forms; $p = [System.Windows.Forms.Cursor]::Position; \"$($p.X),$($p.Y)\""]).toString().trim();

let C: number | undefined;
let W: number | undefined;
s.after(async () => {
  // Restored before it closes: Calculator opens next time the way it was closed, and other tests expect its usual size
  if (C) await tech.call("focus_window", { window: C, action: "restore", after: "none" }).catch(() => undefined);
  if (C) await tech.call("focus_window", { window: C, action: "close", after: "none" }).catch(() => undefined);
  if (W) await tech.call("focus_window", { window: W, action: "close", after: "none" }).catch(() => undefined);
  rmSync(pagesDir, { recursive: true, force: true, maxRetries: 8, retryDelay: 250 });
});

await s.run(async () => {
  await tech.connectLocalDriver({ launch: false, waitMs: 30_000 });
  const url = pathToFileURL(path.join(root, "test/web/p3.html")).href;
  const coldPages = ["ColdOne", "ColdTwo"].map((marker) => {
    const file = path.join(pagesDir, marker + ".html");
    writeFileSync(file, "<title>" + marker + "</title><p>" + marker + "</p>");
    return pathToFileURL(file).href;
  });
  const cold = await Promise.all(coldPages.map((target) => tech.call("open_app", { target, browser: "asctrl", after: "none" })));
  say(cold.every((r, i) => r.ok && r.text?.includes(coldPages[i])), "both requests to a cold browser returned their own page");
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
  // Date and time fields take no typed text: their value is set, as their picker would, from the way people write it
  await act("a date field, from a date as people write it", "set_value", { target: "Arrival date", value: "24 December 2026" }, /arrive 2026-12-24/);
  await act("a time field, typed to", "type_text", { target: "Arrival time", text: "8:00 PM" }, /at 20:00/);
  await act("a change that comes a moment later", "click", { target: "Late change" }, /late done/);
  await act("a field inside an iframe", "set_value", { target: "Ticket subject", value: "Printer" }, /Asctrl Page Test/);
  await act("a list inside an iframe", "set_value", { target: "Priority", value: "High" }, /Asctrl Page Test/);
  await act("a button inside an iframe", "click", { target: "Open ticket" }, /ticket Printer High/);

  const all = await page();
  say(/END-OF-PAGE-MARKER/.test(all), `read_text read the whole page, below the fold too (${all.length} characters)`);

  const d = await tech.call("click", { window: W, target: "Duplicate", after: "none" });
  say((d.data as { channel?: string } | undefined)?.channel !== "page", `two buttons with the same name are left to the usual lookup: ${first(d.text ?? d.error)}`);

  const front = ((await tech.call("observe", { max: 1 })).data as ObserveData).windows.find((x) => x.front)?.handle;
  const moved = cursor();
  say(front === C && moved === mouse, `Calculator stayed in front and the mouse stayed at ${mouse} (now ${moved})`);

  // A reference (from observe) in the covered browser: the click goes through the page, nothing comes forward
  const seen = (await tech.call("observe", { window: W, query: "Duplicate" })).text ?? "";
  const ref = /\[(e\d+)\][^\n]*Duplicate/.exec(seen)?.[1];
  const byRef = await tech.call("click", { window: W, target: ref ?? "e0", after: "none" });
  const front2 = ((await tech.call("observe", { max: 1 })).data as ObserveData).windows.find((x) => x.front)?.handle;
  say(!!ref && byRef.ok && (byRef.data as { channel?: string } | undefined)?.channel === "page" && /duplicate/.test(await page()) && front2 === C, `a reference (${ref}) clicked through the page while covered, Calculator still in front: ${first(byRef.text ?? byRef.error)}`);

  const n = await tech.call("click", { window: W, target: "Next page", after: "none" });
  say(n.ok && /It loaded "Second Page"/.test(n.text ?? ""), `a link, waited for until the next page loaded: ${first(n.text ?? n.error)}`);
  for (const marker of ["Alpha", "Beta"]) {
    const file = path.join(pagesDir, marker + ".html");
    writeFileSync(file, "<title>Duplicate fixture title</title><p>Fixture" + marker + "</p><label>Marker <input oninput='document.querySelector(\"output\").textContent=this.value'></label><output></output>");
    const opened = await tech.call("open_app", { target: pathToFileURL(file).href, browser: "asctrl", after: "none" });
    if (!opened.ok) throw new Error("duplicate tab fixture failed to open");
  }
  say(/FixtureBeta/.test(await page()), "newly active tab supplies its own text");
  await tech.call("press_keys", { window: W, keys: "ctrl+shift+tab", after: "none" });
  await new Promise((r) => setTimeout(r, 300));
  const switched = await tech.call("read_text", { window: W });
  const set = await tech.call("set_value", { window: W, target: "Marker", value: "OnlyOnAlpha", after: "none" });
  const alpha = await page();
  await tech.call("press_keys", { window: W, keys: "ctrl+tab", after: "none" });
  await new Promise((r) => setTimeout(r, 300));
  const beta = await page();
  say(set.ok && /FixtureAlpha/.test(alpha) && /OnlyOnAlpha/.test(alpha) && /FixtureBeta/.test(beta) && !/OnlyOnAlpha/.test(beta), "an edit affected only the active same-title form");
  say(/FixtureAlpha/.test(switched.text ?? "") && (switched.data as { channel?: string })?.channel === "page", "switching between equal titles reads the visible tab");
});
