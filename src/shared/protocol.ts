// WebSocket message format between the three parties (agent / relay / tech).
// Every message is a JSON text frame.

export type ToolName =
  | "screenshot"
  | "run_powershell"
  // The following are supported only by the driver app (agent-app)
  // Looking
  | "observe"
  | "read_text"
  | "wait_for"
  | "read_grid"
  | "watch"
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
  /** A program: runs on the computer, comes back when it ends or needs the AI (see ProgramData) */
  | "exec"
  | "resume"
  /** What happened to the windows lately (the timeline) */
  | "timeline"
  /** Programs that worked, kept to run again (exec with learn / skill) */
  | "skills"
  /** A plan (run) starts or ends: tells the overlay, does nothing else */
  | "plan_mark"
  // Programs, windows, clipboard
  | "open_app"
  | "focus_window"
  | "clipboard"
  | "job"
  | "list_files"
  // Shell, files, system
  | "read_file"
  | "write_file"
  | "edit_file"
  | "search_files"
  | "disk_usage"
  | "upload_file"
  | "download_file"
  | "request_admin"
  | "restart_computer";

// ---- Handshake ----

/**
 * The version of this protocol: the driver says which it speaks when it registers (and when a controller on the
 * same machine connects), and the controller is told. 2: typed data in replies, programs, the five-tool surface.
 * A driver that says nothing is older than 2.
 */
export const PROTOCOL = 2;

/** agent -> relay: register, with the code shown to the user on screen */
export interface AgentHello {
  type: "agent_hello";
  code: string;
  hostname: string;
  /** The protocol it speaks (see PROTOCOL) */
  protocol?: number;
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
  /** The protocol the driver speaks (see PROTOCOL); absent: older than 2 */
  protocol?: number;
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
  /** Structured results, for a program reading the reply: ActionData, BatchData, ObserveData. Programs read
   *  these, never the wording of text or error, which is written for the AI and may change. */
  data?: unknown;
}

// ---- Structured results (ToolResult.data) ----

/** A control offered when a target missed */
export interface Candidate {
  /** Its reference, "e12" */
  ref: string;
  role: string;
  name: string;
  /** What it is and how it stands, without where: `Button "Save" (disabled)` */
  label: string;
}

/** Why a target could not be pointed at */
export interface Miss {
  /** ambiguous: several things fit equally well; absent: nothing fits (or a list has no such item); stale: the
   *  screen is not what the AI saw; other: anything else */
  kind: "ambiguous" | "absent" | "stale" | "other";
  /** The controls to choose among: the equal matches, then (after a batch step's recovery) the closest ones */
  candidates: Candidate[];
  /** After a batch step's recovery: the window they are in */
  window?: { handle: number; program: string; title: string };
}

/** An action's reply (click, type_text, set_value, …) */
export interface ActionData {
  /** What the screen did; absent when it wasn't watched (fast mode) */
  effect?: "changed" | "unchanged";
  /** Set when the target missed */
  miss?: Miss;
}

/** One step of a batch */
export interface BatchStepResult {
  ok: boolean;
  text: string;
  /** It went in without waiting for the screen */
  fast: boolean;
  data?: ActionData;
  /** What the driver did to get the step through: "↷ …" lines of their own, "(…)" words on the step's line */
  notes?: string[];
}

/** batch's reply */
export interface BatchData {
  steps: BatchStepResult[];
  /** The step it stopped at */
  failed: number | null;
  /** When it failed: the failed step's window as observe reads it (unless asked where: false) */
  where?: string;
  /** The plan ended with it (asked with end): what the popup watcher did during the plan, one line each */
  ended?: string;
  /** Titled windows that came up, went away or came to the front while it ran (the last 30), from the driver's
   *  timeline; absent when the timeline is off */
  windows?: WindowEvent[];
}

/** Something that happened to a window, as the driver's timeline saw it */
export interface WindowEvent {
  /** Since the batch started */
  ms: number;
  kind: "shown" | "hidden" | "destroyed" | "front" | "minimized" | "restored";
  handle: number;
  program: string;
  /** Its title then (a window that went away: the last one it had) */
  title: string;
}

/** request_admin's reply when the person agreed: the driver is about to restart elevated */
export interface AdminData {
  restarting: true;
}

/** observe's reply */
export interface ObserveData {
  /** Front to back */
  windows: { handle: number; program: string; title: string; front: boolean }[];
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

/** Where a program (exec / resume) stands: `data.program` of their replies */
export interface ProgramData {
  program: {
    id: number;
    state: "running" | "stuck" | "asking" | "done" | "failed" | "stopped";
    /** The program's line of the action running, stuck or asking, or of its error */
    line: number | null;
    actions: number;
    result: unknown;
    error: string | null;
    /** Stuck: the action that failed, and its reply */
    call: { tool: string; args: Record<string, unknown> } | null;
    failure: { ok: boolean; text: string | null; error: string | null; data: unknown } | null;
    /** Asking: what it asks */
    question: { question: unknown; details: unknown } | null;
    log: string[];
    /** The skill it runs, if it is one */
    skill: string | null;
    /** What became of it as a skill (with learn) */
    learned: string | null;
  };
}
