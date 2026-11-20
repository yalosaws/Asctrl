// The action log, on a real desktop: nothing waits for the person at the computer, and every action is logged.
// Usage: node test/log.ts   (opens the bench window ~30 s)
//
// 1. "Submit" sends something out: it is done at once, with no question.
// 2. A declaration raises the level: "Agree" with effect: "irreversible" is logged as irreversible, and done too.
// 3. The action log has each action and its level, and typed text only by its length.

import { existsSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { Suite } from "./desk.ts";

const log = path.join(tmpdir(), `asctrl-l5-actions-${process.pid}.log`);
const replay = path.join(tmpdir(), `asctrl-l5-replay-${process.pid}.log`);
const sample = path.join(tmpdir(), `asctrl-l5-fixture-${process.pid}.txt`);
const s = new Suite("Log", { bench: true, env: { ASCTRL_ACTION_LOG: log, ASCTRL_FX_LOG: replay } });
const { tech, say } = s;
const first = (s?: string) => (s ?? "").split("\n")[0];
const result = async () => (await tech.call("observe", { window: "Asctrl Bench", query: "Submitted" })).text ?? "";

s.after(async () => {
  for (const file of [log, replay, sample]) rmSync(file, { force: true, maxRetries: 6, retryDelay: 100 });
});

await s.run(async () => {
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

  const multi = await tech.call("set_value", { window: "Asctrl Bench", fields: [{ target: "Name", value: "private-form-583" }], after: "none" });
  say(multi.ok, "multiple fields set through the driver");
  const payload = Buffer.from("private-upload-136").toString("base64");
  const file = await tech.call("write_file", { path: sample, content: "private-file-207" });
  const uploaded = await tech.call("upload_file", { path: sample, data: payload });
  const nested = await tech.call("exec", { plan: { steps: [{ tool: "set_value", args: { target: "Name", value: "private-step-308", window: "Asctrl Bench" } }] }, learn: false, waitSec: 30 });
  say(file.ok && uploaded.ok && nested.ok, "file, upload and nested actions completed");
  // 3.
  const lines = existsSync(log) ? readFileSync(log, "utf8").trim().split("\n").map((l) => JSON.parse(l)) : [];
  const clicks = lines.filter((l) => l.tool === "click").map((l) => `${l.effect}:${l.ok}`).join(" | ");
  say(clicks === "external:true | irreversible:true", `the log has each click and its level: ${clicks}`);
  const recorded = JSON.stringify(lines) + readFileSync(replay, "utf8");
  say(["secret123", "private-form-583", "private-file-207", payload, "private-step-308"].every((s) => !recorded.includes(s))
    && lines.every((entry) => Number.isFinite(entry.elapsedMs)), "both logs exclude payloads and the action log retains elapsed time");
  const typed = JSON.stringify(lines.filter((l) => l.tool === "set_value"));
  say(!/secret123|private-form-583/.test(JSON.stringify(lines)) && /<9 characters>/.test(typed), `typed values are logged by their length only`);
});
