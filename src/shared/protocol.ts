// 三方（被控端 agent / 中继 relay / 控制端 tech）之间的 WebSocket 消息格式。
// 所有消息都是 JSON 文本帧。

export type ToolName =
  | "screenshot"
  | "system_info"
  | "list_processes"
  | "event_log"
  | "run_powershell";

// ---- 握手阶段 ----

/** agent -> relay：注册，带上屏幕上显示给用户的连接码 */
export interface AgentHello {
  type: "agent_hello";
  code: string;
  hostname: string;
}

/** tech -> relay：请求连接某个连接码 */
export interface TechHello {
  type: "tech_hello";
  code: string;
  techName: string;
  token?: string;
}

/** relay -> agent：有技术员请求连接，需要用户同意 */
export interface ConsentRequest {
  type: "consent_request";
  techName: string;
}

/** agent -> relay：用户的决定 */
export interface ConsentReply {
  type: "consent_reply";
  allow: boolean;
}

/** relay -> tech：配对成功 */
export interface Paired {
  type: "paired";
  hostname: string;
}

/** relay -> 任一方：出错或对方断开 */
export interface ErrorMsg {
  type: "error";
  message: string;
}

export interface PeerLeft {
  type: "peer_left";
}

// ---- 配对后，relay 原样转发以下消息 ----

/** tech -> agent：调用一个工具 */
export interface ToolCall {
  type: "call";
  id: string;
  tool: ToolName;
  args: Record<string, unknown>;
}

/** agent -> tech：工具结果 */
export interface ToolResult {
  type: "result";
  id: string;
  ok: boolean;
  /** 文本结果 */
  text?: string;
  /** 图片结果（base64） */
  image?: { data: string; mimeType: string };
  error?: string;
}

export type Message =
  | AgentHello
  | TechHello
  | ConsentRequest
  | ConsentReply
  | Paired
  | ErrorMsg
  | PeerLeft
  | ToolCall
  | ToolResult;

export function parse(raw: unknown): Message | null {
  try {
    const msg = JSON.parse(String(raw));
    return msg && typeof msg.type === "string" ? (msg as Message) : null;
  } catch {
    return null;
  }
}

export const DEFAULT_RELAY_URL = "ws://localhost:8787";
