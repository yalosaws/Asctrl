// End-to-end test: starts a local relay + an agent (auto-accept), then the tech client calls every tool in turn.
// Usage: npm test                      tests the Node agent
//        npm test -- --app <exe path>  tests the GUI agent (needs a debug build)

import { spawn } from "node:child_process";
import { writeFileSync, mkdirSync } from "node:fs";
import { startRelay } from "../src/relay/server.ts";
import { TechClient } from "../src/console/tech-client.ts";

const PORT = 18787;
const CODE = "123456";
const relay = startRelay(PORT);

// Non-ASCII sample text. It is test data on purpose: it exercises UTF-8, UTF-16 and the
// system ANSI code page (GBK on Chinese Windows), which plain ASCII would not.
const HELLO = "你好";
const LINE1 = "第一行";
const LINE3 = "第三行";
const ANSI_TEXT = "中文测试";
const DISK_FULL = "磁盘已满";
const UPLOAD_NAME = "上传.txt";

const appIdx = process.argv.indexOf("--app");
const appExe = appIdx >= 0 ? process.argv[appIdx + 1] : undefined;

const agent = appExe
  ? spawn(appExe, [], {
      stdio: ["ignore", "pipe", "inherit"],
      env: { ...process.env, ASCTRL_RELAY_URL: `ws://localhost:${PORT}`, ASCTRL_CODE: CODE, ASCTRL_AUTO_ACCEPT: "1" },
    })
  : spawn(
      process.execPath,
      ["src/agent/agent.ts", "--relay", `ws://localhost:${PORT}`, "--code", CODE, "--auto-accept"],
      { stdio: ["ignore", "pipe", "inherit"] },
    );
agent.stdout.on("data", (d) => process.stdout.write(`  [agent] ${d}`));

let failed = 0;
function check(name: string, cond: boolean, detail = "") {
  console.log(`${cond ? "✔" : "✘"} ${name}${detail ? " — " + detail : ""}`);
  if (!cond) failed++;
}

