// 手动演示用：连上被控端，按顺序执行 JSON 里的步骤，截图存到 test/out/。
// 用法：node test/demo-step.ts <relayUrl> <code> '<json 步骤数组>'   （或 @steps.json）
//   步骤：{ "tool": "click", "args": {...} } 或 { "wait": 毫秒 }

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { TechClient } from "../src/console/tech-client.ts";
import type { ToolName } from "../src/shared/protocol.ts";

const [relay, code, json] = process.argv.slice(2);
const steps: Array<{ tool?: ToolName; args?: Record<string, unknown>; wait?: number; save?: string }> = JSON.parse(
  json.startsWith("@") ? readFileSync(json.slice(1), "utf8") : json,
);

const tech = new TechClient(relay, "演示");
await tech.connect(code);
mkdirSync("test/out", { recursive: true });
for (const s of steps) {
  if (s.wait) {
    await new Promise((r) => setTimeout(r, s.wait));
    continue;
  }
  const r = await tech.call(s.tool!, s.args ?? {});
  console.log(`${r.ok ? "✔" : "✘"} ${s.tool} ${r.text ?? r.error ?? ""}`.trim());
  if (r.image && s.save) {
    writeFileSync(`test/out/${s.save}`, Buffer.from(r.image.data, "base64"));
    console.log(`  截图已保存 test/out/${s.save}`);
  }
}
tech.disconnect();
process.exit(0);
