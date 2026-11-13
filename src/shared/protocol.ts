// WebSocket message format between the three parties (agent / relay / tech).
// Every message is a JSON text frame.

export type ToolName =
  | "screenshot"
  | "system_info"
  | "list_processes"
  | "event_log"
  | "run_powershell"
  // The following are supported only by the GUI agent (agent-app)
  | "reset_shell"
  | "click"
  | "scroll"
  | "drag"
  | "type_text"
  | "press_keys"
  | "upload_file"
  | "download_file"
  | "request_admin"
  | "restart_computer"
  | "zoom"
  | "cursor_position"
  | "mouse_move"
  | "mouse_look"
  | "mouse_down"
  | "mouse_up"
  | "hold_key"
  | "list_windows"
  | "focus_window"
  | "list_elements"
  | "element_action"
  | "open_app"
  | "read_clipboard"
  | "write_clipboard"
  | "read_file"
  | "write_file"
  | "edit_file"
  | "list_dir"
  | "find_files"
  | "search_files"
  | "disk_usage"
  | "job_output"
  | "stop_job"
  | "find_text"
  | "click_text"
  | "wait_for"
  | "batch";

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

/** relay -> tech: paired */
export interface Paired {
  type: "paired";
  hostname: string;
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
