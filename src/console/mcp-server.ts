// 控制端 MCP 服务器：让 Claude（Claude Code / Claude 桌面版）直接调用远程电脑上的工具。
// 用法：在 MCP 客户端里配置 `node src/console/mcp-server.ts`，环境变量 RELAY_URL / RELAY_TOKEN / TECH_NAME 可选。

import { existsSync, statSync } from "node:fs";
import { readFile, writeFile, mkdir } from "node:fs/promises";
import path from "node:path";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import { TechClient } from "./tech-client.ts";
import { DEFAULT_RELAY_URL, type ToolName, type ToolResult } from "../shared/protocol.ts";

const client = new TechClient(
  process.env.RELAY_URL ?? DEFAULT_RELAY_URL,
  process.env.TECH_NAME ?? "技术员",
  process.env.RELAY_TOKEN,
);

const server = new McpServer({ name: "asctrl", version: "0.3.0" });

type Content = { type: "text"; text: string } | { type: "image"; data: string; mimeType: string };
type Reply = { content: Content[]; isError?: boolean };

/** 上一次截图用的宽度：操作后自动截图沿用它，保证坐标比例不变 */
let lastMaxWidth = 1600;
/** 上一次截图的显示器：操作后自动截图沿用它 */
let lastDisplay = 0;

function toContent(r: ToolResult): Reply {
  const content: Content[] = [];
  if (r.image) content.push({ type: "image", data: r.image.data, mimeType: r.image.mimeType });
  if (r.text) content.push({ type: "text", text: r.text });
  if (!r.ok) content.push({ type: "text", text: `失败：${r.error ?? "未知错误"}` });
  if (!content.length) content.push({ type: "text", text: "(无输出)" });
  return { content, isError: !r.ok };
}

function errorReply(e: unknown): Reply {
  return { content: [{ type: "text", text: `调用失败：${(e as Error).message ?? e}` }], isError: true };
}

async function forward(tool: ToolName, args: Record<string, unknown>): Promise<Reply> {
  try {
    return toContent(await client.call(tool, args));
  } catch (e) {
    return errorReply(e);
  }
}

/** 鼠标键盘操作：成功后默认再截一张图，方便看到操作结果 */
async function act(tool: ToolName, args: Record<string, unknown> & { screenshot?: boolean }): Promise<Reply> {
  const { screenshot = true, ...rest } = args;
  const r = await forward(tool, rest);
  if (r.isError || !screenshot) return r;
  const shot = await forward("screenshot", { maxWidth: lastMaxWidth, display: lastDisplay });
  return { content: [...r.content, ...shot.content], isError: shot.isError };
}

const screenshotFlag = z.boolean().optional().describe("操作后是否自动截图，默认 true");
const coord = (what: string) => z.number().min(0).describe(`${what}（最近一张截图上的像素坐标）`);

server.registerTool(
  "connect",
  {
    description:
      "用对方报给你的 6 位协助码连接远程电脑。对方需要在电脑上点允许，最多等 2 分钟。" +
      "提权重启或重启电脑之后重连时传 waitSec，会一直重试到对方程序重新上线。",
    inputSchema: {
      code: z.string().regex(/^\d{6}$/).describe("6 位协助码"),
      waitSec: z.number().int().min(0).max(600).optional().describe("对方程序还没上线时最多等多少秒，默认 0（不等）"),
    },
  },
  async ({ code, waitSec = 0 }) => {
    const deadline = Date.now() + waitSec * 1000;
    for (;;) {
      try {
        const host = await client.connect(code);
        return { content: [{ type: "text", text: `已连接到 ${host}` }] };
      } catch (e) {
        const msg = (e as Error).message;
        // 只有"对方还没上线"才值得重试；拒绝、超时等直接返回
        const offline = /找不到这个连接码|已有人在连接|连接已关闭|ECONNREFUSED/.test(msg);
        if (!offline || Date.now() + 5000 > deadline) {
          return { content: [{ type: "text", text: `连接失败：${msg}` }], isError: true };
        }
        await new Promise((r) => setTimeout(r, 5000));
      }
    }
  },
);

server.registerTool("disconnect", { description: "断开当前远程电脑。" }, async () => {
  client.disconnect();
  return { content: [{ type: "text", text: "已断开" }] };
});

