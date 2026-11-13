// Technician-side MCP server: lets Claude (Claude Code / Claude desktop) call tools on the remote computer directly.
// Usage: configure `node src/console/mcp-server.ts` in your MCP client; RELAY_URL / RELAY_TOKEN / TECH_NAME env vars are optional.

import { execFileSync } from "node:child_process";
import { existsSync, statSync } from "node:fs";
import { readFile, writeFile, mkdir } from "node:fs/promises";
import path from "node:path";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import { TechClient } from "./tech-client.ts";
import { DEFAULT_RELAY_URL, type ToolName, type ToolResult } from "../shared/protocol.ts";

/**
 * The relay token normally arrives via RELAY_TOKEN (expanded from ASCTRL_RELAY_TOKEN in .mcp.json).
 * A GUI app launched before the variable was set keeps a stale environment, so the expansion is empty.
 * Fall back to a gitignored .env next to package.json, then to the Windows user environment in the registry.
 */
function resolveRelayToken(): string | undefined {
  const fromEnv = process.env.RELAY_TOKEN || process.env.ASCTRL_RELAY_TOKEN;
  if (fromEnv) return fromEnv;
  try {
    const envFile = path.resolve(import.meta.dirname, "../../.env");
    if (existsSync(envFile)) {
      process.loadEnvFile(envFile);
      if (process.env.ASCTRL_RELAY_TOKEN) return process.env.ASCTRL_RELAY_TOKEN;
    }
  } catch {}
  if (process.platform === "win32") {
    try {
      const out = execFileSync("reg", ["query", "HKCU\\Environment", "/v", "ASCTRL_RELAY_TOKEN"], {
        encoding: "utf8",
        stdio: ["ignore", "pipe", "ignore"],
        windowsHide: true,
      });
      const m = out.match(/ASCTRL_RELAY_TOKEN\s+REG_(?:EXPAND_)?SZ\s+(.+)/);
      if (m) return m[1].trim();
    } catch {}
  }
  return undefined;
}

const client = new TechClient(
  process.env.RELAY_URL ?? DEFAULT_RELAY_URL,
  process.env.TECH_NAME ?? "Technician",
  resolveRelayToken(),
);

const server = new McpServer({ name: "asctrl", version: "0.1.0" });

type Content = { type: "text"; text: string } | { type: "image"; data: string; mimeType: string };
type Reply = { content: Content[]; isError?: boolean };

/** Width of the last screenshot: automatic screenshots after actions reuse it so the coordinate scale stays the same */
let lastMaxWidth = 1600;
/** Display of the last screenshot: automatic screenshots after actions reuse it */
let lastDisplay = 0;

function toContent(r: ToolResult): Reply {
  const content: Content[] = [];
  if (r.image) content.push({ type: "image", data: r.image.data, mimeType: r.image.mimeType });
  if (r.text) content.push({ type: "text", text: r.text });
  if (!r.ok) content.push({ type: "text", text: `Failed: ${r.error ?? "unknown error"}` });
  if (!content.length) content.push({ type: "text", text: "(no output)" });
  return { content, isError: !r.ok };
}

function errorReply(e: unknown): Reply {
  return { content: [{ type: "text", text: `Call failed: ${(e as Error).message ?? e}` }], isError: true };
}

async function forward(tool: ToolName, args: Record<string, unknown>): Promise<Reply> {
  try {
    return toContent(await client.call(tool, args));
  } catch (e) {
    return errorReply(e);
  }
}

/** Mouse and keyboard actions: by default take a screenshot afterwards so the result is visible */
async function act(tool: ToolName, args: Record<string, unknown> & { screenshot?: boolean }, shotByDefault = true): Promise<Reply> {
  const { screenshot = shotByDefault, ...rest } = args;
  const r = await forward(tool, rest);
  if (r.isError || !screenshot) return r;
  const shot = await forward("screenshot", { maxWidth: lastMaxWidth, display: lastDisplay });
  // The action itself succeeded. Reporting the whole call as failed because the follow-up screenshot
  // didn't work would invite a retry, and the click or keystroke would happen twice.
  if (shot.isError) return { content: [...r.content, { type: "text", text: "(the action succeeded, but the screenshot after it failed; call screenshot to see the result)" }] };
  return { content: [...r.content, ...shot.content] };
}

