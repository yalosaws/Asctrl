// run, end to end: plans by text, the decision model only when text can't tell, plans queued while "thinking".
// Usage: node test/run.ts [--gmail]   (opens the bench window and Calculator; leave the computer alone ~30 s)
//   --gmail also composes a mail in the browser's Gmail and discards it (never sends)

import { spawn } from "node:child_process";
import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { TechClient } from "../src/console/tech-client.ts";
import { Runner, type Plan } from "../src/console/run.ts";
import type { ObserveData } from "../src/shared/protocol.ts";

const root = path.resolve(import.meta.dirname, "..");
const exe = path.join(root, "agent-app/src-tauri/target/release/asctrl-agent.exe");
const localFile = path.join(tmpdir(), `asctrl-run-local-${process.pid}.json`);
process.env.ASCTRL_LOCAL_FILE = localFile;
// Programs that work are kept as skills by themselves: in a file of the test's own, not the computer's
process.env.ASCTRL_SKILLS_FILE ??= path.join(tmpdir(), `asctrl-test-skills-${process.pid}.json`);
// Its driver acts without asking the person first (L5's check has a test of its own: test/l5.ts)
process.env.ASCTRL_CONFIRM ??= "off";
// A cache of its own, empty: the checks count decision-model calls
const cacheFile = path.join(tmpdir(), `asctrl-run-cache-${process.pid}.json`);
process.env.ASCTRL_DREX_CACHE = cacheFile;
const agent = spawn(exe, ["--relay", "ws://127.0.0.1:1"], { stdio: ["ignore", "ignore", "inherit"] });
const bench = spawn("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(root, "test/bench-app.ps1")], { stdio: "ignore", windowsHide: true });
const tech = new TechClient("ws://127.0.0.1:1", "RunTest");
const runner = new Runner(tech);
let failures = 0;
/** Calls the runner has made to the driver: each is a round trip, over the internet when driving a remote computer */
const calls = () => (runner as unknown as { calls: number }).calls;
/** The handle of the window whose title matches */
async function windowNamed(title: RegExp) {
  const wins = ((await tech.call("observe", { max: 1 })).data as ObserveData | undefined)?.windows ?? [];
  const w = wins.find((x) => title.test(x.title));
  if (!w) throw new Error(`no window titled ${title}`);
  return w.handle;
}

async function check(label: string, plan: Plan, want: (text: string) => boolean) {
  const t = performance.now();
  const c0 = calls();
  const r = await runner.run(plan, true);
  const ok = want(r.text);
  if (!ok) failures++;
  console.log(`\n${ok ? "✓" : "✗"} ${label} — ${Math.round(performance.now() - t)} ms, ${calls() - c0} driver call(s)\n${r.text.split("\n").slice(0, 14).map((l) => "    " + l).join("\n")}`);
}

try {
  await tech.connectLocalDriver({ launch: false, waitMs: 30_000 });
  await tech.call("wait_for", { appears: "Asctrl Bench", timeoutSec: 15 });
  const B = await windowNamed(/^Asctrl Bench$/);

  await check("bench form by labels, all local", { window: B, steps: [{ fill: { Name: "Alice", Agree: true } }, { click: "Submit" }, { expect: "Submitted: Alice" }] }, (t) => t.includes("✓ done") && !t.includes("decision model"));
  await check("no exact match → decision model", { window: B, goal: "send the form with the name Bob", steps: [{ fill: { "person's name": "Bob" } }, { click: "send the form" }, { expect: "Submitted: Bob" }] }, (t) => t.includes("✓ done") && t.includes("decision model"));
  await check("same again → remembered, no model call", { window: B, goal: "send the form with the name Bob", steps: [{ fill: { "person's name": "Bob2" } }, { click: "send the form" }, { expect: "Submitted: Bob2" }] }, (t) => t.includes("✓ done") && t.includes("remembered") && !t.includes("call(s)"));
  await check(
    "repeat with data, fast",
    { window: B, fast: true, repeat: [["Ann"], ["Ben"], ["Cy"]], steps: [{ fill: { Name: "$1" } }, { click: "Submit" }, { expect: "Submitted: $1" }] },
    (t) => t.includes("✓ done") && t.includes("rows 2–3 the same way"),
  );
  await check("absent target → not guessed", { window: B, goal: "print the page", steps: [{ click: "Print preview" }] }, (t) => t.includes("✗") && !t.includes("✓ click"));
  // Wait first, judge after: the dialog opens 0.8 s after the click, and the plan names it in other words
  await check(
    "late dialog: waited for, then decided",
    { goal: "open the bench dialog and close it again", steps: [{ click: "Open dialog", window: B }, { click: "close the dialog" }, { await: "Bench Dialog", gone: true }] },
    (t) => t.includes("✓ done") && /close the dialog" → Button "Close me"/.test(t),
  );
  // check: the decision model judges the window, for what a text search can't settle
  await check(
    "check: a claim that holds",
    { window: B, steps: [{ fill: { Name: "Kim" } }, { click: "Submit" }, { check: "The form has been submitted with the name Kim." }] },
    (t) => t.includes("✓ done") && t.includes('✓ check "'),
  );
  await check("check: a claim that doesn't", { window: B, steps: [{ fill: { Agree: false } }, { check: "The Agree box is ticked.", timeoutSec: 1 }] }, (t) => t.includes('✗ check "'));
  // The whole plan is checked before any of it runs
  await check(
    "a plan with mistakes: nothing runs",
    { window: B, steps: [{ fill: { Name: "Never" } }, { click: "Submit", colour: "red" }, { tool: "format_disk" }, { click: "board[1,1]" }] },
    (t) => t.startsWith("Not queued: the plan has mistakes") && t.includes('"colour"') && t.includes("format_disk") && t.includes('no grid named "board"'),
  );
  // A popup that turns up while a plan runs is dismissed in between its steps (here by the plan's rule); the
  // dialog is modal, so the fill after it works only once it is gone
  await check(
    "popup mid-plan: dismissed in between steps",
    { window: B, on: { "Bench Dialog": "Close me" }, steps: [{ click: "Open dialog" }, { tool: "wait_for", seconds: 1.5 }, { fill: { Name: "Pop" } }, { click: "Submit" }, { expect: "Submitted: Pop" }] },
    (t) => t.includes("✓ done") && t.includes('pressed "Close me"'),
  );
  // The pace is chosen per step: rows after the first go fast once the first showed the steps bring nothing up
  await check(
    "repeat, pace chosen per step",
    { window: B, repeat: [["A1"], ["A2"], ["A3"], ["A4"]], steps: [{ fill: { Name: "$1" } }, { click: "Submit" }, { expect: "Submitted: $1" }] },
    (t) => t.includes("✓ done") && /\d+ steps? fast/.test(t),
  );
  // Settled by an expect, then a dialog that opens late: the model may say "not ready yet", and it is waited for
  await check(
    "dialog still opening after a passed expect",
    { goal: "open the bench dialog and close it again", steps: [{ click: "Open dialog", window: B }, { expect: "Asctrl Bench" }, { click: "dismiss the popup" }, { await: "Bench Dialog", gone: true }] },
    (t) => t.includes("✓ done"),
  );
  // A plan that fails keeps nothing the decision model chose in it
  await check("failed plan: choice not kept", { window: B, steps: [{ fill: { "the name box": "Zed" } }, { expect: "never shown", timeoutSec: 1 }] }, (t) => t.includes("not remembered"));
  await check(
    "…so the same wording is decided again",
    { window: B, steps: [{ fill: { "the name box": "Zed2" } }, { expect: "never shown either", timeoutSec: 0.5 }] },
    (t) => t.includes("(decision model") && !t.includes("(remembered)"),
  );
  await runner.progress(0);

  // Queued: three plans sent at once, as an AI would while thinking; the second waits for what the first brings up
  await tech.call("open_app", { target: "calc", after: "none" });
  await tech.call("wait_for", { appears: "计算器|Calculator", timeoutSec: 10 });
  const C = await windowNamed(/^(计算器|Calculator)$/);
  const t0 = performance.now();
  const q1 = await runner.run({ window: C, steps: [{ keys: "escape" }, { click: "一" }, { click: "二" }, { click: "×" }, { click: "七" }, { click: "等于" }, { expect: "84" }], goal: "compute 12 × 7" }, false);
  const q2 = await runner.run({ window: C, when: "84", steps: [{ click: "除以" }, { click: "四" }, { click: "等于" }, { expect: "21" }] }, false);
  const queuedMs = performance.now() - t0;
  const rep = await runner.progress(60);
  const okQ = (rep.text.match(/^r\d+ ✓ done/gm) ?? []).length === 2;
  if (!okQ) failures++;
  console.log(`\n${okQ ? "✓" : "✗"} two plans queued in ${Math.round(queuedMs)} ms, all done ${Math.round(performance.now() - t0)} ms after the first was sent\n    ${q1.text}\n    ${q2.text}\n${rep.text.split("\n").map((l) => "    " + l).join("\n")}`);

  // What was learnt in the bench window is not used in another program
  await check("memory stays with its program", { window: C, steps: [{ click: "send the form" }] }, (t) => !t.includes("(remembered)") && t.includes("✗"));
  // A failure cancels what was queued behind it, and the next run is told instead of queued
  await runner.run({ window: C, steps: [{ click: "zzz no such button" }] }, false);
  await runner.run({ window: C, steps: [{ click: "一" }] }, false);
  // Until the failure has happened (not reported yet): a target that isn't there is looked for twice, which takes a few seconds
  for (let t = 0; t < 100 && !(runner as unknown as { unreported?: unknown }).unreported; t++) await new Promise((r) => setTimeout(r, 200));
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
