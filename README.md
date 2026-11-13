# Asctrl — a computer-use framework for AI agents

Asctrl gives an AI eyes and hands on a Windows computer: it reads what is on screen as structured text and as images, clicks, types, fills forms, runs commands and edits files — on the machine the AI runs on, or on a remote one reached with a 6-digit code. Any MCP client (Claude Code, Claude desktop, your own agent) can use it.

It is built around three ideas:

- **One way to point.** Every action takes a `target` — a control's reference such as `"e12"`, or simply the text the thing shows (`"Save"`) — or `x`, `y` on the latest screenshot. Text is looked up among the window's real controls first and read off the pixels (OCR) where a program draws its own UI.
- **Look once.** `observe` reads a window's controls as a tree with references that stay valid for as long as the control exists. After that, every action's reply says what it did, whether the screen reacted, and what changed among the controls, with references to anything new. A screenshot comes back only when text can't tell. The AI rarely has to look again just to learn what its last action did.
- **Never act on a stale picture.** A click by coordinates is checked against the windows as they were when the AI last looked: if the window moved, the click moves with it; if another program's window has come in front, or the window was resized or closed, nothing is clicked and a fresh screenshot goes back. A control is scrolled into view and its window uncovered before it is clicked. Typed text is read back from the field.

## Architecture

```
                         this computer                                   a remote computer
┌─────────────────────┐  stdio  ┌────────────┐  loopback WebSocket  ┌──────────────────────────┐
│ AI (any MCP client) │ ◀─────▶ │ MCP server │ ◀──────────────────▶ │ Driver (agent-app)        │
└─────────────────────┘         │  (Node)    │                      │  look · locate · act ·    │
                                │            │  WebSocket  ┌───────┐│  report · shell · files   │
                                │            │ ◀─────────▶ │ Relay │◀──── driver on the remote  │
                                └────────────┘   by code   └───────┘│  overlay shows every step │
                                                                    └──────────────────────────┘
```

- The **driver** (`agent-app/`, Tauri: Rust backend + web overlay) runs on the computer being used. It does all the work natively: screen capture, UI Automation, OCR, input, the shell and files. It has no window: an orb of light on a transparent always-on-top overlay shows what the AI is doing (where it clicks, what it runs), and a gradient border runs round the screen while a session is active.
- The **MCP server** (`src/console/mcp-server.ts`) exposes the tools. It reaches a driver one of two ways:
  - **Local link** — straight to the driver on the same machine over a loopback WebSocket. No relay, no code, no trip over the internet. The driver writes its port and a random token to `%LOCALAPPDATA%\Asctrl\local.json` (readable only by the user); a connection must present the token, and connections from web pages are refused. `connect` with no arguments uses it and starts the driver if it is not running.
  - **Relay** (`src/relay/server.ts`) — both sides connect out to a relay and are paired by the 6-digit code shown next to the orb, so the computer can be anywhere, behind any router. `connect` with a code uses it.
- One controller drives at a time: there is one mouse and one keyboard.

| Path | Description |
|---|---|
| `agent-app/src-tauri/src/tools/` | The tools: `look` (observe, screenshot, find, read_text), `locate` (targets), `pointer` / `keys` / `values` (actions), `report` (what a reply says), `wait`, `batch`, `apps`, `system` |
| `agent-app/src-tauri/src/uia.rs` | UI Automation: scenes, stable references, change detection between readings, operating controls |
| `agent-app/src-tauri/src/screen.rs` | Telling whether the screen reacted to an action and when it has settled |
| `agent-app/src-tauri/src/link.rs` | The local link and the relay link; running tool calls |
| `agent-app/src/` | The overlay UI |
| `src/console/` | MCP server and the controller library (`TechClient`) |
| `src/relay/server.ts` | Relay server |
| `src/shared/protocol.ts` | Message format shared by all parties |
| `src/agent/` | A minimal command-line driver (Node, 5 tools), for testing the relay protocol |
| `test/` | End-to-end test, benchmark, manual demo |

## Quick start (Node.js 24+, Rust, Visual Studio C++ build tools)

```bash
npm install
```

```bash
cd agent-app && npm install && npx tauri build --no-bundle
```

Open this folder in Claude Code: the project's `.mcp.json` already configures the `asctrl` tools. Then:

> Connect to this computer and tidy up my Downloads folder

`connect` starts `agent-app/src-tauri/target/release/asctrl-agent.exe` if it is not running (set `ASCTRL_DRIVER_EXE` to use another copy). For Claude desktop, add the contents of `.mcp.json` to its MCP configuration and set `args` to the absolute path of `mcp-server.ts`.