const screenshotFlag = z.boolean().optional().describe("Take a screenshot after the action; default true");
const coord = (what: string) => z.number().min(0).describe(`${what} (pixel coordinate on the latest screenshot)`);

/** Code of the last successful connection, so request_admin can reconnect by itself */
let lastCode = "";

/** Connect to a code; if their app is not online yet, keep retrying for waitSec seconds */
async function connectWait(code: string, waitSec: number): Promise<Reply> {
  const deadline = Date.now() + waitSec * 1000;
  for (;;) {
    try {
      const host = await client.connect(code);
      lastCode = code;
      return { content: [{ type: "text", text: `Connected to ${host}` }] };
    } catch (e) {
      const msg = (e as Error).message;
      // Only "their app is not online yet" is worth retrying; other errors return immediately
      const offline = /No agent with this code|already connected|Connection closed|ECONNREFUSED/.test(msg);
      if (!offline || Date.now() + 5000 > deadline) {
        return { content: [{ type: "text", text: `Connection failed: ${msg}` }], isError: true };
      }
      await new Promise((r) => setTimeout(r, 5000));
    }
  }
}

server.registerTool(
  "connect",
  {
    description:
      "Connect to a remote computer with the 6-digit code the user gives you. Connects right away; the user doesn't need to confirm. " +
      "When reconnecting after a reboot, pass waitSec to keep retrying until their app is back online.",
    inputSchema: {
      code: z.string().regex(/^\d{6}$/).describe("6-digit assist code"),
      waitSec: z.number().int().min(0).max(600).optional().describe("How many seconds to keep waiting if their app is not online yet; default 0 (don't wait)"),
    },
  },
  ({ code, waitSec = 0 }) => connectWait(code, waitSec),
);

server.registerTool("disconnect", { description: "Disconnect from the current remote computer." }, async () => {
  client.disconnect();
  return { content: [{ type: "text", text: "Disconnected" }] };
});

// ---------- Viewing ----------

server.registerTool(
  "screenshot",
  {
    description:
      "Capture the remote computer's screen (JPEG). By default all displays are stitched together; with several displays, pass display to capture just one, more clearly. " +
      "Coordinates for click/scroll/drag etc. refer to the latest screenshot. Use zoom when small text is hard to read.",
    inputSchema: {
      maxWidth: z.number().int().min(320).max(3840).optional().describe("Maximum width after scaling; default 1600"),
      display: z.number().int().min(0).max(16).optional().describe("Display number (starting at 1); 0 or omitted means all"),
    },
    annotations: { readOnlyHint: true },
  },
  (a) => {
    if (a.maxWidth) lastMaxWidth = a.maxWidth;
    if (a.display !== undefined) lastDisplay = a.display;
    return forward("screenshot", { maxWidth: lastMaxWidth, display: lastDisplay });
  },
);

server.registerTool(
  "zoom",
  {
    description:
      "Re-capture a rectangle of the latest screenshot at native resolution, to read small text, icons and error details. " +
      "Arguments are coordinates on the latest screenshot; the zoomed image is for viewing only, and later click coordinates still refer to that screenshot.",
    inputSchema: {
      x0: coord("Top-left x"),
      y0: coord("Top-left y"),
      x1: coord("Bottom-right x"),
      y1: coord("Bottom-right y"),
      maxWidth: z.number().int().min(320).max(3840).optional().describe("Maximum width of the zoomed image; default 1600"),
    },
    annotations: { readOnlyHint: true },
  },
  (a) => forward("zoom", a),
);

server.registerTool(
  "cursor_position",
  { description: "Get the current mouse position (as coordinates on the latest screenshot).", annotations: { readOnlyHint: true } },
  () => forward("cursor_position", {}),
);

server.registerTool(
  "wait",
  {
    description: "Wait a few seconds (e.g. for a program to open or an install to progress), then take a screenshot.",
    inputSchema: { seconds: z.number().min(0.1).max(60).describe("Seconds to wait"), screenshot: screenshotFlag },
    annotations: { readOnlyHint: true },
  },
  async ({ seconds, screenshot = true }) => {
    await new Promise((r) => setTimeout(r, seconds * 1000));
    if (!screenshot) return { content: [{ type: "text", text: `Waited ${seconds} s` }] };
    return forward("screenshot", { maxWidth: lastMaxWidth, display: lastDisplay });
  },
);

