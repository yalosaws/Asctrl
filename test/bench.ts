// Speed and reliability benchmark for the driver: real tasks on a real desktop, each timed and checked.
// Starts the driver app, connects to it directly (the local link, no relay), opens test/bench-app.ps1 (a window
// with known controls, a canvas only OCR can read, and a dialog that opens late) and an Explorer folder, and
// drives them through the tools the way an AI would: by target, reading each reply instead of looking again.
// Usage: npm run bench                     uses agent-app/src-tauri/target/release/asctrl-agent.exe
//        npm run bench -- --app <exe>       another build
// It moves the mouse and types: leave the computer alone for the ~20 seconds it runs.

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
const exe = appIdx >= 0 ? process.argv[appIdx + 1] : path.join(root, "agent-app/src-tauri/target/release/asctrl-agent.exe");
const benchApp = path.join(root, "test/bench-app.ps1");
const WIN = "Asctrl Bench";
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

// This run's driver writes its address to its own file, so a driver the user already has running is left alone
const localFile = path.join(tmpdir(), `asctrl-bench-local-${process.pid}.json`);
process.env.ASCTRL_LOCAL_FILE = localFile;

// The driver also registers with a relay; give it a local one rather than the hosted one
const relay = startRelay(PORT);
const agent = spawn(exe, ["--code", CODE, "--relay", `ws://localhost:${PORT}`], { stdio: ["ignore", "ignore", "inherit"], env: { ...process.env, ASCTRL_CODE: CODE } });
// Started directly, with no console of its own: nothing for a terminal app to take over
const app = spawn("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", benchApp], { stdio: "ignore", windowsHide: true });

const tech = new TechClient(`ws://localhost:${PORT}`, "Bench");
const latency = new Map<string, number[]>();

/** Call a tool; throws on failure, so a task stops at its first broken step */
async function call(tool: ToolName, args: Record<string, unknown> = {}): Promise<ToolResult & { ms: number; text: string }> {
  const t = performance.now();
  const r = await tech.call(tool, args, 60_000);
  const ms = performance.now() - t;
  latency.set(tool, [...(latency.get(tool) ?? []), ms]);
  if (!r.ok) throw new Error(`${tool}: ${r.error}`);
  return { ...r, ms, text: r.text ?? "" };
}

/** A call that is expected to fail: returns the failure */
async function refused(tool: ToolName, args: Record<string, unknown>): Promise<ToolResult & { ms: number }> {
  const t = performance.now();
  const r = await tech.call(tool, args, 60_000);
  if (r.ok) throw new Error(`${tool} was expected to be refused, but: ${r.text}`);
  return { ...r, ms: performance.now() - t };
}

