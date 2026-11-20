import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { promisify } from "node:util";
import { AuthStore } from "../src/console/auth.ts";
import { Runner, normalizeStep, translate, validate } from "../src/console/run.ts";
import { TechClient } from "../src/console/tech-client.ts";

test("remote relay connections require TLS before opening a socket", async () => {
  const client = new TechClient("ws://relay.invalid:8787");
  await assert.rejects(client.connect("123456"), /require wss/);
  await assert.rejects(new TechClient("invalid").connect("123456"), /Invalid URL/);
});

test("independent writers preserve other keys and revocations", async () => {
  const dir = mkdtempSync(path.join(tmpdir(), "asctrl-auth-writers-"));
  try {
    const file = path.join(dir, "auth.json");
    const store = new AuthStore(file);
    const old = store.addKey("old");
    const worker = [
      'const { AuthStore } = await import(process.argv[1]);',
      'const s = new AuthStore(process.argv[2]);',
      'for (let i = 0; i < 10; i++) s.addKey(process.argv[3] + i);',
      's.revokeKey("old");',
    ].join("\n");
    const auth = new URL("../src/console/auth.ts", import.meta.url).href;
    await Promise.all(["a", "b", "c", "d"].map((name) => promisify(execFile)(process.execPath, ["--input-type=module", "-e", worker, auth, file, name])));
    assert.equal(store.listKeys().length, 40);
    assert.equal(store.keyByValue(old.key), undefined);
    assert.throws(() => store.issueTokens(old.id, "fixture", []));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("progress publishes a failed plan after observing and cancelling its queue", async () => {
  const observing = Promise.withResolvers<void>();
  const finish = Promise.withResolvers<void>();
  const client = new TechClient("ws://127.0.0.1:1");
  client.call = async (tool) => {
    const message = { type: "result" as const, id: tool };
    if (tool === "wait_for") return { ...message, ok: false };
    if (tool === "observe") { observing.resolve(); await finish.promise; return { ...message, ok: true, text: "fixture context" }; }
    return { ...message, ok: true };
  };
  const runner = new Runner(client);
  await runner.run({ when: "fixture", steps: [{ keys: "shift" }] }, false);
  await observing.promise;
  await runner.run({ steps: [{ keys: "shift" }] }, false);
  assert.match((await runner.progress(0)).text, /running/);
  finish.resolve();
  const report = await runner.progress(2);
  assert.equal(report.ok, false);
  assert.match(report.text, /fixture context/);
  assert.match(report.text, /cancelled/);
  assert.equal((await runner.progress(0)).text, "Nothing queued or running.");
});

test("plan steps written like single actions are read as the plan's own", () => {
  const cases: [Record<string, unknown>, Record<string, unknown>][] = [
    [{ type: "To recipients", text: "a@b.c" }, { type: "a@b.c", into: "To recipients" }],
    [{ type: "hi", target: "Subject", enter: true }, { type: "hi", into: "Subject", enter: true }],
    [{ click: { x: 10, y: 20 }, window: 5 }, { tool: "click", x: 10, y: 20, window: 5 }],
    [{ wait: { gone: "Loading", timeoutSec: 8 } }, { await: "Loading", gone: true, timeoutSec: 8 }],
    [{ wait: { text: "Done" } }, { await: "Done" }],
    [{ wait: 2 }, { tool: "wait_for", seconds: 2 }],
    [{ type: "hi", into: "Subject" }, { type: "hi", into: "Subject" }],
  ];
  for (const [given, read] of cases) assert.deepEqual(normalizeStep(given), read);
  const steps = cases.map(([s]) => normalizeStep(s));
  assert.deepEqual(validate({ steps }), []);
  // Still named when it fits none of them
  assert.match(validate({ steps: [normalizeStep({ type: "x", text: "y", target: "z" })] })[0], /not understood/);
});

test("press, await and until steps go to the driver's own tools", () => {
  const box = { x0: 10, y0: 10, x1: 90, y1: 30 };
  const plan = { steps: [{ press: "OK" }, { await: "Saved" }, { until: ">= 100 | <= 5", in: box, confirm: 3 }] };
  assert.deepEqual(validate(plan), []);
  const [press, wait, until] = plan.steps.map((s) => translate(s, {}, {}));
  // A press is a click without the mouse
  assert.equal(press.tool, "click");
  assert.equal(press.args.mode, "direct");
  // What a plan awaits may be a window's title or text a window shows: whichever comes first
  assert.equal(wait.tool, "wait_for");
  assert.equal(wait.args.appears, "Saved");
  assert.equal(wait.args.text, "Saved");
  // A number watched in a region is a wait_for with until
  assert.equal(until.tool, "wait_for");
  assert.deepEqual({ ...until.args, after: undefined }, { ...box, until: ">= 100 | <= 5", confirm: 3, after: undefined });
  // The region is needed, and so is the condition
  assert.match(validate({ steps: [{ until: ">= 1" }] })[0], /until takes in/);
  assert.match(validate({ steps: [{ until: "", in: box }] })[0], /until takes the condition/);
  // The old way of writing it is named as not a step
  assert.match(validate({ steps: [{ watch: box, until: ">= 1" }] }).join(" "), /"watch" not understood/);
});

test("a plan runs as a program: its steps reported, a stuck step put right by the runner, popups told", async () => {
  process.env.ASCTRL_DREX_CACHE = path.join(mkdtempSync(path.join(tmpdir(), "asctrl-drex-")), "cache.json");
  const calls: [string, Record<string, unknown>][] = [];
  const client = new TechClient("ws://127.0.0.1:1");
  const program = (state: string, more: Record<string, unknown>) => ({ type: "result" as const, id: "p", ok: true, data: { program: { id: 7, state, log: [], steps: [], ...more } } });
  client.call = async (tool, args = {}) => {
    calls.push([tool, args]);
    if (tool === "exec") {
      return program("stuck", {
        steps: [{ i: 0, ok: true, text: "Clicked", data: { effect: "changed" } }],
        call: { tool: "click", args: { target: "Send", probe: true, timeoutSec: 0, __i: 1 } },
        failure: { ok: false, text: null, error: "Nothing shows Send", data: { miss: { kind: "absent", candidates: [] } } },
      });
    }
    // The runner's own try: it showed up late
    if (tool === "click") return { type: "result" as const, id: "c", ok: true, text: "Clicked Send", data: { effect: "changed" } };
    if (tool === "resume") return program("done", { steps: [{ i: 2, ok: true, text: "Typed", data: {} }], log: ['Popups dismissed along the way:\n↷ pressed "Not now" in "Update"'] });
    return { type: "result" as const, id: tool, ok: true };
  };
  const r = await new Runner(client).run({ steps: [{ click: "Compose" }, { click: "Send" }, { keys: "ctrl+s" }], on: { "Leave site?": "Leave" } }, true);
  assert.equal(r.ok, true, r.text);
  assert.match(r.text, /1\. ✓ click "Compose"\n2\. ✓ click "Send" \(showed up late\)\n3\. ✓ keys ctrl\+s\n4\. ↷ pressed "Not now"/);
  // One program for the whole plan, with its steps, its popup rules and how a missed step may be recovered
  const exec = calls.find(([t]) => t === "exec")![1] as { plan: { steps: { tool: string; args: Record<string, unknown> }[]; on: unknown } };
  assert.deepEqual(exec.plan.steps.map((x) => x.tool), ["click", "click", "press_keys"]);
  assert.deepEqual(exec.plan.on, { "Leave site?": "Leave" });
  assert.deepEqual(exec.plan.steps[1].args.recover, { kind: "click", late: true });
  // Put right by the runner, the program skips the step and goes on
  assert.deepEqual(calls.find(([t]) => t === "resume")![1], { id: 7, skip: true, value: { ok: true }, waitSec: 60 });
  assert.ok(!calls.some(([t]) => t === "batch" || t === "plan_mark"));
});

test("a check the runner can't judge stops the plan and its program", async () => {
  process.env.NACE_API_KEY = "";
  const calls: string[] = [];
  const client = new TechClient("ws://127.0.0.1:1");
  client.call = async (tool, args = {}) => {
    calls.push(tool + (args.stop ? ":stop" : ""));
    if (tool === "exec") return { type: "result" as const, id: "p", ok: true, data: { program: { id: 3, state: "asking", log: [], steps: [], question: { question: "check", details: { i: 0 } } } } };
    return { type: "result" as const, id: tool, ok: true, text: "fixture" };
  };
  const r = await new Runner(client).run({ steps: [{ check: "the email was sent" }] }, true);
  assert.equal(r.ok, false);
  assert.match(r.text, /✗ check "the email was sent": no decision model is set up/);
  assert.ok(calls.includes("resume:stop"), calls.join(", "));
});
