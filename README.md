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
| `agent-app/src-tauri/src/tools/` | The tools: `look` (observe, screenshot, read_text), `locate` (targets), `pointer` / `keys` / `values` (actions), `report` (what a reply says), `wait`, `grid` (read_grid), `batch` (runs a plan's steps), `popups` (dismissed while a plan runs), `apps`, `system` |
| `agent-app/src-tauri/src/uia.rs` | UI Automation: scenes, stable references, change detection between readings, operating controls |
| `agent-app/src-tauri/src/screen.rs` | Telling whether the screen reacted to an action and when it has settled |
| `agent-app/src-tauri/src/link.rs` | The local link and the relay link; running tool calls |
| `agent-app/src/` | The overlay UI |
| `src/console/` | MCP server and the controller library (`TechClient`) |
| `src/relay/server.ts` | Relay server |
| `src/shared/protocol.ts` | Message format shared by all parties |
| `test/` | End-to-end test, plan tests, benchmark, profiler |

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

30 tools. The first groups are what make it a computer-use framework; the rest are the shell and file tools an agent expects.

| Category | Tool | Purpose |
|---|---|---|
| Connection | `connect` / `disconnect` | No arguments: this computer, directly. `code`: a remote one. `waitSec` keeps retrying until the driver is back (after elevation or a restart) |
| Plans | `run` / `progress` | The main way in: a plan written by the text it acts on, checked whole, run in one go by the driver and queued so the AI writes the next one while it runs (see [Plans](#plans)); `progress` reports how they went |
| Looking | `observe` | A window as text: its controls as a tree with references, state, value and position; open menus and popups included; text read off the pixels where the controls say little. Lists every other window with its handle. `query`: where something is instead, best match first, on the pixels too. `image: true` adds a screenshot |
| | `screenshot` | The whole screen, one display, or one window (sharper, cheaper); `x0, y0, x1, y1`: part of the latest one again at full resolution. `x`, `y` of later actions refer to the latest screenshot |
| | `read_text` | One control's full text, a window's text in reading order, or a region read off the pixels |
| | `wait_for` | Return the moment a window `appears`, some `text` shows in `window` (or either goes away), or the screen settles; lists the controls of what appeared |
| Acting | `click` | By target or `x`,`y`. Ordinary programs' controls are pressed through UI Automation (exact, no pointer movement); web pages, games and coordinates get a real mouse click, with real motion on arrival (games that read raw input, such as Roblox, ignore a pointer that was only placed) and the button held about a frame. `mode: "direct"` never uses the mouse |
| | `type_text` | Type into a target, or into what has the focus; `replace`, `enter`. Reads the field back |
| | `press_keys` | A key, a combination, or a sequence (`ctrl+a delete`); `repeat`, `holdSec`; `window` brings a window forward first |
| | `set_value` | Make a control hold a value whatever kind it is — text, on/off, a chosen item, a number, expanded or not — and verify it. Idempotent. `fields` fills a whole form in one call |
| | `scroll` / `drag` | Wheel over a target; press on one target, move to another, release |
| | `mouse` | Low-level steps: `move` (hover), `down` / `up` (fine drags), `look` (relative motion for game cameras) |
| Programs | `open_app` | Open a program, file, folder, URL or `ms-settings:` page; waits for its window (for a URL, until the page has loaded) and lists its controls |
| | `focus_window` | Switch to / minimize / maximize / restore / close / move and resize a window |
| | `clipboard` | With `text`: put it on the clipboard. Without: its text, the list of copied files, or a note that it holds an image |
| Shell | `run_powershell` | A persistent session (directory and variables carry over); `background: true` for long jobs; `reset: true` for a fresh session. System information, processes and the event log are a command away |
| | `job` | A background job's new output (`all` for everything), `stop: true` to stop it; without `jobId`, every job |
| Files | `read_file` / `write_file` / `edit_file` | Read (line numbers, in parts), write, exact replace. Detects UTF-8 / UTF-16 / ANSI and keeps encoding and line endings |
| | `list_files` / `search_files` / `disk_usage` | A folder's contents, or with `pattern` the files below it by name; search contents by regex; where disk space goes |
| | `upload_file` / `download_file` | Copy files between the MCP server's machine and the connected computer (≤ 20 MB) |
| Privileges | `request_admin` | Ask the person at the computer for administrator rights; the driver restarts elevated and the console reconnects by itself |
| | `restart_computer` | Restart with the person's consent; the driver reopens after sign-in with the same code |

### Targets

```jsonc
{ "target": "e12" }                      // a reference from observe or any reply
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

### Plans

Most of a task's time is the AI thinking between steps. `run` takes a whole plan at once and returns straight away (with the reports of plans that finished since), so the AI plans the next part while this one runs. It is the way in: the AI writes a plan without looking first, and looks only when a plan fails (its report says where things stand) or when the next plan needs something it can't guess:

```jsonc
run({ goal: "send 你好 to someone@example.com",
      steps: [ { "open": "https://mail.google.com" }, { "expect": "Compose", "timeoutSec": 15 },
               { "click": "Compose" }, { "expect": "Message Body" },
               { "fill": { "To": "someone@example.com", "Message Body": "你好" } },
               { "click": "Send" }, { "expect": "Message sent" } ],
      on: { "Leave site?": "Leave" } })
```

Steps: `open`, `click`, `press` (no mouse), `fill` (`{label: value}`), `type` (`into`, `enter`), `keys`, `expect` (text shown on success; `gone`), `check` (a claim about the window, such as "the email was sent", that the decision model judges from the window's controls; asked again until it holds or `timeoutSec` runs out), `await` (a window), `scroll`, `read` (a grid), `watch` (see below), or `{ tool, …args }` for a tool that can be a step. `when` holds a plan until its text is on screen; `on` says what to press when a known popup turns up. The whole plan is checked before any of it runs: an unknown step, an argument a step doesn't take or a cell outside its grid is reported, and nothing runs.

The driver runs the plan's steps in one call per stretch (up to the next `check`), so no step waits on a round trip. Each step is tried as written: the driver matches text on the computer, which costs nothing. Only when the text matches several controls or none does the runner step in for that step, with the plan's `on` rules first and then a decision model ([Drex](https://console.nace.ai), `NACE_API_KEY` in `.env`) asked to choose, among up to 40 short candidate lines (typically 150–550 tokens, about 0.5 s). Its answer is acted on only when it is sure (confidence ≥ 0.7, well ahead of the runner-up), and never when it is something irreversible (delete, send, replace…) that the plan didn't name. Besides the candidates, the model may answer that the window is not ready yet (it is waited for), that a popup is in the way (Escape is pressed) or that none fits, and it is told the plan's last steps and whether each changed the screen; when it is unsure the window is given a moment and it is asked once more, and a click it chose that visibly changed nothing is handed back as likely wrong. Right after an action (or a fast input) a name that shows nowhere is first given a moment to appear, so "not there yet" is not taken for "called something else". Answers are remembered per program (`~/.asctrl/drex-cache.json`, keyed by the program and its window title), so the same wording later goes straight to the control there and nowhere else; they are kept only once the whole plan has worked, checks included, so a wrong choice is never learnt. Anything else stops the plan with the candidates and a short look at the window.

The same steps over many sets of data are written once: `repeat: [["7","3","21"], …]` runs them per row, filling `$1 $2 $3` (or `${name}` from `[{name: …}, …]`) in any step text, and the report lists the first row and only what went differently after.

The pace is chosen per step. An input normally waits until the screen has reacted and stopped changing; on a screen that animates, that wait is most of the time. A step repeated over rows runs at the normal pace the first time, and if that brought no window up and nothing else to the front, its later runs go fast (about 25 ms each); the screen settles once before anything that reads it, and anything unexpected in a fast run puts the rest back to the normal pace. `fast: true` / `false` forces either. An Extreme sudoku on sudoku.com (58 cells, a click and a key each) is filled in 6.2 s this way, against 45.7 s at the normal pace.

While a plan runs, the driver watches for windows that weren't there before. One that shows none of the text the plan works with, and has a button the plan named in `on` or one that only dismisses (`Not now`, `Close`, `关闭`, `以后再说`…), is dismissed with a pointer of its own: the press waits its turn among the plan's actions, so it goes in between two steps, and the report says what was pressed. OK, Cancel, Yes and anything destructive are never pressed this way.

A screen drawn as a grid (a game board, a calendar) is declared once, by its corners on the latest screenshot: `grids: { board: { x0, y0, x1, y1, rows: 9, cols: 9 } }`. Steps then name cells as targets (`{ "click": "board[3,5]" }`, also with `$1` from rows), and `{ "read": "board" }` reads every cell: each cell's glyph is cut out against the cell's own background and the glyphs are read together as one line, since the OCR engine reads neither a lone glyph nor a board with its lines. A mark it can't read, or a cell something lies over (a banner, a popup: its background is far from the board's usual one), comes back as `?`, never as a guess or as empty. Like a click by coordinates, a read is checked against the windows as they were on that screenshot: if the window moved, the grid moves with it; if another window has come in front of it, nothing is read. A 9×9 board reads in about 50 ms.

Some steps must wait on a number rather than on text: a price crossing a line, a countdown, a download's percentage, a score. `{ "watch": { x0, y0, x1, y1 }, "until": ">= 141 | <= 133" }` reads the number in that region about 8 times a second and goes on to the next step the moment one condition holds (`>`, `>=`, `<`, `<=`, `=`, or text to wait for), so the reaction goes in within a fraction of a second with no round trip to the AI; it fails if none held in `timeoutSec` (default 60, up to 600), naming the window over the region in case it was not the one meant. A number counts only when the reading before agrees with it and the one believed so far does too (a digit cut off at the region's edge, or a stray mark, must not set anything off), and a condition must hold on `confirm` readings in a row (default 2). Numbers are read with the English OCR engine where its pack is installed (`Add-WindowsCapability -Online -Name Language.OCR~~~en-US~0.0.1.0`, as administrator), since engines for other scripts misread digits: the Chinese one reads `4,816.93` as `816 · 93 ， 4 》`. A take-profit / stop-loss is then `[{ "click": "Buy" }, { "watch": {…}, "until": ">= 15 | <= -10" }, { "click": "Sell all" }]`.

A plan that fails cancels the plans queued behind it, since they were written assuming it would work; the next `run` returns that failure instead of queueing.

### Speed

- The local link removes the relay round trip for an AI on the same machine.
- Actions wait only until the part of the screen they affect stops changing, never a fixed time; what moves on its own (a video, an ad) is left out. Nothing waits for the overlay's animations.
- Reading a window fetches only what is on screen, in one cross-process call.
- A plan runs in one driver call per stretch of steps, and `set_value fields` fills a form in one call.

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

End-to-end, against the release build (starts a local relay and the driver; exercises the relay link, the local link, files, shell, clipboard, jobs; `--app <exe>` tests another build):

```bash
npm test
```

Speed and reliability on a real desktop (moves the mouse and types for about 20 seconds; leave the computer alone):

```bash
npm run bench
```

It opens `test/bench-app.ps1` (a window with ordinary controls, a canvas only OCR can read and a dialog that opens late) and an Explorer folder, and runs 14 tasks the way an AI would: fill and submit a form in two calls, type 500 mixed characters and read them back (with the clipboard coming back as it was), tick a checkbox idempotently, click by coordinates read before the screenshot was taken, catch a click that did nothing, notice a screenshot framed differently, follow a window that moved and refuse a click after a resize, click text on a canvas, wait for a late dialog, a batch. Each result is checked, and the time per task and per tool is printed.

Where the time goes, action by action (the same few actions with and without the report, several times each; moves the mouse for a few seconds):

```bash
npm run prof
```

Plans (`run`): local matching, the decision model and its memory, a plan with mistakes refused whole, a popup dismissed mid-plan, the pace chosen per step, queued plans, a failure stopping the queue (bench window and Calculator; `--gmail` also composes and discards a mail in the browser's Gmail, never sending):

```bash
node test/run.ts
```

## Status (version 0.2.0, November 2026)

What has been shown, on one Windows 11 machine (2560x1440 at 150 %, one display):

- Unit tests 29 / 29, end-to-end 59 / 59, plan tests 17 / 17, benchmark 14 / 14 tasks in 11.2 s.
- Per call: pressing a control directly 140–215 ms, a real mouse click about 160 ms, switching windows about 300 ms, `observe` about 35 ms, a screenshot about 50 ms.
- An AI (Claude, through these tools) solved Extreme sudokus on sudoku.com: the page opened and waited for, the board read with `read` in under 50 ms (a banner over it reported, not misread), and 53–58 cells filled in about 6 s with the pace chosen per step. Gmail compose-and-discard runs as a plan.

What has not been shown:

- **Few real tasks.** Apart from the sudoku and Gmail runs, every test is a script with fixed steps: they measure whether the tools are fast and correct, not whether an agent finishes tasks more often.
- **Real programs are barely covered.** Two web pages, a small WinForms window, Calculator and one Explorer folder. Electron programs (VS Code, WeChat), Office, games and several displays are untested.
- **No comparison with any other framework**, and no public benchmark has been run.
- **Little or no test coverage**: drag, scroll, input methods, several displays, windows running as administrator, right-to-left layouts.
- The code has had no independent review.

Known limits:

- Screenshots use GDI: a window behind others can't be captured, and some hardware-accelerated content may come out blank.
- No numbered marks drawn on screenshots (Set-of-Marks), so UI a program draws itself is reached by text read off the pixels or by coordinates only.
- When the keyboard focus leaves a text box, Windows fades its border for about 250 ms and the action waits for that (about 380 ms instead of 170 ms).
- Single clicks repeated on the same spot are kept about 550 ms apart, so Windows does not take them for a double click. Pressing a control directly has no such wait.
- The overlay is left out of screen captures, so where the orb points, frames and draws is checked by eye only.
- Windows only.

Suggested order from here: have an agent do more real tasks in a browser, Settings and Office and fix what fails; an independent code review; then the limits above.

## Security

Software like this is, structurally, remote-control software; the difference is **informed consent and visibility**.

- A remote connection needs the code, which the person reads out themselves; administrator access and restarts must be confirmed at the computer
- The local link accepts only a process that can read the user's own `local.json`, and never a web page
- A running session is visible at all times (gradient border around the screen, the orb showing each action) and can be ended from the orb (click it for the menu)
- Quitting the driver disconnects. Left Alt + right Alt held together quits it at once, even if the overlay is stuck
- The relay can require a `RELAY_TOKEN`

**Still to do before wide use:**

- [ ] Put the relay behind an HTTPS reverse proxy and use `wss://` (currently plaintext)
- [ ] Limit attempts / expire codes to prevent brute-force guessing
- [ ] Optional confirmation at the computer for dangerous commands
- [ ] Write an action log to disk
- [ ] Code signing, to reduce SmartScreen / antivirus blocking