/** @(x,y) on the line of a listing that contains `label` */
function posOf(listing: string, label: string): { x: number; y: number } {
  const line = listing.split("\n").find((l) => /\[e\d+\]/.test(l) && l.includes(label));
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
const folderName = path.basename(folder);

try {
  await tech.connectLocalDriver({ launch: false, waitMs: 30_000 });
  console.log(`connected directly to the driver on ${tech.hostname}`);

  await task("Wait for a program's window; its controls come back listed", async () => {
    const w = await call("wait_for", { appears: WIN, timeoutSec: 30 });
    expect(/\[e\d+\] Button "Submit"/.test(w.text), `controls not listed:\n${w.text}`);
    return `(noticed in ${Math.round(w.ms)} ms)`;
  });

  await task("Fill a form and submit it: 2 calls, the reply shows the result", async () => {
    await call("set_value", { target: "Name", value: "Alice 测试", window: WIN });
    const c = await call("click", { target: "Submit", window: WIN });
    expect(c.text.includes("Submitted: Alice 测试"), `the reply does not show the outcome:\n${c.text}`);
    expect(!c.image, "a screenshot came back although the controls told the story");
    return `(click ${Math.round(c.ms)} ms, pressed directly)`;
  });

  await task("Type 500 characters (mixed English / Chinese) into a named field", async () => {
    const sample = "Quick brown fox 敏捷的狐狸 0123456789. ".repeat(20).slice(0, 500);
    // Long text goes in through the clipboard, which must come out of it holding what it held
    const held = await tech.call("clipboard");
    const mine = `what was on the clipboard ${Date.now()}`;
    await call("clipboard", { text: mine });
    const notes = async () => (await call("read_text", { target: "Notes", window: WIN })).text.replace(/^Notes\n/, "").replace(/\r/g, "");
    const t = await call("type_text", { text: sample, target: "Notes", window: WIN, replace: true });
    const got = await notes();
    expect(got === sample, `typed text differs: got ${got.length} chars: ${JSON.stringify(got.slice(0, 90))}`);
    const clip = (await call("clipboard")).text.trim();
    expect(clip === mine, `the clipboard was not put back: ${JSON.stringify(clip.slice(0, 60))}`);
    // The same text again: the field reading it already proves nothing, so the paste must still get its time
    await call("type_text", { text: sample, target: "Notes", window: WIN, replace: true });
    await call("type_text", { text: sample.slice(0, 250), target: "Notes", window: WIN });
    const twice = await notes();
    expect(twice === sample + sample.slice(0, 250), `pasting again went wrong: ${twice.length} chars: ${JSON.stringify(twice.slice(0, 90))}`);
    expect((await call("clipboard")).text.trim() === mine, "the clipboard was not put back after pasting again");
    // The benchmark borrowed the real clipboard: hand back the text it held
    if (held.ok && held.text && !held.text.startsWith("[") && !held.text.startsWith("(")) await call("clipboard", { text: held.text });
    return `(${Math.round((sample.length / t.ms) * 1000)} chars/s end to end)`;
  });

  await task("Tick a checkbox by value; doing it again changes nothing", async () => {
    const a = await call("set_value", { target: "Agree", value: true, window: WIN });
    expect(/is checked/.test(a.text), a.text);
    const b = await call("set_value", { target: "Agree", value: true, window: WIN });
    expect(/is checked/.test(b.text) && /No control changed/.test(b.text), `second call should change nothing:\n${b.text}`);
    return `(${Math.round(a.ms)} ms, then ${Math.round(b.ms)} ms)`;
  });

  let listing = "";
  await task("Click by coordinates read before the screenshot was taken; the change is reported", async () => {
    // Positions given before any screenshot are on the picture a plain screenshot then gives
    listing = (await call("observe", { window: WIN })).text;
    const s = await call("screenshot", {});
    expect(!/framed differently/.test(s.text), `the first plain screenshot changed what positions mean: ${s.text}`);
    const c = await call("click", posOf(listing, 'CheckBox "Agree"'));
    expect(/Screen changed/.test(c.text), `no screen change reported: ${c.text}`);
    expect(/CheckBox "Agree": now unchecked/.test(c.text), `the control change is not in the reply:\n${c.text}`);
    return `(click ${Math.round(c.ms)} ms)`;
  });

  await task("A click that does nothing is flagged", async () => {
    const c = await call("click", { ...posOf(listing, "Submitted"), after: "none" });
    expect(/No visible change/.test(c.text), `expected "No visible change", got: ${c.text}`);
    return `(${Math.round(c.ms)} ms)`;
  });

  await task("A screenshot framed differently says that earlier positions no longer apply", async () => {
    const w = await call("screenshot", { window: WIN });
    expect(/framed differently/.test(w.text), `no warning on a window screenshot: ${w.text}`);
    const again = await call("screenshot", { window: WIN });
    expect(!/framed differently/.test(again.text), `warned although the picture is framed the same: ${again.text}`);
    // On the window's own picture, positions are within it
    const p = posOf((await call("observe", { window: WIN })).text, 'CheckBox "Agree"');
    const c = await call("click", { ...p, after: "ui" });
    expect(/CheckBox "Agree": now checked/.test(c.text), `the click on the window's picture missed:\n${c.text}`);
    const back = await call("screenshot", {});
    expect(/framed differently/.test(back.text), `no warning going back to the whole screen: ${back.text}`);
  });

  await task("A click aimed at a stale screenshot follows a moved window, and is refused after a resize", async () => {
    // What the AI last saw: the bench window where it was
    await call("screenshot", {});
    listing = (await call("observe", { window: WIN })).text;
    const p = posOf(listing, 'Button "Submit"');
    const m = listing.match(/"Asctrl Bench"[^\n]*?\((\d+),(\d+)\)-\((\d+),(\d+)\)/);
    if (!m) throw new Error(`no window position in:\n${listing.split("\n")[0]}`);
    const [l, t, r, b] = m.slice(1).map(Number);
    // Unseen by the AI, the window moves, and the name changes so a press of Submit shows
    await call("set_value", { target: "Name", value: "Bob", window: WIN, after: "none" });
    await call("focus_window", { window: WIN, action: "move", x: l + 40, y: t + 30, after: "none" });
    const c = await call("click", { ...p, after: "ui" });
    expect(/moved with it/.test(c.text), `the click did not say it followed the window: ${c.text}`);
    expect((await call("read_text", { window: WIN })).text.includes("Submitted: Bob"), "the click did not land on Submit after the move");
    // A look, then an unseen resize: what was at that point is somewhere else now
    await call("screenshot", {});
    const p2 = posOf((await call("observe", { window: WIN })).text, 'Button "Submit"');
    await call("set_value", { target: "Name", value: "Carol", window: WIN, after: "none" });
    await call("focus_window", { window: WIN, action: "move", width: r - l + 60, height: b - t + 40, after: "none" });
    const no = await refused("click", p2);
    expect(/resized/.test(no.error ?? ""), `wrong reason: ${no.error}`);
    expect(!!no.image, "no fresh screenshot came with the refusal");
    expect((await call("read_text", { window: WIN })).text.includes("Submitted: Bob"), "the refused click landed anyway");
    return `(followed in ${Math.round(c.ms)} ms; refused in ${Math.round(no.ms)} ms, with a fresh screenshot)`;
  });

  await task("Find text only the pixels show (OCR)", async () => {
    const f = await call("observe", { query: "CANVAS-7391", window: WIN });
    expect(/On the pixels/.test(f.text), f.text);
    return `(${Math.round(f.ms)} ms)`;
  });

  await task("Click text on a canvas by naming it; the reply shows the counter move", async () => {
    const c = await call("click", { target: "CANVAS-7391", window: WIN });
    expect(/Canvas clicks: 1/.test(c.text), `the counter change is not in the reply:\n${c.text}`);
    return `(${Math.round(c.ms)} ms)`;
  });

  await task("Wait for a dialog that opens late, close it", async () => {
    await call("click", { target: "Open dialog", window: WIN, after: "none" });
    const w = await call("wait_for", { appears: "Bench Dialog", timeoutSec: 5 });
    expect(/Button "Close me"/.test(w.text), `the dialog's controls are not listed:\n${w.text}`);
    await call("click", { target: "Close me", window: "Bench Dialog", after: "none" });
    await call("wait_for", { appears: "Bench Dialog", gone: true, timeoutSec: 3 });
    return `(dialog noticed ${Math.round(w.ms)} ms after asking; it opens at ~800 ms)`;
  });

  await task("Batch: 4 steps in one call, one report", async () => {
    const b = await call("batch", {
      steps: [
        { tool: "click", target: "CANVAS-7391", window: WIN },
        { tool: "click", target: "CANVAS-7391", window: WIN },
        { tool: "click", target: "Submit", window: WIN },
        { tool: "wait_for", text: "Canvas clicks: 3", window: WIN, timeoutSec: 3 },
      ],
    });
    expect(/All 4 steps done/.test(b.text), b.text);
    return `(${Math.round(b.ms)} ms)`;
  });

  await task("Open a folder in Explorer, see the file in the reply, close it", async () => {
    mkdirSync(folder, { recursive: true });
    writeFileSync(path.join(folder, "bench-file-4821.txt"), "hello");
    const o = await call("open_app", { target: folder });
    let seen = o.text.includes("bench-file-4821");
    if (!seen) seen = (await call("wait_for", { text: "bench-file-4821", window: folderName, timeoutSec: 10 })).ok;
    expect(seen, "the file never showed");
    await call("focus_window", { window: folderName, action: "close", after: "none" });
    await call("wait_for", { appears: folderName, gone: true, timeoutSec: 5 });
    return `(open_app ${Math.round(o.ms)} ms${o.text.includes("bench-file-4821") ? ", file already in its reply" : ""})`;
  });

  await task("Close the program", async () => {
    await call("focus_window", { window: WIN, action: "close", after: "none" });
    await call("wait_for", { appears: WIN, gone: true, timeoutSec: 5 });
  });
} catch (e) {
  console.log(`✘ could not run: ${(e as Error).message}`);
  rows.push({ name: "setup", ok: false, ms: 0, note: String(e) });
} finally {
  // Leave nothing open even when a task failed half-way
  for (const w of ["Bench Dialog", WIN, folderName]) await tech.call("focus_window", { window: w, action: "close", after: "none" }, 5000).catch(() => {});
  await sleep(300);
  tech.disconnect();
  app.kill();
  agent.kill();
  relay.close();
  rmSync(folder, { recursive: true, force: true });
  rmSync(localFile, { force: true });
}

const passed = rows.filter((r) => r.ok).length;
const total = rows.reduce((s, r) => s + r.ms, 0);
console.log(`\n${passed}/${rows.length} tasks passed, ${(total / 1000).toFixed(1)} s in all\n`);
console.log("Average time per tool call:");
for (const [tool, ms] of [...latency].sort()) {
  console.log(`  ${tool.padEnd(16)} ${Math.round(ms.reduce((a, b) => a + b, 0) / ms.length)} ms  (×${ms.length})`);
}
process.exit(passed === rows.length ? 0 : 1);
