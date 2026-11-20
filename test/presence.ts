// Presence, on a real desktop: the person at the computer comes first, and actions that need no mouse go on beside them.
// Usage: node test/presence.ts   (opens the bench window and Asctrl's browser for about 40 s; leave the computer alone:
// the test plays the person itself, by moving the mouse with test/jiggle.ps1)
//
// 1. Someone moving the mouse: a mouse click waits until they have stopped, and says so.
// 2. Meanwhile a click in Asctrl's browser (through the page) goes ahead at once.
// 3. Someone who never stops: after ASCTRL_YIELD_MAX (4 s here) the click gives up, and nothing was clicked.
// 4. Two programs at once, one in the browser and one in the bench window, take less time than one after another.
// 5. A button UI Automation can press goes ahead in the background while the person moves the mouse.

import { owned } from "./fixtures.ts";
import { spawn } from "node:child_process";
import { existsSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { Suite, root } from "./desk.ts";
import type { ObserveData, ProgramData } from "../src/shared/protocol.ts";

const s = new Suite("Presence", { bench: true, env: { ASCTRL_YIELD_MAX: "4" } });
const { tech, say } = s;
const first = (s?: string) => (s ?? "").split("\n")[0];
const prog = (r: { data?: unknown }) => (r.data as ProgramData | undefined)?.program;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Someone moves the mouse for `seconds`: resolves once they have started, with a promise for when they stop
 *  (inside an object: an async function returning the promise itself would wait for it) */
async function person(seconds: number): Promise<{ stopped: Promise<void> }> {
  const signal = path.join(tmpdir(), `asctrl-p6-person-${process.pid}-${Date.now()}`);
  const p = owned(() => spawn("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(root, "test/jiggle.ps1"), "-Seconds", String(seconds), "-Signal", signal], { stdio: "ignore", windowsHide: true }));
  const done = new Promise<void>((r) => p.on("exit", () => r()));
  while (!existsSync(signal)) await sleep(20);
  rmSync(signal, { force: true });
  return { stopped: done };
}

let W: number | undefined;
s.after(async () => {
  if (W) await tech.call("focus_window", { window: W, action: "close", after: "none" });
});

await s.run(async () => {
  await tech.connectLocalDriver({ launch: false, waitMs: 30_000 });
  await tech.call("wait_for", { appears: "Asctrl Bench", timeoutSec: 15 });
  const B = ((await tech.call("observe", { max: 1 })).data as ObserveData).windows.find((x) => x.title === "Asctrl Bench")!.handle;
  const o = await tech.call("open_app", { target: pathToFileURL(path.join(root, "test/web/p3.html")).href, browser: "asctrl", after: "none" });
  W = (o.data as { handle?: number } | undefined)?.handle;
  await tech.call("focus_window", { window: B, after: "none" });
  await sleep(1200);

  // 1. A mouse click waits for the person
  let { stopped } = await person(2);
  await sleep(300);
  let t = performance.now();
  const c1 = await tech.call("click", { window: B, target: "Submit", mode: "mouse", after: "none" });
  const ms1 = Math.round(performance.now() - t);
  await stopped;
  say(c1.ok && ms1 >= 2200 && /waited \d+\.\d s for the person/.test(c1.text ?? ""), `a mouse click waited for the person to stop (${ms1} ms): ${first(c1.text ?? c1.error)}`);

  // 2. A page click goes ahead meanwhile
  ({ stopped } = await person(2));
  await sleep(300);
  t = performance.now();
  const c2 = await tech.call("click", { window: W, target: "Save", after: "none" });
  const ms2 = Math.round(performance.now() - t);
  await stopped;
  say(c2.ok && ms2 < 1000 && !/waited/.test(c2.text ?? "") && (c2.data as { channel?: string }).channel === "page", `a click in Asctrl's browser went ahead while the person moved the mouse (${ms2} ms): ${first(c2.text ?? c2.error)}`);

  // 3. Someone who never stops
  await tech.call("set_value", { window: B, target: "Name", value: "before", after: "none" });
  await tech.call("click", { window: B, target: "Submit", after: "none" });
  // What a click on Submit would show, if it got through
  await tech.call("set_value", { window: B, target: "Name", value: "during", after: "none" });
  ({ stopped } = await person(6));
  await sleep(300);
  t = performance.now();
  const c3 = await tech.call("click", { window: B, target: "Submit", mode: "mouse", after: "none" });
  const ms3 = Math.round(performance.now() - t);
  await stopped;
  const result = (await tech.call("observe", { window: B, query: "Submitted:" })).text ?? "";
  say(!c3.ok && (c3.data as { busy?: boolean } | undefined)?.busy === true && ms3 >= 3800 && ms3 < 6000 && !/Submitted: during/.test(result), `gave up after ${ms3} ms and clicked nothing: ${first(c3.error ?? c3.text)}`);
  await sleep(1200);

  // 4. Two programs at once
  const inPage = { script: `for (let i = 0; i < 6; i++) click('Save', { window: args.w }); return read(args.w).match(/saved \\d+/g).pop();`, args: { w: W } };
  const inBench = { script: `for (const n of ['Ann', 'Bob', 'Cy']) { fill({ Name: n }, { window: args.w }); click('Submit', { window: args.w }); } return 'ok';`, args: { w: B } };
  t = performance.now();
  await tech.call("exec", inPage);
  const tPage = performance.now() - t;
  t = performance.now();
  await tech.call("exec", inBench);
  const tBench = performance.now() - t;
  t = performance.now();
  const [a, b] = await Promise.all([tech.call("exec", { ...inPage, waitSec: 60 }), tech.call("exec", { ...inBench, waitSec: 60 })]);
  const both = performance.now() - t;
  say(prog(a)?.state === "done" && prog(b)?.state === "done" && both < (tPage + tBench) * 0.85, `two programs at once took ${Math.round(both)} ms, one after another ${Math.round(tPage)} + ${Math.round(tBench)} ms`);

  // 5. While the person uses the mouse, a button of an ordinary window that UI Automation can press is pressed
  //    in the background, without waiting for them, and the window in front stays theirs
  await tech.call("set_value", { window: B, target: "Name", value: "beside", after: "none" });
  await tech.call("focus_window", { window: W, after: "none" });
  ({ stopped } = await person(3));
  await sleep(300);
  t = performance.now();
  const c5 = await tech.call("click", { window: B, target: "Submit", after: "none" });
  const ms5 = Math.round(performance.now() - t);
  const front5 = ((await tech.call("observe", { max: 1 })).data as ObserveData).windows.find((x) => x.front)?.handle;
  await stopped;
  const shown5 = (await tech.call("observe", { window: B, query: "Submitted:" })).text ?? "";
  say(c5.ok && ms5 < 1500 && /in the background/.test(c5.text ?? "") && /Submitted: beside/.test(shown5) && front5 === W, `a UI Automation press went ahead in the background while the person moved the mouse (${ms5} ms), and their window stayed in front: ${first(c5.text ?? c5.error)} | front ${front5} (theirs ${W}) | ${/Submitted[^\n|]*/.exec(shown5)?.[0]}`);
});
