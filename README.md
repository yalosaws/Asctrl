# Asctrl — AI 辅助远程修电脑

帮不懂电脑的人修电脑：对方运行一个小程序、报给你一个 6 位连接码，你（和 AI）就能远程截图、查系统信息、看进程和错误日志、执行命令，对方屏幕上能看到你的每一步操作。

## 架构

```
 需要帮助的电脑                   你的服务器                     你的电脑
┌──────────────┐   WebSocket   ┌──────────────┐   WebSocket   ┌─────────────────────┐
│ 被控端 agent │ ────────────▶ │  中继 relay  │ ◀──────────── │ MCP 控制端          │
│ 显示连接码   │               │ 按连接码配对 │               │  ↑ stdio            │
│ 询问是否同意 │ ◀──转发调用── │ 原样转发消息 │ ──转发调用──▶ │ Claude（AI + 你）   │
└──────────────┘               └──────────────┘               └─────────────────────┘
```

- 两边都只**主动连出**到中继，所以对方在路由器/NAT 后面也能连。
- 每次连接都要对方在被控端点 **同意**；所有操作都显示在对方屏幕上。
- 控制端是一个 **MCP 服务器**，Claude Code / Claude 桌面版可以直接把远程电脑当工具用。

| 目录 | 说明 |
|---|---|
| `src/shared/protocol.ts` | 三方通用的消息格式 |
| `src/relay/server.ts` | 中继服务器 |
| `agent-app/` | **图形界面被控端**（Tauri：Rust 后台 + 网页界面），发给对方用的就是它 |
| `src/agent/` | 命令行被控端（Node 版，开发和测试用） |
| `src/console/tech-client.ts` | 控制端连接库 |
| `src/console/mcp-server.ts` | 控制端 MCP 服务器 |
| `test/e2e.ts` | 端到端测试（两种被控端都能测） |

## 图形界面被控端（agent-app）

打开后大字显示 6 位协助码；有人连接时窗口自动弹到最前，问对方"允许 / 拒绝"（2 分钟不点自动拒绝）；协助中实时显示每一步操作和完整命令，随时可以点"结束协助"。

开发需要 Rust（`rustup`）和 Visual Studio C++ 生成工具。

```bash
cd agent-app
npm install
```

界面调试（浏览器里用演示数据，地址加 `?state=ready|consent|connected|offline|connecting` 切换状态）：

```bash
npm run dev
```

真窗口调试：

```bash
npm run tauri dev
```

打包正式版（生成 exe 和安装包，在 `src-tauri/target/release/`）：

```bash
npm run tauri build
```

**中继地址**按以下顺序决定：

1. 环境变量 `ASCTRL_RELAY_URL`
2. exe 同目录下的 `relay.txt`（里面写一行地址）
3. 编译时的环境变量 `ASCTRL_RELAY_URL`（推荐：编译前设好，发给对方的 exe 就不用配置）
4. 默认 `ws://localhost:8787`

`ASCTRL_CODE`（固定协助码）和 `ASCTRL_AUTO_ACCEPT`（自动同意）只在 debug 编译里生效，用于自动化测试，正式版里无效。

## 现有工具

| 工具 | 作用 |
|---|---|
| `connect` / `disconnect` | 用连接码连接 / 断开 |
| `screenshot` | 全屏截图（JPEG，默认缩到 1600 宽） |
| `system_info` | 系统版本、开机时长、CPU、内存、硬盘剩余 |
| `list_processes` | 按内存排序的进程列表 |
| `event_log` | System / Application 日志里的错误和警告 |
| `run_powershell` | 执行任意 PowerShell 脚本 |

## 快速开始（需要 Node.js 24+）

```bash
npm install
```

1. 启动中继（本机测试用 localhost，正式使用放到有公网 IP 的服务器上）：

```bash
npm run relay
```

2. 在需要帮助的电脑上启动被控端，记下屏幕上的连接码：

```bash
npm run agent -- --relay ws://你的服务器:8787
```

3. 在你的电脑上用 Claude Code 打开这个文件夹，项目里的 `.mcp.json` 已经配好 `asctrl` 工具（改一下里面的 `RELAY_URL`），然后对 Claude 说：

> 用连接码 123456 连上去，看看这台电脑为什么这么卡

Claude 桌面版的话，把 `.mcp.json` 里的内容加到它的 MCP 配置里，`args` 写成 `mcp-server.ts` 的绝对路径。

## 测试

```bash
npm test
```

本机起一个中继和一个自动同意的被控端，依次调用全部工具，截图保存在 `test/out/screenshot.jpg`。

测图形界面版（先在 `agent-app` 里运行 `npm run tauri build -- --debug --no-bundle`）：

```bash
node test/e2e.ts --app agent-app/src-tauri/target/debug/asctrl-agent.exe
```

```bash
npm run typecheck
```

## 安全

这类软件在结构上和远控木马一样，区别全在**对方知情同意**。目前做到了：

- 每次连接需要被控端用户确认
- 被控端显示每一个操作和完整命令
- 关闭窗口即断开
- 中继可设置 `RELAY_TOKEN` 环境变量，只有带对的 token 的控制端才能连

**正式使用前必须补上：**

- [ ] 中继放在 HTTPS 反向代理后面，用 `wss://`（现在是明文）
- [ ] 连接码限制尝试次数 / 过期时间，防暴力猜测
- [ ] 危险命令在被控端二次确认（可选）
- [ ] 操作日志落盘

## 后续路线

- [x] 被控端图形界面（大字协助码 + 同意弹窗 + 操作记录）
- [ ] 代码签名，减少 SmartScreen / 杀毒软件拦截
- [ ] 托盘图标、开机不自启（只在需要时打开）
- [ ] 鼠标点击、键盘输入工具（让 AI 能直接操作界面）
- [ ] 实时画面（或接入 RustDesk / MeshCentral 做远程桌面）
- [ ] 文件上传下载
- [ ] 网页版控制台：不用 Claude 时自己也能看屏幕、点按钮
