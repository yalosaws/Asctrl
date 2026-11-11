// 控制端 MCP 服务器：让 Claude（Claude Code / Claude 桌面版）直接调用远程电脑上的工具。
// 用法：在 MCP 客户端里配置 `node src/console/mcp-server.ts`，环境变量 RELAY_URL / RELAY_TOKEN / TECH_NAME 可选。

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

const server = new McpServer({ name: "asctrl", version: "0.1.0" });

type Content = { type: "text"; text: string } | { type: "image"; data: string; mimeType: string };

function toContent(r: ToolResult) {
  const content: Content[] = [];
  if (r.image) content.push({ type: "image", data: r.image.data, mimeType: r.image.mimeType });
  if (r.text) content.push({ type: "text", text: r.text });
  if (!r.ok) content.push({ type: "text", text: `失败：${r.error ?? "未知错误"}` });
  if (!content.length) content.push({ type: "text", text: "(无输出)" });
  return { content, isError: !r.ok };
}

async function forward(tool: ToolName, args: Record<string, unknown>) {
  try {
    return toContent(await client.call(tool, args));
  } catch (e) {
    return { content: [{ type: "text" as const, text: `调用失败：${(e as Error).message}` }], isError: true };
  }
}

server.registerTool(
  "connect",
  {
    description: "用对方报给你的 6 位连接码连接远程电脑。对方需要在他的电脑上点同意，最多等 2 分钟。",
    inputSchema: { code: z.string().regex(/^\d{6}$/).describe("6 位连接码") },
  },
  async ({ code }) => {
    try {
      const host = await client.connect(code);
      return { content: [{ type: "text", text: `已连接到 ${host}` }] };
    } catch (e) {
      return { content: [{ type: "text", text: `连接失败：${(e as Error).message}` }], isError: true };
    }
  },
);

server.registerTool(
  "disconnect",
  { description: "断开当前远程电脑。" },
  async () => {
    client.disconnect();
    return { content: [{ type: "text", text: "已断开" }] };
  },
);

server.registerTool(
  "screenshot",
  {
    description: "截取远程电脑的整个屏幕（JPEG）。",
    inputSchema: { maxWidth: z.number().int().min(320).max(3840).optional().describe("缩放后的最大宽度，默认 1600") },
    annotations: { readOnlyHint: true },
  },
  (a) => forward("screenshot", a),
);

server.registerTool(
  "system_info",
  {
    description: "远程电脑的基本信息：系统版本、开机时长、CPU、内存、硬盘剩余空间。",
    annotations: { readOnlyHint: true },
  },
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

server.registerTool(
  "run_powershell",
  {
    description:
      "在远程电脑上以当前用户身份运行 PowerShell 脚本并返回输出。对方屏幕上会显示命令内容。修改系统、删除文件前先向技术员说明并确认。",
    inputSchema: {
      script: z.string().describe("PowerShell 脚本"),
      timeoutSec: z.number().int().min(1).max(600).optional().describe("超时秒数，默认 60"),
    },
    annotations: { destructiveHint: true },
  },
  (a) => forward("run_powershell", a),
);

await server.connect(new StdioServerTransport());
