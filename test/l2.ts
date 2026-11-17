// L2 / L3, on a real desktop: a program's own popup rules, waiting on a condition, and the AI answering a stuck
// step with code.
// Usage: node test/l2.ts   (opens the bench window for about 20 s; leave the computer alone)
//
// 1. on(): a dialog that comes up in the middle of a program is closed in between its steps, and it goes on.
// 2. until(): waits for a condition the program checks itself.
// 3. Stuck on a button that isn't there: the report lists what windows came and went; the AI answers with code,
//    which does several steps in the stuck one's place, and the program goes on with what that code returned.

import { spawn } from "node:child_process";
import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { TechClient } from "../src/console/tech-client.ts";
import type { ObserveData, ProgramData } from "../src/shared/protocol.ts";

const root = path.resolve(import.meta.dirname, "..");
const exe = path.join(root, "agent-app/src-tauri/target/release/asctrl-agent.exe");
const localFile = path.join(tmpdir(), `asctrl-l2-local-${process.pid}.json`);
process.env.ASCTRL_LOCAL_FILE = localFile;
const agent = spawn(exe, ["--relay", "ws://127.0.0.1:1"], { stdio: ["ignore", "ignore", "inherit"], env: { ...process.env, ASCTRL_CONFIRM: "off", ASCTRL_SKILLS_FILE: path.join(tmpdir(), `asctrl-l2-skills-${process.pid}.json`) } });
const bench = spawn("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(root, "test/bench-app.ps1")], { stdio: "ignore", windowsHide: true });
const tech = new TechClient("ws://127.0.0.1:1", "L2Test");
let failures = 0;
const say = (ok: boolean, line: string) => {
  if (!ok) failures++;
  console.log(`${ok ? "✓" : "✗"} ${line}`);
};
const prog = (r: { data?: unknown }) => (r.data as ProgramData | undefined)?.program;
const brief = (s?: string) => (s ?? "").split("\n").slice(0, 3).join(" | ");
const submitted = async () => /Submitted: \w*/.exec((await tech.call("observe", { window: "Asctrl Bench", query: "Submitted" })).text ?? "")?.[0] ?? "";

try {
  await tech.connectLocalDriver({ launch: false, waitMs: 30_000 });
  await tech.call("wait_for", { appears: "Asctrl Bench", timeoutSec: 15 });
  await tech.call("focus_window", { window: "Asctrl Bench", after: "none" });

  // 1.
  const r1 = await tech.call("exec", {
    script: `on('Close me', 'Close me');\nclick('Open dialog', { window: 'Asctrl Bench' });\nsleep(1500);\nfill({ Name: 'onward' }, { window: 'Asctrl Bench' });\nclick('Submit', { window: 'Asctrl Bench' });\nreturn 'ok';`,
    learn: false,
    waitSec: 40,
  });
  const wins = ((await tech.call("observe", { max: 1 })).data as ObserveData).windows.map((w) => w.title);
  say(prog(r1)?.state === "done" && (await submitted()) === "Submitted: onward" && !wins.includes("Bench Dialog") && /Popups dismissed/.test((prog(r1)?.log ?? []).join("\n")), `on(): the dialog that came up was closed between steps, and the program went on: ${brief(prog(r1)?.log.join(" "))}`);

  // 2.
  const r2 = await tech.call("exec", { script: `fill({ Name: 'later' }, { window: 'Asctrl Bench' }); click('Submit', { window: 'Asctrl Bench' }); return until(() => read('Asctrl Bench').match(/Submitted: later/), { timeoutSec: 5 })[0];`, learn: false, waitSec: 30 });
  say(prog(r2)?.state === "done" && prog(r2)?.result === "Submitted: later", `until(): waited for what the program looked for: ${JSON.stringify(prog(r2)?.result)}`);

  // 3.
  let r = await tech.call("exec", { script: `click('Open dialog', { window: 'Asctrl Bench' });\nwaitFor('Bench Dialog');\nclick('Close me', { window: 'Bench Dialog' });\nconst r = click('Send everything', { window: 'Asctrl Bench' });\nreturn 'went on with ' + r;`, learn: false, waitSec: 30 });
  const p = prog(r);
  const events = (p as unknown as { events?: { kind: string; title: string }[] } | undefined)?.events ?? [];
  say(p?.state === "stuck" && p?.line === 4 && events.some((e) => e.title === "Bench Dialog") && !!r.image, `stuck at line ${p?.line}; the report has a picture of the window and lists the windows that came and went: ${events.map((e) => `${e.kind} ${e.title}`).join(", ")}`);
  r = await tech.call("resume", { id: p?.id, code: `fill({ Name: 'patched' }, { window: 'Asctrl Bench' }); click('Submit', { window: 'Asctrl Bench' }); return 'the patch';`, waitSec: 30 });
  say(prog(r)?.state === "done" && prog(r)?.result === "went on with the patch" && (await submitted()) === "Submitted: patched", `answered with code: it ran in the step's place and the program went on: ${JSON.stringify(prog(r)?.result)}`);
} catch (e) {
  console.error(e);
  failures++;
} finally {
  console.log(failures ? `\n${failures} check(s) failed` : "\nAll checks passed");
  process.exitCode = failures ? 1 : 0;
  tech.disconnect();
  bench.kill();
  agent.kill();
  rmSync(localFile, { force: true });
  setTimeout(() => process.exit(), 300);
}