try {
  await new Promise((r) => setTimeout(r, 1500)); // wait for the agent to register

  const bad = new TechClient(`ws://localhost:${PORT}`);
  await bad.connect("000000").then(
    () => check("wrong code is rejected", false),
    (e) => check("wrong code is rejected", true, e.message),
  );

  const tech = new TechClient(`ws://localhost:${PORT}`, "Tester");
  const host = await tech.connect(CODE);
  check("paired", !!host, host);

  // A second technician is turned away, and its leaving must not end the first one's session
  await new TechClient(`ws://localhost:${PORT}`, "Intruder").connect(CODE).then(
    () => check("second technician is rejected", false),
    (e) => check("second technician is rejected", /already connected/.test(e.message), e.message),
  );
  await new Promise((r) => setTimeout(r, 300));
  const still = await tech.call("run_powershell", { script: "'still here'" });
  check("first session survives the rejected one", still.ok && !!still.text?.includes("still here"), still.text ?? still.error);

  const info = await tech.call("system_info");
  check("system_info", info.ok && !!info.text?.includes("OS"), info.text?.slice(0, 80).replace(/\s+/g, " "));

  const procs = await tech.call("list_processes", { top: 5 });
  check("list_processes", procs.ok && !!procs.text?.includes("Name"));

  const ps = await tech.call("run_powershell", { script: `Write-Output '${HELLO} asctrl'; 1+1` });
  check("run_powershell non-ASCII output", ps.ok && !!ps.text?.includes(`${HELLO} asctrl`) && !!ps.text?.includes("2"), ps.text);

  const log = await tech.call("event_log", { hours: 24, max: 3 });
  check("event_log", log.ok);

  const shot = await tech.call("screenshot", { maxWidth: 800 });
  const ok = shot.ok && !!shot.image?.data;
  check("screenshot", ok, ok ? `${Math.round((shot.image!.data.length * 3) / 4 / 1024)} KB` : shot.error);
  if (ok) {
    mkdirSync("test/out", { recursive: true });
    writeFileSync("test/out/screenshot.jpg", Buffer.from(shot.image!.data, "base64"));
  }

  if (appExe) {
    // ---- Only the GUI agent supports these ----
    check("screenshot reports its size", /Screenshot \d+x\d+/.test(shot.text ?? ""), shot.text);
    const jpg = Buffer.from(shot.image!.data, "base64");
    check("screenshot is a JPEG", jpg[0] === 0xff && jpg[1] === 0xd8 && jpg.length > 2000, `${jpg.length} bytes`);

    await tech.call("run_powershell", { script: "Set-Location $env:TEMP; $asctrlVar = 'kept'" });
    const keep = await tech.call("run_powershell", { script: "\"$((Get-Location).Path)|$asctrlVar\"" });
    check("session keeps directory and variables", keep.ok && /Temp\|kept/i.test(keep.text ?? ""), keep.text);

    await tech.call("reset_shell");
    const fresh = await tech.call("run_powershell", { script: "\"[$asctrlVar]\"" });
    check("reset_shell clears the session", fresh.ok && fresh.text?.trim() === "[]", fresh.text);

    // Write-Host -NoNewline leaves the line open, so the end marker follows it on the same line
    const noNl = await tech.call("run_powershell", { script: "Write-Host -NoNewline 'no newline'", timeoutSec: 15 });
    check("output without a trailing newline", noNl.ok && noNl.text?.trim() === "no newline", noNl.text ?? noNl.error);

    // A reset must stop a command that is still running, not wait behind it
    const slow = tech.call("run_powershell", { script: "Start-Sleep -Seconds 60; 'finished'", timeoutSec: 90 });
    await new Promise((r) => setTimeout(r, 1500));
    const t0 = Date.now();
    await tech.call("reset_shell");
    const cut = await slow;
    check("reset_shell interrupts a running command", !cut.ok && Date.now() - t0 < 10_000, `${Date.now() - t0} ms, ${cut.error}`);
    const after = await tech.call("run_powershell", { script: "'ok again'" });
    check("session works again after the interrupt", after.ok && !!after.text?.includes("ok again"), after.text ?? after.error);

    const content = `asctrl file test ${HELLO} ${Date.now()}`;
    const up = await tech.call("upload_file", {
      path: `%TEMP%\\asctrl-e2e\\${UPLOAD_NAME}`,
      data: Buffer.from(content).toString("base64"),
    });
    check("upload_file", up.ok, up.text ?? up.error);
    const down = await tech.call("download_file", { path: `%TEMP%\\asctrl-e2e\\${UPLOAD_NAME}` });
    const back = down.file ? Buffer.from(down.file.data, "base64").toString() : "";
    check("download_file round-trips", down.ok && back === content && down.file?.name === UPLOAD_NAME, down.error);
    await tech.call("run_powershell", { script: "Remove-Item -Recurse -Force $env:TEMP\\asctrl-e2e" });

    const out = await tech.call("click", { x: 99999, y: 10 });
    check("click outside the screenshot is rejected", !out.ok && !!out.error?.includes("outside"), out.error);

    const badKey = await tech.call("press_keys", { keys: "ctrl+notakey" });
    check("unknown key is rejected", !badKey.ok, badKey.error);

    const shift = await tech.call("press_keys", { keys: "shift" });
    check("press_keys (Shift alone)", shift.ok, shift.error);

    // ---- Viewing ----
    const zoom = await tech.call("zoom", { x0: 0, y0: 0, x1: 200, y1: 100 });
    check("zoom into a region", zoom.ok && !!zoom.image?.data && !!zoom.text?.includes("Zoomed"), zoom.text ?? zoom.error);
    const one = await tech.call("screenshot", { maxWidth: 800, display: 1 });
    check("screenshot of one display", one.ok && !!one.text?.includes("display #1"), one.text ?? one.error);
    const none = await tech.call("screenshot", { display: 16 });
    check("missing display is rejected", !none.ok, none.error);
    const cur = await tech.call("cursor_position");
    check("cursor_position", cur.ok && /\(\d+, \d+\)/.test(cur.text ?? ""), cur.text ?? cur.error);
    const wins = await tech.call("list_windows");
    check("list_windows", wins.ok && /\d+ windows/.test(wins.text ?? ""), wins.text?.split("\n")[0] ?? wins.error);
    const noWin = await tech.call("focus_window", { title: "no-such-window-asctrl" });
    check("focus_window errors when not found", !noWin.ok, noWin.error);
    const badHold = await tech.call("hold_key", { keys: "notakey", seconds: 0.1 });
    check("hold_key rejects unknown keys", !badHold.ok, badHold.error);
    const noLook = await tech.call("mouse_look", {});
    check("mouse_look rejects an empty move", !noLook.ok, noLook.error);
    const sym = await tech.call("press_keys", { keys: "shift+/" });
    check("press_keys supports symbol keys", sym.ok, sym.error);

    // ---- Files ----
    const dir = "%TEMP%\\asctrl-e2e-files";
    const w1 = await tech.call("write_file", { path: `${dir}\\a.txt`, content: `${LINE1}\r\nfoo = 1\r\n${LINE3}\r\n` });
    check("write_file creates (UTF-8)", w1.ok && !!w1.text?.includes("UTF-8"), w1.text ?? w1.error);
    const r1 = await tech.call("read_file", { path: `${dir}\\a.txt` });
    check("read_file has line numbers", r1.ok && /\s+2\tfoo = 1/.test(r1.text ?? ""), r1.error);
    const e1 = await tech.call("edit_file", { path: `${dir}\\a.txt`, oldText: "foo = 1\n", newText: "foo = 2\n" });
    const r2 = await tech.call("read_file", { path: `${dir}\\a.txt`, offset: 2, limit: 1 });
    check("edit_file adapts to CRLF", e1.ok && !!r2.text?.includes("foo = 2") && !r2.text?.includes(LINE3), e1.error ?? r2.text);
    // LINE1 and LINE3 share their last character, so it appears twice
    const e2 = await tech.call("edit_file", { path: `${dir}\\a.txt`, oldText: LINE1.slice(-1), newText: "X" });
    check("edit_file requires replaceAll on multiple matches", !e2.ok && !!e2.error?.includes("2 times"), e2.error);
    const w2 = await tech.call("write_file", { path: `${dir}\\gbk.bat`, content: `echo ${ANSI_TEXT}\r\n`, encoding: "ansi" });
    const r3 = await tech.call("read_file", { path: `${dir}\\gbk.bat` });
    check("ANSI (GBK) write reads back correctly", w2.ok && !!r3.text?.includes("ANSI") && !!r3.text?.includes(`echo ${ANSI_TEXT}`), r3.text ?? r3.error);
    await tech.call("write_file", { path: `${dir}\\sub\\u16.log`, content: `error: ${DISK_FULL}\n`, encoding: "utf16le" });
    const e3 = await tech.call("edit_file", { path: `${dir}\\sub\\u16.log`, oldText: DISK_FULL.slice(2), newText: "OK" });
    const r4 = await tech.call("read_file", { path: `${dir}\\sub\\u16.log` });
    check("UTF-16 file keeps its encoding after edit", e3.ok && !!r4.text?.includes("UTF-16 LE") && !!r4.text?.includes(`${DISK_FULL.slice(0, 2)}OK`), r4.text ?? e3.error);
    const ls = await tech.call("list_dir", { path: dir });
    check("list_dir", ls.ok && !!ls.text?.includes("sub\\") && !!ls.text?.includes("a.txt"), ls.error);
    const ff = await tech.call("find_files", { path: dir, pattern: "*.log;*.bat" });
    check("find_files recursive + multiple patterns", ff.ok && !!ff.text?.includes("u16.log") && !!ff.text?.includes("gbk.bat"), ff.text ?? ff.error);
    const sf = await tech.call("search_files", { path: dir, pattern: `${DISK_FULL.slice(0, 2)}|${ANSI_TEXT.slice(0, 2)}`, context: 0 });
    check("search_files across encodings", sf.ok && !!sf.text?.startsWith("2 matches"), sf.text ?? sf.error);
    const lossy = await tech.call("write_file", { path: `${dir}\\lossy.txt`, content: "emoji \u{1F600}", encoding: "ansi" });
    check("ANSI write refuses characters it would lose", !lossy.ok && !!lossy.error?.includes("code page"), lossy.error ?? lossy.text);
    const few = await tech.call("find_files", { path: dir, pattern: "*", maxResults: 1 });
    check("find_files stops at maxResults", few.ok && !!few.text?.startsWith("Found 1+"), few.text?.split("\n")[0] ?? few.error);
    const bin = await tech.call("read_file", { path: "%WINDIR%\\notepad.exe" });
    check("binary file is not read as text", !bin.ok && !!bin.error?.includes("binary"), bin.error);

    // ---- Clipboard (restored afterwards) ----
    const before = await tech.call("read_clipboard");
    const mark = `asctrl clipboard ${HELLO} ${Date.now()}`;
    await tech.call("write_clipboard", { text: mark });
    const clip = await tech.call("read_clipboard");
    check("clipboard read/write", clip.ok && clip.text?.trim() === mark, clip.text ?? clip.error);
    // Longer than a Windows command line can carry
    const big = `${HELLO} `.repeat(15_000).trim();
    const wBig = await tech.call("write_clipboard", { text: big });
    const rBig = await tech.call("read_clipboard");
    check("clipboard takes long text", wBig.ok && rBig.text?.trim() === big, wBig.error ?? `${rBig.text?.length} chars back`);
    if (before.ok && before.text && !before.text.startsWith("[") && !before.text.startsWith("(")) {
      await tech.call("write_clipboard", { text: before.text });
    }

    // ---- Background jobs ----
    const job = await tech.call("run_powershell", {
      background: true,
      script: "Write-Output 'started'; Start-Sleep -Seconds 2; Write-Output 'finished'; exit 3",
    });
    const jobId = Number(/as job (\d+)/.exec(job.text ?? "")?.[1]);
    check("background job starts", job.ok && jobId > 0, job.text ?? job.error);
    await new Promise((r) => setTimeout(r, 4000));
    const jo = await tech.call("job_output", { jobId });
    check("background job output and exit code", jo.ok && !!jo.text?.includes("finished") && !!jo.text?.includes("exit code 3"), jo.text);
    const jo2 = await tech.call("job_output", { jobId });
    check("job_output returns only new output", jo2.ok && !jo2.text?.includes("started"), jo2.text);
    const long = await tech.call("run_powershell", { background: true, script: "ping -n 30 127.0.0.1" });
    const longId = Number(/as job (\d+)/.exec(long.text ?? "")?.[1]);
    await new Promise((r) => setTimeout(r, 1500));
    const stop = await tech.call("stop_job", { jobId: longId });
    await new Promise((r) => setTimeout(r, 1000));
    const list = await tech.call("job_output");
    check("stop_job stops a long job", stop.ok && new RegExp(`\\[${longId}\\] Finished`).test(list.text ?? ""), list.text);

    // ---- Resume after restart (register then cancel; no real restart) ----
    const prep = await tech.call("restart_computer", { prepareOnly: true });
    const reg = await tech.call("run_powershell", {
      script: "(Get-ItemProperty 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\RunOnce').AsctrlResume",
    });
    check("restart_computer prepareOnly registers auto-start", prep.ok && !!reg.text?.includes(`--code ${CODE}`), reg.text);
    const cancel = await tech.call("restart_computer", { cancel: true });
    const reg2 = await tech.call("run_powershell", {
      script: "[bool](Get-ItemProperty 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\RunOnce').AsctrlResume",
    });
    check("restart_computer cancel clears the entry", cancel.ok && reg2.text?.trim() === "False", reg2.text);

    await tech.call("run_powershell", { script: "Remove-Item -Recurse -Force $env:TEMP\\asctrl-e2e-files" });
  }

  const hanging = tech.call("run_powershell", { script: "Start-Sleep -Seconds 20" });
  tech.disconnect();
  await Promise.race([hanging, new Promise((r) => setTimeout(() => r("hung"), 3000))]).then(
    (v) => check("disconnect fails calls still waiting", false, v === "hung" ? "still pending after 3 s" : "resolved"),
    (e) => check("disconnect fails calls still waiting", true, e.message),
  );
} catch (e) {
  check("unexpected exception", false, String(e));
} finally {
  agent.kill();
  relay.close();
  console.log(failed ? `\n${failed} failed` : "\nAll passed");
  process.exit(failed ? 1 : 0);
}
