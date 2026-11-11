// 被控端：装在需要帮助的那台电脑上。
// 启动后显示一个 6 位连接码，用户把码告诉你；你连接时这边会询问用户是否同意。
// 用法：node src/agent/agent.ts [--relay ws://host:8787] [--auto-accept]

import os from "node:os";
import readline from "node:readline/promises";
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
/** 仅用于自动化测试：跳过用户确认 */
const AUTO_ACCEPT = args.includes("--auto-accept");
const CODE = flag("--code") ?? String(randomInt(0, 1_000_000)).padStart(6, "0");

function send(ws: WebSocket, msg: Message) {
  if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
}

async function askConsent(techName: string): Promise<boolean> {
  if (AUTO_ACCEPT) return true;
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout });
  try {
    const ans = await rl.question(`\n>>> 「${techName}」请求远程协助你的电脑，是否允许？(y/n) `);
    return ans.trim().toLowerCase().startsWith("y");
  } finally {
    rl.close();
  }
}

function describe(call: ToolCall) {
  if (call.tool === "run_powershell") return `执行命令：\n${String(call.args.script)}`;
  return `${call.tool} ${JSON.stringify(call.args)}`;
}

function connect() {
  const ws = new WebSocket(RELAY_URL, { maxPayload: 32 * 1024 * 1024 });

  ws.on("open", () => {
    send(ws, { type: "agent_hello", code: CODE, hostname: os.hostname() });
    console.log("========================================");
    console.log(`  远程协助已就绪，你的连接码是：  ${CODE}`);
    console.log("  把这个码告诉帮你修电脑的人。");
    console.log("  随时关闭这个窗口即可断开。");
    console.log("========================================");
  });

  ws.on("message", async (raw) => {
    const msg = parse(raw);
    if (!msg) return;
    switch (msg.type) {
      case "consent_request": {
        const allow = await askConsent(msg.techName);
        send(ws, { type: "consent_reply", allow });
        console.log(allow ? `已允许 ${msg.techName} 连接。你会在这里看到对方的每一个操作。` : "已拒绝。");
        break;
      }
      case "call": {
        console.log(`\n[对方操作] ${describe(msg)}`);
        const out = await runTool(msg).catch((e) => ({ ok: false, error: String(e) }));
        send(ws, { type: "result", id: msg.id, ...out });
        break;
      }
      case "peer_left":
        console.log("\n对方已断开。连接码仍然有效，可以再次连接。");
        break;
      case "error":
        console.error(`错误：${msg.message}`);
        break;
    }
  });

  ws.on("close", () => {
    console.log("与服务器的连接断开，5 秒后重连…");
    setTimeout(connect, 5000);
  });
  ws.on("error", (e) => console.error(`连接服务器失败：${e.message}`));
}

connect();