// ---------- 看 ----------

server.registerTool(
  "screenshot",
  {
    description:
      "截取远程电脑的屏幕（JPEG）。默认把所有显示器拼在一起截；多显示器时传 display 只截某一个，更清楚。" +
      "click/scroll/drag 等的坐标以最近一张截图为准。看不清小字时用 zoom。",
    inputSchema: {
      maxWidth: z.number().int().min(320).max(3840).optional().describe("缩放后的最大宽度，默认 1600"),
      display: z.number().int().min(0).max(16).optional().describe("第几个显示器（从 1 开始），0 或不传表示全部"),
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
      "把最近一张截图里的一块矩形区域按原始分辨率重新截取放大，用来看清小字、图标、报错细节。" +
      "参数是最近一张截图上的坐标；放大图只用来看，之后的点击坐标仍然以那张 screenshot 为准。",
    inputSchema: {
      x0: coord("左上角横坐标"),
      y0: coord("左上角纵坐标"),
      x1: coord("右下角横坐标"),
      y1: coord("右下角纵坐标"),
      maxWidth: z.number().int().min(320).max(3840).optional().describe("放大图最大宽度，默认 1600"),
    },
    annotations: { readOnlyHint: true },
  },
  (a) => forward("zoom", a),
);

server.registerTool(
  "cursor_position",
  { description: "查看鼠标现在的位置（换算成最近一张截图上的坐标）。", annotations: { readOnlyHint: true } },
  () => forward("cursor_position", {}),
);

server.registerTool(
  "wait",
  {
    description: "等待几秒（例如等程序打开、等安装进度），然后自动截图。",
    inputSchema: { seconds: z.number().min(0.1).max(60).describe("等待秒数"), screenshot: screenshotFlag },
    annotations: { readOnlyHint: true },
  },
  async ({ seconds, screenshot = true }) => {
    await new Promise((r) => setTimeout(r, seconds * 1000));
    if (!screenshot) return { content: [{ type: "text", text: `已等待 ${seconds} 秒` }] };
    return forward("screenshot", { maxWidth: lastMaxWidth, display: lastDisplay });
  },
);

server.registerTool(
  "system_info",
  { description: "远程电脑的基本信息：系统版本、开机时长、CPU、内存、硬盘剩余空间。", annotations: { readOnlyHint: true } },
  () => forward("system_info", {}),
);

server.registerTool(
  "list_processes",
  {
    description: "按内存占用列出远程电脑上的进程。",
    inputSchema: { top: z.number().int().min(1).max(200).optional().describe("显示前几个，默认 30") },
    annotations: { readOnlyHint: true },
  },
  (a) => forward("list_processes", a),
);

server.registerTool(
  "event_log",
  {
    description: "读取远程电脑 Windows 事件日志里的错误和警告。",
    inputSchema: {
      log: z.enum(["System", "Application"]).optional().describe("默认 System"),
      hours: z.number().int().min(1).max(720).optional().describe("最近多少小时，默认 24"),
      max: z.number().int().min(1).max(200).optional().describe("最多几条，默认 30"),
    },
    annotations: { readOnlyHint: true },
  },
  (a) => forward("event_log", a),
);

// ---------- 命令 ----------

server.registerTool(
  "run_powershell",
  {
    description:
      "在远程电脑上运行 PowerShell 脚本并返回输出。命令在同一个常驻会话里执行，当前目录和变量会保留（初始目录是用户目录）。" +
      "不能运行需要交互输入的命令。对方屏幕上会显示命令内容。修改系统、删除文件前先向技术员说明并确认。" +
      "可能超过几分钟的命令（sfc /scannow、DISM、Windows 更新、大文件复制、安装程序）用 background: true 在后台跑，再用 job_output 看进度。",
    inputSchema: {
      script: z.string().describe("PowerShell 脚本"),
      timeoutSec: z.number().int().min(1).max(600).optional().describe("超时秒数，默认 60。超时会重置会话。后台运行时无效"),
      background: z
        .boolean()
        .optional()
        .describe("在独立的 PowerShell 里后台运行，立即返回任务编号；不共享会话的目录和变量。技术员断开时会被停止"),
    },
    annotations: { destructiveHint: true },
  },
  (a) => forward("run_powershell", a),
);

server.registerTool(
  "job_output",
  {
    description: "查看后台任务的状态和上次查看之后的新输出。不传 jobId 列出所有后台任务。",
    inputSchema: {
      jobId: z.number().int().min(1).optional(),
      all: z.boolean().optional().describe("返回全部输出而不是只返回新输出"),
    },
    annotations: { readOnlyHint: true },
  },
  (a) => forward("job_output", a),
);

server.registerTool(
  "stop_job",
  { description: "停止一个后台任务（连同它启动的子进程）。", inputSchema: { jobId: z.number().int().min(1) } },
  (a) => forward("stop_job", a),
);

server.registerTool(
  "reset_shell",
  { description: "重置远程 PowerShell 会话（清掉当前目录和变量）。" },
  () => forward("reset_shell", {}),
);

server.registerTool(
  "request_admin",
  {
    description:
      "请求管理员权限。对方会先在程序里点允许，再在 Windows 确认窗口点「是」；之后程序以管理员身份重启，" +
      "需要等约 5 秒用同一个协助码重新 connect（对方要再点一次允许）。只在确实需要时使用，例如修复系统文件、管理服务、安装软件。",
  },
  () => forward("request_admin", {}),
);

server.registerTool(
  "restart_computer",
  {
    description:
      "重启远程电脑。会先在对方程序里请他同意（提醒保存文件），然后倒计时重启；重启登录后协助程序自动打开，协助码不变，" +
      "之后用 connect（建议 waitSec: 300）重新连接，对方需要再点一次允许。" +
      "prepareOnly: 只登记“下次登录自动打开”而不重启（安装程序要重启、或请对方手动重启时用）；cancel: 取消待执行的重启。",
    inputSchema: {
      delaySec: z.number().int().min(5).max(600).optional().describe("多少秒后重启，默认 15"),
      prepareOnly: z.boolean().optional(),
      cancel: z.boolean().optional(),
    },
    annotations: { destructiveHint: true },
  },
  (a) => forward("restart_computer", a),
);

// ---------- 鼠标键盘 ----------

server.registerTool(
  "click",
  {
    description: "在远程屏幕上点击。坐标用最近一张截图上的像素坐标。",
    inputSchema: {
      x: coord("横坐标"),
      y: coord("纵坐标"),
      button: z.enum(["left", "right", "middle"]).optional().describe("默认 left"),
      count: z.number().int().min(1).max(3).optional().describe("点击次数，2 为双击，3 为三击（选中整行），默认 1"),
      modifiers: z.string().optional().describe("点击时按住的键，例如 ctrl、shift、ctrl+shift"),
      screenshot: screenshotFlag,
    },
  },
  (a) => act("click", a),
);

server.registerTool(
  "scroll",
  {
    description: "把鼠标移到指定位置后滚动滚轮。",
    inputSchema: {
      x: coord("横坐标"),
      y: coord("纵坐标"),
      amount: z.number().int().min(-50).max(50).optional().describe("滚动格数，正数向下（横向时向右），负数向上（向左），默认 3"),
      horizontal: z.boolean().optional().describe("横向滚动"),
      screenshot: screenshotFlag,
    },
  },
  (a) => act("scroll", a),
);

server.registerTool(
  "drag",
  {
    description: "按住鼠标左键从一点拖到另一点。",
    inputSchema: {
      fromX: coord("起点横坐标"),
      fromY: coord("起点纵坐标"),
      toX: coord("终点横坐标"),
      toY: coord("终点纵坐标"),
      screenshot: screenshotFlag,
    },
  },
  (a) => act("drag", a),
);

server.registerTool(
  "mouse_move",
  {
    description: "把鼠标移到某处但不点击，用来触发悬停提示、展开悬停菜单。",
    inputSchema: { x: coord("横坐标"), y: coord("纵坐标"), screenshot: screenshotFlag },
  },
  (a) => act("mouse_move", a),
);

const buttonArg = z.enum(["left", "right", "middle"]).optional().describe("默认 left");

server.registerTool(
  "mouse_down",
  {
    description: "按下鼠标键不松开（可先移到某处）。配合 mouse_move、mouse_up 做精细拖动，例如拖滑块、框选。",
    inputSchema: { x: coord("横坐标").optional(), y: coord("纵坐标").optional(), button: buttonArg, screenshot: screenshotFlag },
  },
  (a) => act("mouse_down", a),
);

server.registerTool(
  "mouse_up",
  {
    description: "松开鼠标键（可先移到某处）。",
    inputSchema: { x: coord("横坐标").optional(), y: coord("纵坐标").optional(), button: buttonArg, screenshot: screenshotFlag },
  },
  (a) => act("mouse_up", a),
);

server.registerTool(
  "type_text",
  {
    description: "在当前焦点位置输入文字，支持中文，不受对方输入法影响。换行符会按回车。不确定焦点时先 click 输入框。",
    inputSchema: { text: z.string().min(1), screenshot: screenshotFlag },
  },
  (a) => act("type_text", a),
);

server.registerTool(
  "press_keys",
  {
    description:
      "按键或组合键，例如 enter、esc、tab、ctrl+c、ctrl+shift+esc、win+r、alt+f4、f5。" +
      "按键发给当前有焦点的窗口，不确定焦点时先 click 一下目标窗口。",
    inputSchema: { keys: z.string().describe("用 + 连接的按键名"), screenshot: screenshotFlag },
  },
  (a) => act("press_keys", a),
);

server.registerTool(
  "hold_key",
  {
    description: "按住按键或组合键一段时间再松开，例如按住 shift 3 秒、长按电源相关快捷键。",
    inputSchema: {
      keys: z.string().describe("用 + 连接的按键名"),
      seconds: z.number().min(0.05).max(10).optional().describe("按住多少秒，默认 1"),
      screenshot: screenshotFlag,
    },
  },
  (a) => act("hold_key", a),
);

// ---------- 窗口、程序、剪贴板 ----------

server.registerTool(
  "list_windows",
  {
    description: "列出远程电脑上打开的窗口（标题、程序名、是否最小化、在截图上的中心坐标），从最前面到最后面。",
    annotations: { readOnlyHint: true },
  },
  () => forward("list_windows", {}),
);

server.registerTool(
  "focus_window",
  {
    description:
      "切换到某个窗口（或最小化 / 最大化 / 还原 / 关闭它）。用 list_windows 里的 handle，或者用标题、程序名里的关键字。",
    inputSchema: {
      handle: z.number().int().optional().describe("list_windows 里方括号中的数字"),
      title: z.string().optional().describe("标题或程序名关键字，例如 微信、chrome、记事本"),
      action: z.enum(["focus", "minimize", "maximize", "restore", "close"]).optional().describe("默认 focus"),
      screenshot: screenshotFlag,
    },
  },
  (a) => act("focus_window", a),
);

server.registerTool(
  "open_app",
  {
    description:
      "像双击一样打开程序、文件、文件夹或网址。例如 notepad、control、taskmgr、ms-settings:display、" +
      "C:\\Users\\xx\\Desktop\\报告.docx、https://example.com。以对方当前权限运行。",
    inputSchema: {
      target: z.string().describe("程序名、路径、网址或 ms-settings: 之类的链接"),
      args: z.string().optional().describe("命令行参数"),
      cwd: z.string().optional().describe("工作目录"),
      screenshot: screenshotFlag,
    },
  },
  (a) => act("open_app", a),
);

server.registerTool(
  "read_clipboard",
  { description: "读取远程电脑剪贴板里的文字（或复制的文件列表）。", annotations: { readOnlyHint: true } },
  () => forward("read_clipboard", {}),
);

server.registerTool(
  "write_clipboard",
  {
    description: "把文字放进远程电脑的剪贴板，之后可以用 press_keys ctrl+v 粘贴。长文本比 type_text 快。",
    inputSchema: { text: z.string() },
  },
  (a) => forward("write_clipboard", a),
);

// ---------- 文件 ----------

const remotePath = z.string().describe("远程路径，支持 ~ 和 %环境变量%，相对路径以对方用户目录为准");

server.registerTool(
  "read_file",
  {
    description:
      "读取远程电脑上的文本文件，带行号。自动识别 UTF-8 / UTF-16 / GBK 编码。大文件用 offset、limit 分段读。" +
      "适合看日志、配置文件、hosts、脚本。",
    inputSchema: {
      path: remotePath,
      offset: z.number().int().min(1).optional().describe("从第几行开始，默认 1"),
      limit: z.number().int().min(1).max(20000).optional().describe("读多少行，默认 2000"),
    },
    annotations: { readOnlyHint: true },
  },
  (a) => forward("read_file", a),
);

server.registerTool(
  "write_file",
  {
    description: "在远程电脑上写入文本文件（整个覆盖，不存在就新建，会自动建文件夹）。已有文件默认保持原来的编码。改动小时优先用 edit_file。",
    inputSchema: {
      path: remotePath,
      content: z.string(),
      encoding: z.enum(["utf8", "utf8bom", "utf16le", "ansi"]).optional().describe("默认：已有文件沿用原编码，新文件 UTF-8。.bat/.cmd 建议 ansi"),
    },
    annotations: { destructiveHint: true },
  },
  (a) => forward("write_file", a),
);

server.registerTool(
  "edit_file",
  {
    description:
      "精确替换远程文本文件里的一段内容，保持原编码和换行方式。oldText 要和文件内容完全一致（先 read_file 看，不要带行号），" +
      "并且在文件里只出现一次，否则多带几行上下文或传 replaceAll。",
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
    description: "列出远程文件夹里的内容（修改时间、大小、名字）。",
    inputSchema: { path: remotePath.optional().describe("默认用户目录") },
    annotations: { readOnlyHint: true },
  },
  (a) => forward("list_dir", a),
);

server.registerTool(
  "find_files",
  {
    description: "在远程文件夹里按文件名递归查找，支持 * 和 ? 通配符，多个用 ; 隔开（例如 *.log;*.dmp）。结果按修改时间从新到旧。",
    inputSchema: {
      path: remotePath.describe("从哪个文件夹开始找"),
      pattern: z.string().describe("文件名通配符"),
      maxResults: z.number().int().min(1).max(5000).optional().describe("默认 200"),
    },
    annotations: { readOnlyHint: true },
  },
  (a) => forward("find_files", a),
);

server.registerTool(
  "search_files",
  {
    description: "在远程文件或文件夹里按内容搜索（正则表达式），自动识别编码，跳过二进制和超过 10 MB 的文件。",
    inputSchema: {
      path: remotePath.describe("文件或文件夹"),
      pattern: z.string().describe("正则表达式"),
      glob: z.string().optional().describe("只搜这些文件名，例如 *.log;*.txt"),
      ignoreCase: z.boolean().optional().describe("默认 true"),
      context: z.number().int().min(0).max(10).optional().describe("每处匹配前后多显示几行，默认 0"),
      maxResults: z.number().int().min(1).max(2000).optional().describe("最多几处匹配，默认 100"),
    },
    annotations: { readOnlyHint: true },
  },
  (a) => forward("search_files", a),
);

server.registerTool(
  "upload_file",
  {
    description: "把你这台电脑上的文件传到远程电脑（最大 20 MB）。",
    inputSchema: {
      localPath: z.string().describe("你这边的文件路径"),
      remotePath: z.string().describe("远程保存路径，支持 ~ 和 %环境变量%，相对路径以对方用户目录为准"),
    },
  },
  async ({ localPath, remotePath }) => {
    try {
      const buf = await readFile(localPath);
      if (buf.length > 20 * 1024 * 1024) throw new Error("文件超过 20 MB");
      return await forward("upload_file", { path: remotePath, data: buf.toString("base64") });
    } catch (e) {
      return errorReply(e);
    }
  },
);

server.registerTool(
  "download_file",
  {
    description: "把远程电脑上的文件取回到你这台电脑（最大 20 MB）。",
    inputSchema: {
      remotePath: z.string().describe("远程文件路径，支持 ~ 和 %环境变量%"),
      localPath: z.string().describe("你这边的保存路径（文件或已存在的文件夹）"),
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
      return { content: [{ type: "text", text: `已保存到 ${dest}（${buf.length} 字节）` }] };
    } catch (e) {
      return errorReply(e);
    }
  },
);

await server.connect(new StdioServerTransport());