server.registerTool(
  "system_info",
  { description: "Basic info about the remote computer: OS version, uptime, CPU, memory, free disk space.", annotations: { readOnlyHint: true } },
  () => forward("system_info", {}),
);

server.registerTool(
  "list_processes",
  {
    description: "List processes on the remote computer by memory use.",
    inputSchema: { top: z.number().int().min(1).max(200).optional().describe("How many to show; default 30") },
    annotations: { readOnlyHint: true },
  },
  (a) => forward("list_processes", a),
);

server.registerTool(
  "event_log",
  {
    description: "Read errors and warnings from the remote computer's Windows event log.",
    inputSchema: {
      log: z.enum(["System", "Application"]).optional().describe("Default System"),
      hours: z.number().int().min(1).max(720).optional().describe("How many recent hours; default 24"),
      max: z.number().int().min(1).max(200).optional().describe("Maximum entries; default 30"),
    },
    annotations: { readOnlyHint: true },
  },
  (a) => forward("event_log", a),
);

// ---------- Commands ----------

server.registerTool(
  "run_powershell",
  {
    description:
      "Run a PowerShell script on the remote computer and return its output. Commands run in one persistent session, so the current directory and variables carry over (starting in the user's home). " +
      "Commands that need interactive input can't run. The command is shown on the user's screen. Explain and confirm with the technician before changing the system or deleting files. " +
      "For commands that may take more than a few minutes (sfc /scannow, DISM, Windows Update, large copies, installers), use background: true and check progress with job_output.",
    inputSchema: {
      script: z.string().describe("PowerShell script"),
      timeoutSec: z.number().int().min(1).max(600).optional().describe("Timeout in seconds; default 60. A timeout resets the session. Ignored for background runs"),
      background: z
        .boolean()
        .optional()
        .describe("Run in the background in a separate PowerShell and return a job id immediately; does not share the session's directory or variables. Stopped when the technician disconnects"),
    },
    annotations: { destructiveHint: true },
  },
  (a) => forward("run_powershell", a),
);

server.registerTool(
  "job_output",
  {
    description: "Show a background job's status and any output since the last check. Without jobId, list all background jobs.",
    inputSchema: {
      jobId: z.number().int().min(1).optional(),
      all: z.boolean().optional().describe("Return all output instead of only new output"),
    },
    annotations: { readOnlyHint: true },
  },
  (a) => forward("job_output", a),
);

server.registerTool(
  "stop_job",
  { description: "Stop a background job (including the child processes it started).", inputSchema: { jobId: z.number().int().min(1) } },
  (a) => forward("stop_job", a),
);

server.registerTool(
  "reset_shell",
  { description: "Reset the remote PowerShell session (clears the current directory and variables)." },
  () => forward("reset_shell", {}),
);

server.registerTool(
  "request_admin",
  {
    description:
      "Request administrator access. The user clicks Allow in the app, then \"Yes\" in the Windows confirmation; the app then restarts as administrator " +
      "and this tool reconnects to it automatically (takes about 5-15 seconds). " +
      "Call it whenever the task needs admin rights: repairing system files, managing services, installing software, " +
      "or when a mouse/keyboard tool reports that the target window runs as administrator (Windows blocks input to it until the app is elevated).",
  },
  async () => {
    const code = lastCode;
    const r = await forward("request_admin", {});
    const text = r.content.find((c) => c.type === "text")?.text ?? "";
    if (r.isError || !/restarting as administrator/.test(text) || !code) return r;
    // Give the old process time to exit, so the reconnect reaches the new elevated one
    await new Promise((res) => setTimeout(res, 3000));
    const c = await connectWait(code, 90);
    if (c.isError) {
      return {
        content: [...r.content, ...c.content, { type: "text", text: `Reconnect with connect (code ${code}, waitSec: 60).` }],
        isError: true,
      };
    }
    return { content: [...r.content, { type: "text", text: "Reconnected; now running as administrator." }] };
  },
);

