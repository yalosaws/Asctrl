// How long single actions take on a real desktop, and how much of it is waiting for the screen afterwards.
// Usage: node test/settle.ts [--no-events]   (opens the bench window; leave the computer alone ~20 s)
// Prints, per action, the reply time and the settle the reply reports; for comparing before and after a change
// to how actions wait.

import { driverExecutable, owned } from "./fixtures.ts";
import { spawn } from "node:child_process";
import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { TechClient } from "../src/console/tech-client.ts";
import type { ObserveData, ToolName } from "../src/shared/protocol.ts";

const root = path.resolve(import.meta.dirname, "..");
const exe = driverExecutable(root);
const off = process.argv.includes("--no-events");
const localFile = path.join(tmpdir(), `asctrl-settle-local-${process.pid}.json`);
process.env.ASCTRL_LOCAL_FILE = localFile;
// Programs that work are kept as skills by themselves: in a file of the test's own, not the computer's
process.env.ASCTRL_SKILLS_FILE ??= path.join(tmpdir(), `asctrl-test-skills-${process.pid}.json`);
// Its driver acts without asking the person first (L5's check has a test of its own: test/l5.ts)
process.env.ASCTRL_CONFIRM ??= "off";
const agent = owned(() => spawn(exe, ["--relay", "ws://127.0.0.1:1"], { stdio: ["ignore", "ignore", "inherit"], env: { ...process.env, ...(off ? { ASCTRL_NO_EVENTS: "1" } : {}) } }));
const bench = owned(() => spawn("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(root, "test/bench-app.ps1")], { stdio: "ignore", windowsHide: true }));
const tech = new TechClient("ws://127.0.0.1:1", "SettleTest");
let failures = 0;

const median = (xs: number[]) => [...xs].sort((a, b) => a - b)[Math.floor(xs.length / 2)] ?? 0;

try {
  await tech.connectLocalDriver({ launch: false, waitMs: 30_000 });
  await tech.call("wait_for", { appears: "Asctrl Bench", timeoutSec: 15 });
  const wins = ((await tech.call("observe", { max: 1 })).data as ObserveData | undefined)?.windows ?? [];
  const B = wins.find((x) => x.title === "Asctrl Bench")!.handle;
  await tech.call("focus_window", { window: B, after: "none" });
  await tech.call("wait_for", { stable: true, timeoutSec: 3, after: "none" });
  const cases: [string, ToolName, Record<string, unknown>][] = [
    ["press a button (UIA)", "click", { window: B, target: "Submit", after: "none" }],
    ["tick a box (UIA)", "click", { window: B, target: "Agree", after: "none" }],
    ["click text that does nothing", "click", { window: B, target: "Notes", after: "none" }],
    ["type into a field", "type_text", { window: B, target: "Name", text: "abc", after: "none" }],
    ["a key that does nothing", "press_keys", { keys: "shift", after: "none" }],
    ["click the canvas (pixels)", "click", { window: B, target: "CANVAS-7391", after: "none" }],
  ];
  console.log(`timeline ${off ? "off" : "on"}\n`);
  // Round after round rather than one action five times over: two clicks on one spot within the double-click
  // time are kept apart on purpose, which would count as waiting here
  const ms = cases.map(() => [] as number[]);
  const settled = cases.map(() => [] as number[]);
  const last = cases.map(() => "");
  for (let n = 0; n < 5; n++) {
    for (const [i, [, tool, args]] of cases.entries()) {
      const t = performance.now();
      const r = await tech.call(tool, args);
      ms[i].push(performance.now() - t);
      last[i] = (r.text ?? r.error ?? "").split("\n")[0];
      if (!r.ok) failures++;
      const s = /settled in (\d+) ms/.exec(r.text ?? "");
      if (s) settled[i].push(Number(s[1]));
    }
  }
  let all = 0;
  for (const [i, [label]] of cases.entries()) {
    all += median(ms[i]);
    console.log(`${label.padEnd(30)} ${Math.round(median(ms[i])).toString().padStart(5)} ms   settle ${settled[i].length ? `${median(settled[i])} ms` : "-"}   ${last[i].slice(0, 90)}`);
  }
  console.log(`\nsum of medians ${Math.round(all)} ms`);
} catch (e) {
  console.error(e);
  failures++;
} finally {
  console.log(failures ? `\n${failures} action(s) failed` : "");
  process.exitCode = failures ? 1 : 0;
  tech.disconnect();
  bench.kill();
  agent.kill();
  rmSync(localFile, { force: true });
  setTimeout(() => process.exit(), 300);
}
