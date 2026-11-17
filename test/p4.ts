// P4, on a real desktop: programs (exec / resume).
// Usage: node test/p4.ts   (opens the bench window and Asctrl's browser for about 20 s; leave the computer alone)
//
// 1. A loop over rows in one call: fill the bench form for each name, submit, read what it says.
// 2. An action that fails stops the program at its line (stuck); resume with another action in its place and
//    it goes on from there.
// 3. A program that asks, and goes on with the answer.
// 4. Abort: the program gets an error it can catch.
// 5. A running program stopped.
// 6. A program in Asctrl's own browser: the page channel from inside a program.

import { spawn } from "node:child_process";
import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { TechClient } from "../src/console/tech-client.ts";
import type { ObserveData, ProgramData } from "../src/shared/protocol.ts";

const root = path.resolve(import.meta.dirname, "..");
const exe = path.join(root, "agent-app/src-tauri/target/release/asctrl-agent.exe");
const localFile = path.join(tmpdir(), `asctrl-p4-local-${process.pid}.json`);
process.env.ASCTRL_LOCAL_FILE = localFile;
const agent = spawn(exe, ["--relay", "ws://127.0.0.1:1"], { stdio: ["ignore", "ignore", "inherit"] });
const bench = spawn("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(root, "test/bench-app.ps1")], { stdio: "ignore", windowsHide: true });
const tech = new TechClient("ws://127.0.0.1:1", "P4Test");
let failures = 0;
const say = (ok: boolean, line: string) => {
  if (!ok) failures++;
  console.log(`${ok ? "✓" : "✗"} ${line}`);
};
const prog = (r: { data?: unknown }) => (r.data as ProgramData | undefined)?.program;
const brief = (s?: string) => (s ?? "").split("\n").slice(0, 3).join(" | ");

let W: number | undefined;
try {
  await tech.connectLocalDriver({ launch: false, waitMs: 30_000 });
  await tech.call("wait_for", { appears: "Asctrl Bench", timeoutSec: 15 });
  const B = ((await tech.call("observe", { max: 1 })).data as ObserveData).windows.find((x) => x.title === "Asctrl Bench")!.handle;

  // 1. Rows in one call
  let t = performance.now();
  const r1 = await tech.call("exec", {
    script: `
      const seen = [];
      for (const name of args.names) {
        fill({ Name: name }, { window: args.w });
        click('Submit', { window: args.w });
        seen.push(look({ window: args.w, query: 'Submitted:' }).text.match(/Submitted: \\w+/)[0]);
      }
      return seen;`,
    args: { names: ["Ann", "Bob", "Cy"], w: B },
  });
  const p1 = prog(r1);
  say(p1?.state === "done" && JSON.stringify(p1.result) === JSON.stringify(["Submitted: Ann", "Submitted: Bob", "Submitted: Cy"]), `three rows in one call, ${p1?.actions} actions in ${Math.round(performance.now() - t)} ms: ${JSON.stringify(p1?.result ?? brief(r1.text ?? r1.error))}`);

  // 2. Stuck, then something else in its place
  t = performance.now();
  const r2 = await tech.call("exec", {
    script: `fill({ Name: 'Dee' }, { window: args.w });\nclick('Send it', { window: args.w, timeoutSec: 0 });\nreturn look({ window: args.w, query: 'Submitted:' }).text.match(/Submitted: \\w+/)[0];`,
    args: { w: B },
  });
  const p2 = prog(r2);
  say(p2?.state === "stuck" && p2.line === 2 && p2.call?.tool === "click", `a failed action stopped the program at its line: ${brief(r2.text)}`);
  const r2b = await tech.call("resume", { id: p2?.id, instead: { tool: "click", args: { target: "Submit", window: B, after: "none" } } });
  say(prog(r2b)?.state === "done" && prog(r2b)?.result === "Submitted: Dee", `resumed with another action in its place, it went on: ${JSON.stringify(prog(r2b)?.result ?? brief(r2b.text ?? r2b.error))} (${Math.round(performance.now() - t)} ms)`);

  // 3. Asking
  const r3 = await tech.call("exec", { script: `const who = ask('Which name?', { choices: ['Eve', 'Fay'] });\nfill({ Name: who }, { window: args.w });\nclick('Submit', { window: args.w });\nreturn who;`, args: { w: B } });
  const p3 = prog(r3);
  say(p3?.state === "asking" && /Which name/.test(r3.text ?? ""), `the program asked: ${brief(r3.text)}`);
  const r3b = await tech.call("resume", { id: p3?.id, answer: "Fay" });
  const shown = (await tech.call("observe", { window: B, query: "Submitted: Fay" })).text ?? "";
  say(prog(r3b)?.state === "done" && /Submitted: Fay/.test(shown), `answered, it went on with the answer: ${JSON.stringify(prog(r3b)?.result)}`);

  // 4. Abort, caught
  const r4 = await tech.call("exec", { script: `try { click('No such thing', { window: args.w, timeoutSec: 0 }); } catch (e) { return 'caught: ' + e.message; }`, args: { w: B } });
  const r4b = await tech.call("resume", { id: prog(r4)?.id, abort: "not there today" });
  say(prog(r4b)?.result === "caught: not there today", `aborted, the program caught it: ${JSON.stringify(prog(r4b)?.result ?? brief(r4b.text ?? r4b.error))}`);

  // 5. Stop
  const r5 = await tech.call("exec", { script: `let n = 0;\nwhile (true) { n++; sleep(50); }`, waitSec: 0.5 });
  const r5b = await tech.call("resume", { id: prog(r5)?.id, stop: true });
  say(prog(r5)?.state === "running" && prog(r5b)?.state === "stopped", `a running program was stopped: ${brief(r5b.text)}`);

  // 6. In Asctrl's browser
  t = performance.now();
  const url = pathToFileURL(path.join(root, "test/web/p3.html")).href;
  const r6 = await tech.call("exec", {
    script: `
      const o = open(args.url, { browser: 'asctrl' });
      const w = o.data.handle;
      for (let i = 0; i < 3; i++) click('Save', { window: w });
      type('Search here', 'from a program', { window: w });
      return { w, saved: (read(w).match(/saved \\d+/g) || []).pop() };`,
    args: { url },
  });
  const p6 = prog(r6);
  W = (p6?.result as { w?: number } | undefined)?.w;
  say(p6?.state === "done" && (p6.result as { saved?: string }).saved === "saved 3", `a program in Asctrl's browser: ${JSON.stringify(p6?.result ?? brief(r6.text ?? r6.error))}, ${p6?.actions} actions in ${Math.round(performance.now() - t)} ms`);
} catch (e) {
  console.error(e);
  failures++;
} finally {
  if (W) await tech.call("focus_window", { window: W, action: "close", after: "none" }).catch(() => undefined);
  console.log(failures ? `\n${failures} check(s) failed` : "\nAll checks passed");
  process.exitCode = failures ? 1 : 0;
  tech.disconnect();
  bench.kill();
  agent.kill();
  rmSync(localFile, { force: true });
  setTimeout(() => process.exit(), 300);
}
