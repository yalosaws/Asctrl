// The relay turns a technician away after too many wrong codes, before it looks at the code at all.
// Usage: node test/relay-limit.ts   (a relay of its own on a spare port; nothing on the desktop)

import { WebSocket } from "ws";
import { startRelay } from "../src/relay/server.ts";

const port = 18999;
const wss = startRelay(port);
const ask = (msg: object) =>
  new Promise<string>((resolve) => {
    const ws = new WebSocket(`ws://127.0.0.1:${port}`);
    ws.on("open", () => ws.send(JSON.stringify(msg)));
    ws.on("message", (raw) => {
      const m = JSON.parse(String(raw));
      resolve(m.message ?? m.type);
      ws.close();
    });
  });

// A computer showing 123456, letting in whoever has the code
const agent = new WebSocket(`ws://127.0.0.1:${port}`);
await new Promise((r) => agent.on("open", r));
agent.send(JSON.stringify({ type: "agent_hello", code: "123456", hostname: "test", protocol: 2 }));
agent.on("message", (raw) => {
  const m = JSON.parse(String(raw));
  if (m.type === "consent_request") agent.send(JSON.stringify({ type: "consent_reply", allow: true }));
});
await new Promise((r) => setTimeout(r, 200));

const out: string[] = [];
for (let i = 0; i < 5; i++) out.push(await ask({ type: "tech_hello", code: String(100000 + i), techName: "guesser" }));
// The sixth try, now with the right code, is turned away as well
out.push(await ask({ type: "tech_hello", code: "123456", techName: "guesser" }));
const ok = out.slice(0, 5).every((m) => /No agent/.test(m)) && /Too many wrong codes/.test(out[5]);
console.log(`${ok ? "✓" : "✗"} turned away after 5 wrong codes, even with the right one: ${out[5]}`);
agent.close();
wss.close();
process.exit(ok ? 0 : 1);
