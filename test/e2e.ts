// 端到端测试：本机起中继 + 被控端（自动同意），控制端依次调用每个工具。
// 用法：npm test

import { spawn } from "node:child_process";
import { writeFileSync, mkdirSync } from "node:fs";
import { startRelay } from "../src/relay/server.ts";
import { TechClient } from "../src/console/tech-client.ts";

const PORT = 18787;
const CODE = "123456";
const relay = startRelay(PORT);

const agent = spawn(
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

  tech.disconnect();
} catch (e) {
  check("未预期的异常", false, String(e));
} finally {
  agent.kill();
  relay.close();
  console.log(failed ? `\n${failed} 项失败` : "\n全部通过");
  process.exit(failed ? 1 : 0);
}