server.registerTool(
  "restart_computer",
  {
    description:
      "Restart the remote computer. The user is asked to agree in the app first (and reminded to save files), then it counts down and restarts; after restart and sign-in the assist app opens automatically with the same code. " +
      "Then reconnect with connect (waitSec: 300 recommended). " +
      "prepareOnly: only register \"open automatically at next sign-in\" without restarting (for installers that restart, or a manual restart by the user); cancel: cancel a pending restart.",
    inputSchema: {
      delaySec: z.number().int().min(5).max(600).optional().describe("Seconds until restart; default 15"),
      prepareOnly: z.boolean().optional(),
      cancel: z.boolean().optional(),
    },
    annotations: { destructiveHint: true },
  },
  (a) => forward("restart_computer", a),
);

// ---------- Mouse and keyboard ----------

server.registerTool(
  "click",
  {
    description:
      "Click on the remote screen by moving the user's real mouse pointer. Coordinates are pixels on the latest screenshot. " +
      "For buttons, boxes, menus and lists in ordinary programs, prefer list_elements + element_action: they don't take over the user's mouse. " +
      "Use click for games, canvases, remote desktops, or when element_action says it can't.",
    inputSchema: {
      x: coord("x"),
      y: coord("y"),
      button: z.enum(["left", "right", "middle"]).optional().describe("Default left"),
      count: z.number().int().min(1).max(3).optional().describe("Number of clicks: 2 = double-click, 3 = triple-click (selects a line); default 1"),
      modifiers: z.string().optional().describe("Keys held while clicking, e.g. ctrl, shift, ctrl+shift"),
      screenshot: screenshotFlag,
    },
  },
  (a) => act("click", a),
);

server.registerTool(
  "scroll",
  {
    description: "Move the mouse to a position, then scroll the wheel.",
    inputSchema: {
      x: coord("x"),
      y: coord("y"),
      amount: z.number().int().min(-50).max(50).optional().describe("Notches to scroll: positive down (right when horizontal), negative up (left); default 3"),
      horizontal: z.boolean().optional().describe("Scroll horizontally"),
      screenshot: screenshotFlag,
    },
  },
  (a) => act("scroll", a),
);

server.registerTool(
  "drag",
  {
    description: "Drag with the left mouse button from one point to another.",
    inputSchema: {
      fromX: coord("Start x"),
      fromY: coord("Start y"),
      toX: coord("End x"),
      toY: coord("End y"),
      screenshot: screenshotFlag,
    },
  },
  (a) => act("drag", a),
);

server.registerTool(
  "mouse_move",
  {
    description: "Move the mouse somewhere without clicking, to trigger hover tooltips or open hover menus.",
    inputSchema: { x: coord("x"), y: coord("y"), screenshot: screenshotFlag },
  },
  (a) => act("mouse_move", a),
);

server.registerTool(
  "mouse_look",
  {
    description:
      "Move the mouse relative to where it is, like a physical mouse, in raw mouse counts. Use it to turn the camera in games " +
      "(first-person views ignore mouse_move, which jumps the pointer to a position). Positive dx turns right, positive dy turns down; " +
      "how far a count turns depends on the game's sensitivity, so start small (e.g. 200), check the screenshot, then adjust.",
    inputSchema: {
      dx: z.number().int().min(-20000).max(20000).optional().describe("Horizontal counts; default 0"),
      dy: z.number().int().min(-20000).max(20000).optional().describe("Vertical counts; default 0"),
      seconds: z.number().min(0).max(5).optional().describe("Spread the motion over this long; default 0.2"),
      screenshot: screenshotFlag,
    },
  },
  (a) => act("mouse_look", a),
);

const buttonArg = z.enum(["left", "right", "middle"]).optional().describe("Default left");

server.registerTool(
  "mouse_down",
  {
    description: "Press a mouse button and hold it (optionally moving first). Combine with mouse_move and mouse_up for fine drags, e.g. sliders or box selection.",
    inputSchema: { x: coord("x").optional(), y: coord("y").optional(), button: buttonArg, screenshot: screenshotFlag },
  },
  (a) => act("mouse_down", a),
);

server.registerTool(
  "mouse_up",
  {
    description: "Release a mouse button (optionally moving first).",
    inputSchema: { x: coord("x").optional(), y: coord("y").optional(), button: buttonArg, screenshot: screenshotFlag },
  },
  (a) => act("mouse_up", a),
);

server.registerTool(
  "type_text",
  {
    description: "Type text at the current focus. Any language works and the user's input method doesn't matter. Newlines press Enter. Click the input box first if unsure where focus is.",
    inputSchema: { text: z.string().min(1), screenshot: screenshotFlag },
  },
  (a) => act("type_text", a),
);

