// L5, on a real desktop: nothing waits for the person at the computer, and every action is logged.
// Usage: node test/l5.ts   (opens the bench window ~30 s)
//
// 1. "Submit" sends something out: it is done at once, with no question.
// 2. A declaration raises the level: "Agree" with effect: "irreversible" is logged as irreversible, and done too.
// 3. The action log has each action and its level, and typed text only by its length.

import { spawn } from "node:child_process";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { TechClient } from "../src/console/tech-client.ts";

const root = path.resolve(import.meta.dirname, "..");
const exe = path.join(root, "agent-app/src-tauri/target/release/asctrl-agent.exe");
const localFile = path.join(tmpdir(), `asctrl-l5-local-${process.pid}.json`);
const log = path.join(tmpdir(), `asctrl-l5-actions-${process.pid}.log`);
process.env.ASCTRL_LOCAL_FILE = localFile;
// Programs that work are kept as skills by themselves: in a file of the test's own, not the computer's
process.env.ASCTRL_SKILLS_FILE ??= path.join(tmpdir(), `asctrl-test-skills-${process.pid}.json`);
const agent = spawn(exe, ["--relay", "ws://127.0.0.1:1"], { stdio: ["ignore", "ignore", "inherit"], env: { ...process.env, ASCTRL_ACTION_LOG: log } });
const bench = spawn("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(root, "test/bench-app.ps1")], { stdio: "ignore", windowsHide: true });
const tech = new TechClient("ws://127.0.0.1:1", "L5Test");
let failures = 0;
const say = (ok: boolean, line: string) => {
  if (!ok) failures++;
  console.log(`${ok ? "✓" : "✗"} ${line}`);
};
const first = (s?: string) => (s ?? "").split("\n")[0];
const result = async () => (await tech.call("observe", { window: "Asctrl Bench", query: "Submitted" })).text ?? "";

try {
  await tech.connectLocalDriver({ launch: false, waitMs: 30_000 });
  await tech.call("wait_for", { appears: "Asctrl Bench", timeoutSec: 15 });
  const named = await tech.call("set_value", { window: "Asctrl Bench", target: "Name", value: "secret123", after: "none" });
  if (!named.ok) throw new Error(`could not type the name: ${named.error}`);

  // 1.
  let t = performance.now();
  let r = await tech.call("click", { window: "Asctrl Bench", target: "Submit", after: "none" });
  say(r.ok && performance.now() - t < 3000 && /Submitted: secret123/.test(await result()), `"Submit" was done with no question (${Math.round(performance.now() - t)} ms): ${first(r.text ?? r.error)}`);

  // 2.
  t = performance.now();
  r = await tech.call("click", { window: "Asctrl Bench", target: "Agree", effect: "irreversible", after: "none" });
  say(r.ok && performance.now() - t < 3000, `declared irreversible, the checkbox was still done with no question (${Math.round(performance.now() - t)} ms)`);

  // 3.
  const lines = existsSync(log) ? readFileSync(log, "utf8").trim().split("\n").map((l) => JSON.parse(l)) : [];
  const clicks = lines.filter((l) => l.tool === "click").map((l) => `${l.aimed}:${l.effect}:${l.ok}`).join(" | ");
  say(clicks === "Submit:external:true | Agree:irreversible:true", `the log has each click and its level: ${clicks}`);
  const typed = JSON.stringify(lines.filter((l) => l.tool === "set_value"));
  say(!/secret123/.test(typed) && /<9 characters>/.test(typed), `typed values are logged by their length only`);
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
  rmSync(log, { force: true });
  setTimeout(() => process.exit(), 300);
}
