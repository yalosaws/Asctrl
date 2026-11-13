// Speed and reliability benchmark for the GUI agent: real tasks on a real desktop, each timed and checked.
// Starts a local relay and the agent app, opens test/bench-app.ps1 (a window with known controls, a canvas
// only OCR can read, and a dialog that opens late) and an Explorer folder, and drives them through the tools.
// Usage: npm run bench                     uses agent-app/src-tauri/target/debug/asctrl-agent.exe
//        npm run bench -- --app <exe>       another build
// It moves the mouse and types: leave the computer alone for the ~30 seconds it runs.

import { spawn } from "node:child_process";
import { mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { startRelay } from "../src/relay/server.ts";
import { TechClient } from "../src/console/tech-client.ts";
import type { ToolName, ToolResult } from "../src/shared/protocol.ts";

const PORT = 18788;
const CODE = "246810";
const root = path.resolve(import.meta.dirname, "..");
const appIdx = process.argv.indexOf("--app");
const exe = appIdx >= 0 ? process.argv[appIdx + 1] : path.join(root, "agent-app/src-tauri/target/debug/asctrl-agent.exe");
const benchApp = path.join(root, "test/bench-app.ps1");
const WIN = "Asctrl Bench";

const relay = startRelay(PORT);
const agent = spawn(exe, [], { stdio: ["ignore", "ignore", "inherit"], env: { ...process.env, ASCTRL_RELAY_URL: `ws://localhost:${PORT}`, ASCTRL_CODE: CODE } });

const tech = new TechClient(`ws://localhost:${PORT}`, "Bench");
const latency = new Map<string, number[]>();

/** Call a tool; throws on failure, so a task stops at its first broken step */
async function call(tool: ToolName, args: Record<string, unknown> = {}): Promise<ToolResult & { ms: number }> {
  const t = performance.now();
  const r = await tech.call(tool, args, 60_000);
  const ms = performance.now() - t;
  latency.set(tool, [...(latency.get(tool) ?? []), ms]);
  if (!r.ok) throw new Error(`${tool}: ${r.error}`);
  return { ...r, ms };
}

/** The id of the control whose listing line contains `label` */
function idOf(listing: string | undefined, label: string): number {
  const line = (listing ?? "").split("\n").find((l) => /\[\d+\]/.test(l) && l.includes(label));
  const m = line?.match(/\[(\d+)\]/);
  if (!m) throw new Error(`no control "${label}" in:\n${listing}`);
  return Number(m[1]);
}

/** @(x,y) of the control whose listing line contains `label` */
function posOf(listing: string | undefined, label: string): { x: number; y: number } {
  const line = (listing ?? "").split("\n").find((l) => /\[\d+\]/.test(l) && l.includes(label));
  const m = line?.match(/@\((\d+),(\d+)\)/);
  if (!m) throw new Error(`no position for "${label}" in:\n${listing}`);
  return { x: Number(m[1]), y: Number(m[2]) };
}

function expect(cond: boolean, what: string) {
  if (!cond) throw new Error(what);
}

type Row = { name: string; ok: boolean; ms: number; note: string };
const rows: Row[] = [];

async function task(name: string, run: () => Promise<string | void>) {
  const t = performance.now();
  try {
    const note = (await run()) ?? "";
    rows.push({ name, ok: true, ms: performance.now() - t, note });
    console.log(`✔ ${name} — ${Math.round(performance.now() - t)} ms ${note}`);
  } catch (e) {
    rows.push({ name, ok: false, ms: performance.now() - t, note: (e as Error).message.split("\n")[0] });
    console.log(`✘ ${name} — ${(e as Error).message}`);
  }
}

const folder = path.join(tmpdir(), `asctrl-bench-${Date.now() % 100000}`);

try {
  // The agent registers with the relay a moment after it starts
  for (let i = 0; ; i++) {
    try {
      await tech.connect(CODE);
      break;
    } catch (e) {
      if (i > 30) throw e;
      await new Promise((r) => setTimeout(r, 500));
    }
  }

  await task("Open a program and wait until it is ready", async () => {
    const o = await call("open_app", { target: "powershell", args: `-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "${benchApp}"` });
    const w = await call("wait_for", { window: WIN, timeoutSec: 20 });
    return `(open_app ${Math.round(o.ms)} ms, then wait_for ${Math.round(w.ms)} ms)`;
  });

  let listing = "";
  await task("Fill a form and submit it (UI Automation)", async () => {
    listing = (await call("wait_for", { element: "Submit", title: WIN, timeoutSec: 5 })).text ?? "";
    await call("element_action", {
      actions: [
        { id: idOf(listing, '"Name"'), action: "set_value", value: "Alice 测试" },
        { id: idOf(listing, '"Submit"'), action: "click" },
      ],
      sequence: true,
    });
    await call("wait_for", { element: "Submitted: Alice 测试", title: WIN, timeoutSec: 3 });
  });

  await task("Type 500 characters (mixed English / Chinese)", async () => {
    const sample = ("Quick brown fox 敏捷的狐狸 0123456789. ").repeat(20).slice(0, 500);
    await call("element_action", { id: idOf(listing, '"Notes"'), action: "focus" });
    const t = await call("type_text", { text: sample });
    // The text action answers "name\nvalue" for a named box
    const got = ((await call("element_action", { id: idOf(listing, '"Notes"'), action: "text" })).text ?? "").replace(/^Notes\n/, "");
    expect(got.replace(/\r/g, "") === sample, `typed text differs: got ${got.length} chars: ${JSON.stringify(got.slice(0, 90))}`);
    return `(${Math.round((sample.length / t.ms) * 1000)} chars/s; ${t.text?.replace(/^Typed\. /, "")})`;
  });

  await task("Click a checkbox by coordinates, change is reported", async () => {
    await call("screenshot", { maxWidth: 1600 });
    const l = (await call("list_elements", { title: WIN, query: "Agree" })).text;
    const p = posOf(l, "Agree");
    const c = await call("click", p);
    expect(/Screen changed/.test(c.text ?? ""), `no change reported: ${c.text}`);
    const after = (await call("list_elements", { title: WIN, query: "Agree" })).text ?? "";
    expect(/[(,] ?checked/.test(after), `checkbox not checked: ${after.split("\n").pop()}`);
    return `(click ${Math.round(c.ms)} ms)`;
  });

  await task("A click that does nothing is flagged", async () => {
    const l = (await call("list_elements", { title: WIN, query: "Submitted" })).text;
    const c = await call("click", posOf(l, "Submitted"));
    expect(/No visible change/.test(c.text ?? ""), `expected "No visible change", got: ${c.text}`);
    return `(${Math.round(c.ms)} ms)`;
  });

  await task("Read text UI Automation can't see (OCR)", async () => {
    const f = await call("find_text", { text: "CANVAS-7391", title: WIN });
    return `(${Math.round(f.ms)} ms)`;
  });

  await task("Click text on a canvas (click_text)", async () => {
    const c = await call("click_text", { text: "CANVAS-7391", title: WIN });
    await call("wait_for", { element: "Canvas clicks: 1", title: WIN, timeoutSec: 3 });
    return `(${Math.round(c.ms)} ms)`;
  });

  await task("Wait for a dialog that opens late, close it", async () => {
    const l = (await call("list_elements", { title: WIN, query: "Open dialog" })).text;
    await call("element_action", { id: idOf(l, "Open dialog") });
    const w = await call("wait_for", { window: "Bench Dialog", timeoutSec: 5 });
    await call("click_text", { text: "Close me", title: "Bench Dialog" });
    await call("wait_for", { window: "Bench Dialog", gone: true, timeoutSec: 3 });
    return `(dialog noticed ${Math.round(w.ms)} ms after asking; it opens at ~800 ms)`;
  });

  await task("Batch: 4 steps in one call", async () => {
    const b = await call("batch", {
      steps: [
        { tool: "click_text", text: "CANVAS-7391", title: WIN },
        { tool: "click_text", text: "CANVAS-7391", title: WIN },
        { tool: "click_text", text: "Submit", title: WIN },
        { tool: "wait_for", element: "Canvas clicks: 3", title: WIN, timeoutSec: 3 },
      ],
    });
    return `(${Math.round(b.ms)} ms)`;
  });

  await task("Open a folder in Explorer, find a file, close it", async () => {
    mkdirSync(folder, { recursive: true });
    writeFileSync(path.join(folder, "bench-file-4821.txt"), "hello");
    const name = path.basename(folder);
    await call("open_app", { target: folder });
    await call("wait_for", { element: "bench-file-4821", title: name, timeoutSec: 10 });
    await call("focus_window", { title: name, action: "close" });
    await call("wait_for", { window: name, gone: true, timeoutSec: 5 });
  });

  await task("Close the program", async () => {
    await call("focus_window", { title: WIN, action: "close" });
    await call("wait_for", { window: WIN, gone: true, timeoutSec: 5 });
  });
} finally {
  // Leave nothing open even when a task failed half-way
  await tech.call("focus_window", { title: "Bench Dialog", action: "close" }, 5000).catch(() => {});
  await tech.call("focus_window", { title: WIN, action: "close" }, 5000).catch(() => {});
  await tech.call("focus_window", { title: path.basename(folder), action: "close" }, 5000).catch(() => {});
  agent.kill();
  relay.close();
  rmSync(folder, { recursive: true, force: true });
}

const passed = rows.filter((r) => r.ok).length;
const total = rows.reduce((s, r) => s + r.ms, 0);
console.log(`\n${passed}/${rows.length} tasks passed, ${(total / 1000).toFixed(1)} s in all\n`);
console.log("Average time per tool call:");
for (const [tool, ms] of [...latency].sort()) {
  console.log(`  ${tool.padEnd(16)} ${Math.round(ms.reduce((a, b) => a + b, 0) / ms.length)} ms  (×${ms.length})`);
}
process.exit(passed === rows.length ? 0 : 1);
