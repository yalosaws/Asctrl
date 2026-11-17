// L4, on a real desktop: skills kept by themselves, checked against the window, carried between computers, and
// put right as they are used.
// Usage: node test/l4.ts   (opens the bench window and Notepad for about 30 s; leave the computer alone)
//
// 1. A program that works, sent without learn, is kept by itself; skills finds it by what it did.
// 2. With another window in front, running it is refused (it was made for the bench window); with the bench window
//    in front it runs.
// 3. import takes a skill (as export gives one) whose button has been renamed: it gets stuck; put right with
//    instead, it ends well, and its script now names the new button.
// 4. export gives it back as JSON, with the change.

import { spawn } from "node:child_process";
import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { TechClient } from "../src/console/tech-client.ts";
import type { ProgramData } from "../src/shared/protocol.ts";

const root = path.resolve(import.meta.dirname, "..");
const exe = path.join(root, "agent-app/src-tauri/target/release/asctrl-agent.exe");
const localFile = path.join(tmpdir(), `asctrl-l4-local-${process.pid}.json`);
const skillsFile = path.join(tmpdir(), `asctrl-l4-skills-${process.pid}.json`);
process.env.ASCTRL_LOCAL_FILE = localFile;
const agent = spawn(exe, ["--relay", "ws://127.0.0.1:1"], { stdio: ["ignore", "ignore", "inherit"], env: { ...process.env, ASCTRL_SKILLS_FILE: skillsFile, ASCTRL_CONFIRM: "off" } });
const bench = spawn("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(root, "test/bench-app.ps1")], { stdio: "ignore", windowsHide: true });
const tech = new TechClient("ws://127.0.0.1:1", "L4Test");
let failures = 0;
const say = (ok: boolean, line: string) => {
  if (!ok) failures++;
  console.log(`${ok ? "✓" : "✗"} ${line}`);
};
const prog = (r: { data?: unknown }) => (r.data as ProgramData | undefined)?.program;
const brief = (s?: string) => (s ?? "").split("\n").slice(0, 2).join(" | ");
let N: number | undefined;

try {
  await tech.connectLocalDriver({ launch: false, waitMs: 30_000 });
  await tech.call("wait_for", { appears: "Asctrl Bench", timeoutSec: 15 });
  await tech.call("focus_window", { window: "Asctrl Bench", after: "none" });

  // 1.
  const r1 = await tech.call("exec", { script: `fill({ Name: args.name }); click('Agree'); click('Submit'); return 'ok';`, args: { name: "Ann" }, waitSec: 30 });
  const kept = /Kept by itself as the skill "([^"]+)"/.exec(prog(r1)?.learned ?? "")?.[1];
  const found = await tech.call("skills", { query: "Agree Submit" });
  say(prog(r1)?.state === "done" && !!kept && found.ok && (found.text ?? "").includes(kept), `a program that worked was kept by itself as "${kept}", and found by what it did: ${brief(found.text)}`);

  // 2.
  const n = await tech.call("open_app", { target: "notepad", after: "none" });
  N = (n.data as { handle?: number } | undefined)?.handle;
  await tech.call("wait_for", { stable: true, timeoutSec: 2, after: "none" });
  const r2 = await tech.call("exec", { skill: kept, args: { name: "Bob" }, waitSec: 30 });
  say(!r2.ok && (r2.data as { misfit?: boolean } | undefined)?.misfit === true, `with Notepad in front, the skill was not run: ${brief(r2.error)}`);
  await tech.call("focus_window", { window: "Asctrl Bench", after: "none" });
  const r3 = await tech.call("exec", { skill: kept, args: { name: "Bob" }, waitSec: 30 });
  say(prog(r3)?.state === "done", `with the bench window in front, it ran: ${prog(r3)?.state}`);

  // 3.
  const stale = { name: "submit a name", description: "Types a name into the bench window and submits it", script: `fill({ Name: args.name });\nclick('Send it');\nreturn 'sent';`, params: ["name"], apps: [], marks: [], runs: 0, done: 0, stuck: 0, learned: 0 };
  const imp = await tech.call("skills", { import: [stale] });
  say(imp.ok, `imported a skill: ${brief(imp.text ?? imp.error)}`);
  let r = await tech.call("exec", { skill: "submit a name", args: { name: "Cy" }, waitSec: 30 });
  const id = prog(r)?.id;
  say(prog(r)?.state === "stuck" && prog(r)?.line === 2, `its renamed button made it stuck at line ${prog(r)?.line}`);
  r = await tech.call("resume", { id, instead: { tool: "click", args: { target: "Submit", window: "Asctrl Bench" } }, waitSec: 30 });
  const shown = await tech.call("skills", { show: "submit a name" });
  say(prog(r)?.state === "done" && /click\('Submit'\)/.test(shown.text ?? "") && /changed to match/.test(prog(r)?.learned ?? ""), `put right with instead, it ended well and its script changed to match: ${brief(prog(r)?.learned ?? "")}`);

  // 4.
  const ex = await tech.call("skills", { export: "submit a name" });
  say(ex.ok && /"script":"fill\(\{ Name: args.name \}\);\\nclick\('Submit'\)/.test(ex.text ?? ""), `export gives it as JSON, changed: ${(ex.text ?? ex.error ?? "").slice(0, 120)}`);
} catch (e) {
  console.error(e);
  failures++;
} finally {
  if (N) await tech.call("focus_window", { window: N, action: "close", after: "none" }).catch(() => undefined);
  console.log(failures ? `\n${failures} check(s) failed` : "\nAll checks passed");
  process.exitCode = failures ? 1 : 0;
  tech.disconnect();
  bench.kill();
  agent.kill();
  rmSync(localFile, { force: true });
  rmSync(skillsFile, { force: true });
  setTimeout(() => process.exit(), 300);
}
