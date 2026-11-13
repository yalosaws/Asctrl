// WebSocket message format between the three parties (agent / relay / tech).
// Every message is a JSON text frame.

export type ToolName =
  | "screenshot"
  | "system_info"
  | "list_processes"
  | "event_log"
  | "run_powershell"
  // The following are supported only by the driver app (agent-app)
  // Looking
  | "observe"
  | "zoom"
  | "find"
  | "read_text"
  | "list_windows"
  | "wait_for"
  // Acting
  | "click"
  | "scroll"
  | "drag"
  | "mouse"
  | "type_text"
  | "press_keys"
  | "set_value"
  | "element_action"
  | "batch"
  // Programs, windows, clipboard
  | "open_app"
  | "focus_window"
  | "read_clipboard"
  | "write_clipboard"
  // Shell, files, system
  | "reset_shell"
  | "job_output"
  | "stop_job"
  | "read_file"
  | "write_file"
  | "edit_file"
  | "list_dir"
  | "find_files"
  | "search_files"
  | "disk_usage"
  | "upload_file"
  | "download_file"
  | "request_admin"
  | "restart_computer";

// ---- Handshake ----

/** agent -> relay: register, with the code shown to the user on screen */
export interface AgentHello {
  type: "agent_hello";
  code: string;
  hostname: string;
}

/** tech -> relay: ask to connect to a code */
export interface TechHello {
  type: "tech_hello";
  code: string;
  techName: string;
  token?: string;
}

/** relay -> agent: a technician wants to connect; the user must consent */
export interface ConsentRequest {
  type: "consent_request";
  techName: string;
}

/** agent -> relay: the user's decision */
export interface ConsentReply {
  type: "consent_reply";
  allow: boolean;
}

/** controller -> driver, on the same machine (no relay): the token from %LOCALAPPDATA%\Asctrl\local.json */
export interface LocalHello {
  type: "local_hello";
  token: string;
  name: string;
}

/** relay -> tech, or driver -> local controller: paired */
export interface Paired {
  type: "paired";
  hostname: string;
  /** Connected straight to a driver on this machine */
  local?: boolean;
  /** The driver runs as administrator */
  admin?: boolean;
}

/** relay -> either side: an error, or the other side disconnected */
export interface ErrorMsg {
  type: "error";
  message: string;
}

export interface PeerLeft {
  type: "peer_left";
}

// ---- Once paired, the relay forwards these messages unchanged ----

/** tech -> agent: call a tool */
export interface ToolCall {
  type: "call";
  id: string;
  tool: ToolName;
  args: Record<string, unknown>;
}

/** agent -> tech: tool result */
export interface ToolResult {
  type: "result";
  id: string;
  ok: boolean;
  /** Text result */
  text?: string;
  /** Image result (base64) */
  image?: { data: string; mimeType: string };
  /** File result (download_file, base64) */
  file?: { name: string; data: string };
  error?: string;
}

export type Message =
  | AgentHello
  | TechHello
  | LocalHello
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
