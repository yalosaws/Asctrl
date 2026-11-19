import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { promisify } from "node:util";
import { AuthStore } from "../src/console/auth.ts";
import { Runner, normalizeStep, validate } from "../src/console/run.ts";
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
  const ending = Promise.withResolvers<void>();
  const release = Promise.withResolvers<void>();
  const client = new TechClient("ws://127.0.0.1:1");
  client.call = async (tool, args) => {
    const message = { type: "result" as const, id: tool };
    if (tool === "plan_mark" && args?.end === true) { ending.resolve(); await release.promise; }
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
  await ending.promise;
  assert.match((await runner.progress(0)).text, /running/);
  release.resolve();
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
