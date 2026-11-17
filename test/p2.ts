// P2, on a real desktop: the window timeline and reading a window that something covers.
// Usage: node test/p2.ts [--no-events]   (opens the bench window and Calculator; leave the computer alone ~20 s)
//   --no-events runs the driver with the timeline off (ASCTRL_NO_EVENTS=1), to compare with how it was
//
// 1. A window that opens late is noticed as it opens: the dialog opens 800 ms after its button is pressed, and
//    wait_for should return soon after that, not up to a poll later.
// 2. A batch says which windows came and went while it ran.
// 3. Text drawn in a window that another program's window covers is still read (Windows Graphics Capture), and
//    a click on it lands in that window.
// 4. A screenshot of a covered window shows it as it draws itself without moving anything, and a click at a
//    point on that picture lands in it.

import { spawn } from "node:child_process";
import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { TechClient } from "../src/console/tech-client.ts";
import type { BatchData, ObserveData } from "../src/shared/protocol.ts";

const root = path.resolve(import.meta.dirname, "..");
const exe = path.join(root, "agent-app/src-tauri/target/release/asctrl-agent.exe");
const off = process.argv.includes("--no-events");
const localFile = path.join(tmpdir(), `asctrl-p2-local-${process.pid}.json`);
process.env.ASCTRL_LOCAL_FILE = localFile;
// Programs that work are kept as skills by themselves: in a file of the test's own, not the computer's
process.env.ASCTRL_SKILLS_FILE ??= path.join(tmpdir(), `asctrl-test-skills-${process.pid}.json`);
// Its driver acts without asking the person first (L5's check has a test of its own: test/l5.ts)
process.env.ASCTRL_CONFIRM ??= "off";
const agent = spawn(exe, ["--relay", "ws://127.0.0.1:1"], { stdio: ["ignore", "ignore", "inherit"], env: { ...process.env, ...(off ? { ASCTRL_NO_EVENTS: "1" } : {}) } });
const bench = spawn("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(root, "test/bench-app.ps1"), "-NotOnTop"], { stdio: "ignore", windowsHide: true });
const tech = new TechClient("ws://127.0.0.1:1", "P2Test");
let failures = 0;
const say = (ok: boolean, line: string) => {
  if (!ok) failures++;
  console.log(`${ok ? "✓" : "✗"} ${line}`);
};

async function windowNamed(title: RegExp) {
  const wins = ((await tech.call("observe", { max: 1 })).data as ObserveData | undefined)?.windows ?? [];
  return wins.find((x) => title.test(x.title))?.handle;
}

let C: number | undefined;
try {
  await tech.connectLocalDriver({ launch: false, waitMs: 30_000 });
  console.log(`timeline ${off ? "off" : "on"}`);
  await tech.call("wait_for", { appears: "Asctrl Bench", timeoutSec: 15 });
  const B = (await windowNamed(/^Asctrl Bench$/))!;

  // 1. How long after the dialog opens wait_for notices it
  const late: number[] = [];
  for (let n = 0; n < 5; n++) {
    await tech.call("click", { window: B, target: "Open dialog", after: "none" });
    const t = performance.now();
    const r = await tech.call("wait_for", { appears: "Bench Dialog", timeoutSec: 5, after: "none" });
    const ms = performance.now() - t;
    if (!r.ok) {
      say(false, `dialog ${n + 1} not noticed: ${r.error}`);
      continue;
    }
    late.push(ms);
    await tech.call("click", { window: "Bench Dialog", target: "Close me", after: "none" });
    await tech.call("wait_for", { appears: "Bench Dialog", gone: true, timeoutSec: 5, after: "none" });
  }
  const avg = late.reduce((a, b) => a + b, 0) / Math.max(late.length, 1);
  // The click returns once the screen has settled (a few hundred ms), so the wait is shorter than 800 ms: what
  // counts is how it compares with the timeline off
  say(late.length === 5, `wait_for noticed the late dialog after ${late.map((x) => Math.round(x)).join(", ")} ms (average ${Math.round(avg)} ms)`);

  // 2. A batch's windows
  const r = await tech.call("batch", {
    steps: [
      { tool: "click", window: B, target: "Open dialog", after: "none" },
      { tool: "wait_for", appears: "Bench Dialog", timeoutSec: 5, after: "none" },
      { tool: "click", window: "Bench Dialog", target: "Close me", after: "none" },
      { tool: "wait_for", appears: "Bench Dialog", gone: true, timeoutSec: 5, after: "none" },
    ],
  });
  const wins = (r.data as BatchData).windows;
  const shown = wins?.find((w) => w.kind === "shown" && w.title === "Bench Dialog");
  if (off) say(wins === undefined, "without the timeline a batch says nothing about windows");
  else say(!!shown, `the batch saw the dialog come and go: ${(wins ?? []).map((w) => `${w.ms} ms ${w.kind} "${w.title}"`).join("; ")}`);

  // 3. Cover the bench window with Calculator, maximized, and read the canvas text underneath
  await tech.call("open_app", { target: "calc", after: "none" });
  await tech.call("wait_for", { appears: "计算器|Calculator", timeoutSec: 10, after: "none" });
  C = await windowNamed(/^(计算器|Calculator)$/);
  await tech.call("focus_window", { window: C, action: "maximize", after: "none" });
  await tech.call("wait_for", { stable: true, timeoutSec: 3, after: "none" });
  const t3 = performance.now();
  const q = await tech.call("observe", { window: B, query: "CANVAS-7391", ocr: true });
  const read3 = Math.round(performance.now() - t3);
  say(!!q.ok && /CANVAS-7391/.test(q.text ?? "") && /pixels|on screen/i.test(q.text ?? ""), `text on the covered window's canvas read in ${read3} ms:\n    ${(q.text ?? q.error ?? "").split("\n").slice(0, 3).join("\n    ")}`);
  const c = await tech.call("click", { window: B, target: "CANVAS-7391", after: "none" });
  const n = await tech.call("observe", { window: B, query: "Canvas clicks: 1" });
  say(c.ok && /Canvas clicks: 1/.test(n.text ?? ""), `a click on text in the covered window landed there: ${(c.text ?? c.error ?? "").split("\n")[0]}`);

  // 4. Cover it again: a screenshot of it shows it as it draws itself and moves nothing, and a click at a point
  //    on that picture brings it forward and lands there
  await tech.call("focus_window", { window: C, after: "none" });
  await tech.call("wait_for", { stable: true, timeoutSec: 3, after: "none" });
  const front = async () => ((await tech.call("observe", { max: 1 })).data as ObserveData | undefined)?.windows?.find((w) => w.front);
  const before = await front();
  const s = await tech.call("screenshot", { window: B });
  const still = (await front())?.handle;
  say(
    !!s.ok && !!s.image && /draws itself/.test(s.text ?? "") && still === C,
    `screenshot of the covered window, Calculator still in front (in front before: "${before?.title}", after: ${still === C ? "Calculator" : still}): ${(s.text ?? s.error ?? "").split("\n")[0]}`,
  );
  const at = /@\((\d+),(\d+)\) in "CANVAS-7391"/.exec((await tech.call("observe", { window: B, query: "CANVAS-7391", ocr: true })).text ?? "");
  const c2 = at ? await tech.call("click", { x: Number(at[1]), y: Number(at[2]), after: "none" }) : undefined;
  const n2 = await tech.call("observe", { window: B, query: "Canvas clicks: 2" });
  say(!!c2?.ok && /Canvas clicks: 2/.test(n2.text ?? ""), `a click at a point on that picture landed in it: ${at ? (c2?.text ?? c2?.error ?? "").split("\n")[0] : "the text's position was not given"}`);
} catch (e) {
  console.error(e);
  failures++;
} finally {
  // Restored before it closes: Calculator opens next time the way it was closed, and other tests expect its usual size
  if (C) await tech.call("focus_window", { window: C, action: "restore", after: "none" }).catch(() => undefined);
  if (C) await tech.call("focus_window", { window: C, action: "close", after: "none" }).catch(() => undefined);
  console.log(failures ? `\n${failures} check(s) failed` : "\nAll checks passed");
  process.exitCode = failures ? 1 : 0;
  tech.disconnect();
  bench.kill();
  agent.kill();
  rmSync(localFile, { force: true });
  setTimeout(() => process.exit(), 300);
}
