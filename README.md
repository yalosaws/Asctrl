# Asctrl — AI 辅助远程修电脑

帮不懂电脑的人修电脑：对方运行一个小程序、报给你一个 6 位协助码，你（和 AI）就能远程看屏幕、点鼠标、打字、执行命令、传文件，必要时申请管理员权限，对方屏幕上能看到你的每一步操作。

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

界面调试（浏览器里用演示数据，地址加 `?state=ready|consent|admin|reboot|connected|offline|connecting` 切换状态）：

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

1. 启动参数 `--relay <地址>`
2. 环境变量 `ASCTRL_RELAY_URL`
3. exe 同目录下的 `relay.txt`（里面写一行地址）
4. 编译时的环境变量 `ASCTRL_RELAY_URL`（推荐：编译前设好，发给对方的 exe 就不用配置）
5. 默认 `ws://localhost:8787`

启动参数 `--code <6位数字>` 可以指定协助码（管理员重启时用它保持同一个码）。`ASCTRL_CODE`（固定协助码）和 `ASCTRL_AUTO_ACCEPT`（自动同意）环境变量只在 debug 编译里生效，用于自动化测试。

**管理员模式**：AI 调用 `request_admin` → 程序里弹出说明，对方点允许 → Windows 弹出 UAC，对方点"是" → 程序以管理员身份重启，协助码不变 → AI 重新 `connect`，对方再点一次允许。顶部会显示"管理员"标记。

## 线上中继

中继部署在 `ws://124.222.182.40:8787`（服务器上 systemd 服务 `asctrl-relay`，代码在 `~/asctrl-relay`，腾讯云安全组和 ufw 已放行 8787）。

- 被控端：打包时设 `ASCTRL_RELAY_URL=ws://124.222.182.40:8787`，地址就编进 exe，对方不用配置。
- 控制端：`.mcp.json` 已指向这个地址；中继启用了 token，从环境变量 `ASCTRL_RELAY_TOKEN` 读取（不进仓库，向维护者索取）。
- 更新中继代码：把 `src/relay/server.ts` 和 `src/shared/protocol.ts` 拷到服务器 `~/asctrl-relay/src/` 对应位置，然后 `sudo systemctl restart asctrl-relay`。

## 现有工具

共 37 个，对标 Claude 直接控制本机时的工具（命令行、文件读写搜索、屏幕鼠标键盘、剪贴板、窗口、后台任务）。✅ 表示命令行被控端（Node 版）也支持，其余只有图形界面版支持。

| 类别 | 工具 | 作用 |
|---|---|---|
| 连接 | `connect` ✅ / `disconnect` ✅ | 用协助码连接 / 断开。`waitSec` 会一直重试到对方程序重新上线（提权、重启后用） |
| 看 | `screenshot` ✅ | 截图（JPEG，默认缩到 1600 宽）。多显示器时 `display` 只截某一个 |
| | `zoom` | 按原始分辨率放大截图里的一块，看清小字和图标 |
| | `cursor_position` | 鼠标当前在截图上的坐标 |
| | `wait` | 等几秒后自动截图（只在控制端实现） |
| 系统 | `system_info` ✅ / `list_processes` ✅ / `event_log` ✅ | 系统概况、进程、错误日志 |
| 命令 | `run_powershell` ✅ | 在常驻会话里执行（目录和变量保留）；`background: true` 后台运行，不受超时限制 |
| | `job_output` / `stop_job` | 查看后台任务进度和新输出 / 停止（连子进程一起） |
| | `reset_shell` | 重置 PowerShell 会话 |
| 鼠标 | `click` | 单击 / 双击 / 三击，左中右键，可按住 ctrl、shift 等 |
| | `mouse_move` / `mouse_down` / `mouse_up` | 悬停；分步按下、移动、松开做精细拖动 |
| | `drag` / `scroll` | 拖动；纵向或横向滚动 |
| 键盘 | `type_text` | 输入文字，支持中文，不受输入法影响 |
| | `press_keys` / `hold_key` | 按键 / 组合键（含符号键、音量键），或按住一段时间 |
| 窗口 | `list_windows` | 打开的窗口：标题、程序、状态、在截图上的位置 |
| | `focus_window` | 切换到 / 最小化 / 最大化 / 还原 / 关闭某个窗口 |
| | `open_app` | 打开程序、文件、文件夹、网址或 `ms-settings:` 页面 |
| 剪贴板 | `read_clipboard` / `write_clipboard` | 读写剪贴板（长文本比 `type_text` 快） |
| 文件 | `read_file` / `write_file` / `edit_file` | 读（带行号、分段）、写、精确替换。自动识别 UTF-8 / UTF-16 / GBK，写回保持原编码和换行 |
| | `list_dir` / `find_files` / `search_files` | 列目录；按文件名通配符递归找；按内容正则搜索 |
| | `upload_file` / `download_file` | 在两台电脑之间传文件（≤ 20 MB） |
| 权限 | `request_admin` | 申请管理员权限（见上） |
| | `restart_computer` | 经对方同意后重启电脑，重启登录后程序自动打开、协助码不变；`prepareOnly` 只登记自动打开，`cancel` 取消 |

鼠标、键盘、窗口操作完成后，MCP 控制端会自动再截一张图返回，AI 能马上看到操作结果（传 `screenshot: false` 可以关掉）。

**重启续连**：`restart_computer` → 对方在程序里点允许 → 倒计时重启 → 登录后程序通过注册表 `RunOnce` 自动打开一次（同一个协助码）→ AI 用 `connect`（`waitSec: 300`）重新连接，对方再点一次允许。

**后台任务**在独立的 PowerShell 进程里运行，不共享常驻会话的目录和变量；技术员断开时全部停止。

**限制**：普通权限下无法操作以管理员身份运行的窗口；UAC 确认窗口和锁屏界面任何权限都点不了，只能请对方自己点。

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

图形界面版额外测试：会话保留目录和变量、`reset_shell`、文件上传下载往返、超范围坐标和未知按键被拒绝、`zoom` 和指定显示器截图、窗口列表、文件读写改搜（UTF-8 / GBK / UTF-16）、剪贴板往返（测完还原）、后台任务和停止、重启续连的登记与取消（不会真的重启）。鼠标点击和打字会动到真实桌面，不放在自动测试里，用 `test/demo-step.ts` 手动演示：

```bash
node test/demo-step.ts ws://localhost:8787 <协助码> @steps.json
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
- [x] 鼠标点击、键盘输入工具（让 AI 能直接操作界面）
- [x] 文件上传下载
- [x] 常驻 PowerShell 会话
- [x] 管理员模式（对方同意 + UAC 后提权重启）
- [ ] 实时画面（或接入 RustDesk / MeshCentral 做远程桌面）
- [ ] 网页版控制台：不用 Claude 时自己也能看屏幕、点按钮
