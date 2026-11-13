// Agent: runs on the computer that needs help.
// On start it shows a 6-digit code for the user to give you; when you connect, it asks the user to allow it.
// Usage: node src/agent/agent.ts [--relay ws://host:8787] [--auto-accept]

import os from "node:os";
import { randomInt } from "node:crypto";
import { WebSocket } from "ws";
import { parse, DEFAULT_RELAY_URL, type Message, type ToolCall } from "../shared/protocol.ts";
import { runTool } from "./tools.ts";

const args = process.argv.slice(2);
const flag = (name: string) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : undefined;
};
const RELAY_URL = flag("--relay") ?? process.env.RELAY_URL ?? DEFAULT_RELAY_URL;
const FIXED_CODE = flag("--code");
const randomCode = () => String(randomInt(0, 1_000_000)).padStart(6, "0");
let CODE = FIXED_CODE ?? randomCode();

function send(ws: WebSocket, msg: Message) {
  if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
}

function describe(call: ToolCall) {
  if (call.tool === "run_powershell") return `Run command:\n${String(call.args.script)}`;
  return `${call.tool} ${JSON.stringify(call.args)}`;
}

function connect() {
  const ws = new WebSocket(RELAY_URL, { maxPayload: 32 * 1024 * 1024 });

  ws.on("open", () => {
    send(ws, { type: "agent_hello", code: CODE, hostname: os.hostname() });
    console.log("========================================");
    console.log(`  Remote assist is ready. Your code is:  ${CODE}`);
    console.log("  Give this code to the person helping you.");
    console.log("  Close this window at any time to disconnect.");
    console.log("========================================");
  });

  ws.on("message", async (raw) => {
    const msg = parse(raw);
    if (!msg) return;
    switch (msg.type) {
      case "consent_request": {
        // Having the code is enough to connect
        send(ws, { type: "consent_reply", allow: true });
        console.log(`${msg.techName} connected. You will see every action they take here.`);
        break;
      }
      case "call": {
        console.log(`\n[technician] ${describe(msg)}`);
        const out = await runTool(msg).catch((e) => ({ ok: false, error: String(e) }));
        send(ws, { type: "result", id: msg.id, ...out });
        break;
      }
      case "peer_left":
        console.log("\nThe technician disconnected. The code is still valid for another connection.");
        break;
      case "error":
        console.error(`Error: ${msg.message}`);
        // The only error the relay sends an agent is a code collision: try another code next time
        if (!FIXED_CODE) CODE = randomCode();
        break;
    }
  });

  ws.on("close", () => {
    console.log("Lost the connection to the server; reconnecting in 5 seconds…");
    setTimeout(connect, 5000);
  });
  ws.on("error", (e) => console.error(`Failed to connect to the server: ${e.message}`));
}

connect();
