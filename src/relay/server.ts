// Relay server: the agent and the technician both connect out to it; it pairs them by code and forwards messages unchanged.
// Usage: node src/relay/server.ts   (PORT and RELAY_TOKEN env vars are optional)

import { timingSafeEqual } from "node:crypto";
import { WebSocketServer, WebSocket } from "ws";
import { parse, type Message } from "../shared/protocol.ts";

const PORT = Number(process.env.PORT ?? 8787);
/** When set, technicians must present the same token to connect (stops strangers guessing codes) */
const RELAY_TOKEN = process.env.RELAY_TOKEN ?? "";
/** How often every connection is pinged */
const PING_MS = 30_000;
/**
 * A connection that has sent nothing (not even a pong) for this long is dead: a laptop that went to
 * sleep or a dropped Wi-Fi never closes its socket. Without this the room stays, the code stays "in use",
 * and the agent can't register again when it comes back.
 */
const DEAD_MS = 90_000;
/** Names are shown to the person on the consent card; keep them short */
const MAX_NAME = 64;

interface Room {
  agent: WebSocket;
  hostname: string;
  tech?: WebSocket;
  /** Waiting for the user's consent */
  pending?: WebSocket;
}

function send(ws: WebSocket | undefined, msg: Message) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
}

function tokenMatches(given: unknown): boolean {
  const a = Buffer.from(typeof given === "string" ? given : "");
  const b = Buffer.from(RELAY_TOKEN);
  return a.length === b.length && timingSafeEqual(a, b);
}

const name = (v: unknown, fallback: string) => (typeof v === "string" && v.trim() ? v.trim().slice(0, MAX_NAME) : fallback);

export function startRelay(port = PORT) {
  // Rooms belong to this server instance, so two relays in one process (tests) never share codes
  const rooms = new Map<string, Room>();
  const lastSeen = new WeakMap<WebSocket, number>();
  const wss = new WebSocketServer({ port, maxPayload: 32 * 1024 * 1024 });

  const heartbeat = setInterval(() => {
    const now = Date.now();
    for (const ws of wss.clients) {
      if (now - (lastSeen.get(ws) ?? now) > DEAD_MS) ws.terminate();
      else if (ws.readyState === WebSocket.OPEN) ws.ping();
    }
  }, PING_MS);
  wss.on("close", () => clearInterval(heartbeat));

  wss.on("connection", (ws) => {
    let role: "agent" | "tech" | null = null;
    let code = "";
    const seen = () => lastSeen.set(ws, Date.now());
    seen();
    ws.on("pong", seen);
    ws.on("ping", seen);
    // Without a listener an error would crash the whole relay; "close" follows and does the cleanup
    ws.on("error", () => {});

    ws.on("message", (raw) => {
      seen();
      const msg = parse(raw);
      if (!msg) return;

      // ---- The first message decides the role ----
      if (!role) {
        if (msg.type === "agent_hello") {
          if (typeof msg.code !== "string" || !/^\d{6}$/.test(msg.code) || rooms.has(msg.code)) {
            send(ws, { type: "error", message: "Invalid code, or the code is already in use" });
            return ws.close();
          }
          role = "agent";
          code = msg.code;
          const hostname = name(msg.hostname, "unknown");
          rooms.set(code, { agent: ws, hostname });
          log(`agent online code=${code} host=${hostname}`);
          return;
        }
        if (msg.type === "tech_hello") {
          if (RELAY_TOKEN && !tokenMatches(msg.token)) {
            send(ws, { type: "error", message: "Invalid token" });
            return ws.close();
          }
          const room = typeof msg.code === "string" ? rooms.get(msg.code) : undefined;
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
          const techName = name(msg.techName, "Technician");
          send(room.agent, { type: "consent_request", techName });
          log(`technician ${techName} requested code=${code}, waiting for consent`);
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
        if (msg.allow === true) {
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
      } else if (role === "tech" && (room.tech === ws || room.pending === ws)) {
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
