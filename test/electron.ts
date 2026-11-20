// An Electron program worked through its pages: VS Code, started by open_app with electron: true.
// Usage: node test/electron.ts   (about 30 s; leave the computer alone)
// VS Code runs with a user-data and extensions folder of the test's own, so it is a copy of its own even when the
// person has VS Code open, and their settings and windows are not touched.
//
// 1. open_app electron: true starts it with a debugging port and says its windows are worked through their pages.
// 2. read_text reads the window through the page: the open file's text is there.
// 3. A click by an element's name (the Explorer view) goes through the page: the mouse does not move.

import { fixtureDirectory } from "./fixtures.ts";
import { execFileSync } from "node:child_process";
import { rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { Suite, sleep } from "./desk.ts";

const s = new Suite("Electron");
const { tech, say } = s;
const work = fixtureDirectory(path.join(tmpdir(), "asctrl-electron-"));
const file = path.join(work, "hello.txt");
writeFileSync(file, "hello from asctrl electron test\n");
const first = (s?: string) => (s ?? "").split("\n")[0];
const cursor = () => execFileSync("powershell.exe", ["-NoProfile", "-Command", "Add-Type -AssemblyName System.Windows.Forms; $p = [System.Windows.Forms.Cursor]::Position; \"$($p.X),$($p.Y)\""]).toString().trim();
let W: number | undefined;

s.after(async () => {
  if (W) await tech.call("focus_window", { window: W, action: "close", after: "none" });
  // The copy of VS Code the test started (by its folder) is closed for good before its folder goes
  try {
    execFileSync("powershell.exe", ["-NoProfile", "-Command", `Get-CimInstance Win32_Process -Filter "Name='Code.exe'" | Where-Object { $_.CommandLine -like '*${path.basename(work)}*' } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }`]);
  } catch {}
  await sleep(2200);
  rmSync(work, { recursive: true, force: true });
});

await s.run(async () => {
  await tech.connectLocalDriver({ launch: false, waitMs: 30_000 });
  // 1.
  const o = await tech.call("open_app", { target: "code", args: `--new-window --skip-welcome --skip-release-notes --disable-workspace-trust --user-data-dir "${path.join(work, "data")}" --extensions-dir "${path.join(work, "ext")}" "${file}"`, electron: true, timeoutSec: 30, after: "none" });
  W = (o.data as { handle?: number } | undefined)?.handle;
  say(o.ok && /worked through their pages/.test(o.text ?? ""), `VS Code started with a debugging port: ${first(o.text ?? o.error)}`);
  await tech.call("wait_for", { appears: "hello.txt", window: W, timeoutSec: 20, after: "none" });

  // 2.
  // The title shows the file before the editor has drawn it: read again until it has (up to 8 s)
  let r = await tech.call("read_text", { window: W });
  for (let i = 0; i < 16 && r.ok && !/hello from asctrl electron test/.test(r.text ?? ""); i++) {
    await new Promise((res) => setTimeout(res, 500));
    r = await tech.call("read_text", { window: W });
  }
  say(r.ok && (r.data as { channel?: string } | undefined)?.channel === "page" && /hello from asctrl electron test/.test(r.text ?? ""), `read through the page, the file's text is there: ${first(r.text ?? r.error)}`);

  // 3.
  const mouse = cursor();
  const pageClick = () => tech.call("click", { window: W, target: "Explorer (Ctrl+Shift+E)", mode: "page", timeoutSec: 2, after: "none" });
  let c = await pageClick();
  for (let i = 0; i < 12 && !c.ok; i++) {
    await new Promise((res) => setTimeout(res, 500));
    c = await pageClick();
  }
  say(c.ok && (c.data as { channel?: string } | undefined)?.channel === "page" && cursor() === mouse, `a click by name through the page, the mouse stayed put: ${first(c.text ?? c.error)}`);
});
