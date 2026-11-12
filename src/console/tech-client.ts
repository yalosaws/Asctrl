// Technician connection library: connects to the relay, pairs by code, sends tool calls and awaits results.
// Used by the MCP server and the test scripts.

import { randomUUID } from "node:crypto";
import { WebSocket } from "ws";
import { parse, type Message, type ToolName, type ToolResult } from "../shared/protocol.ts";

export class TechClient {
  private ws?: WebSocket;
  private pending = new Map<string, { resolve: (r: ToolResult) => void; reject: (e: Error) => void }>();
  hostname = "";

  private relayUrl: string;
  private techName: string;
  private token?: string;

  constructor(relayUrl: string, techName = "Technician", token?: string) {
    this.relayUrl = relayUrl;
    this.techName = techName;
    this.token = token;
  }

  get connected() {
    return this.ws?.readyState === WebSocket.OPEN && !!this.hostname;
  }

  /** Connect to a code and wait for the user to allow it (up to consentTimeoutMs) */
  connect(code: string, consentTimeoutMs = 120_000): Promise<string> {
    this.disconnect();
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(this.relayUrl, { maxPayload: 32 * 1024 * 1024 });
      this.ws = ws;
      const timer = setTimeout(() => {
        reject(new Error("Timed out waiting for the user to allow the connection"));
        ws.close();
      }, consentTimeoutMs);

      ws.on("open", () => {
        ws.send(JSON.stringify({ type: "tech_hello", code, techName: this.techName, token: this.token } satisfies Message));
      });
      ws.on("message", (raw) => {
        const msg = parse(raw);
        if (!msg) return;
        if (msg.type === "paired") {
          clearTimeout(timer);
          this.hostname = msg.hostname;
          resolve(msg.hostname);
        } else if (msg.type === "error") {
          clearTimeout(timer);
          reject(new Error(msg.message));
          ws.close();
        } else if (msg.type === "peer_left") {
          this.failAll("The remote computer disconnected");
          ws.close();
        } else if (msg.type === "result") {
          const p = this.pending.get(msg.id);
          if (p) {
            this.pending.delete(msg.id);
            p.resolve(msg);
          }
        }
      });
      ws.on("close", () => {
        clearTimeout(timer);
        reject(new Error("Connection closed"));
        // A late close event from an old connection must not affect a new one
        if (this.ws !== ws) return;
        this.hostname = "";
        this.failAll("Connection closed");
      });
      ws.on("error", (e) => reject(e));
    });
  }

  call(tool: ToolName, args: Record<string, unknown> = {}, timeoutMs = 660_000): Promise<ToolResult> {
    if (!this.connected || !this.ws) return Promise.reject(new Error("Not connected to any computer; call connect first"));
    const id = randomUUID();
    const ws = this.ws;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error("Timed out waiting for the result"));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (r) => (clearTimeout(timer), resolve(r)),
        reject: (e) => (clearTimeout(timer), this.pending.delete(id), reject(e)),
      });
      ws.send(JSON.stringify({ type: "call", id, tool, args } satisfies Message), (err) => {
        if (err) this.pending.get(id)?.reject(err);
      });
    });
  }

  disconnect() {
    this.ws?.close();
    this.ws = undefined;
    this.hostname = "";
    // The old socket's close handler ignores itself once replaced, so calls still waiting must be failed here;
    // otherwise they hang until their own timeout (11 minutes)
    this.failAll("Disconnected");
  }

  private failAll(reason: string) {
    for (const p of [...this.pending.values()]) p.reject(new Error(reason));
    this.pending.clear();
  }
}
