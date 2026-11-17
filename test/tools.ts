// The five-tool surface (connect, query, act, exec, resume), through the MCP server as an AI client sees it.
// Usage: node test/tools.ts   (opens the bench window for about 15 s; leave the computer alone)
//
// 1. Five tools by default, every other one reached by name through query or act; full lists them all.
// 2. Wrong args for an action are refused with what it takes; help says what an action does.
// 3. On the bench window, only through the five: a plan (act run), a look (query observe), a single action
//    (act set_value), a program (exec).

import { spawn } from "node:child_process";
import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const root = path.resolve(import.meta.dirname, "..");
const exe = path.join(root, "agent-app/src-tauri/target/release/asctrl-agent.exe");
const localFile = path.join(tmpdir(), `asctrl-tools-local-${process.pid}.json`);
const env = { ...process.env, RELAY_URL: "ws://127.0.0.1:1", ASCTRL_LOCAL_FILE: localFile, TECH_NAME: "ToolsTest", ASCTRL_NO_LAUNCH: "1", ASCTRL_CONFIRM: "off", ASCTRL_SKILLS_FILE: path.join(tmpdir(), `asctrl-tools-skills-${process.pid}.json`) } as Record<string, string>;
let failures = 0;
const say = (ok: boolean, line: string) => {
  if (!ok) failures++;
  console.log(`${ok ? "✓" : "✗"} ${line}`);
};
type Reply = { content: { type: string; text?: string }[]; isError?: boolean };
const text = (r: Reply) => r.content.filter((c) => c.type === "text").map((c) => c.text).join("\n");
const open = async (mode?: string) => {
  const c = new Client({ name: "tools-test", version: "1" });
  await c.connect(new StdioClientTransport({ command: process.execPath, args: [path.join(root, "src/console/mcp-server.ts")], env: mode ? { ...env, ASCTRL_TOOLS: mode } : env, stderr: "inherit" }));
  return c;
};

let agent: ReturnType<typeof spawn> | undefined;
let bench: ReturnType<typeof spawn> | undefined;
const c = await open();
const call = async (name: string, args: Record<string, unknown>) => (await c.callTool({ name, arguments: args }, undefined, { timeout: 120_000 })) as Reply;
try {
  // 1.
  const five = (await c.listTools()).tools.map((t) => t.name).sort();
  const full = await open("full");
  const all = (await full.listTools()).tools.length;
  await full.close();
  say(five.join(",") === "act,connect,exec,query,resume" && all > 30, `five tools by default (${five.join(", ")}); full lists ${all}`);

  // 2.
  const bad = await call("act", { do: "click", args: { count: 9 } });
  const help = await call("act", { do: "run", help: true });
  say(!!bad.isError && /count/.test(text(bad)) && /Carry out a plan/.test(text(help)), `wrong args are refused with what the action takes; help describes it: ${text(bad).split("\n")[0]}`);

  // 3.
  agent = spawn(exe, ["--relay", "ws://127.0.0.1:1"], { stdio: ["ignore", "ignore", "inherit"], env });
  bench = spawn("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(root, "test/bench-app.ps1")], { stdio: "ignore", windowsHide: true });
  const conn = await call("connect", { waitSec: 30 });
  say(!conn.isError, `connected: ${text(conn).split("\n")[0]}`);
  await call("query", { what: "wait_for", args: { appears: "Asctrl Bench", timeoutSec: 15 } });
  const plan = await call("act", { do: "run", args: { window: "Asctrl Bench", steps: [{ fill: { Name: "five" } }, { click: "Submit" }, { expect: "Submitted: five" }], wait: true } });
  say(!plan.isError && /✓/.test(text(plan)), `a plan through act: ${text(plan).split("\n")[0]}`);
  const look = await call("query", { what: "observe", args: { window: "Asctrl Bench", query: "Submitted" } });
  say(/Submitted: five/.test(text(look)), `a look through query: ${text(look).split("\n").find((l) => /Submitted/.test(l))?.trim()}`);
  const one = await call("act", { do: "set_value", args: { window: "Asctrl Bench", target: "Name", value: "single" } });
  say(!one.isError, `a single action through act: ${text(one).split("\n")[0]}`);
  const shot = (await c.callTool({ name: "query", arguments: { what: "screenshot", args: { window: "Asctrl Bench", marks: true } } }, undefined, { timeout: 60_000 })) as Reply & { content: { type: string; data?: string }[] };
  const marked = text(shot);
  const ref = /^(\d+): e\1 Button "Submit"/m.exec(marked)?.[1];
  say(!!ref && shot.content.some((x) => x.type === "image"), `a screenshot with the controls numbered by their references (Submit is ${ref})`);
  const byRef = await call("act", { do: "click", args: { target: `e${ref}`, window: "Asctrl Bench" } });
  say(!byRef.isError, `the number on the picture is a target: ${text(byRef).split("\n")[0]}`);
  const tl = await call("query", { what: "timeline", args: { seconds: 120 } });
  say(/(shown|front) "Asctrl Bench"/.test(text(tl)), `the timeline says what came up: ${text(tl).split("\n").find((l) => /Asctrl Bench/.test(l))?.trim()}`);
  const prog = await call("exec", { script: `click('Submit', { window: 'Asctrl Bench' }); return read('Asctrl Bench').match(/Submitted: \\w+/)[0];`, learn: false, waitSec: 30 });
  say(/Submitted: single/.test(text(prog)), `a program through exec: ${/Result: .*/.exec(text(prog))?.[0]}`);
} catch (e) {
  console.error(e);
  failures++;
} finally {
  console.log(failures ? `\n${failures} check(s) failed` : "\nAll checks passed");
  process.exitCode = failures ? 1 : 0;
  await c.close().catch(() => undefined);
  bench?.kill();
  agent?.kill();
  rmSync(localFile, { force: true });
  setTimeout(() => process.exit(), 300);
}
