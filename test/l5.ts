// L5, on a real desktop: what can't be taken back waits for the person at the computer, and every action is logged.
// Usage: node test/l5.ts   (opens the bench window ~30 s; the overlay asks twice and the test answers it itself)
//
// 1. "Submit" sends something out: the overlay asks; the person says no: nothing happens, the reply says so.
// 2. Asked again, the person says yes: it is done.
// 3. A checkbox is reversible: no question.
// 4. A declaration raises the level: "Agree" with effect: "irreversible" asks too (and a no stops it).
// 5. The action log has each action, its level and the answer, and typed text only by its length.

import { spawn, execFile } from "node:child_process";
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
const agent = spawn(exe, ["--relay", "ws://127.0.0.1:1"], { stdio: ["ignore", "ignore", "inherit"], env: { ...process.env, ASCTRL_CONFIRM: "external,irreversible", ASCTRL_ACTION_LOG: log } });
const bench = spawn("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(root, "test/bench-app.ps1")], { stdio: "ignore", windowsHide: true });
const tech = new TechClient("ws://127.0.0.1:1", "L5Test");
let failures = 0;
const say = (ok: boolean, line: string) => {
  if (!ok) failures++;
  console.log(`${ok ? "✓" : "✗"} ${line}`);
};
const first = (s?: string) => (s ?? "").split("\n")[0];
/** The person answers the overlay's next question */
const person = (answer: "Allow" | "Deny") =>
  new Promise<string>((r) => execFile("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(root, "test/answer.ps1"), "-Answer", answer, "-AgentPid", String(agent.pid)], (_e, out) => r(String(out).trim())));
const result = async () => (await tech.call("observe", { window: "Asctrl Bench", query: "Submitted" })).text ?? "";

try {
  await tech.connectLocalDriver({ launch: false, waitMs: 30_000 });
  await tech.call("wait_for", { appears: "Asctrl Bench", timeoutSec: 15 });
  const named = await tech.call("set_value", { window: "Asctrl Bench", target: "Name", value: "secret123", after: "none" });
  if (!named.ok) throw new Error(`could not type the name: ${named.error}`);

  // 1.
  let said = person("Deny");
  let r = await tech.call("click", { window: "Asctrl Bench", target: "Submit", after: "none" });
  let pressed = await said;
  say(pressed === "pressed Deny" && !r.ok && (r.data as { refused?: boolean } | undefined)?.refused === true && !/Submitted: /.test(await result()), `"Submit" waited for the person, who said no: nothing was done: ${first(r.error)}`);

  // 2.
  said = person("Allow");
  r = await tech.call("click", { window: "Asctrl Bench", target: "Submit", after: "none" });
  pressed = await said;
  say(pressed === "pressed Allow" && r.ok && /Submitted: secret123/.test(await result()), `asked again, the person said yes (${pressed}), and it was done: ${first(r.text ?? r.error)} | shows: ${/Submitted[^\n|]*/.exec(await result())?.[0]}`);

  // 3.
  let t = performance.now();
  r = await tech.call("click", { window: "Asctrl Bench", target: "Agree", after: "none" });
  say(r.ok && performance.now() - t < 3000, `a checkbox is reversible: no question (${Math.round(performance.now() - t)} ms)`);

  // 4.
  said = person("Deny");
  r = await tech.call("click", { window: "Asctrl Bench", target: "Agree", effect: "irreversible", after: "none" });
  pressed = await said;
  say(pressed === "pressed Deny" && !r.ok, `declared irreversible, the same checkbox asks first (and the no stopped it): ${first(r.error)}`);

  // 5.
  const lines = existsSync(log) ? readFileSync(log, "utf8").trim().split("\n").map((l) => JSON.parse(l)) : [];
  const clicks = lines.filter((l) => l.tool === "click");
  const asked = clicks.map((l) => `${l.aimed}:${l.effect}:${l.asked}:${l.ok}`).join(" | ");
  say(asked === "Submit:external:false:false | Submit:external:true:true | Agree:reversible:null:true | Agree:irreversible:false:false", `the log has each click, its level and the answer: ${asked}`);
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
