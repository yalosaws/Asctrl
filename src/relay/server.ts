// Relay server: the agent and the technician both connect out to it; it pairs them by code and forwards messages unchanged.
// Usage: node src/relay/server.ts   (PORT and RELAY_TOKEN env vars are optional)

import { WebSocketServer, WebSocket } from "ws";
import { parse, type Message } from "../shared/protocol.ts";

const PORT = Number(process.env.PORT ?? 8787);
/** When set, technicians must present the same token to connect (stops strangers guessing codes) */
const RELAY_TOKEN = process.env.RELAY_TOKEN ?? "";

interface Room {
  agent: WebSocket;
  hostname: string;
  tech?: WebSocket;
  /** Waiting for the user's consent */
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

      // ---- The first message decides the role ----
      if (!role) {
        if (msg.type === "agent_hello") {
          if (!/^\d{6}$/.test(msg.code) || rooms.has(msg.code)) {
            send(ws, { type: "error", message: "Invalid code, or the code is already in use" });
            return ws.close();
          }
          role = "agent";
          code = msg.code;
          rooms.set(code, { agent: ws, hostname: msg.hostname });
          log(`agent online code=${code} host=${msg.hostname}`);
          return;
        }
        if (msg.type === "tech_hello") {
          if (RELAY_TOKEN && msg.token !== RELAY_TOKEN) {
            send(ws, { type: "error", message: "Invalid token" });
            return ws.close();
          }
          const room = rooms.get(msg.code);
          if (!room) {
            send(ws, { type: "error", message: "No agent with this code; make sure the app is open on their computer" });
            return ws.close();
          }
          if (room.tech || room.pending) {
            send(ws, { type: "error", message: "Someone is already connected to this computer" });
            return ws.close();
          }
          role = "tech";
          code = msg.code;
          room.pending = ws;
          send(room.agent, { type: "consent_request", techName: msg.techName });
          log(`technician ${msg.techName} requested code=${code}, waiting for consent`);
          return;
        }
        return ws.close();
      }

      const room = rooms.get(code);
      if (!room) return;

      // ---- User allows / denies ----
      if (role === "agent" && msg.type === "consent_reply") {
        const tech = room.pending;
        room.pending = undefined;
        if (!tech) return;
        if (msg.allow) {
          room.tech = tech;
          send(tech, { type: "paired", hostname: room.hostname });
          log(`code=${code} user allowed, paired`);
        } else {
          send(tech, { type: "error", message: "The user declined the connection" });
          tech.close();
          log(`code=${code} user declined`);
        }
        return;
      }

      // ---- Once paired, forward as-is ----
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
        log(`agent offline code=${code}`);
      } else if (role === "tech") {
        if (room.tech === ws) room.tech = undefined;
        if (room.pending === ws) room.pending = undefined;
        send(room.agent, { type: "peer_left" });
        log(`technician disconnected code=${code}`);
      }
    });
  });

  log(`relay listening on ws://0.0.0.0:${port}${RELAY_TOKEN ? " (token required)" : ""}`);
  return wss;
}

function log(s: string) {
  console.log(`[relay ${new Date().toLocaleTimeString()}] ${s}`);
}

if (import.meta.main) startRelay();
