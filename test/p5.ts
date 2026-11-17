// P5, on a real desktop: skills (exec with learn / skill, and the skills tool).
// Usage: node test/p5.ts   (opens the bench window for about 15 s; leave the computer alone)
// Skills go to a file of their own for the test (ASCTRL_SKILLS_FILE), not the computer's own list.
//
// 1. A program that ends well with learn is kept as a skill.
// 2. skills finds it by the window in front (no query) and by what it is for, and not for something else.
// 3. It runs again with new args and no script: one call.
// 4. Run without the args it takes: refused, saying which are missing.
// 5. A skill that no longer fits the window gets stuck; skills says where; learning it again under its name
//    replaces it, keeping its record, and the new one works.
// 6. show and forget.

import { spawn } from "node:child_process";
import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { TechClient } from "../src/console/tech-client.ts";
import type { ObserveData, ProgramData } from "../src/shared/protocol.ts";

const root = path.resolve(import.meta.dirname, "..");
const exe = path.join(root, "agent-app/src-tauri/target/release/asctrl-agent.exe");
const localFile = path.join(tmpdir(), `asctrl-p5-local-${process.pid}.json`);
const skillsFile = path.join(tmpdir(), `asctrl-p5-skills-${process.pid}.json`);
process.env.ASCTRL_LOCAL_FILE = localFile;
const agent = spawn(exe, ["--relay", "ws://127.0.0.1:1"], { stdio: ["ignore", "ignore", "inherit"], env: { ...process.env, ASCTRL_SKILLS_FILE: skillsFile } });
const bench = spawn("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(root, "test/bench-app.ps1")], { stdio: "ignore", windowsHide: true });
const tech = new TechClient("ws://127.0.0.1:1", "P5Test");
let failures = 0;
const say = (ok: boolean, line: string) => {
  if (!ok) failures++;
  console.log(`${ok ? "✓" : "✗"} ${line}`);
};
const prog = (r: { data?: unknown }) => (r.data as ProgramData | undefined)?.program;
const brief = (s?: string) => (s ?? "").split("\n").slice(0, 3).join(" | ");
type Found = { skills?: { name: string; fits: number; stuck: number; problem: string | null; runs: number }[] };

const SUBMIT = `
fill({ Name: args.name });
click('Submit');
return look({ query: 'Submitted:' }).text.match(/Submitted: \\w+/)[0];`;

try {
  await tech.connectLocalDriver({ launch: false, waitMs: 30_000 });
  await tech.call("wait_for", { appears: "Asctrl Bench", timeoutSec: 15 });
  const B = ((await tech.call("observe", { max: 1 })).data as ObserveData).windows.find((x) => x.title === "Asctrl Bench")!.handle;
  await tech.call("focus_window", { window: B, after: "none" });

  // 1. Learned
  const r1 = await tech.call("exec", { script: SUBMIT, args: { name: "Ann" }, learn: { name: "bench-submit", description: "Submit the bench form with a name and read what it says" } });
  say(prog(r1)?.state === "done" && /Kept as the skill "bench-submit"/.test(r1.text ?? ""), `kept as a skill: ${brief(r1.text ?? r1.error)}`);

  // 2. Found by the window in front, and by words
  const byWindow = (await tech.call("skills", {})).data as Found;
  const top = byWindow.skills?.[0];
  say(top?.name === "bench-submit" && top.fits >= 0.8, `with the bench in front and no query, it comes first, fitting ${Math.round((top?.fits ?? 0) * 100)}%`);
  const byWords = (await tech.call("skills", { query: "submit a name in the bench form" })).data as Found;
  const none = await tech.call("skills", { query: "send an email" });
  say(byWords.skills?.[0]?.name === "bench-submit" && none.ok && !(none.data as Found | undefined)?.skills?.length, `found by what it is for, and not for something else: ${brief(none.text ?? none.error)}`);

  // 3. Again, with new args and no script
  const t = performance.now();
  const r3 = await tech.call("exec", { skill: "bench-submit", args: { name: "Zoe" } });
  say(prog(r3)?.state === "done" && prog(r3)?.result === "Submitted: Zoe", `run again in one call with new args, in ${Math.round(performance.now() - t)} ms: ${JSON.stringify(prog(r3)?.result ?? brief(r3.text ?? r3.error))}`);

  // 4. Missing args
  const r4 = await tech.call("exec", { skill: "bench-submit", args: {} });
  say(!r4.ok && /missing name/.test(r4.error ?? r4.text ?? ""), `refused without its args: ${brief(r4.error ?? r4.text)}`);

  // 5. A skill that no longer fits: stuck, then learned again
  await tech.call("exec", { script: SUBMIT.replace("click('Submit')", "click(args.button)"), args: { name: "Bo", button: "Submit" }, learn: { name: "bench-old", description: "Submit the bench form, the old way" } });
  const r5 = await tech.call("exec", { skill: "bench-old", args: { name: "Cy", button: "Send now", timeoutSec: 0 } });
  const p5 = prog(r5);
  await tech.call("resume", { id: p5?.id, abort: true });
  const after5 = ((await tech.call("skills", { query: "old way" })).data as Found).skills?.[0];
  say(p5?.state === "stuck" && after5?.stuck === 1 && /at line 3: click failed/.test(after5.problem ?? ""), `a run that no longer fits got stuck, and the skill says where: ${after5?.problem}`);
  const r5b = await tech.call("exec", { script: SUBMIT, args: { name: "Cy" }, learn: { name: "bench-old", description: "Submit the bench form, the old way" } });
  const again = ((await tech.call("skills", { query: "old way" })).data as Found).skills?.[0];
  const r5c = await tech.call("exec", { skill: "bench-old", args: { name: "Di" } });
  say(/in place of the one of that name/.test(r5b.text ?? "") && again?.runs === 1 && again.problem === null && prog(r5c)?.result === "Submitted: Di", `learned again in its place (record kept: ${again?.runs} run), and the new one works: ${JSON.stringify(prog(r5c)?.result)}`);

  // 6. show, forget
  const shown = await tech.call("skills", { show: "bench-submit" });
  const gone = await tech.call("skills", { forget: "bench-old" });
  const left = ((await tech.call("skills", { query: "bench" })).data as Found).skills?.map((s) => s.name);
  say(/fill\(\{ Name: args.name \}\)/.test(shown.text ?? "") && /Takes args: name/.test(shown.text ?? "") && gone.ok && JSON.stringify(left) === '["bench-submit"]', `show gives its script and args; forget drops one (left: ${JSON.stringify(left)})`);
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
  rmSync(skillsFile, { force: true });
  setTimeout(() => process.exit(), 300);
}
