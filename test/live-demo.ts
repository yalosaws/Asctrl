// Live demo on this computer: the orb flings virtual cursors at Calculator's buttons, first one at a time,
// then five at once. The real mouse pointer is never moved.
// Needs the UI dev server (npm --prefix agent-app run dev) so the debug agent's overlay can load.
// Usage: node test/live-demo.ts <path to asctrl-agent.exe (debug build)>

import { spawn, execSync } from "node:child_process";
import { startRelay } from "../src/relay/server.ts";
import { TechClient } from "../src/console/tech-client.ts";

const PORT = 18789;
const CODE = "246810";
const relay = startRelay(PORT);
const agent = spawn(process.argv[2], [], {
  stdio: ["ignore", "ignore", "inherit"],
  env: { ...process.env, ASCTRL_RELAY_URL: `ws://localhost:${PORT}`, ASCTRL_CODE: CODE },
});
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const say = (s: string) => console.log(`» ${s}`);

try {
  await sleep(3000);
  const tech = new TechClient(`ws://localhost:${PORT}`, "Demo");
  await tech.connect(CODE);
  say("connected");
  await sleep(2500);

  execSync("start calc", { shell: "cmd.exe" });
  await sleep(2500);
  const wins = (await tech.call("list_windows", {})).text ?? "";
  const calc = wins.includes("计算器") ? "计算器" : "Calculator";
  await tech.call("focus_window", { title: calc });
  await sleep(1000);

  const list = (await tech.call("list_elements", { title: calc })).text ?? "";
  const id = (re: RegExp) => Number(list.split("\n").find((l) => re.test(l))?.match(/\[(\d+)\]/)?.[1] ?? 0);
  const btn = (zh: string, en: string) => id(new RegExp(`Button "(${zh}|${en})"`));
  const press = async (zh: string, en: string) => {
    const r = await tech.call("element_action", { id: btn(zh, en) });
    say(r.ok ? (r.text ?? "") : `failed: ${r.error}`);
  };

  say("one at a time: 7 × 6 =");
  for (const [zh, en] of [["七", "Seven"], ["乘以", "Multiply by"], ["六", "Six"], ["等于", "Equals"]]) {
    await press(zh, en);
    await sleep(1400);
  }
  await sleep(1500);

  await press("清除", "Clear");
  await sleep(1600);

  say("five at once: 1 2 3 4 5 (they land in whatever order they finish)");
  const digits = [["一", "One"], ["二", "Two"], ["三", "Three"], ["四", "Four"], ["五", "Five"]];
  const r = await tech.call("element_action", { actions: digits.map(([zh, en]) => ({ id: btn(zh, en) })) });
  say((r.text ?? r.error ?? "").replace(/\n/g, " | "));
  await sleep(3500);

  const shown = (await tech.call("list_elements", { title: calc, query: "显示为" })).text ?? "";
  say(shown.split("\n").slice(2).join(" ") || "(display not found)");
  await sleep(2000);
  await tech.call("focus_window", { title: calc, action: "close" });
  say("done");
  await sleep(1500);
} catch (e) {
  console.error(e);
} finally {
  agent.kill();
  relay.close?.();
  process.exit(0);
}
