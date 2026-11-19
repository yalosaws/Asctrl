// MCP server: gives an AI (Claude Code, Claude desktop, any MCP client) eyes and hands on a computer through
// the Asctrl driver — this computer directly, or a remote one through the relay with a 6-digit code.
// Usage: configure `node src/console/mcp-server.ts` in your MCP client; RELAY_URL / RELAY_TOKEN / TECH_NAME env vars are optional.

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, statSync } from "node:fs";
import { readFile, writeFile, mkdir } from "node:fs/promises";
import path from "node:path";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import { TechClient } from "./tech-client.ts";
import { Runner, STEP_TOOLS } from "./run.ts";
import { DEFAULT_RELAY_URL, type AdminData, type ProgramData, type ToolName, type ToolResult } from "../shared/protocol.ts";

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

const CONNECT_LOCAL = "1. connect (no arguments = this computer; a 6-digit code = a remote one).";
const CONNECT_HOSTED =
  "1. connect with the 6-digit code shown in the Asctrl app on the computer to drive (ask the user for it if you don't have it).";

/** In compact mode: how the tools named above are reached */
const COMPACT_NOTE = `

Five tools: connect, query (looking: observe, screenshot, read_text, wait_for, progress, skills, files), act (doing: run a plan, click, type_text, set_value, press_keys, open_app, focus_window, shell and files, ...), exec and resume. query and act take the name of what they do and its arguments: act { do: "run", args: { steps: [...] } }, query { what: "observe", args: { window: "Notepad" } }.`;

const instructions = (hosted: boolean) => `Asctrl lets you see and operate a Windows computer. Work like this:

${hosted ? CONNECT_HOSTED : CONNECT_LOCAL}
2. Plan first, with run. Write the steps by the text they act on ("Compose", "To"), the way you expect the screen to be, without looking first, and end with expect so success is checked. run returns at once: write the next plan while it runs. Text is matched on the computer; only when it fits several controls or none does a decision model choose, and only when it is sure. Popups that turn up while a plan runs are dismissed in between its steps (give the ones you expect in on).
3. When a plan fails, its report says where things stand (the window's controls, with references): re-plan from there. Look (observe, or screenshot for layout, images and programs that draw their own UI) only when the next plan needs something you can't guess, such as what a page lists.
4. Single actions (click, type_text, set_value, …) are for one-off corrections. They take target, a reference ("e12") or the text the thing shows, or x and y on the latest screenshot, and their reply says what changed, so you rarely need to look again.

Work that loops over rows or branches on what the screen shows is a program (exec): JavaScript the computer runs itself, which comes back only when it ends, asks, or an action fails (it then waits at that line for resume). Give it learn so it is kept as a skill, and look in skills first: a task done before runs again with skill and new args.

For a screen drawn as a grid (a game board, a calendar), declare it in run's grids and name cells "board[r,c]"; {read: "board"} reads every cell. Never guess coordinates: use a target, or x and y you can see on the latest screenshot.`;

type Icon = { src: string; mimeType: string; sizes: string[] };
let icons: Icon[] | undefined;

export const ICON_DIR = path.resolve(import.meta.dirname, "../../agent-app/src-tauri/icons");
/** The orb at each size clients ask for: [file in ICON_DIR, size] */
export const ICON_SIZES: [string, string][] = [["32x32.png", "32x32"], ["128x128.png", "128x128"], ["128x128@2x.png", "256x256"]];

/**
 * The app's orb, so connector lists show it instead of a letter. Inline by default (a client of the stdio server
 * can't reach local files); from iconBaseUrl when the server is on the web, where clients fetch https icons.
 */
function orbIcons(iconBaseUrl?: string): Icon[] {
  if (iconBaseUrl) return ICON_SIZES.map(([file, size]) => ({ src: new URL(`icons/${file}`, iconBaseUrl).href, mimeType: "image/png", sizes: [size] }));
  // Read once
  return (icons ??= ICON_SIZES.flatMap(([file, size]) => {
    try {
      const data = readFileSync(path.join(ICON_DIR, file)).toString("base64");
      return [{ src: `data:image/png;base64,${data}`, mimeType: "image/png", sizes: [size] }];
    } catch {
      return [];
    }
  }));
}

type Content = { type: "text"; text: string } | { type: "image"; data: string; mimeType: string };
type Reply = { content: Content[]; isError?: boolean };

function toContent(r: ToolResult): Reply {
  const content: Content[] = [];
  if (r.text) content.push({ type: "text", text: r.text });
  if (!r.ok) content.push({ type: "text", text: `Failed: ${r.error ?? "unknown error"}` });
  if (r.image) content.push({ type: "image", data: r.image.data, mimeType: r.image.mimeType });
  if (!content.length) content.push({ type: "text", text: "(no output)" });
  return { content, isError: !r.ok };
}

function errorReply(e: unknown): Reply {
  return { content: [{ type: "text", text: `Call failed: ${(e as Error).message ?? e}` }], isError: true };
}

const say = (text: string, isError = false): Reply => ({ content: [{ type: "text", text }], isError });

/**
 * Everything about driving one computer that must outlive an MCP session: the connection, the plans queued
 * on it, and the code it was reached by. Clients that open a new session per tool call (Grok) would
 * otherwise lose the computer between calls.
 */
export interface Link {
  client: TechClient;
  runner: Runner;
  /** Code of the last relay connection, so request_admin can reconnect by itself */
  lastCode: string;
}

export const newLink = (client: TechClient): Link => ({ client, runner: new Runner(client), lastCode: "" });

export interface ServerOptions {
  /** The computer this session drives; shared by every session of the same caller on the HTTP server */
  link: Link;
  /**
   * Running on a server rather than on the user's computer (the HTTP connector): there is no
   * "this computer" to drive, and the server's own files must stay out of reach.
   */
  hosted?: boolean;
  /** Where the icons are served (ending in /), for a server on the web; otherwise they are sent inline */
  iconBaseUrl?: string;
  /**
   * Which tools the AI sees. compact (default): five — connect, query (looking), act (doing), exec and resume —
   * with every other tool reached through query or act by its name. full: every tool on its own.
   * ASCTRL_TOOLS sets it when this isn't given.
   */
  tools?: "compact" | "full";
}