server.registerTool(
  "press_keys",
  {
    description:
      "Press a key or key combination, e.g. enter, esc, tab, ctrl+c, ctrl+shift+esc, win+r, alt+f4, f5. " +
      "Keys go to the focused window; click the target window first if unsure where focus is.",
    inputSchema: { keys: z.string().describe("Key names joined with +"), screenshot: screenshotFlag },
  },
  (a) => act("press_keys", a),
);

server.registerTool(
  "hold_key",
  {
    description: "Hold a key or key combination for a while, then release it, e.g. hold shift for 3 seconds.",
    inputSchema: {
      keys: z.string().describe("Key names joined with +"),
      seconds: z.number().min(0.05).max(10).optional().describe("Seconds to hold; default 1"),
      screenshot: screenshotFlag,
    },
  },
  (a) => act("hold_key", a),
);

// ---------- Windows, programs, clipboard ----------

server.registerTool(
  "list_windows",
  {
    description: "List open windows on the remote computer (title, program, minimized or not, centre on the screenshot), from front to back.",
    annotations: { readOnlyHint: true },
  },
  () => forward("list_windows", {}),
);

server.registerTool(
  "focus_window",
  {
    description:
      "Switch to a window (or minimize / maximize / restore / close it). Use the handle from list_windows, or a keyword from the title or program name.",
    inputSchema: {
      handle: z.number().int().optional().describe("The number in brackets from list_windows"),
      title: z.string().optional().describe("Title or program-name keyword, e.g. chrome, notepad, excel"),
      action: z.enum(["focus", "minimize", "maximize", "restore", "close"]).optional().describe("Default focus"),
      screenshot: screenshotFlag,
    },
  },
  (a) => act("focus_window", a),
);

// ---------- Controls (UI Automation): operate programs without the mouse ----------

const elementVerb = z.enum(["click", "toggle", "select", "expand", "collapse", "set_value", "focus", "scroll", "scroll_into_view", "text"]);

server.registerTool(
  "list_elements",
  {
    description:
      "List the controls of a window (buttons, text boxes, checkboxes, menus, list and tree items, tabs, links, text) as text, numbered for element_action. " +
      "Each line: [id] Type \"name\" value=\"…\" (state) {what it supports} @(x,y centre on the latest screenshot). " +
      "Much cheaper than a screenshot for finding and reading things, and works without one. Defaults to the window in front. " +
      "Ids are valid until the next list_elements. Programs that draw their own UI (games, remote desktops, some canvases) show little or nothing; use screenshot + click there.",
    inputSchema: {
      handle: z.number().int().optional().describe("Window handle from list_windows; default the window in front"),
      title: z.string().optional().describe("Or a title / program-name keyword"),
      query: z.string().optional().describe("Only list controls whose line contains this text (case-insensitive), e.g. a button label or 'Edit'"),
      maxResults: z.number().int().min(1).max(2000).optional().describe("Default 300"),
      includeOffscreen: z.boolean().optional().describe("Also list controls scrolled out of view; default false"),
    },
    annotations: { readOnlyHint: true },
  },
  (a) => forward("list_elements", a),
);

