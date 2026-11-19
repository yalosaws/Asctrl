// P4, second part, on a real desktop: a program stuck on words that fit no control exactly is decided by the
// decision model on the console's side (as a plan's step is) and goes on, before the AI hears of it.
// Usage: node test/p4b.ts   (needs NACE_API_KEY, in .env or the environment; opens the bench window ~20 s)
// Goes through the MCP server itself, where the decision is made, as an AI client would.
//
// 1. "Open the dialog" fits no control exactly; the model picks "Open dialog", the program finishes, and the
//    reply says what was decided.
// 2. "Erase all records" fits nothing there: the model is not sure (or names something that can't be undone):
//    the program stays stuck for the AI to decide.

import { driverExecutable, owned } from "./fixtures.ts";
import { spawn } from "node:child_process";
import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const root = path.resolve(import.meta.dirname, "..");
const exe = driverExecutable(root);
const localFile = path.join(tmpdir(), `asctrl-p4b-local-${process.pid}.json`);
const cache = path.join(tmpdir(), `asctrl-p4b-drex-${process.pid}.json`);
process.env.ASCTRL_LOCAL_FILE = localFile;
// Programs that work are kept as skills by themselves: in a file of the test's own, not the computer's
process.env.ASCTRL_SKILLS_FILE ??= path.join(tmpdir(), `asctrl-test-skills-${process.pid}.json`);
// Its driver acts without asking the person first (L5's check has a test of its own: test/l5.ts)
process.env.ASCTRL_CONFIRM ??= "off";
const agent = owned(() => spawn(exe, ["--relay", "ws://127.0.0.1:1"], { stdio: ["ignore", "ignore", "inherit"] }));
const bench = owned(() => spawn("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(root, "test/bench-app.ps1")], { stdio: "ignore", windowsHide: true }));
const env = { ...process.env, RELAY_URL: "ws://127.0.0.1:1", ASCTRL_LOCAL_FILE: localFile, ASCTRL_DREX_CACHE: cache, TECH_NAME: "P4bTest", ASCTRL_NO_LAUNCH: "1", ASCTRL_TOOLS: "full" } as Record<string, string>;
const mcp = new Client({ name: "p4b-test", version: "1" });
let failures = 0;
const say = (ok: boolean, line: string) => {
  if (!ok) failures++;
  console.log(`${ok ? "✓" : "✗"} ${line}`);
};
type Reply = { content: { type: string; text?: string }[]; structuredContent?: { program?: { id: number; state: string } }; isError?: boolean };
const text = (r: Reply) => r.content.filter((c) => c.type === "text").map((c) => c.text).join("\n");
const call = async (name: string, args: Record<string, unknown>) => (await mcp.callTool({ name, arguments: args }, undefined, { timeout: 180_000 })) as Reply;

try {
  await mcp.connect(new StdioClientTransport({ command: process.execPath, args: [path.join(root, "src/console/mcp-server.ts")], env, stderr: "inherit" }));
  const c = await call("connect", {});
  say(!c.isError, `connected: ${text(c).split("\n")[0]}`);
  await call("wait_for", { appears: "Asctrl Bench", timeoutSec: 15 });

  // 1.
  let t = performance.now();
  const r1 = await call("exec", { script: `click('Open the dialog', { window: 'Asctrl Bench' }); waitFor('Bench Dialog'); click('Close me', { window: 'Bench Dialog' }); return 'closed';`, waitSec: 60 });
  const t1 = Math.round(performance.now() - t);
  say(!r1.isError && /Decided on the computer's side/.test(text(r1)) && /Open dialog/.test(text(r1)) && /closed/.test(text(r1)), `a program went on after the decision model chose for it (${t1} ms): ${text(r1).split("\n").slice(0, 3).join(" | ")}`);

  // 2.
  t = performance.now();
  const r2 = await call("exec", { script: `click('Erase all records', { window: 'Asctrl Bench' }); return 'erased';`, waitSec: 60 });
  const body = text(r2);
  say(/stuck/i.test(body) && !/erased/.test(body), `a program stays stuck for the AI when the model isn't sure (${Math.round(performance.now() - t)} ms): ${body.split("\n").slice(0, 2).join(" | ")}`);
  const id = /program (\d+)|"id":\s*(\d+)|id[: ]+(\d+)/i.exec(body)?.slice(1).find(Boolean);
  if (id) await call("resume", { id: Number(id), stop: true });
} catch (e) {
  console.error(e);
  failures++;
} finally {
  console.log(failures ? `\n${failures} check(s) failed` : "\nAll checks passed");
  process.exitCode = failures ? 1 : 0;
  await mcp.close().catch(() => undefined);
  bench.kill();
  agent.kill();
  rmSync(localFile, { force: true });
  rmSync(cache, { force: true });
  setTimeout(() => process.exit(), 300);
}
