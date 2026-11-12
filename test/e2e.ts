// 端到端测试：本机起中继 + 被控端（自动同意），控制端依次调用每个工具。
// 用法：npm test                      测 Node 版被控端
//       npm test -- --app <exe 路径>  测图形界面版被控端（需要 debug 编译）

import { spawn } from "node:child_process";
import { writeFileSync, mkdirSync } from "node:fs";
import { startRelay } from "../src/relay/server.ts";
import { TechClient } from "../src/console/tech-client.ts";

const PORT = 18787;
const CODE = "123456";
const relay = startRelay(PORT);

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
  await new Promise((r) => setTimeout(r, 1500)); // 等被控端注册

  const bad = new TechClient(`ws://localhost:${PORT}`);
  await bad.connect("000000").then(
    () => check("错误连接码被拒绝", false),
    (e) => check("错误连接码被拒绝", true, e.message),
  );

  const tech = new TechClient(`ws://localhost:${PORT}`, "测试员");
  const host = await tech.connect(CODE);
  check("配对成功", !!host, host);

  const info = await tech.call("system_info");
  check("system_info", info.ok && !!info.text?.includes("OS"), info.text?.slice(0, 80).replace(/\s+/g, " "));

  const procs = await tech.call("list_processes", { top: 5 });
  check("list_processes", procs.ok && !!procs.text?.includes("Name"));

  const ps = await tech.call("run_powershell", { script: "Write-Output '你好 asctrl'; 1+1" });
  check("run_powershell 中文输出", ps.ok && !!ps.text?.includes("你好 asctrl") && !!ps.text?.includes("2"), ps.text);

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
    // ---- 只有图形界面版支持的能力 ----
    check("截图返回尺寸说明", !!shot.text?.includes("×"), shot.text);

    await tech.call("run_powershell", { script: "Set-Location $env:TEMP; $asctrlVar = 'kept'" });
    const keep = await tech.call("run_powershell", { script: "\"$((Get-Location).Path)|$asctrlVar\"" });
    check("会话保留目录和变量", keep.ok && /Temp\|kept/i.test(keep.text ?? ""), keep.text);

    await tech.call("reset_shell");
    const fresh = await tech.call("run_powershell", { script: "\"[$asctrlVar]\"" });
    check("reset_shell 清空会话", fresh.ok && fresh.text?.trim() === "[]", fresh.text);

    const content = `asctrl 文件测试 ${Date.now()}`;
    const up = await tech.call("upload_file", {
      path: "%TEMP%\\asctrl-e2e\\上传.txt",
      data: Buffer.from(content).toString("base64"),
    });
    check("upload_file", up.ok, up.text ?? up.error);
    const down = await tech.call("download_file", { path: "%TEMP%\\asctrl-e2e\\上传.txt" });
    const back = down.file ? Buffer.from(down.file.data, "base64").toString() : "";
    check("download_file 内容一致", down.ok && back === content && down.file?.name === "上传.txt", down.error);
    await tech.call("run_powershell", { script: "Remove-Item -Recurse -Force $env:TEMP\\asctrl-e2e" });

    const out = await tech.call("click", { x: 99999, y: 10 });
    check("超出截图范围的点击被拒绝", !out.ok && !!out.error?.includes("超出"), out.error);

    const badKey = await tech.call("press_keys", { keys: "ctrl+notakey" });
    check("不认识的按键被拒绝", !badKey.ok, badKey.error);

    const shift = await tech.call("press_keys", { keys: "shift" });
    check("press_keys（单按 Shift）", shift.ok, shift.error);

    // ---- 看 ----
    const zoom = await tech.call("zoom", { x0: 0, y0: 0, x1: 200, y1: 100 });
    check("zoom 放大区域", zoom.ok && !!zoom.image?.data && !!zoom.text?.includes("放大"), zoom.text ?? zoom.error);
    const one = await tech.call("screenshot", { maxWidth: 800, display: 1 });
    check("screenshot 指定显示器", one.ok && !!one.text?.includes("1 号显示器"), one.text ?? one.error);
    const none = await tech.call("screenshot", { display: 16 });
    check("不存在的显示器被拒绝", !none.ok, none.error);
    const cur = await tech.call("cursor_position");
    check("cursor_position", cur.ok && /\(\d+, \d+\)/.test(cur.text ?? ""), cur.text ?? cur.error);
    const wins = await tech.call("list_windows");
    check("list_windows", wins.ok && /个窗口/.test(wins.text ?? ""), wins.text?.split("\n")[0] ?? wins.error);
    const noWin = await tech.call("focus_window", { title: "不存在的窗口-asctrl" });
    check("focus_window 找不到时报错", !noWin.ok, noWin.error);
    const badHold = await tech.call("hold_key", { keys: "notakey", seconds: 0.1 });
    check("hold_key 不认识的按键被拒绝", !badHold.ok, badHold.error);
    const sym = await tech.call("press_keys", { keys: "shift+/" });
    check("press_keys 支持符号键", sym.ok, sym.error);

    // ---- 文件 ----
    const dir = "%TEMP%\\asctrl-e2e-files";
    const w1 = await tech.call("write_file", { path: `${dir}\\a.txt`, content: "第一行\r\nfoo = 1\r\n第三行\r\n" });
    check("write_file 新建（UTF-8）", w1.ok && !!w1.text?.includes("UTF-8"), w1.text ?? w1.error);
    const r1 = await tech.call("read_file", { path: `${dir}\\a.txt` });
    check("read_file 带行号", r1.ok && /\s+2\tfoo = 1/.test(r1.text ?? ""), r1.error);
    const e1 = await tech.call("edit_file", { path: `${dir}\\a.txt`, oldText: "foo = 1\n", newText: "foo = 2\n" });
    const r2 = await tech.call("read_file", { path: `${dir}\\a.txt`, offset: 2, limit: 1 });
    check("edit_file 适配 CRLF", e1.ok && !!r2.text?.includes("foo = 2") && !r2.text?.includes("第三行"), e1.error ?? r2.text);
    const e2 = await tech.call("edit_file", { path: `${dir}\\a.txt`, oldText: "行", newText: "X" });
    check("edit_file 多处匹配时要求 replaceAll", !e2.ok && !!e2.error?.includes("2 次"), e2.error);
    const w2 = await tech.call("write_file", { path: `${dir}\\gbk.bat`, content: "echo 中文测试\r\n", encoding: "ansi" });
    const r3 = await tech.call("read_file", { path: `${dir}\\gbk.bat` });
    check("ANSI(GBK) 写入后能正确读回", w2.ok && !!r3.text?.includes("ANSI") && !!r3.text?.includes("echo 中文测试"), r3.text ?? r3.error);
    await tech.call("write_file", { path: `${dir}\\sub\\u16.log`, content: "error: 磁盘已满\n", encoding: "utf16le" });
    const e3 = await tech.call("edit_file", { path: `${dir}\\sub\\u16.log`, oldText: "已满", newText: "正常" });
    const r4 = await tech.call("read_file", { path: `${dir}\\sub\\u16.log` });
    check("UTF-16 文件修改后保持编码", e3.ok && !!r4.text?.includes("UTF-16 LE") && !!r4.text?.includes("磁盘正常"), r4.text ?? e3.error);
    const ls = await tech.call("list_dir", { path: dir });
    check("list_dir", ls.ok && !!ls.text?.includes("sub\\") && !!ls.text?.includes("a.txt"), ls.error);
    const ff = await tech.call("find_files", { path: dir, pattern: "*.log;*.bat" });
    check("find_files 递归 + 多模式", ff.ok && !!ff.text?.includes("u16.log") && !!ff.text?.includes("gbk.bat"), ff.text ?? ff.error);
    const sf = await tech.call("search_files", { path: dir, pattern: "磁盘|中文", context: 0 });
    check("search_files 跨编码搜索", sf.ok && !!sf.text?.startsWith("2 处匹配"), sf.text ?? sf.error);
    const bin = await tech.call("read_file", { path: "%WINDIR%\\notepad.exe" });
    check("二进制文件不当文本读", !bin.ok && !!bin.error?.includes("二进制"), bin.error);

    // ---- 剪贴板（测完还原） ----
    const before = await tech.call("read_clipboard");
    const mark = `asctrl 剪贴板 ${Date.now()}`;
    await tech.call("write_clipboard", { text: mark });
    const clip = await tech.call("read_clipboard");
    check("剪贴板读写", clip.ok && clip.text?.trim() === mark, clip.text ?? clip.error);
    if (before.ok && before.text && !before.text.startsWith("[") && !before.text.startsWith("(")) {
      await tech.call("write_clipboard", { text: before.text });
    }

    // ---- 后台任务 ----
    const job = await tech.call("run_powershell", {
      background: true,
      script: "Write-Output '开始'; Start-Sleep -Seconds 2; Write-Output '完成'; exit 3",
    });
    const jobId = Number(/任务编号 (\d+)/.exec(job.text ?? "")?.[1]);
    check("后台任务启动", job.ok && jobId > 0, job.text ?? job.error);
    await new Promise((r) => setTimeout(r, 4000));
    const jo = await tech.call("job_output", { jobId });
    check("后台任务输出和退出码", jo.ok && !!jo.text?.includes("完成") && !!jo.text?.includes("退出码 3"), jo.text);
    const jo2 = await tech.call("job_output", { jobId });
    check("job_output 只返回新输出", jo2.ok && !jo2.text?.includes("开始"), jo2.text);
    const long = await tech.call("run_powershell", { background: true, script: "ping -n 30 127.0.0.1" });
    const longId = Number(/任务编号 (\d+)/.exec(long.text ?? "")?.[1]);
    await new Promise((r) => setTimeout(r, 1500));
    const stop = await tech.call("stop_job", { jobId: longId });
    await new Promise((r) => setTimeout(r, 1000));
    const list = await tech.call("job_output");
    check("stop_job 停止长任务", stop.ok && new RegExp(`\\[${longId}\\] 已结束`).test(list.text ?? ""), list.text);

    // ---- 重启续连（只登记再取消，不真的重启） ----
    const prep = await tech.call("restart_computer", { prepareOnly: true });
    const reg = await tech.call("run_powershell", {
      script: "(Get-ItemProperty 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\RunOnce').AsctrlResume",
    });
    check("restart_computer prepareOnly 登记自启动", prep.ok && !!reg.text?.includes(`--code ${CODE}`), reg.text);
    const cancel = await tech.call("restart_computer", { cancel: true });
    const reg2 = await tech.call("run_powershell", {
      script: "[bool](Get-ItemProperty 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\RunOnce').AsctrlResume",
    });
    check("restart_computer cancel 清除登记", cancel.ok && reg2.text?.trim() === "False", reg2.text);

    await tech.call("run_powershell", { script: "Remove-Item -Recurse -Force $env:TEMP\\asctrl-e2e-files" });
  }

  tech.disconnect();
} catch (e) {
  check("未预期的异常", false, String(e));
} finally {
  agent.kill();
  relay.close();
  console.log(failed ? `\n${failed} 项失败` : "\n全部通过");
  process.exit(failed ? 1 : 0);
}
