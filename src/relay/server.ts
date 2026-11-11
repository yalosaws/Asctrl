// 中继服务器：被控端和控制端都只主动连它，它按连接码把两边配对，然后原样转发消息。
// 用法：node src/relay/server.ts   （环境变量 PORT、RELAY_TOKEN 可选）

import { WebSocketServer, WebSocket } from "ws";
import { parse, type Message } from "../shared/protocol.ts";

const PORT = Number(process.env.PORT ?? 8787);
/** 设了这个值后，控制端必须带上相同 token 才能连接（防止陌生人猜连接码） */
const RELAY_TOKEN = process.env.RELAY_TOKEN ?? "";

interface Room {
  agent: WebSocket;
  hostname: string;
  tech?: WebSocket;
  /** 等待用户同意中 */
  pending?: WebSocket;
}

const rooms = new Map<string, Room>();

function send(ws: WebSocket | undefined, msg: Message) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
}

export function startRelay(port = PORT) {
  const wss = new WebSocketServer({ port, maxPayload: 32 * 1024 * 1024 });

  wss.on("connection", (ws) => {
    let role: "agent" | "tech" | null = null;
    let code = "";

    ws.on("message", (raw) => {
      const msg = parse(raw);
      if (!msg) return;

      // ---- 第一条消息决定身份 ----
      if (!role) {
        if (msg.type === "agent_hello") {
          if (!/^\d{6}$/.test(msg.code) || rooms.has(msg.code)) {
            send(ws, { type: "error", message: "连接码无效或已被占用" });
            return ws.close();
          }
          role = "agent";
          code = msg.code;
          rooms.set(code, { agent: ws, hostname: msg.hostname });
          log(`被控端上线 code=${code} host=${msg.hostname}`);
          return;
        }
        if (msg.type === "tech_hello") {
          if (RELAY_TOKEN && msg.token !== RELAY_TOKEN) {
            send(ws, { type: "error", message: "token 不正确" });
            return ws.close();
          }
          const room = rooms.get(msg.code);
          if (!room) {
            send(ws, { type: "error", message: "找不到这个连接码，确认对方客户端已打开" });
            return ws.close();
          }
          if (room.tech || room.pending) {
            send(ws, { type: "error", message: "该电脑已有人在连接" });
            return ws.close();
          }
          role = "tech";
          code = msg.code;
          room.pending = ws;
          send(room.agent, { type: "consent_request", techName: msg.techName });
          log(`技术员 ${msg.techName} 请求连接 code=${code}，等待用户同意`);
          return;
        }
        return ws.close();
      }

      const room = rooms.get(code);
      if (!room) return;

      // ---- 用户同意/拒绝 ----
      if (role === "agent" && msg.type === "consent_reply") {
        const tech = room.pending;
        room.pending = undefined;
        if (!tech) return;
        if (msg.allow) {
          room.tech = tech;
          send(tech, { type: "paired", hostname: room.hostname });
          log(`code=${code} 用户已同意，配对成功`);
        } else {
          send(tech, { type: "error", message: "对方拒绝了连接" });
          tech.close();
          log(`code=${code} 用户拒绝了连接`);
        }
        return;
      }

      // ---- 配对后原样转发 ----
      if (role === "tech" && msg.type === "call" && room.tech === ws) {
        send(room.agent, msg);
      } else if (role === "agent" && msg.type === "result") {
        send(room.tech, msg);
      }
    });

    ws.on("close", () => {
      const room = rooms.get(code);
      if (!room) return;
      if (role === "agent" && room.agent === ws) {
        send(room.tech ?? room.pending, { type: "peer_left" });
        room.tech?.close();
        room.pending?.close();
        rooms.delete(code);
        log(`被控端下线 code=${code}`);
      } else if (role === "tech") {
        if (room.tech === ws) room.tech = undefined;
        if (room.pending === ws) room.pending = undefined;
        send(room.agent, { type: "peer_left" });
        log(`技术员断开 code=${code}`);
      }
    });
  });

  log(`中继服务器已启动 ws://0.0.0.0:${port}${RELAY_TOKEN ? "（已启用 token）" : ""}`);
  return wss;
}

function log(s: string) {
  console.log(`[relay ${new Date().toLocaleTimeString()}] ${s}`);
}

if (import.meta.main) startRelay();