To drive a **remote** computer, run the driver there, read the 6-digit code next to the orb, and:

> Connect with code 123456 and find out why this computer is so slow

## Tools

39 tools. The first group is what makes it a computer-use framework; the rest are the shell and file tools an agent expects.

| Category | Tool | Purpose |
|---|---|---|
| Connection | `connect` / `disconnect` | No arguments: this computer, directly. `code`: a remote one. `waitSec` keeps retrying until the driver is back (after elevation or a restart) |
| Looking | `observe` | A window as text: its controls as a tree with references, state, value and position; open menus and popups included; text read off the pixels where the controls say little. Names the window in front and the others. `image: true` adds a screenshot |
| | `screenshot` | The whole screen, one display, or one window (sharper, cheaper). `x`, `y` of later actions refer to the latest one |
| | `zoom` | Re-capture part of the screenshot at full resolution, to read small text and icons |
| | `find` | Where something is: matching controls best first (with references), and places the text appears on the pixels |
| | `read_text` | One control's full text, a window's text in reading order, or a region read off the pixels |
| | `list_windows` | Open windows, front to back, dialogs included |
| | `wait_for` | Return the moment a window or some text appears (or goes away), or the screen settles; lists the controls of what appeared |
| Acting | `click` | By target or `x`,`y`. Ordinary programs' controls are pressed through UI Automation (exact, no pointer movement); web pages, games and coordinates get a real mouse click |
| | `type_text` | Type into a target, or into what has the focus; `replace`, `enter`. Reads the field back |
| | `press_keys` | A key, a combination, or a sequence (`ctrl+a delete`); `repeat`, `holdSec`; `window` brings a window forward first |
| | `set_value` | Make a control hold a value whatever kind it is — text, on/off, a chosen item, a number — and verify it. Idempotent. `fields` fills a whole form in one call |
| | `scroll` / `drag` | Wheel over a target; press on one target, move to another, release |
| | `mouse` | Low-level steps: `move` (hover), `down` / `up` (fine drags), `look` (relative motion for game cameras) |
| | `element_action` | One UI Automation action (press, toggle, select, expand, collapse, focus, scroll, scroll_into_view) that touches neither mouse, focus nor window order: works on windows in the background |
| | `batch` | Several steps in one call, stopping at the first failure, with one report at the end. `fast` for many inputs into a screen that stays put |
| Programs | `open_app` | Open a program, file, folder, URL or `ms-settings:` page; waits for its window and lists its controls |
| | `focus_window` | Switch to / minimize / maximize / restore / close / move and resize a window |
| Clipboard | `read_clipboard` / `write_clipboard` | Text, the list of copied files, or a note that it holds an image |
| Shell | `run_powershell` | A persistent session (directory and variables carry over); `background: true` for long jobs |
| | `job_output` / `stop_job` / `reset_shell` | Background job progress / stop it / reset the session |
| System | `system_info` / `list_processes` / `event_log` | System overview, processes, error log |
| Files | `read_file` / `write_file` / `edit_file` | Read (line numbers, in parts), write, exact replace. Detects UTF-8 / UTF-16 / ANSI and keeps encoding and line endings |
| | `list_dir` / `find_files` / `search_files` / `disk_usage` | List; find by name; search contents by regex; where disk space goes |
| | `upload_file` / `download_file` | Copy files between the MCP server's machine and the connected computer (≤ 20 MB) |
| Privileges | `request_admin` | Ask the person at the computer for administrator rights; the driver restarts elevated and the console reconnects by itself |
| | `restart_computer` | Restart with the person's consent; the driver reopens after sign-in with the same code |

### Targets

```jsonc
{ "target": "e12" }                      // a reference from observe, find, or any reply
{ "target": "Save" }                     // the text the thing shows: a control's name, else text on the pixels
{ "target": "Delete", "nth": 2 }         // the second of several equally good matches, in reading order
{ "target": "Name", "role": "edit" }     // only controls of that kind
{ "target": "OK", "window": "Save As" }  // in that window rather than the one in front
{ "x": 412, "y": 300 }                   // a point on the latest screenshot
```

Positions in text replies (`@(x,y)`) are in the same pixels as the latest screenshot, and before any screenshot in those a plain screenshot gives, so reading a window first and looking at a picture later agree. A screenshot framed differently from the one before (one window, another display, another width) says so, because earlier positions no longer apply.

A text target waits briefly (`timeoutSec`, default 1.5) for something still on its way. When several controls match equally well it does not guess: the reply lists them with references. When nothing matches, the reply names the closest candidates.

### What a reply says

