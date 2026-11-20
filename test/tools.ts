// The five tools (connect, query, act, exec, resume), through the MCP server as an AI client sees it.
// Usage: node test/tools.ts   (opens the bench window for about 30 s; leave the computer alone)
//
// 1. Five tools, every other one reached by name through query or act.
// 2. Wrong args for an action are refused with what it takes; help says what an action does.
// 3. On the bench window, only through the five: a plan (act run), a look (query observe), a single action
//    (act set_value), a screenshot numbered by references, the timeline, a program (exec).
// 4. The decision model inside programs (only with NACE_API_KEY, in .env or the environment): a program stuck on
//    words that fit no control exactly ("Open the dialog") is decided on the server's side and goes on, and the
//    reply says what was decided; one stuck on something that isn't there ("Erase all records") stays stuck for
//    the AI.

import { existsSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { Suite, root } from "./desk.ts";

const s = new Suite("Tools", { bench: true });
const { say } = s;
// A decision-model cache of its own, empty: the model is asked, not a cache
const cache = path.join(tmpdir(), `asctrl-tools-drex-${process.pid}.json`);
s.after(() => rmSync(cache, { force: true }));
const env = { ...process.env, RELAY_URL: "ws://127.0.0.1:1", TECH_NAME: "ToolsTest", ASCTRL_NO_LAUNCH: "1", ASCTRL_DREX_CACHE: cache } as Record<string, string>;
const envFile = path.join(root, ".env");
const model = !!process.env.NACE_API_KEY || (existsSync(envFile) && /^\s*NACE_API_KEY\s*=/m.test(readFileSync(envFile, "utf8")));

type Reply = { content: { type: string; text?: string; data?: string }[]; isError?: boolean };
const text = (r: Reply) => r.content.filter((c) => c.type === "text").map((c) => c.text).join("\n");
const c = new Client({ name: "tools-test", version: "1" });
s.after(() => c.close());
const call = async (name: string, args: Record<string, unknown>, timeout = 120_000) => (await c.callTool({ name, arguments: args }, undefined, { timeout })) as Reply;

await s.run(async () => {
  await c.connect(new StdioClientTransport({ command: process.execPath, args: [path.join(root, "src/console/mcp-server.ts")], env, stderr: "inherit" }));

  // 1.
  const five = (await c.listTools()).tools.map((t) => t.name).sort();
  say(five.join(",") === "act,connect,exec,query,resume", `five tools (${five.join(", ")})`);

  // 2.
  const bad = await call("act", { do: "click", args: { count: 9 } });
  const help = await call("act", { do: "run", help: true });
  say(!!bad.isError && /count/.test(text(bad)) && /Carry out a plan/.test(text(help)), `wrong args are refused with what the action takes; help describes it: ${text(bad).split("\n")[0]}`);

  // 3.
  const conn = await call("connect", { waitSec: 30 });
  say(!conn.isError, `connected: ${text(conn).split("\n")[0]}`);
  await call("query", { what: "wait_for", args: { appears: "Asctrl Bench", timeoutSec: 15 } });
  const denied = await call("query", { what: "skills", args: { forget: "fixture" } });
  say(!!denied.isError && /Use act/.test(text(denied)), "read-only query refuses skill writes");
  const writable = await call("act", { do: "skills", help: true });
  say(!writable.isError && /forget/.test(text(writable)), "skill writes are available through act");
  const plan = await call("act", { do: "run", args: { window: "Asctrl Bench", steps: [{ fill: { Name: "five" } }, { click: "Submit" }, { expect: "Submitted: five" }], wait: true } });
  say(!plan.isError && /✓/.test(text(plan)), `a plan through act: ${text(plan).split("\n")[0]}`);
  const look = await call("query", { what: "observe", args: { window: "Asctrl Bench", query: "Submitted" } });
  say(/Submitted: five/.test(text(look)), `a look through query: ${text(look).split("\n").find((l) => /Submitted/.test(l))?.trim()}`);
  const one = await call("act", { do: "set_value", args: { window: "Asctrl Bench", target: "Name", value: "single" } });
  say(!one.isError, `a single action through act: ${text(one).split("\n")[0]}`);
  const shot = await call("query", { what: "screenshot", args: { window: "Asctrl Bench", marks: true } }, 60_000);
  const ref = /^(\d+): e\1 Button "Submit"/m.exec(text(shot))?.[1];
  say(!!ref && shot.content.some((x) => x.type === "image"), `a screenshot with the controls numbered by their references (Submit is ${ref})`);
  const byRef = await call("act", { do: "click", args: { target: `e${ref}`, window: "Asctrl Bench" } });
  say(!byRef.isError, `the number on the picture is a target: ${text(byRef).split("\n")[0]}`);
  const tl = await call("query", { what: "timeline", args: { seconds: 120 } });
  say(/(shown|front) "Asctrl Bench"/.test(text(tl)), `the timeline says what came up: ${text(tl).split("\n").find((l) => /Asctrl Bench/.test(l))?.trim()}`);
  const prog = await call("exec", { script: `click('Submit', { window: 'Asctrl Bench' }); return read('Asctrl Bench').match(/Submitted: \\w+/)[0];`, learn: false, waitSec: 30 });
  say(/Submitted: single/.test(text(prog)), `a program through exec: ${/Result: .*/.exec(text(prog))?.[0]}`);

  // 4.
  if (!model) {
    console.log("- the decision model inside programs: not checked (no NACE_API_KEY in .env or the environment)");
    return;
  }
  let t = performance.now();
  const r1 = await call("exec", { script: `click('Open the dialog', { window: 'Asctrl Bench' }); waitFor('Bench Dialog'); click('Close me', { window: 'Bench Dialog' }); return 'closed';`, waitSec: 60 }, 180_000);
  say(!r1.isError && /Decided on the computer's side/.test(text(r1)) && /Open dialog/.test(text(r1)) && /closed/.test(text(r1)), `a program went on after the decision model chose for it (${Math.round(performance.now() - t)} ms): ${text(r1).split("\n").slice(0, 3).join(" | ")}`);
  t = performance.now();
  const r2 = await call("exec", { script: `click('Erase all records', { window: 'Asctrl Bench' }); return 'erased';`, waitSec: 60 }, 180_000);
  const body = text(r2);
  say(/stuck/i.test(body) && !/erased/.test(body), `a program stays stuck for the AI when the model isn't sure (${Math.round(performance.now() - t)} ms): ${body.split("\n").slice(0, 2).join(" | ")}`);
  const id = /program (\d+)|"id":\s*(\d+)|id[: ]+(\d+)/i.exec(body)?.slice(1).find(Boolean);
  if (id) await call("resume", { id: Number(id), stop: true });
});
