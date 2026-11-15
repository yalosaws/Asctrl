// run, end to end: plans by text, the decision model only when text can't tell, plans queued while "thinking".
// Usage: node test/run.ts [--gmail]   (opens the bench window and Calculator; leave the computer alone ~30 s)
//   --gmail also composes a mail in the browser's Gmail and discards it (never sends)

import { spawn } from "node:child_process";
import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { TechClient } from "../src/console/tech-client.ts";
import { Runner, type Plan } from "../src/console/run.ts";

const root = path.resolve(import.meta.dirname, "..");
const exe = path.join(root, "agent-app/src-tauri/target/release/asctrl-agent.exe");
const localFile = path.join(tmpdir(), `asctrl-run-local-${process.pid}.json`);
process.env.ASCTRL_LOCAL_FILE = localFile;
// A cache of its own, empty: the checks count decision-model calls
const cacheFile = path.join(tmpdir(), `asctrl-run-cache-${process.pid}.json`);
process.env.ASCTRL_DREX_CACHE = cacheFile;
const agent = spawn(exe, ["--relay", "ws://127.0.0.1:1"], { stdio: ["ignore", "ignore", "inherit"] });
const bench = spawn("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(root, "test/bench-app.ps1")], { stdio: "ignore", windowsHide: true });
const tech = new TechClient("ws://127.0.0.1:1", "RunTest");
const runner = new Runner(tech);
let failures = 0;

async function check(label: string, plan: Plan, want: (text: string) => boolean) {
  const t = performance.now();
  const r = await runner.run(plan, true);
  const ok = want(r.text);
  if (!ok) failures++;
  console.log(`\n${ok ? "✓" : "✗"} ${label} — ${Math.round(performance.now() - t)} ms\n${r.text.split("\n").slice(0, 14).map((l) => "    " + l).join("\n")}`);
}

try {
  await tech.connectLocalDriver({ launch: false, waitMs: 30_000 });
  await tech.call("wait_for", { window: "Asctrl Bench", timeoutSec: 15 });
  const lw = (await tech.call("list_windows", {})).text ?? "";
  const B = Number(lw.match(/\[(\d+)\][^\n]*"Asctrl Bench"/)![1]);

  await check("bench form by labels, all local", { window: B, steps: [{ fill: { Name: "Alice", Agree: true } }, { click: "Submit" }, { expect: "Submitted: Alice" }] }, (t) => t.includes("✓ done") && !t.includes("decision model"));
  await check("no exact match → decision model", { window: B, goal: "send the form with the name Bob", steps: [{ fill: { "person's name": "Bob" } }, { click: "send the form" }, { expect: "Submitted: Bob" }] }, (t) => t.includes("✓ done") && t.includes("decision model"));
  await check("same again → remembered, no model call", { window: B, goal: "send the form with the name Bob", steps: [{ fill: { "person's name": "Bob2" } }, { click: "send the form" }, { expect: "Submitted: Bob2" }] }, (t) => t.includes("✓ done") && t.includes("remembered") && !t.includes("call(s)"));
  await check("absent target → not guessed", { window: B, goal: "print the page", steps: [{ click: "Print preview" }] }, (t) => t.includes("✗") && !t.includes("✓ click"));
  await runner.progress(0);

  // Queued: three plans sent at once, as an AI would while thinking; the second waits for what the first brings up
  await tech.call("open_app", { target: "calc", after: "none" });
  await tech.call("wait_for", { window: "计算器|Calculator", timeoutSec: 10 });
  const C = Number(((await tech.call("list_windows", {})).text ?? "").match(/\[(\d+)\][^\n]*"(计算器|Calculator)"/)![1]);
  const t0 = performance.now();
  const q1 = await runner.run({ window: C, steps: [{ keys: "escape" }, { click: "一" }, { click: "二" }, { click: "×" }, { click: "七" }, { click: "等于" }, { expect: "84" }], goal: "compute 12 × 7" }, false);
  const q2 = await runner.run({ window: C, when: "84", steps: [{ click: "除以" }, { click: "四" }, { click: "等于" }, { expect: "21" }] }, false);
  const queuedMs = performance.now() - t0;
  const rep = await runner.progress(60);
  const okQ = rep.text.includes("r5 ✓") && rep.text.includes("r6 ✓");
  if (!okQ) failures++;
  console.log(`\n${okQ ? "✓" : "✗"} two plans queued in ${Math.round(queuedMs)} ms, all done ${Math.round(performance.now() - t0)} ms after the first was sent\n    ${q1.text}\n    ${q2.text}\n${rep.text.split("\n").map((l) => "    " + l).join("\n")}`);

  // A failure cancels what was queued behind it, and the next run is told instead of queued
  await runner.run({ window: C, steps: [{ click: "zzz no such button" }] }, false);
  await runner.run({ window: C, steps: [{ click: "一" }] }, false);
  await new Promise((r) => setTimeout(r, 4000));
  const told = await runner.run({ window: C, steps: [{ click: "二" }] }, false);
  const okF = !told.ok && told.text.startsWith("Not queued") && told.text.includes("cancelled");
  if (!okF) failures++;
  console.log(`\n${okF ? "✓" : "✗"} failure stops the queue\n${told.text.split("\n").slice(0, 8).map((l) => "    " + l).join("\n")}`);
  await tech.call("focus_window", { window: C, action: "close", after: "none" });

  if (process.argv.includes("--gmail")) {
    await check(
      "Gmail compose (not sent; discarded)",
      {
        goal: "write an email to 1229376643@qq.com saying 你好, then throw the draft away",
        steps: [
          { open: "https://mail.google.com/mail/u/0/#inbox" },
          { expect: "Compose", timeoutSec: 15 },
          { tool: "wait_for", stable: true, timeoutSec: 6 },
          { click: "Compose" },
          { expect: "Message Body", timeoutSec: 10 },
          { fill: { To: "1229376643@qq.com", "Message Body": "你好" } },
          { click: "Discard draft" },
          { expect: "Message Body", gone: true },
          { keys: "ctrl+w" },
        ],
      },
      (t) => t.includes("✓ done"),
    );
  }
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
  rmSync(cacheFile, { force: true });
  setTimeout(() => process.exit(), 300);
}
