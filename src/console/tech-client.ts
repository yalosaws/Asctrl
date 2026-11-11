// 控制端连接库：连中继、按连接码配对、发送工具调用并等结果。
// MCP 服务器和测试脚本都用它。

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

  constructor(relayUrl: string, techName = "技术员", token?: string) {
    this.relayUrl = relayUrl;
    this.techName = techName;
    this.token = token;
  }

  get connected() {
    return this.ws?.readyState === WebSocket.OPEN && !!this.hostname;
  }

  /** 连接某个连接码，等对方点同意（最多等 consentTimeoutMs） */
  connect(code: string, consentTimeoutMs = 120_000): Promise<string> {
    this.disconnect();
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(this.relayUrl, { maxPayload: 32 * 1024 * 1024 });
      this.ws = ws;
      const timer = setTimeout(() => {
        reject(new Error("等待对方同意超时"));
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
        } else if (msg.type === "peer_left") {
          this.failAll("对方已断开");
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
        reject(new Error("连接已关闭"));
        // 旧连接晚到的 close 事件不能影响新连接
        if (this.ws !== ws) return;
        this.hostname = "";
        this.failAll("连接已关闭");
      });
      ws.on("error", (e) => reject(e));
    });
  }

  call(tool: ToolName, args: Record<string, unknown> = {}, timeoutMs = 660_000): Promise<ToolResult> {
    if (!this.connected || !this.ws) return Promise.reject(new Error("还没有连接到任何电脑，请先调用 connect"));
    const id = randomUUID();
    const ws = this.ws;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error("等待结果超时"));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (r) => (clearTimeout(timer), resolve(r)),
        reject: (e) => (clearTimeout(timer), reject(e)),
      });
      ws.send(JSON.stringify({ type: "call", id, tool, args } satisfies Message));
    });
  }

  disconnect() {
    this.ws?.close();
    this.ws = undefined;
    this.hostname = "";
  }

  private failAll(reason: string) {
    for (const p of this.pending.values()) p.reject(new Error(reason));
    this.pending.clear();
  }
}
