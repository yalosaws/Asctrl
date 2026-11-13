# Asctrl — AI-assisted remote PC repair

Helps people who aren't comfortable with computers get them fixed: they run a small app and read you a 6-digit assist code, and you (with an AI) can then see their screen, click, type, run commands, transfer files and, when needed, request administrator rights remotely. The person can see what is being done on their screen.

## Architecture

```
 Computer needing help            Your server                    Your computer
┌──────────────┐   WebSocket   ┌──────────────┐   WebSocket   ┌─────────────────────┐
│ Agent        │ ────────────▶ │  Relay       │ ◀──────────── │ MCP console         │
│ shows a code │               │ pairs by code│               │  ↑ stdio            │
│ shows actions│ ◀── calls ─── │ forwards msgs│ ─── calls ──▶ │ Claude (AI + you)   │
└──────────────┘               └──────────────┘               └─────────────────────┘
```

- Both sides only **connect out** to the relay, so the other computer can be behind a router/NAT.
- Having the code is enough to connect: the person shares it on purpose, and every action is shown on their screen. Only administrator access and restarts ask the person to click **Allow**.
- The console is an **MCP server**, so Claude Code / Claude desktop can use the remote computer as a set of tools.
- The agent pings the relay every 25 s and the relay pings every connection every 30 s, so a code stays valid while the person is reading it out (idle connections are otherwise forgotten by home routers), and a computer that went to sleep or lost Wi-Fi frees its code after 90 s.

| Path | Description |
|---|---|
| `src/shared/protocol.ts` | Message format shared by all three parties |
| `src/relay/server.ts` | Relay server |
| `agent-app/` | **GUI agent** (Tauri: Rust backend + web UI). This is what you send to the person |
| `src/agent/` | Command-line agent (Node, for development and testing) |
| `src/console/tech-client.ts` | Technician connection library |
| `src/console/mcp-server.ts` | Technician MCP server |
| `test/e2e.ts` | End-to-end test (works with both agents) |

## GUI agent (agent-app)

The agent has no window: it is an orb of light on a transparent, always-on-top fullscreen overlay. When ready, it shows the 6-digit assist code next to the orb. When someone connects with the code, the session starts right away. While the session is active, a flowing gradient border runs around every screen, and commands and file operations are shown in a panel next to the orb. Typing and key presses are not shown. Hover the orb to end the session or quit.

Development needs Rust (`rustup`) and the Visual Studio C++ build tools.

```bash
cd agent-app
npm install
```

UI debugging in the browser with demo data (add `?state=ready|admin|reboot|connected|offline|connecting` to the URL to switch state):

```bash
npm run dev
```

Debugging the real window:

```bash
npm run tauri dev
```

Release build (exe and installer, in `src-tauri/target/release/`):

```bash
npm run tauri build
```

**Relay URL**, in order of precedence:

1. Command-line flag `--relay <url>`
2. Environment variable `ASCTRL_RELAY_URL`
3. `relay.txt` next to the exe (one line with the URL)
4. Compile-time environment variable `ASCTRL_RELAY_URL` (recommended: set it before building so the exe you send needs no setup)
5. Default: the hosted relay `ws://124.222.182.40:8787` in release builds, `ws://localhost:8787` in debug builds

The `--code <6 digits>` flag fixes the assist code (used to keep the same code across an elevated restart). The `ASCTRL_CODE` (fixed code) environment variable only works in debug builds, for automated tests.

**Admin mode**: the AI calls `request_admin` → the app explains and the person clicks Allow → Windows shows UAC and they click "Yes" → the app restarts as administrator with the same code → the console reconnects automatically with the same code. An "Admin" tag appears under the orb.

## Hosted relay

The relay is deployed at `ws://124.222.182.40:8787` (systemd service `asctrl-relay` on the server, code in `~/asctrl-relay`; port 8787 is open in the Tencent Cloud security group and ufw).

- Agent: build with `ASCTRL_RELAY_URL=ws://124.222.182.40:8787` so the address is compiled into the exe.
- Console: `.mcp.json` already points at this address. The relay requires a token, read from the `ASCTRL_RELAY_TOKEN` environment variable (not in the repo; ask the maintainer). If the MCP client was started before the variable was set, the server falls back to `ASCTRL_RELAY_TOKEN=...` in a gitignored `.env` at the repo root, then (on Windows) to the user environment in the registry.
- Updating the relay: copy `src/relay/server.ts` and `src/shared/protocol.ts` to the matching paths under `~/asctrl-relay/src/` on the server, then `sudo systemctl restart asctrl-relay`. Old and new agents work with old and new relays; the dead-connection cleanup only exists on a relay that has been updated.

## Tools

37 tools, modelled on the tools Claude has when it controls a local machine (shell, file read/write/search, screen/mouse/keyboard, clipboard, windows, background jobs). ✅ means the command-line agent (Node) supports it too; the rest are GUI-agent only.