```text
Pressed Button "Submit" [e7] (directly, without moving the mouse). Screen changed around (440,24)-(700,52) (settled in 96 ms)
Changes in "Order form":
~ [e9] Text "Waiting": now "Submitted: Alice"
+ [e31] Button "Print receipt" @(520,90)
```

`after` chooses what comes back: `auto` (default: control changes when they can tell, else a screenshot), `none` (fastest), `ui`, `screenshot`, `both`.

### Speed

- The local link removes the relay round trip for an AI on the same machine.
- Actions wait only until the part of the screen they affect stops changing, never a fixed time; what moves on its own (a video, an ad) is left out. Nothing waits for the overlay's animations.
- Reading a window fetches only what is on screen, in one cross-process call.
- `batch` and `set_value fields` do in one call what would otherwise take a turn each.

## Driver (agent-app)

Debugging the real window:

```bash
cd agent-app && npm run tauri dev
```

UI debugging in the browser with demo data (add `?state=ready|admin|reboot|connected|offline|connecting` to the URL to switch state):

```bash
cd agent-app && npm run dev
```

**Relay URL**, in order of precedence: `--relay <url>`; environment variable `ASCTRL_RELAY_URL`; `relay.txt` next to the exe; the compile-time `ASCTRL_RELAY_URL`; the hosted relay `ws://124.222.182.40:8787` in release builds, `ws://localhost:8787` in debug builds. A driver that can't reach its relay still works over the local link.

`--code <6 digits>` fixes the code (used to keep it across an elevated restart). `ASCTRL_CODE` only works in debug builds. `ASCTRL_LOCAL_FILE` moves the local link's address file (test runs use their own, so they never disturb a driver already running).

**Admin mode**: the AI calls `request_admin` → the person clicks Allow → Windows shows UAC and they click "Yes" → the driver restarts as administrator → the console reconnects automatically.

**Limitations**: without admin rights, windows running as administrator can't be read or controlled; the UAC prompt and the lock screen can't be clicked with any rights. Windows only.

## Hosted relay

The relay is deployed at `ws://124.222.182.40:8787` (systemd service `asctrl-relay`, code in `~/asctrl-relay`). The console's `.mcp.json` points at it; it requires a token, read from `ASCTRL_RELAY_TOKEN` (falling back to a gitignored `.env` at the repo root, then the Windows user environment). To update it, copy `src/relay/server.ts` and `src/shared/protocol.ts` to the matching paths under `~/asctrl-relay/src/` and `sudo systemctl restart asctrl-relay`. The relay needs no update for this version: it forwards calls unchanged.

To run your own:

```bash
npm run relay
```

## Tests

Unit tests (target matching, the stale-click check, change detection, frames):

```bash
cd agent-app/src-tauri && cargo test --lib
```

Type check:

```bash
npm run typecheck
```

End-to-end, against the built driver (starts a local relay and the driver; exercises the relay link, the local link, files, shell, clipboard, jobs):

```bash
node test/e2e.ts --app agent-app/src-tauri/target/release/asctrl-agent.exe
```

Speed and reliability on a real desktop (moves the mouse and types for about 20 seconds; leave the computer alone):

```bash
npm run bench
```

It opens `test/bench-app.ps1` (a window with ordinary controls, a canvas only OCR can read and a dialog that opens late) and an Explorer folder, and runs 13 tasks the way an AI would: fill and submit a form in two calls, type 500 mixed characters and read them back, tick a checkbox idempotently, click by coordinates, catch a click that did nothing, follow a window that moved and refuse a click after a resize, click text on a canvas, wait for a late dialog, a batch. Each result is checked, and the time per task and per tool is printed.

Where the time goes, action by action (the same few actions with and without the report, several times each; moves the mouse for a few seconds):

```bash
npm run prof
```

Single steps by hand:

```bash
node test/demo-step.ts local - @steps.json
```

## Security

Software like this is, structurally, remote-control software; the difference is **informed consent and visibility**.

- A remote connection needs the code, which the person reads out themselves; administrator access and restarts must be confirmed at the computer
- The local link accepts only a process that can read the user's own `local.json`, and never a web page
- A running session is visible at all times (gradient border around the screen, the orb showing each action) and can be ended from the orb
- Quitting the driver disconnects
- The relay can require a `RELAY_TOKEN`

**Still to do before wide use:**

- [ ] Put the relay behind an HTTPS reverse proxy and use `wss://` (currently plaintext)
- [ ] Limit attempts / expire codes to prevent brute-force guessing
- [ ] Optional confirmation at the computer for dangerous commands
- [ ] Write an action log to disk
- [ ] Code signing, to reduce SmartScreen / antivirus blocking