server.registerTool(
  "element_action",
  {
    description:
      "Operate a control from list_elements directly through Windows UI Automation. The user's mouse and keyboard focus are not touched (except action focus), " +
      "and it works even when the window is partly covered. Actions: click (default; presses buttons, links and menu items, or toggles / selects / expands whatever the control is), " +
      "toggle, select, expand, collapse, set_value (fills a text box or slider with value, replacing its content), focus, " +
      "scroll (value up / down / left / right, add :page for a page, e.g. down:page), scroll_into_view, text (returns the control's full text, e.g. a document or a long message). " +
      "Parallel: pass actions (up to 20) instead of id/action/value to run several at the same time, e.g. fill every field of a form or tick several boxes in one call; " +
      "they run concurrently in no set order. Sequence: add sequence: true to run actions (up to 100) one after another in the order given, as fast as the controls take them " +
      "(or intervalMs apart), e.g. keying a sum into a calculator; it stops at the first failure and skips the rest. Controls that only appear after a step " +
      "(a menu that opens, a new dialog) have no id yet, so those still need separate calls with list_elements in between. " +
      "No screenshot afterwards by default: check the result with list_elements (cheap) or pass screenshot: true.",
    inputSchema: {
      id: z.number().int().min(1).optional().describe("The number in brackets from list_elements"),
      action: elementVerb.optional(),
      value: z.string().optional().describe("For set_value: the new content. For scroll: the direction"),
      actions: z
        .array(z.object({ id: z.number().int().min(1), action: elementVerb.optional(), value: z.string().optional() }))
        .min(1)
        .max(100)
        .optional()
        .describe("Several actions instead of id/action/value: up to 20 at the same time, or up to 100 in order with sequence"),
      sequence: z.boolean().optional().describe("Run actions one after another in the order given, stopping at the first failure; default false (all at once)"),
      intervalMs: z.number().int().min(0).max(5000).optional().describe("With sequence: pause between steps in milliseconds; default 0 (as fast as possible)"),
      screenshot: z.boolean().optional().describe("Take a screenshot after the action; default false"),
    },
  },
  (a) =>
    a.id === undefined && !a.actions
      ? Promise.resolve({ content: [{ type: "text" as const, text: "Give id, or actions" }], isError: true })
      : a.actions && !a.sequence && a.actions.length > 20
        ? Promise.resolve({ content: [{ type: "text" as const, text: "Up to 20 actions at the same time; for more, pass sequence: true" }], isError: true })
        : act("element_action", a, false),
);

server.registerTool(
  "open_app",
  {
    description:
      "Open a program, file, folder or URL as if double-clicked. E.g. notepad, control, taskmgr, ms-settings:display, " +
      "C:\\Users\\xx\\Desktop\\report.docx, https://example.com. Runs with the user's current privileges.",
    inputSchema: {
      target: z.string().describe("Program name, path, URL, or a link such as ms-settings:"),
      args: z.string().optional().describe("Command-line arguments"),
      cwd: z.string().optional().describe("Working directory"),
      screenshot: screenshotFlag,
    },
  },
  (a) => act("open_app", a),
);

server.registerTool(
  "read_clipboard",
  { description: "Read the text on the remote computer's clipboard (or the list of copied files).", annotations: { readOnlyHint: true } },
  () => forward("read_clipboard", {}),
);

server.registerTool(
  "write_clipboard",
  {
    description: "Put text on the remote computer's clipboard; paste it afterwards with press_keys ctrl+v. Faster than type_text for long text.",
    inputSchema: { text: z.string() },
  },
  (a) => forward("write_clipboard", a),
);

// ---------- Files ----------

const remotePath = z.string().describe("Remote path; supports ~ and %ENV_VARS%; relative paths are resolved against the user's home");

server.registerTool(
  "read_file",
  {
    description:
      "Read a text file on the remote computer, with line numbers. Detects UTF-8 / UTF-16 / ANSI code-page encodings. Read large files in parts with offset and limit. " +
      "Good for logs, config files, hosts, scripts.",
    inputSchema: {
      path: remotePath,
      offset: z.number().int().min(1).optional().describe("Line to start from; default 1"),
      limit: z.number().int().min(1).max(20000).optional().describe("How many lines to read; default 2000"),
    },
    annotations: { readOnlyHint: true },
  },
  (a) => forward("read_file", a),
);

server.registerTool(
  "write_file",
  {
    description: "Write a text file on the remote computer (overwrites it entirely, creates it and any folders if missing). Existing files keep their encoding by default. Prefer edit_file for small changes.",
    inputSchema: {
      path: remotePath,
      content: z.string(),
      encoding: z.enum(["utf8", "utf8bom", "utf16le", "ansi"]).optional().describe("Default: existing files keep their encoding, new files are UTF-8. ansi is recommended for .bat/.cmd"),
    },
    annotations: { destructiveHint: true },
  },
  (a) => forward("write_file", a),
);

server.registerTool(
  "edit_file",
  {
    description:
      "Replace an exact piece of a remote text file, keeping its encoding and line endings. oldText must match the file exactly (check with read_file first, without line numbers) " +
      "and appear only once; otherwise include more context or pass replaceAll.",
    inputSchema: {
      path: remotePath,
      oldText: z.string().min(1),
      newText: z.string(),
      replaceAll: z.boolean().optional(),
    },
    annotations: { destructiveHint: true },
  },
  (a) => forward("edit_file", a),
);

