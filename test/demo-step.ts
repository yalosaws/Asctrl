// Manual demo: connect to an agent, run the steps from JSON in order, save screenshots to test/out/.
// Usage: node test/demo-step.ts <relayUrl> <code> '<json array of steps>'   (or @steps.json)
//   Step: { "tool": "click", "args": {...} } or { "wait": ms }

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { TechClient } from "../src/console/tech-client.ts";
import type { ToolName } from "../src/shared/protocol.ts";

const [relay, code, json] = process.argv.slice(2);
const steps: Array<{ tool?: ToolName; args?: Record<string, unknown>; wait?: number; save?: string }> = JSON.parse(
  json.startsWith("@") ? readFileSync(json.slice(1), "utf8") : json,
);

const tech = new TechClient(relay, "Demo");
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
    console.log(`  Screenshot saved to test/out/${s.save}`);
  }
}
tech.disconnect();
process.exit(0);