| Category | Tool | Purpose |
|---|---|---|
| Connection | `connect` ✅ / `disconnect` ✅ | Connect with an assist code / disconnect. `waitSec` keeps retrying until the agent is back online (after elevation or a restart) |
| Viewing | `screenshot` ✅ | Screenshot (JPEG, scaled to 1600 wide by default; captured in-process by the GUI agent, without starting PowerShell). With several displays, `display` captures one |
| | `zoom` | Re-capture part of the screenshot at native resolution to read small text and icons |
| | `cursor_position` | Current mouse position on the screenshot |
| | `wait` | Wait a few seconds, then screenshot (console-side only) |
| System | `system_info` ✅ / `list_processes` ✅ / `event_log` ✅ | System overview, processes, error log |
| Commands | `run_powershell` ✅ | Runs in a persistent session (directory and variables carry over); `background: true` runs without the timeout |
| | `job_output` / `stop_job` | Background job progress and new output / stop it (with child processes) |
| | `reset_shell` | Reset the PowerShell session |
| Mouse | `click` | Single / double / triple click, left / middle / right, optionally holding ctrl, shift, etc. |
| | `mouse_move` / `mouse_down` / `mouse_up` | Hover; press, move and release step by step for fine drags |
| | `mouse_look` | Relative mouse motion in raw counts, for turning first-person game cameras |
| | `drag` / `scroll` | Drag; vertical or horizontal scroll |
| Keyboard | `type_text` | Type text in any language, independent of the input method |
| | `press_keys` / `hold_key` | Keys / combinations (including symbol and volume keys), or hold for a while |
| Windows | `list_windows` | Open windows: title, program, state, position on the screenshot |
| | `focus_window` | Switch to / minimize / maximize / restore / close a window |
| | `open_app` | Open a program, file, folder, URL or `ms-settings:` page |
| Clipboard | `read_clipboard` / `write_clipboard` | Read and write the clipboard (faster than `type_text` for long text) |
| Files | `read_file` / `write_file` / `edit_file` | Read (line numbers, in parts), write, exact replace. Detects UTF-8 / UTF-16 / ANSI code page and keeps encoding and line endings |
| | `list_dir` / `find_files` / `search_files` | List a folder; find by file-name wildcard recursively; search contents by regex |
| | `disk_usage` | Where disk space goes: largest subfolders as a tree and the largest files, in one native scan |
| | `upload_file` / `download_file` | Transfer files between the two computers (≤ 20 MB) |
| Privileges | `request_admin` | Request administrator rights (see above) |
| | `restart_computer` | Restart the computer with the person's consent; the app reopens after sign-in with the same code. `prepareOnly` only registers the auto-open, `cancel` cancels |

After mouse, keyboard and window actions, the MCP console automatically returns a fresh screenshot so the AI sees the result right away (pass `screenshot: false` to turn this off).

**Resuming after a restart**: `restart_computer` → the person clicks Allow → countdown and restart → after sign-in the app opens once via the `RunOnce` registry key (same code) → the AI reconnects with `connect` (`waitSec: 300`).

**Background jobs** run in a separate PowerShell process and do not share the persistent session's directory or variables. Each keeps its most recent 2 MB of output. When the technician disconnects or the person clicks End session, all of them stop, and so does a command still running in the session (with the programs it started).

**Limitations**: without admin rights, windows running as administrator can't be controlled; the UAC prompt and the lock screen can't be clicked with any rights, so the person has to do that themselves.

## Quick start (Node.js 24+)

```bash
npm install
```

1. Start the relay (localhost for local testing; for real use, put it on a server with a public IP):

```bash
npm run relay
```

2. Start the agent on the computer that needs help and note the code on screen:

```bash
npm run agent -- --relay ws://your-server:8787
```

3. Open this folder in Claude Code on your computer. The project's `.mcp.json` already configures the `asctrl` tools (change `RELAY_URL` in it), then tell Claude:

> Connect with code 123456 and find out why this computer is so slow

For Claude desktop, add the contents of `.mcp.json` to its MCP configuration and set `args` to the absolute path of `mcp-server.ts`.

## Tests

```bash
npm test
```

Starts a local relay and an agent, calls every tool in turn, and saves a screenshot to `test/out/screenshot.jpg`.

To test the GUI agent (first run `npm run tauri build -- --debug --no-bundle` in `agent-app`):

```bash
node test/e2e.ts --app agent-app/src-tauri/target/debug/asctrl-agent.exe
```

Extra GUI-agent checks: session keeps directory and variables, `reset_shell` (also while a command is running), upload/download round trip, out-of-range coordinates and unknown keys rejected, `zoom` and per-display screenshots, window listing, file read/write/edit/search (UTF-8 / ANSI / UTF-16), clipboard round trip including long text (restored afterwards), background jobs and stopping them, restart-resume registration and cancel (no real restart). Mouse clicks and typing touch the real desktop, so they are not in the automated test; demo them by hand with `test/demo-step.ts`:

```bash
node test/demo-step.ts ws://localhost:8787 <code> @steps.json
```

```bash
npm run typecheck
```

## Security

Structurally, software like this is the same as a remote-access trojan; the whole difference is **informed consent**. Currently:

- Connecting needs the code, which the person reads out themselves; administrator access and restarts must be confirmed at the agent
- A running session is visible at all times (gradient border around the screen) and can be ended from the orb
- Commands and file operations are shown on the agent's screen
- Quitting the app disconnects
- The relay can require a `RELAY_TOKEN`, so only consoles with the right token can connect

**Must be added before real use:**

- [ ] Put the relay behind an HTTPS reverse proxy and use `wss://` (currently plaintext)
- [ ] Limit attempts / expire codes to prevent brute-force guessing (more important now that the code alone connects)
- [ ] Second confirmation on the agent for dangerous commands (optional)
- [ ] Write an action log to disk

## Roadmap

- [x] GUI agent (assist code + on-screen activity)
- [ ] Code signing, to reduce SmartScreen / antivirus blocking
- [ ] Tray icon; no auto-start at boot (open only when needed)
- [x] Mouse click and keyboard input tools (so the AI can operate the UI directly)
- [x] File upload and download
- [x] Persistent PowerShell session
- [x] Admin mode (consent + UAC, then elevated restart)
- [ ] Live view (or integrate RustDesk / MeshCentral for remote desktop)
- [ ] Web console: view the screen and click buttons without Claude
