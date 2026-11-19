// Controller connection library: reaches a driver either directly on this machine or through the relay by
// code, sends tool calls and awaits results. Used by the MCP server and the test scripts.

import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { WebSocket } from "ws";
import { parse, type Message, type ToolName, type ToolResult } from "../shared/protocol.ts";

/** Where the driver on this machine says it is listening */
export interface LocalInfo {
  port: number;
  token: string;
  pid: number;
  exe?: string;
  version?: string;
  admin?: boolean;
}

export function localInfoPath(): string {
  // ASCTRL_LOCAL_FILE points at another driver's file (test runs use their own)
  if (process.env.ASCTRL_LOCAL_FILE) return process.env.ASCTRL_LOCAL_FILE;
  const base = process.env.LOCALAPPDATA ?? path.join(os.homedir(), "AppData", "Local");
  return path.join(base, "Asctrl", "local.json");
}

export function readLocalInfo(): LocalInfo | null {
  try {
    const v = JSON.parse(readFileSync(localInfoPath(), "utf8"));
    return typeof v?.port === "number" && typeof v?.token === "string" ? (v as LocalInfo) : null;
  } catch {
    return null;
  }
}

/** The driver exe to start when none is running: ASCTRL_DRIVER_EXE, the one that last ran here, or this repo's build */
export function findDriverExe(last?: string): string | null {
  const root = path.resolve(import.meta.dirname, "../..");
  const candidates = [
    process.env.ASCTRL_DRIVER_EXE,
    last,
    path.join(root, "agent-app/src-tauri/target/release/asctrl-agent.exe"),
    path.join(root, "agent-app/src-tauri/target/debug/asctrl-agent.exe"),
  ];
  return candidates.find((p): p is string => !!p && existsSync(p)) ?? null;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export class TechClient {
  private ws?: WebSocket;
  private pending = new Map<string, { resolve: (r: ToolResult) => void; reject: (e: Error) => void }>();
  hostname = "";
  /** The protocol the driver speaks (1: older than the version was sent) */
  protocol = 0;
  /** Connected straight to a driver on this machine (no relay) */
  local = false;
  /** Pid of the local driver we are connected to */
  localPid = 0;

  private relayUrl: string;
  /** Shown to the user on the computer's consent card */
  techName: string;
  private token?: string;

  constructor(relayUrl: string, techName = "Technician", token?: string) {
    this.relayUrl = relayUrl;
    this.techName = techName;
    this.token = token;
  }

  get connected() {
    return this.ws?.readyState === WebSocket.OPEN && !!this.hostname;
  }

  /** Connect through the relay to the computer showing `code` */
  async connect(code: string, timeoutMs = 120_000): Promise<string> {
    const url = new URL(this.relayUrl);
    const loopback = url.hostname === "localhost" || url.hostname === "[::1]" || /^127(?:\.\d{1,3}){3}$/.test(url.hostname);
    if (url.protocol !== "wss:" && !(url.protocol === "ws:" && loopback)) {
      throw new Error("Remote relays require wss:// with a trusted certificate");
    }
    return this.open(this.relayUrl, { type: "tech_hello", code, techName: this.techName, token: this.token }, timeoutMs, false);
  }

  /** Connect straight to the driver on this machine */
  async connectLocal(info: LocalInfo, timeoutMs = 5000): Promise<string> {
    const host = await this.open(`ws://127.0.0.1:${info.port}`, { type: "local_hello", token: info.token, name: this.techName }, timeoutMs, true);
    this.localPid = info.pid;
    return host;
  }

  /**
   * Connect to the driver on this machine, starting it when it is not running, and waiting up to `waitMs` for
   * it to come up. `notPid` skips a driver that is on its way out (after it restarted itself as administrator).
   */
  async connectLocalDriver(opts: { waitMs?: number; launch?: boolean; notPid?: number; exe?: string } = {}): Promise<string> {
    const { waitMs = 20_000, launch = true, notPid } = opts;
    const deadline = Date.now() + waitMs;
    let launched = false;
    let last = "The driver is not running on this computer";
    for (;;) {
      const info = readLocalInfo();
      if (info && info.pid !== notPid) {
        try {
          return await this.connectLocal(info);
        } catch (e) {
          last = (e as Error).message;
          // Someone else is driving: waiting will not help
          if (/already controlling/.test(last)) throw e;
        }
      }
      if (launch && !launched) {
        const exe = opts.exe ?? findDriverExe(info?.exe);
        if (!exe) throw new Error("The driver app is not running and was not found. Build it (cd agent-app && npx tauri build --no-bundle) or set ASCTRL_DRIVER_EXE to asctrl-agent.exe");
        spawn(exe, [], { detached: true, stdio: "ignore" }).unref();
        launched = true;
      }
      if (Date.now() + 300 > deadline) throw new Error(`Could not reach the driver on this computer: ${last}`);
      await sleep(300);
    }
  }

  private open(url: string, hello: Message, timeoutMs: number, local: boolean): Promise<string> {
    this.disconnect();
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(url, { maxPayload: 64 * 1024 * 1024 });
      this.ws = ws;
      const timer = setTimeout(() => {
        reject(new Error("Timed out waiting for the connection"));
        ws.close();
      }, timeoutMs);

      ws.on("open", () => ws.send(JSON.stringify(hello)));
      ws.on("message", (raw) => {
        const msg = parse(raw);
        if (!msg) return;
        if (msg.type === "paired") {
          clearTimeout(timer);
          this.hostname = msg.hostname;
          this.protocol = typeof msg.protocol === "number" ? msg.protocol : 1;
          this.local = local;
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
    this.local = false;
    // The old socket's close handler ignores itself once replaced, so calls still waiting must be failed here;
    // otherwise they hang until their own timeout (11 minutes)
    this.failAll("Disconnected");
  }

  private failAll(reason: string) {
    for (const p of [...this.pending.values()]) p.reject(new Error(reason));
    this.pending.clear();
  }
}