/** A fresh MCP server bound to one link to a driver */
export function createAsctrlServer({ link, hosted = false, iconBaseUrl, tools }: ServerOptions): McpServer {
  const { client } = link;
  const compact = (tools ?? process.env.ASCTRL_TOOLS ?? "compact") !== "full";
  const server = new McpServer(
    { name: "asctrl", title: "Asctrl", version: "0.2.0", icons: orbIcons(iconBaseUrl) },
    { instructions: instructions(hosted) + (compact ? COMPACT_NOTE : "") },
  );
  // Compact: the tools below are kept here, not shown; query and act reach them by name (see the end)
  type Kept = { description: string; schema: Record<string, z.ZodType>; annotations?: unknown; handler: (a: Record<string, unknown>) => Promise<Reply> };
  const kept = new Map<string, Kept>();
  const keep = ((name: string, cfg: { description?: string; inputSchema?: Record<string, z.ZodType>; annotations?: unknown }, handler: Kept["handler"]) => {
    kept.set(name, { description: cfg.description ?? "", schema: cfg.inputSchema ?? {}, annotations: cfg.annotations, handler });
    return undefined;
  }) as unknown as McpServer["registerTool"];
  const reg: Pick<McpServer, "registerTool"> = { registerTool: keep };

  async function forward(tool: ToolName, args: Record<string, unknown>): Promise<Reply> {
    try {
      return toContent(await client.call(tool, args));
    } catch (e) {
      return errorReply(e);
    }
  }

  /** exec / resume: a program stuck on words that fit several controls is decided by the decision model first
   *  (as a plan's step is), and resumed, so the AI hears of it only when the model isn't sure */
  async function program(tool: "exec" | "resume", args: Record<string, unknown>): Promise<Reply> {
    try {
      let r = await client.call(tool, args);
      const notes: string[] = [];
      for (let i = 0; i < 6; i++) {
        const p = (r.data as ProgramData | undefined)?.program;
        if (p?.state !== "stuck") break;
        const d = await link.runner.decideForProgram(p);
        if (!d) break;
        notes.push(d.note);
        r = await client.call("resume", { id: p.id, ...d.resume, waitSec: args.waitSec });
      }
      if (notes.length) {
        const head = `Decided on the computer's side, without you:\n${notes.map((n) => `- ${n}`).join("\n")}\n\n`;
        if (r.ok) r = { ...r, text: head + (r.text ?? "") };
        else r = { ...r, error: head + (r.error ?? "") };
      }
      return toContent(r);
    } catch (e) {
      return errorReply(e);
    }
  }

  /** Connect to a code; if their app is not online yet, keep retrying for waitSec seconds */
  async function connectWait(code: string, waitSec: number): Promise<Reply> {
    const deadline = Date.now() + waitSec * 1000;
    for (;;) {
      try {
        const host = await client.connect(code);
        link.lastCode = code;
        return say(`Connected to ${host}. Start with run: a plan of steps by the text they act on.`);
      } catch (e) {
        const msg = (e as Error).message;
        // Only "their app is not online yet" is worth retrying; other errors return immediately
        const offline = /No agent with this code|already connected|Connection closed|ECONNREFUSED/.test(msg);
        if (!offline || Date.now() + 5000 > deadline) return say(`Connection failed: ${msg}`, true);
        await new Promise((r) => setTimeout(r, 5000));
      }
    }
  }

  async function connectLocal(waitSec: number, notPid?: number): Promise<Reply> {
    try {
      const host = await client.connectLocalDriver({ waitMs: Math.max(20, waitSec) * 1000, notPid, launch: notPid === undefined && process.env.ASCTRL_NO_LAUNCH !== "1" });
      link.lastCode = "";
      return say(`Connected to this computer (${host}), directly. Start with run: a plan of steps by the text they act on.`);
    } catch (e) {
      return say(`Connection failed: ${(e as Error).message}`, true);
    }
  }

  reg.registerTool(
    "connect",
    {
      description: hosted
        ? "Connect to a computer: the one showing this 6-digit code in its Asctrl app (ask the user for it). " +
          "When reconnecting after a restart, pass waitSec to keep retrying until the driver is back."
        : "Connect to a computer. Without arguments: this computer, directly (the driver app is started if it is not running). " +
          "With code: a remote computer showing that 6-digit code, through the relay; it connects right away. " +
          "When reconnecting after a restart, pass waitSec to keep retrying until the driver is back.",
      inputSchema: {
        code: hosted
          ? z.string().regex(/^\d{6}$/).describe("6-digit code shown in the Asctrl app on the computer")
          : z.string().regex(/^\d{6}$/).optional().describe("6-digit code shown on the remote computer; omit to use this computer"),
        waitSec: z.number().int().min(0).max(600).optional().describe("How long to keep trying if the driver is not up yet; default 0 for a code, 20 for this computer"),
      },
    },
    ({ code, waitSec = 0 }) =>
      code ? connectWait(code, waitSec) : hosted ? Promise.resolve(say("Give the 6-digit code shown in the Asctrl app on the computer", true)) : connectLocal(waitSec),
  );

  reg.registerTool("disconnect", { description: "Disconnect from the current computer." }, async () => {
    client.disconnect();
    return say("Disconnected");
  });

  // ---------- Shared argument shapes ----------

  const coord = (what: string) => z.number().min(0).describe(`${what} (pixels on the latest screenshot, the same as the @(x,y) positions in replies)`);
  const target = z
    .union([z.string().min(1), z.number().int().min(1)])
    .describe('What to act on: a reference from observe or an earlier reply ("e12"), or the text the thing shows ("Save", "下一步", a file name). Text is matched against control names first, then read off the pixels');
  const windowArg = z.union([z.string().min(1), z.number().int()]).describe("Which window: part of its title or program name (notepad, chrome), or a handle (observe lists them). Default: the window in front");
  const role = z.string().optional().describe("With a text target: only controls of this kind (button, edit, link, checkbox, radio, tab, menuitem, item, combobox, text)");
  const nth = z.number().int().min(1).max(200).optional().describe("With a text target that matches several things equally: which one, in reading order");
  const timeoutSec = z.number().min(0).max(30).optional().describe("With a text target: how long to wait for it to appear; default 1.5");
  const after = z
    .enum(["auto", "none", "ui", "screenshot", "both"])
    .optional()
    .describe(
      "What the reply shows of the result. auto (default): the changes among the window's controls when they can tell, else a screenshot. " +
        "none: just what was done (fastest). ui: control changes. screenshot: an image. both",
    );
  const watch = z
    .object({ x0: z.number().min(0), y0: z.number().min(0), x1: z.number().min(0), y1: z.number().min(0) })
    .optional()
    .describe("Region (screenshot pixels) where the action shows its effect, e.g. a game board. Only it is watched for the screen settling, so a video or ad elsewhere can't hold the action up");
  /** Everything that points at something */
  const pointing = { target: target.optional(), x: coord("x").optional(), y: coord("y").optional(), window: windowArg.optional(), role, nth, timeoutSec };

  // ---------- Looking ----------

  reg.registerTool(
    "observe",
    {
      description:
        "Read a window as text: its controls (buttons, text boxes, checkboxes, menus, list items, links, text) as a tree, each with a reference like [e12], its state and value, and its centre @(x,y). " +
        "Open menus and drop-downs of the window are included. Where a program draws its own UI (games, canvases, remote desktops, some web pages), the text is read off the pixels instead. " +
        "The reply also lists every other open window, front to back, with the handle to pass as window. " +
        "With query: where something is instead, the controls whose name or value contains it (best match first, with references) and, where the program draws its own UI, the places it appears on the pixels. " +
        "References stay valid as long as the control exists, and every action's reply tells you what changed. Much cheaper and more exact than a screenshot; add image: true when you also need to see it.",
      inputSchema: {
        window: windowArg.optional(),
        query: z.string().optional().describe("Text to look for (case and spacing ignored): only what matches it, best first"),
        role,
        image: z.boolean().optional().describe("Also return a screenshot (of the window, when window is given); default false"),
        ocr: z.boolean().optional().describe("Read text off the pixels too. Default: only when the controls say little (with query: when no control matches). true adds text the controls don't mention (a canvas inside an ordinary window)"),
        offscreen: z.boolean().optional().describe("Also list controls scrolled out of view; default false"),
        max: z.number().int().min(1).max(2000).optional().describe("Most lines to list; default 150 (20 with query). Past it, plain text is left out before controls are"),
      },
      annotations: { readOnlyHint: true },
    },
    (a) => forward("observe", a),
  );

  reg.registerTool(
    "screenshot",
    {
      description:
        "Capture the screen as an image. x and y of later actions refer to the latest screenshot. By default the whole screen (all displays); " +
        "window captures just one window (brought to the front if covered) and display just one display: both are sharper and cost less. " +
        "x0, y0, x1, y1: that part of the latest screenshot again at full resolution, to read small text and icons (for reading only: x and y keep referring to the screenshot it came from). " +
        "Use observe to read and find things; a screenshot to see layout, images, colour, or programs that draw their own UI. " +
        "marks: true boxes and numbers each control of the window on the picture (12 is the reference e12), to point at what you see by its number.",
      inputSchema: {
        window: windowArg.optional(),
        marks: z.boolean().optional().describe("Box and number each control of the window (the number is its reference: 12 is e12)"),
        display: z.number().int().min(0).max(16).optional().describe("Display number (from 1); 0 or omitted means all"),
        x0: coord("Region top-left x").optional(),
        y0: coord("Region top-left y").optional(),
        x1: coord("Region bottom-right x").optional(),
        y1: coord("Region bottom-right y").optional(),
        maxWidth: z.number().int().min(320).max(3840).optional().describe("Maximum width after scaling; default 1600"),
      },
      annotations: { readOnlyHint: true },
    },
    (a) => forward("screenshot", a),
  );

  reg.registerTool(
    "read_text",
    {
      description:
        "Read text. With target: the full content of one control (a document, a text box, a long message), untruncated. " +
        "With window (or nothing: the window in front): all the text its controls show, in reading order. " +
        "With a region x0,y0,x1,y1 or display: the text read off the pixels there (OCR), line by line with positions.",
      inputSchema: {
        target: target.optional(),
        window: windowArg.optional(),
        role,
        nth,
        x0: coord("Region top-left x").optional(),
        y0: coord("Region top-left y").optional(),
        x1: coord("Region bottom-right x").optional(),
        y1: coord("Region bottom-right y").optional(),
        display: z.number().int().min(0).max(16).optional().describe("Read a whole display off the pixels"),
        source: z.enum(["auto", "controls", "ocr"]).optional().describe("For a window: controls (exact), ocr (the pixels), or auto (controls when they say enough); default auto"),
      },
      annotations: { readOnlyHint: true },
    },
    (a) => forward("read_text", a),
  );

  reg.registerTool(
    "wait_for",
    {
      description:
        "Wait until something happens, and return the moment it does. Much faster and more reliable than waiting a guessed time. Give exactly one of: " +
        "appears (a window whose title or program name contains this appears), text (this text shows up in window, default the one in front; among its controls or on its pixels), " +
        "stable: true (the screen stops changing, e.g. a page finished loading), or seconds (a plain pause, when nothing else fits). " +
        "Separate alternatives with |, e.g. 'Save As|另存为'. gone: true waits for the window or text to disappear instead. " +
        "The reply lists the controls of what appeared, with references ready to use. Fails with what is in front when the time runs out.",
      inputSchema: {
        appears: z.string().optional().describe("Title or program-name keyword(s) of a window to wait for, e.g. notepad, 'Settings|设置'"),
        text: z.string().optional().describe("Text to wait for"),
        stable: z.boolean().optional().describe("Wait for the screen to stop changing"),
        seconds: z.number().min(0).max(60).optional().describe("Just wait this long"),
        gone: z.boolean().optional().describe("Wait for the window / text to disappear instead"),
        window: windowArg.optional().describe("For text / stable: the window to watch; default the window in front"),
        ocr: z.boolean().optional().describe("For text: also read the pixels. Default: yes, at a slower pace than the controls"),
        timeoutSec: z.number().min(0.2).max(300).optional().describe("Give up after this many seconds; default 10"),
        after,
      },
      annotations: { readOnlyHint: true },
    },
    (a) => forward("wait_for", a),
  );

  // ---------- Acting ----------

  reg.registerTool(
    "click",
    {
      description:
        "Click something: target (a reference like \"e12\", or the text it shows) or x and y on the latest screenshot. " +
        "A control of an ordinary program is pressed directly through UI Automation — exact, works even when partly covered, and the user's pointer stays put; " +
        "web pages, games, canvases and plain coordinates get a real mouse click (the control is scrolled into view and its window brought forward first). " +
        "A click by x,y is refused, with a fresh screenshot, if another window has since appeared over that point or the window was moved away or resized; if the window only moved, the click moves with it. " +
        "The reply says whether the screen reacted and what changed, so a miss shows at once. " +
        "What sends something out (Send, Submit, Pay, Place order) or can't be undone (Delete, Discard) goes ahead at once, with no question at the computer. " +
        "effect records such a click in the action log when its words don't say so.",
      inputSchema: {
        ...pointing,
        effect: z.enum(["reversible", "irreversible", "external"]).optional().describe("What the click does, when its words don't say: irreversible (can't be undone) or external (sends, pays, publishes). Only raises the level the computer works out itself"),
        button: z.enum(["left", "right", "middle"]).optional().describe("Default left"),
        count: z.number().int().min(1).max(3).optional().describe("2 = double-click, 3 = triple-click (selects a line); default 1"),
        modifiers: z.string().optional().describe("Keys held while clicking, e.g. ctrl, shift, ctrl+shift"),
        mode: z.enum(["auto", "mouse", "direct"]).optional().describe("auto (default) as described; mouse: always a real mouse click; direct: always through UI Automation, never the mouse"),
        after,
        watch,
      },
    },
    (a) => forward("click", a),
  );

  reg.registerTool(
    "type_text",
    {
      description:
        "Type text. With target (or x,y): that field gets the focus first. Without: the text goes to whatever has the focus (window brings a window forward first). " +
        "replace: true selects the field's whole content first, so the text replaces it; enter: true presses Enter afterwards. " +
        "Any language works and the input method doesn't matter; characters on the keyboard are pressed as real keys, so games and web pages see them (with a Chinese, Japanese or Korean keyboard, letters go in as characters so the input method can't turn them into candidates; method keys presses real keys anyway, for games). " +
        "Text over 200 characters is pasted (the clipboard is put back). The reply names the field that took the text and reads it back, so text that went to the wrong place or was changed shows at once. " +
        "To fill a form field with a known value, set_value is surer.",
      inputSchema: {
        text: z.string().describe("The text; newlines press Enter"),
        ...pointing,
        replace: z.boolean().optional().describe("Replace the field's content instead of typing at the cursor; with empty text, clears the field"),
        enter: z.boolean().optional().describe("Press Enter after typing"),
        method: z.enum(["auto", "keys", "paste"]).optional().describe("auto (default): paste long text, type short text; keys: always keystrokes (fields that block pasting, games, remote desktops); paste: always paste"),
        after,
        watch,
      },
    },
    (a) => forward("type_text", a),
  );

  reg.registerTool(
    "press_keys",
    {
      description:
        "Press a key or combination, or several in a row separated by spaces: enter, esc, ctrl+s, alt+f4, win+r, 'ctrl+a delete', 'tab tab enter'. " +
        "Keys go to the window in front (window brings one forward first); the reply names it. repeat presses the whole thing several times; holdSec holds one key or combination down.",
      inputSchema: {
        keys: z.string().describe("Key names joined with +; sequences separated by spaces. Names: ctrl shift alt win enter esc tab space backspace delete insert home end pageup pagedown up down left right f1-f24 a-z 0-9 and symbols"),
        window: windowArg.optional().describe("Bring this window to the front first"),
        repeat: z.number().int().min(1).max(100).optional().describe("Press it this many times; default 1"),
        holdSec: z.number().min(0.05).max(10).optional().describe("Hold the key or combination this long before releasing"),
        after,
        watch,
      },
    },
    (a) => forward("press_keys", a),
  );

  reg.registerTool(
    "set_value",
    {
      description:
        "Make a control hold a value, whatever kind of control it is, and check that it does: a text box takes the text (replacing what was there); a checkbox or switch takes true / false; " +
        "a combo box, list or tab strip takes the name of the item to pick; a slider or spinner a number; a radio button or list item \"selected\"; a tree item or section \"expanded\" / \"collapsed\". " +
        "Idempotent: a box already ticked stays ticked. No mouse, no focus change; a field that won't take a value directly (many web pages) is typed into instead and read back. " +
        "fields fills a whole form in one call. The surest way to fill forms and change settings.",
      inputSchema: {
        ...pointing,
        value: z.union([z.string(), z.boolean(), z.number()]).optional().describe("What the control should hold"),
        fields: z
          .array(z.object({ target: target.optional(), x: z.number().optional(), y: z.number().optional(), value: z.union([z.string(), z.boolean(), z.number()]), role, nth, window: windowArg.optional() }))
          .min(1)
          .max(50)
          .optional()
          .describe("Several controls at once, instead of target and value: [{ target, value }, …]. Each is reported; the rest still run if one fails"),
        after,
      },
    },
    (a) => (a.fields || a.value !== undefined ? forward("set_value", a) : Promise.resolve(say("Give value (with target), or fields", true))),
  );

  reg.registerTool(
    "scroll",
    {
      description:
        "Turn the mouse wheel over something: target or x,y, or — with neither — where the pointer is in the window in front. " +
        "The reply says whether the view moved (if not, it is already at its end).",
      inputSchema: {
        ...pointing,
        direction: z.enum(["up", "down", "left", "right"]).optional().describe("Default down"),
        amount: z.number().int().min(1).max(100).optional().describe("Wheel notches; default 3 (about a third of a page in most programs)"),
        after,
        watch,
      },
    },
    (a) => forward("scroll", a),
  );

  reg.registerTool(
    "drag",
    {
      description: "Press a mouse button on one thing, move to another and release: from / to (references or text) or fromX, fromY / toX, toY. Both ends are found before the button goes down.",
      inputSchema: {
        from: target.optional(),
        to: target.optional(),
        fromX: coord("Start x").optional(),
        fromY: coord("Start y").optional(),
        toX: coord("End x").optional(),
        toY: coord("End y").optional(),
        window: windowArg.optional(),
        button: z.enum(["left", "right", "middle"]).optional().describe("Default left"),
        seconds: z.number().min(0.05).max(10).optional().describe("How long the move takes; default 0.4. Slower helps programs that track the drag"),
        after,
        watch,
      },
    },
    (a) => forward("drag", a),
  );

  reg.registerTool(
    "mouse",
    {
      description:
        "Low-level mouse steps, for what click and drag can't express. move: hover over a target or x,y (tooltips, hover menus). " +
        "down / up: press and release a button separately (optionally moving first), for fine drags with stops on the way. " +
        "look: relative motion in raw counts (dx right, dy down), the only motion first-person game cameras follow; start with about 200 and adjust.",
      inputSchema: {
        action: z.enum(["move", "down", "up", "look"]),
        ...pointing,
        button: z.enum(["left", "right", "middle"]).optional().describe("For down / up; default left"),
        dx: z.number().int().min(-20000).max(20000).optional().describe("For look: horizontal counts"),
        dy: z.number().int().min(-20000).max(20000).optional().describe("For look: vertical counts"),
        seconds: z.number().min(0).max(5).optional().describe("For look: spread the motion over this long; default 0.2"),
        after,
      },
    },
    (a) => forward("mouse", a),
  );

  const { runner } = link;

  reg.registerTool(
    "run",
    {
      description:
        "Carry out a plan: steps named by what they act on, written without looking first. The whole plan is checked before any of it runs. Returns at once (queued), " +
        "with the reports of plans that finished since; write the next plan while this one runs, or call progress. A plan that fails cancels the plans queued behind it, " +
        "and the next run call returns that failure instead of queueing. " +
        "Steps: {open: program/URL, browser?: \"asctrl\" | \"user\" (Asctrl's own browser, or the user's through the extension: steps act in the page, without the mouse)} · {click: text} · {press: text} (UI Automation, no mouse) · {fill: {label: value, …}} · {type: text, into?: label, enter?} · " +
        "{keys: \"ctrl+s\"} · {expect: text shown on success, gone?, timeoutSec?} · {check: a claim about the window (\"the email was sent\"), judged by the decision model, timeoutSec?} · " +
        "{await: window title, gone?} · {scroll: up|down|left|right, target?, amount?} · {read: grid name} · " +
        "{watch: {x0, y0, x1, y1} (a region on the latest screenshot), until: \">= 141 | <= 133\" or text, everyMs?, confirm? (readings in a row, default 2), timeoutSec? (default 60, up to 600)}: reads the number there " +
        "about 8 times a second and goes on the moment one condition holds, so the next step (a sell, a click) reacts within a fraction of a second; fails if none held in time · {tool: " + STEP_TOOLS.join("|") + ", …its args}. " +
        "Each step may add window. Text is matched on the computer first; when it matches several controls or none, a decision model picks among a few candidates, " +
        "and only a confident choice is acted on (never something irreversible the plan didn't name). The pace is chosen for you: a step repeated over rows goes fast once its first run " +
        "showed it brings nothing up. Popups that turn up meanwhile are dismissed in between steps: by on, or by a button that only dismisses (Not now, 关闭). End plans with expect.",
      inputSchema: {
        steps: z.array(z.record(z.string(), z.unknown())).min(1).max(60),
        goal: z.string().optional().describe("One line on what the plan is for; given to the decision model when it has to choose"),
        when: z.string().optional().describe("Start only once this text is on screen (alternatives with |), e.g. what the previous plan should have brought up"),
        whenSec: z.number().min(0).max(60).optional().describe("How long to wait for when; default 5"),
        on: z.record(z.string(), z.string()).optional().describe('Popups you expect: {"text a popup shows": "what to press"}, e.g. {"Leave site?": "Leave"}'),
        window: windowArg.optional(),
        repeat: z
          .array(z.union([z.array(z.union([z.string(), z.number(), z.boolean()])), z.record(z.string(), z.union([z.string(), z.number(), z.boolean()]))]))
          .max(1000)
          .optional()
          .describe('Run the steps once per row, writing them only once: [["7","3","21"], …] fills $1 $2 $3 in any step text, [{a:"7"}, …] fills ${a}'),
        grids: z
          .record(z.string(), z.object({ x0: z.number().min(0), y0: z.number().min(0), x1: z.number().min(0), y1: z.number().min(0), rows: z.number().int().min(1).max(100), cols: z.number().int().min(1).max(100) }))
          .optional()
          .describe('Grids drawn on screen, by their outer corners on the latest screenshot: {board: {x0, y0, x1, y1, rows: 9, cols: 9}}. Name a cell "board[r,c]" (from 1) wherever a step takes a target'),
        fast: z.boolean().optional().describe("Leave unset: the pace is chosen per step. true: inputs never wait for the screen to settle; false: they always do"),
        wait: z.boolean().optional().describe("Return only when the plan has run, with its report; default false"),
      },
    },
    async ({ wait, ...plan }) => {
      try {
        const r = await runner.run(plan, !!wait);
        return say(r.text, !r.ok);
      } catch (e) {
        return errorReply(e);
      }
    },
  );

  reg.registerTool(
    "progress",
    {
      description: "How queued plans went: a report for each that finished since last asked (✓/✗ per step, and where things stand after a failure), and where the running one is.",
      inputSchema: {
        waitSec: z.number().min(0).max(120).optional().describe("Wait up to this long for everything queued to finish; default 0 (answer now)"),
      },
      annotations: { readOnlyHint: true },
    },
    async ({ waitSec }) => {
      const r = await runner.progress(waitSec ?? 0);
      return say(r.text, !r.ok);
    },
  );

  reg.registerTool(
    "timeline",
    {
      description:
        "What happened to the windows lately, from the computer's own timeline (no looking needed): what came up, went away, came to the front, was minimized or restored, with how long ago. " +
        "Answers \"what just popped up?\" or \"did the dialog close?\" at once.",
      inputSchema: {
        seconds: z.number().min(1).max(3600).optional().describe("How far back; default 60"),
        window: z.string().optional().describe("Only windows whose title or program has this"),
      },
      annotations: { readOnlyHint: true },
    },
    (a) => forward("timeline", a),
  );

  reg.registerTool(
    "exec",
    {
      description:
        "Run a program on the computer: JavaScript (async; top-level await and return work) that the computer runs itself, step after step, " +
        "with loops, ifs and variables, so a long or branching task needs no round trip per step. It has no files, network or processes of its own: " +
        "only these calls, each a tool run as you would run it (replies are { ok, text, error, data }): " +
        "click(target, opts) · press(target) · type(target, text, opts) (target null: where the focus is) · fill({ label: value, … }) · keys(\"ctrl+s\") · " +
        "open(appOrUrl, opts) (opts.browser: \"asctrl\" for Asctrl's own browser, \"user\" for the user's through the extension) · waitFor(text, opts) · waitGone(text) · expect(text, opts) (stops there if it doesn't show in 5 s) · " +
        "shows(text) → true/false · read(window) → text · look(window) → observe's reply · tool(name, args) for any other tool · attempt(name, args) (returns a failure instead of stopping) · " +
        "ask(question, details) → your answer · sleep(ms) · log(…) · on(text, press) (a popup showing text that may turn up at any point: press that in it, in between steps) · " +
        "until(() => condition, { timeoutSec, everyMs }) (waits until it is true, e.g. until(() => shows('Done')) or a number read() gives) · args (what you pass). opts are the tool's own arguments (window, nth, role, timeoutSec, …). " +
        "When an action fails, the program does not end: it stops at that line (stuck) and this call returns with the failure; fix what is needed with single actions, " +
        "then resume it (retry, skip, instead, code, abort); being stuck also lists the windows that came up and went while it ran. Returns when the program ends, gets stuck, asks, or waitSec passes (then resume waits again). " +
        "Write it over args (the rows, the values) and give learn: a program that ends well is then kept as a skill on that computer, and skill: name runs it again with new args, no planning needed. " +
        "A program that ends well without learn is kept too, named after what it did (learn: false keeps it from being kept). A skill runs only when the window in front looks like one it was made in (force: true runs it anyway); " +
        "one put right with instead (the same action aimed at another control) is changed to match once that run ends well.",
      inputSchema: {
        script: z.string().optional().describe("The program's JavaScript (or skill)"),
        skill: z.string().optional().describe("Run this kept skill instead of a script (skills finds them); args are its args"),
        learn: z.union([z.object({ name: z.string(), description: z.string() }), z.literal(false)]).optional().describe("Keep the program as a skill when it ends well: a short name, and what it does in words a later search would use (the program, the task). false: don't keep it at all"),
        force: z.boolean().optional().describe("With skill: run it even if the window in front doesn't look like one it was made in"),
        args: z.unknown().optional().describe("Given to the program as args (rows to go through, values to fill in)"),
        waitSec: z.number().min(0).max(300).optional().describe("Wait up to this long before answering; default 30"),
        timeoutSec: z.number().min(1).max(3600).optional().describe("Longest the program may run in all; default 600"),
      },
    },
    (a) => program("exec", a),
  );

  reg.registerTool(
    "resume",
    {
      description:
        "Answer a program that is stuck or asking, or wait for a running one again. Stuck on an action: retry (after you fixed things), skip (value: what the program gets back instead), " +
        "instead { tool, args } (do this action in its place), code (JavaScript run in the program in the action's place, with its calls and args: several steps, a loop; what it returns is what the action returns), " +
        "or abort (the program gets an error it may catch). Asking: answer. stop: true ends it. Nothing: wait and report where it stands.",
      inputSchema: {
        id: z.union([z.number(), z.string()]).describe("The program's id, from exec"),
        retry: z.boolean().optional(),
        skip: z.boolean().optional(),
        value: z.unknown().optional().describe("With skip: what the action returns to the program"),
        instead: z.object({ tool: z.string(), args: z.record(z.string(), z.unknown()).optional() }).optional(),
        code: z.string().optional().describe("Stuck: JavaScript to run in the program in the action's place (its calls and args are there; return what the action should return)"),
        abort: z.union([z.boolean(), z.string()]).optional().describe("true, or the error message the program gets"),
        answer: z.unknown().optional().describe("The answer to its question"),
        stop: z.boolean().optional(),
        waitSec: z.number().min(0).max(300).optional().describe("Wait up to this long before answering; default 30"),
      },
    },
    (a) => program("resume", a),
  );

  reg.registerTool(
    "skills",
    {
      description:
        "Programs that worked, kept on this computer to run again (exec with learn keeps one; exec with skill runs one). Lists them best first: by query (what the task is) " +
        "and by how well each fits the window in front now (the buttons and fields of the windows it was made in). show: one skill's script and record; forget: drop one. " +
        "Look here before writing a program for a task that may have been done before. export and import carry skills to another computer.",
      inputSchema: {
        query: z.string().optional().describe("What the task is, in words"),
        window: z.union([z.string(), z.number()]).optional().describe("Fit against this window instead of the one in front"),
        show: z.string().optional().describe("A skill's name: its script, args and how its runs went"),
        forget: z.string().optional().describe("A skill's name: drop it"),
        export: z.string().optional().describe("A skill's name, or \"all\": as JSON, to import on another computer"),
        import: z.unknown().optional().describe("Skills as export gave them (the JSON, or the list): kept here, replacing any of the same name"),
        max: z.number().min(1).max(50).optional(),
      },
      annotations: { readOnlyHint: false },
    },
    (a) => forward("skills", a),
  );

  // ---------- Programs, windows, clipboard ----------

  reg.registerTool(
    "open_app",
    {
      description:
        "Open a program, file, folder or URL as if double-clicked: notepad, control, taskmgr, ms-settings:display, C:\\Users\\xx\\Desktop\\report.docx, https://example.com. " +
        "Waits for its window, brings it to the front, and lists its controls with references. Runs with the user's current privileges. " +
        "browser: \"asctrl\" opens a URL in Asctrl's own browser instead (Chrome or Edge with a profile of its own, kept between runs: the user's usual sign-ins are not there): " +
        "in it, text targets are found in the page itself and clicked and typed into through the browser, without moving the mouse and even while other windows cover it; " +
        "it waits for the page to load, and read_text on its window reads the whole page. " +
        "browser: \"user\" opens it in a new tab of the user's own browser, with their sign-ins, worked the same way through Asctrl's extension (once the user has installed it; pages already open in that browser are worked the same way).",
      inputSchema: {
        target: z.string().describe("Program name, path, URL, or a link such as ms-settings:"),
        browser: z.enum(["default", "asctrl", "user"]).optional().describe("For a URL: the user's default browser as a double-click would (default); asctrl, Asctrl's own browser driven through the page; user, the user's own browser driven through the page by Asctrl's extension"),
        args: z.string().optional().describe("Command-line arguments"),
        electron: z.boolean().optional().describe("An Electron program (VS Code, Slack, Teams, Discord, …): start it with a debugging port so its windows are worked through their pages, as in a browser, without the mouse. It must not be running already. The port listens on this computer only, while the program runs"),
        cwd: z.string().optional().describe("Working directory"),
        timeoutSec: z.number().min(0).max(60).optional().describe("How long to wait for its window; default 5"),
        after,
      },
    },
    (a) => forward("open_app", a),
  );

  reg.registerTool(
    "focus_window",
    {
      description:
        "Switch to a window, or minimize / maximize / restore / close / move it. action move (or just giving x, y, width, height) places and sizes it, e.g. to put two windows side by side.",
      inputSchema: {
        window: windowArg,
        action: z.enum(["focus", "minimize", "maximize", "restore", "close", "move"]).optional().describe("Default focus"),
        x: z.number().optional().describe("For move: left edge, in screenshot pixels"),
        y: z.number().optional().describe("For move: top edge"),
        width: z.number().min(1).optional().describe("For move: new width, in screenshot pixels"),
        height: z.number().min(1).optional().describe("For move: new height"),
        after,
      },
    },
    (a) => forward("focus_window", a),
  );

  reg.registerTool(
    "clipboard",
    {
      description: "The clipboard. With text: put it there (paste it afterwards with press_keys ctrl+v). Without: what is on it, its text, the list of copied files, or a note that it holds an image.",
      inputSchema: { text: z.string().optional().describe("Text to put on the clipboard; omit to read it") },
    },
    (a) => forward("clipboard", a),
  );

  // ---------- Shell and system ----------

  reg.registerTool(
    "run_powershell",
    {
      description:
        "Run a PowerShell script on the computer and return its output. Commands run in one persistent session, so the current directory and variables carry over (starting in the user's home). " +
        "Often the fastest and surest way to do something that has a command: files, processes, services, settings, installs, system information, the event log. Commands that need interactive input can't run. " +
        "For commands that may take more than a few minutes (sfc /scannow, DISM, Windows Update, large copies, installers), use background: true and follow it with job. " +
        "reset: true starts a fresh session first (directory and variables cleared, background jobs stopped).",
      inputSchema: {
        script: z.string().optional().describe("PowerShell script; may be left out with reset"),
        timeoutSec: z.number().int().min(1).max(600).optional().describe("Timeout in seconds; default 60. A timeout resets the session. Ignored for background runs"),
        background: z
          .boolean()
          .optional()
          .describe("Run in the background in a separate PowerShell and return a job id immediately; does not share the session's directory or variables. Stopped when you disconnect"),
        reset: z.boolean().optional().describe("Start a fresh session first"),
      },
      annotations: { destructiveHint: true },
    },
    (a) => forward("run_powershell", a),
  );

  reg.registerTool(
    "job",
    {
      description: "A background job (run_powershell background): its status and the output since the last look (all: everything), or stop: true to stop it with the processes it started. Without jobId: every job.",
      inputSchema: {
        jobId: z.number().int().min(1).optional(),
        all: z.boolean().optional().describe("All output instead of only the new"),
        stop: z.boolean().optional().describe("Stop the job"),
      },
    },
    (a) => forward("job", a),
  );

  reg.registerTool(
    "request_admin",
    {
      description:
        "Request administrator rights. The user clicks \"Yes\" in the Windows confirmation; the driver then restarts as administrator " +
        "and this tool reconnects to it automatically (takes about 5-15 seconds). " +
        "Call it whenever the task needs admin rights: repairing system files, managing services, installing software, " +
        "or when a tool reports that the target window runs as administrator (Windows blocks input to it until the driver is elevated).",
    },
    async () => {
      const code = link.lastCode;
      const wasLocal = client.local;
      const oldPid = client.localPid;
      let res: ToolResult;
      try {
        res = await client.call("request_admin", {});
      } catch (e) {
        return errorReply(e);
      }
      const r = toContent(res);
      if (!res.ok || !(res.data as AdminData | undefined)?.restarting || (!code && !wasLocal)) return r;
      // Give the old process time to exit, so the reconnect reaches the new elevated one
      await new Promise((res) => setTimeout(res, 2000));
      const c = wasLocal ? await connectLocal(90, oldPid) : await connectWait(code, 90);
      if (c.isError) {
        const how = wasLocal ? "connect (waitSec: 60)" : `connect (code ${code}, waitSec: 60)`;
        return { content: [...r.content, ...c.content, { type: "text", text: `Reconnect with ${how}.` }], isError: true };
      }
      return { content: [...r.content, { type: "text", text: "Reconnected; now running as administrator. References from before are gone: observe again." }] };
    },
  );

  reg.registerTool(
    "restart_computer",
    {
      description:
        "Restart the computer. It counts down and restarts without asking, so unsaved work is lost; after restart and sign-in the driver opens automatically with the same code. " +
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

  // ---------- Files ----------

  const remotePath = z.string().describe("Path on the computer; supports ~ and %ENV_VARS%; relative paths are resolved against the user's home");

  reg.registerTool(
    "read_file",
    {
      description:
        "Read a text file on the computer, with line numbers. Detects UTF-8 / UTF-16 / ANSI code-page encodings. Read large files in parts with offset and limit. " +
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

  reg.registerTool(
    "write_file",
    {
      description: "Write a text file on the computer (overwrites it entirely, creates it and any folders if missing). Existing files keep their encoding by default. Prefer edit_file for small changes.",
      inputSchema: {
        path: remotePath,
        content: z.string(),
        encoding: z.enum(["utf8", "utf8bom", "utf16le", "ansi"]).optional().describe("Default: existing files keep their encoding, new files are UTF-8. ansi is recommended for .bat/.cmd"),
      },
      annotations: { destructiveHint: true },
    },
    (a) => forward("write_file", a),
  );

  reg.registerTool(
    "edit_file",
    {
      description:
        "Replace an exact piece of a text file, keeping its encoding and line endings. oldText must match the file exactly (check with read_file first, without line numbers) " +
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

  reg.registerTool(
    "list_files",
    {
      description:
        "A folder's contents (modified time, size, name). With pattern: the files under it, at any depth, whose names match, newest first; * and ? wildcards, several separated by ; (e.g. *.log;*.dmp).",
      inputSchema: {
        path: remotePath.optional().describe("Folder; defaults to the user's home"),
        pattern: z.string().optional().describe("File-name wildcard, to search below the folder"),
        maxResults: z.number().int().min(1).max(5000).optional().describe("With pattern: most files listed; default 200"),
      },
      annotations: { readOnlyHint: true },
    },
    (a) => forward("list_files", a),
  );

  reg.registerTool(
    "search_files",
    {
      description: "Search file contents in a file or folder (regular expression). Detects encodings and skips binary files and files over 10 MB.",
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

  reg.registerTool(
    "disk_usage",
    {
      description:
        "Show where disk space goes under a folder: the largest subfolders as a tree (size and share of the parent) and the largest files. " +
        "One native scan, much faster than PowerShell; a whole system drive usually takes under a minute (stops after 5 minutes with partial totals). " +
        "For free space per drive, run_powershell Get-PSDrive. Folders the user can't read are skipped and reported; after request_admin the totals are complete. " +
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

  // Copying between "this side" and the computer means this server's own disk: fine on the user's machine,
  // never on a shared server, where it would hand every caller the server's files
  if (hosted) return publish();

  reg.registerTool(
    "upload_file",
    {
      description: "Copy a file from the machine this server runs on to the connected computer (max 20 MB). For a computer connected directly, an ordinary file copy does the same.",
      inputSchema: {
        localPath: z.string().describe("File path on this side"),
        remotePath: z.string().describe("Destination path on the computer; supports ~ and %ENV_VARS%; relative paths are resolved against the user's home"),
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

  reg.registerTool(
    "download_file",
    {
      description: "Copy a file from the connected computer to the machine this server runs on (max 20 MB).",
      inputSchema: {
        remotePath: z.string().describe("File path on the computer; supports ~ and %ENV_VARS%"),
        localPath: z.string().describe("Destination on this side (a file, or an existing folder)"),
      },
    },
    async ({ remotePath, localPath }) => {
      try {
        const r = await client.call("download_file", { path: remotePath });
        if (!r.ok || !r.file) return toContent(r);
        let dest = path.resolve(localPath);
        if (/[\\/]$/.test(localPath) || (existsSync(dest) && statSync(dest).isDirectory())) {
          // The name comes from the other computer: only its last part, so it can't reach outside this folder
          const name = path.basename(String(r.file.name).replace(/\\/g, "/"));
          if (!name || name === "." || name === "..") throw new Error(`The file's name "${r.file.name}" can't be used; give a file path instead of a folder`);
          const inside = path.join(dest, name);
          if (path.dirname(inside) !== dest) throw new Error(`The file's name "${r.file.name}" can't be used; give a file path instead of a folder`);
          dest = inside;
        }
        await mkdir(path.dirname(dest), { recursive: true });
        const buf = Buffer.from(r.file.data, "base64");
        await writeFile(dest, buf);
        return say(`Saved to ${dest} (${buf.length} bytes)`);
      } catch (e) {
        return errorReply(e);
      }
    },
  );

  /** What the AI is offered: every tool kept above, or the five (see tools) */
  function publish(): McpServer {
    // Tools this server is not to offer at all (ASCTRL_TOOLS_DENY: names, comma-separated), e.g. a benchmark that
    // wants the AI to work through the apps rather than the shell
    for (const n of (process.env.ASCTRL_TOOLS_DENY ?? "").split(",").map((x) => x.trim()).filter(Boolean)) kept.delete(n);
    if (!compact) {
      for (const [name, k] of kept) server.registerTool(name, { description: k.description, inputSchema: k.schema, annotations: k.annotations as never }, k.handler as never);
    } else {
      // connect, exec and resume as they are
      for (const name of ["connect", "exec", "resume"]) {
        const k = kept.get(name);
        if (!k) continue;
        server.registerTool(name, { description: k.description, inputSchema: k.schema, annotations: k.annotations as never }, k.handler as never);
      }
      const LOOK = ["observe", "screenshot", "read_text", "wait_for", "timeline", "progress", "skills", "list_files", "read_file", "search_files", "disk_usage"].filter((n) => kept.has(n));
      const DO = [...kept.keys()].filter((n) => !LOOK.includes(n) && !["connect", "exec", "resume"].includes(n));
      // Each tool in a line: what it does (its first sentence) and its args
      const line = (n: string) => {
        const k = kept.get(n)!;
        const what = (/^[^.]*\./.exec(k.description)?.[0] ?? k.description).slice(0, 200);
        const args = Object.entries(k.schema).map(([a, t]) => (t.isOptional() ? a : `${a}*`)).join(", ");
        return `- ${n}: ${what}${args ? ` Args: ${args}` : ""}`;
      };
      const through = async (n: string, args: unknown): Promise<Reply> => {
        const k = kept.get(n);
        if (!k) return say(`No such tool: ${n}`, true);
        const parsed = z.object(k.schema).safeParse(args ?? {});
        if (!parsed.success) return say(`${n}: ${parsed.error.issues.map((i) => `${i.path.join(".") || "args"}: ${i.message}`).join("; ")}\n${k.description}`, true);
        return k.handler(parsed.data as Record<string, unknown>);
      };
      server.registerTool(
        "query",
        {
          description:
            "Look at the computer: what: the kind of look, args: its arguments (* = needed). Reading the screen as controls (observe) is cheaper and more exact than a screenshot. " +
            "help: a kind's full description.\n" +
            LOOK.map(line).join("\n"),
          inputSchema: {
            what: z.enum(LOOK as [string, ...string[]]).describe("The kind of look"),
            args: z.record(z.string(), z.unknown()).optional().describe("Its arguments"),
            help: z.boolean().optional().describe("Only say what this kind does and takes"),
          },
          annotations: { readOnlyHint: true },
        },
        async ({ what, args, help }) => (help ? say(`${what}: ${kept.get(what)?.description}\nArgs:\n${Object.entries(kept.get(what)?.schema ?? {}).map(([a, t]) => `- ${a}: ${t.description ?? ""}`).join("\n")}`) : through(what, args)),
      );
      server.registerTool(
        "act",
        {
          description:
            "Do something on the computer: do: the action, args: its arguments (* = needed). run carries out a whole plan in one call; the others are single actions for corrections, " +
            "and the computer's own work (shell, files, admin). help: an action's full description.\n" +
            DO.map(line).join("\n"),
          inputSchema: {
            do: z.enum(DO as [string, ...string[]]).describe("The action"),
            args: z.record(z.string(), z.unknown()).optional().describe("Its arguments"),
            help: z.boolean().optional().describe("Only say what this action does and takes"),
          },
        },
        async (a) => (a.help ? say(`${a.do}: ${kept.get(a.do)?.description}\nArgs:\n${Object.entries(kept.get(a.do)?.schema ?? {}).map(([k, t]) => `- ${k}: ${t.description ?? ""}`).join("\n")}`) : through(a.do, a.args)),
      );
    }

    return server;
  }

  return publish();
}

if (import.meta.main) {
  const client = new TechClient(process.env.RELAY_URL ?? DEFAULT_RELAY_URL, process.env.TECH_NAME ?? "AI", resolveRelayToken());
  await createAsctrlServer({ link: newLink(client) }).connect(new StdioServerTransport());
}