server.registerTool(
  "list_dir",
  {
    description: "List the contents of a remote folder (modified time, size, name).",
    inputSchema: { path: remotePath.optional().describe("Defaults to the user's home") },
    annotations: { readOnlyHint: true },
  },
  (a) => forward("list_dir", a),
);

server.registerTool(
  "find_files",
  {
    description: "Find files by name recursively in a remote folder. Supports * and ? wildcards; separate several with ; (e.g. *.log;*.dmp). Results are newest first.",
    inputSchema: {
      path: remotePath.describe("Folder to search from"),
      pattern: z.string().describe("File-name wildcard"),
      maxResults: z.number().int().min(1).max(5000).optional().describe("Default 200"),
    },
    annotations: { readOnlyHint: true },
  },
  (a) => forward("find_files", a),
);

server.registerTool(
  "search_files",
  {
    description: "Search file contents in a remote file or folder (regular expression). Detects encodings and skips binary files and files over 10 MB.",
    inputSchema: {
      path: remotePath.describe("File or folder"),
      pattern: z.string().describe("Regular expression"),
      glob: z.string().optional().describe("Only search these file names, e.g. *.log;*.txt"),
      ignoreCase: z.boolean().optional().describe("Default true"),
      context: z.number().int().min(0).max(10).optional().describe("Lines of context around each match; default 0"),
      maxResults: z.number().int().min(1).max(2000).optional().describe("Maximum matches; default 100"),
    },
    annotations: { readOnlyHint: true },
  },
  (a) => forward("search_files", a),
);

server.registerTool(
  "disk_usage",
  {
    description:
      "Show where disk space goes under a folder: the largest subfolders as a tree (size and share of the parent) and the largest files. " +
      "One native scan, much faster than PowerShell; a whole system drive usually takes under a minute (stops after 5 minutes with partial totals). " +
      "Use system_info for free space per drive. Folders the user can't read are skipped and reported; after request_admin the totals are complete. " +
      "Online-only OneDrive files are not counted; C:\\Windows\\WinSxS looks larger than it is because of hard links.",
    inputSchema: {
      path: remotePath.optional().describe("Folder to scan; default C:\\"),
      depth: z.number().int().min(0).max(6).optional().describe("Folder levels to break down; default 2 (0 = only the largest files)"),
      perFolder: z.number().int().min(1).max(50).optional().describe("Largest subfolders shown per folder; default 8"),
      topFiles: z.number().int().min(0).max(500).optional().describe("How many of the largest files to list; default 20 (0 = none)"),
    },
    annotations: { readOnlyHint: true },
  },
  (a) => forward("disk_usage", a),
);

server.registerTool(
  "upload_file",
  {
    description: "Upload a file from your computer to the remote computer (max 20 MB).",
    inputSchema: {
      localPath: z.string().describe("File path on your side"),
      remotePath: z.string().describe("Remote destination path; supports ~ and %ENV_VARS%; relative paths are resolved against the user's home"),
    },
  },
  async ({ localPath, remotePath }) => {
    try {
      const buf = await readFile(localPath);
      if (buf.length > 20 * 1024 * 1024) throw new Error("File is larger than 20 MB");
      return await forward("upload_file", { path: remotePath, data: buf.toString("base64") });
    } catch (e) {
      return errorReply(e);
    }
  },
);

server.registerTool(
  "download_file",
  {
    description: "Download a file from the remote computer to yours (max 20 MB).",
    inputSchema: {
      remotePath: z.string().describe("Remote file path; supports ~ and %ENV_VARS%"),
      localPath: z.string().describe("Destination on your side (a file, or an existing folder)"),
    },
  },
  async ({ remotePath, localPath }) => {
    try {
      const r = await client.call("download_file", { path: remotePath });
      if (!r.ok || !r.file) return toContent(r);
      let dest = path.resolve(localPath);
      if (/[\\/]$/.test(localPath) || (existsSync(dest) && statSync(dest).isDirectory())) {
        dest = path.join(dest, r.file.name);
      }
      await mkdir(path.dirname(dest), { recursive: true });
      const buf = Buffer.from(r.file.data, "base64");
      await writeFile(dest, buf);
      return { content: [{ type: "text", text: `Saved to ${dest} (${buf.length} bytes)` }] };
    } catch (e) {
      return errorReply(e);
    }
  },
);

await server.connect(new StdioServerTransport());
