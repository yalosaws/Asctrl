// Manual check of list_elements / element_action against Calculator, without touching the mouse.
// Usage: node test/uia-check.ts <path to asctrl-agent.exe (debug build)>

import { spawn, execSync } from "node:child_process";
import { startRelay } from "../src/relay/server.ts";
import { TechClient } from "../src/console/tech-client.ts";

const PORT = 18788;
const CODE = "654321";
const relay = startRelay(PORT);
const exe = process.argv[2];
const agent = spawn(exe, [], {
  stdio: ["ignore", "pipe", "inherit"],
  env: { ...process.env, ASCTRL_RELAY_URL: `ws://localhost:${PORT}`, ASCTRL_CODE: CODE },
});
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
let failed = 0;
const check = (name: string, ok: boolean, detail = "") => {
  console.log(`${ok ? "✔" : "✘"} ${name}${detail ? " — " + detail : ""}`);
  if (!ok) failed++;
};

try {
  await sleep(2500);
  const tech = new TechClient(`ws://localhost:${PORT}`, "Tester");
  await tech.connect(CODE);
  const pos = (await tech.call("cursor_position", {})).text;

  execSync("start calc", { shell: "cmd.exe" });
  await sleep(2500);

  const wins = (await tech.call("list_windows", {})).text ?? "";
  const calc = wins.includes("计算器") ? "计算器" : "Calculator";
  const t0 = Date.now();
  const all = await tech.call("list_elements", { title: calc });
  check("list_elements works", all.ok, all.error ?? `${Date.now() - t0} ms, ${all.text?.split("\n").length} lines`);
  console.log(all.text?.split("\n").slice(0, 25).join("\n"));

  // Ids from separate listings are only valid until the next listing, so list the whole keypad once
  const pad = (await tech.call("list_elements", { title: calc, query: "Button" })).text ?? "";
  const id = (name: RegExp) => Number(pad.split("\n").find((l) => name.test(l))?.match(/\[(\d+)\]/)?.[1] ?? 0);
  const seq = [/"(Seven|七)"/, /"(Plus|加)"/, /"(Five|五)"/, /"(Equals|等于)"/];
  for (const re of seq) {
    const r = await tech.call("element_action", { id: id(re) });
    check(`press ${re}`, r.ok, r.text ?? r.error);
  }
  const res = await tech.call("list_elements", { title: calc, query: "12" });
  check("calculator shows 12", !!res.text?.match(/\b12\b/), res.text?.split("\n").slice(1).join(" | "));
  const pos2 = (await tech.call("cursor_position", {})).text;
  check("mouse did not move", pos === pos2, `${pos} / ${pos2}`);

  // Parallel batch: three independent actions in one call, one of them bad
  const pad2 = (await tech.call("list_elements", { title: calc })).text ?? "";
  const id2 = (re: RegExp) => Number(pad2.split("\n").find((l) => re.test(l))?.match(/\[(\d+)\]/)?.[1] ?? 0);
  const t1 = Date.now();
  const batch = await tech.call("element_action", {
    actions: [{ id: id2(/"(Clear|清除)"/) }, { id: id2(/Text "(Display is|显示为)/), action: "text" }, { id: 9999 }],
  });
  const lines = (batch.text ?? batch.error ?? "").split("\n");
  check("batch reports each action", lines.filter((l) => /^\d\. [✓✗]/.test(l)).length === 3, `${Date.now() - t1} ms\n${lines.join("\n")}`);
  check("batch marks the bad one failed", !batch.ok && /3\. ✗/.test(batch.error ?? ""));
  const after = await tech.call("list_elements", { title: calc, query: "显示为 0" });
  const after2 = after.text?.includes("[1]") ? after : await tech.call("list_elements", { title: calc, query: "Display is 0" });
  check("clear ran", !!after2.text?.includes("[1]"), after2.text?.split("\n").slice(2).join(" | "));
  const pos3 = (await tech.call("cursor_position", {})).text;
  check("mouse still did not move", pos === pos3);

  const close = await tech.call("focus_window", { title: calc, action: "close" });
  check("closed calculator", close.ok, close.error);
} catch (e) {
  check("no exception", false, String(e));
} finally {
  agent.kill();
  relay.close?.();
  process.exit(failed ? 1 : 0);
}
