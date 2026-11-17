// Where the time goes: the same few actions with and without the report, several times each.
// Usage: node test/prof.ts [--app <exe>]     (moves the mouse for a few seconds)

import { spawn } from "node:child_process";
import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { startRelay } from "../src/relay/server.ts";
import { TechClient } from "../src/console/tech-client.ts";
import type { ToolName } from "../src/shared/protocol.ts";

const PORT = 18790;
const root = path.resolve(import.meta.dirname, "..");
const appIdx = process.argv.indexOf("--app");
const exe = appIdx >= 0 ? process.argv[appIdx + 1] : path.join(root, "agent-app/src-tauri/target/release/asctrl-agent.exe");
const WIN = "Asctrl Bench";
const localFile = path.join(tmpdir(), `asctrl-prof-local-${process.pid}.json`);
process.env.ASCTRL_LOCAL_FILE = localFile;
// Programs that work are kept as skills by themselves: in a file of the test's own, not the computer's
process.env.ASCTRL_SKILLS_FILE ??= path.join(tmpdir(), `asctrl-test-skills-${process.pid}.json`);
// Its driver acts without asking the person first (L5's check has a test of its own: test/l5.ts)
process.env.ASCTRL_CONFIRM ??= "off";

const relay = startRelay(PORT);
const agent = spawn(exe, ["--code", "135790", "--relay", `ws://localhost:${PORT}`], { stdio: ["ignore", "ignore", "inherit"] });
const app = spawn("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(root, "test/bench-app.ps1")], { stdio: "ignore", windowsHide: true });
const tech = new TechClient(`ws://localhost:${PORT}`, "Prof");

async function time(label: string, tool: ToolName, args: Record<string, unknown>, n = 3) {
  const ms: number[] = [];
  let last = "";
  for (let i = 0; i < n; i++) {
    const t = performance.now();
    const r = await tech.call(tool, args, 30_000);
    ms.push(performance.now() - t);
    last = (r.text ?? r.error ?? "").split("\n")[0].slice(0, 110);
  }
  console.log(`${label.padEnd(46)} ${ms.map((m) => String(Math.round(m)).padStart(4)).join(" ")} ms   ${last}`);
}

try {
  await tech.connectLocalDriver({ launch: false, waitMs: 30_000 });
  await tech.call("wait_for", { appears: WIN, timeoutSec: 30, after: "none" });
  const obs = (await tech.call("observe", { window: WIN })).text ?? "";
  const ref = (label: string) => obs.split("\n").find((l) => l.includes(label))?.match(/\[(e\d+)\]/)?.[1] ?? "";
  const pos = (label: string) => {
    const m = obs.split("\n").find((l) => l.includes(label))?.match(/@\((\d+),(\d+)\)/);
    return { x: Number(m?.[1]), y: Number(m?.[2]) };
  };
  await time("observe (window)", "observe", { window: WIN });
  await time("screenshot (whole screen)", "screenshot", {});
  await time("observe, the windows only", "observe", { max: 1 });
  await time("click ref Submit, direct, after none", "click", { target: ref('Button "Submit"'), after: "none" });
  await time("click ref Submit, direct, after auto", "click", { target: ref('Button "Submit"') });
  await time("click text Submit, direct, after none", "click", { target: "Submit", window: WIN, after: "none" });
  await time("element_action press Submit, after none", "element_action", { target: ref('Button "Submit"'), after: "none" });
  await time("click ref Agree, direct (toggles), none", "click", { target: ref('CheckBox "Agree"'), after: "none" });
  await time("click ref Agree, mouse (toggles), none", "click", { target: ref('CheckBox "Agree"'), mode: "mouse", after: "none" }, 4);
  await time("click x,y Agree (toggles), none", "click", { ...pos('CheckBox "Agree"'), after: "none" }, 4);
  await time("click x,y Agree (toggles), auto", "click", pos('CheckBox "Agree"'), 4);
  await time("set_value Agree true (no-op twice), none", "set_value", { target: ref('CheckBox "Agree"'), value: true, after: "none" });
  await time("set_value Name, after none", "set_value", { target: ref('Edit "Name"'), value: "Zed", after: "none" });
  await time("type_text 20 chars into Notes, none", "type_text", { text: "hello world 1234567 ", target: ref('Edit "Notes"'), after: "none" });
  await time("type_text 300 chars (paste), none", "type_text", { text: "x".repeat(300), target: ref('Edit "Notes"'), replace: true, after: "none" });
  // What a paste leaves for the next action to wait on: the clipboard going back, which happens as soon as
  // the field is seen to hold the new text
  {
    const ms: number[] = [];
    for (let i = 0; i < 3; i++) {
      await tech.call("type_text", { text: String(i).repeat(300), target: ref('Edit "Notes"'), replace: true, after: "none" });
      const t = performance.now();
      await tech.call("set_value", { target: ref('CheckBox "Agree"'), value: i % 2 === 1, after: "none" });
      ms.push(performance.now() - t);
    }
    console.log(`${"set_value right after a paste, none".padEnd(46)} ${ms.map((m) => String(Math.round(m)).padStart(4)).join(" ")} ms`);
  }
  await time("press_keys shift, none", "press_keys", { keys: "shift", window: WIN, after: "none" });
  await time("read_text Notes", "read_text", { target: ref('Edit "Notes"') });
  await time("observe query CANVAS (OCR)", "observe", { query: "CANVAS-7391", window: WIN });
  await time("batch fast: 6 clicks x,y + keys", "batch", {
    pace: "fast",
    after: "none",
    steps: [pos('Edit "Name"'), pos('Edit "Notes"'), pos('Edit "Name"'), pos('Edit "Notes"')].map((p) => ({ tool: "click", ...p })).concat([{ tool: "press_keys", keys: "a b c" } as never]),
  });
} catch (e) {
  console.log(`failed: ${(e as Error).message}`);
} finally {
  await tech.call("focus_window", { window: WIN, action: "close", after: "none" }, 5000).catch(() => {});
  await new Promise((r) => setTimeout(r, 300));
  tech.disconnect();
  app.kill();
  agent.kill();
  relay.close();
  rmSync(localFile, { force: true });
  process.exit(0);
}
