// Where the time of single actions goes, on a real desktop: for comparing before and after a change to how
// actions look, act or wait. Prints numbers; fails only when an action itself fails.
// Usage: node test/timing.ts [--no-events]   (opens the bench window and moves the mouse for about 40 s; leave the
//        computer alone). ASCTRL_TEST_DRIVER=<exe> times another build.
//
// 1. Each kind of action round after round (two clicks on one spot within the double-click time are kept apart on
//    purpose, which would count as waiting if one action were done five times over): the median reply time, and
//    how long the reply says the screen took to settle.
// 2. The same few actions in their variants (by reference or text, mouse or direct, with or without the report),
//    three or four times each.

import { Suite } from "./desk.ts";
import type { ObserveData, ToolName } from "../src/shared/protocol.ts";

const off = process.argv.includes("--no-events");
const s = new Suite("Timing", { bench: true, env: off ? { ASCTRL_NO_EVENTS: "1" } : {} });
const { tech } = s;
const WIN = "Asctrl Bench";
const median = (xs: number[]) => [...xs].sort((a, b) => a - b)[Math.floor(xs.length / 2)] ?? 0;

async function time(label: string, tool: ToolName, args: Record<string, unknown>, n = 3) {
  const ms: number[] = [];
  let last = "";
  for (let i = 0; i < n; i++) {
    const t = performance.now();
    const r = await tech.call(tool, args, 30_000);
    ms.push(performance.now() - t);
    if (!r.ok) s.failures++;
    last = (r.text ?? r.error ?? "").split("\n")[0].slice(0, 110);
  }
  console.log(`${label.padEnd(46)} ${ms.map((m) => String(Math.round(m)).padStart(4)).join(" ")} ms   ${last}`);
}

await s.run(async () => {
  const B = (await s.connect())!;
  await tech.call("focus_window", { window: B, after: "none" });
  await tech.call("wait_for", { stable: true, timeoutSec: 3, after: "none" });

  // 1.
  const cases: [string, ToolName, Record<string, unknown>][] = [
    ["press a button (UIA)", "click", { window: B, target: "Submit", after: "none" }],
    ["tick a box (UIA)", "click", { window: B, target: "Agree", after: "none" }],
    ["click text that does nothing", "click", { window: B, target: "Notes", after: "none" }],
    ["type into a field", "type_text", { window: B, target: "Name", text: "abc", after: "none" }],
    ["a key that does nothing", "press_keys", { keys: "shift", after: "none" }],
    ["click the canvas (pixels)", "click", { window: B, target: "CANVAS-7391", after: "none" }],
  ];
  console.log(`timeline ${off ? "off" : "on"}\n`);
  const ms = cases.map(() => [] as number[]);
  const settled = cases.map(() => [] as number[]);
  const last = cases.map(() => "");
  for (let n = 0; n < 5; n++) {
    for (const [i, [, tool, args]] of cases.entries()) {
      const t = performance.now();
      const r = await tech.call(tool, args);
      ms[i].push(performance.now() - t);
      last[i] = (r.text ?? r.error ?? "").split("\n")[0];
      if (!r.ok) s.failures++;
      const m = /settled in (\d+) ms/.exec(r.text ?? "");
      if (m) settled[i].push(Number(m[1]));
    }
  }
  let all = 0;
  for (const [i, [label]] of cases.entries()) {
    all += median(ms[i]);
    console.log(`${label.padEnd(30)} ${Math.round(median(ms[i])).toString().padStart(5)} ms   settle ${settled[i].length ? `${median(settled[i])} ms` : "-"}   ${last[i].slice(0, 90)}`);
  }
  console.log(`\nsum of medians ${Math.round(all)} ms\n`);

  // 2.
  const obs = (await tech.call("observe", { window: WIN })).text ?? "";
  const ref = (label: string) => obs.split("\n").find((l) => l.includes(label))?.match(/\[(e\d+)\]/)?.[1] ?? "";
  const pos = (label: string) => {
    const m = obs.split("\n").find((l) => l.includes(label))?.match(/@\((\d+),(\d+)\)/);
    return { x: Number(m?.[1]), y: Number(m?.[2]) };
  };
  await time("observe (window)", "observe", { window: WIN });
  await time("screenshot (whole screen)", "screenshot", {});
  await time("observe, the windows only", "observe", { max: 1 });
  await time("click ref Submit, after none", "click", { target: ref('Button "Submit"'), after: "none" });
  await time("click ref Submit, after auto", "click", { target: ref('Button "Submit"') });
  await time("click text Submit, after none", "click", { target: "Submit", window: WIN, after: "none" });
  await time("click ref Submit, mode direct, after none", "click", { target: ref('Button "Submit"'), mode: "direct", after: "none" });
  await time("click ref Agree (toggles), none", "click", { target: ref('CheckBox "Agree"'), after: "none" });
  await time("click ref Agree, mouse (toggles), none", "click", { target: ref('CheckBox "Agree"'), mode: "mouse", after: "none" }, 4);
  await time("click x,y Agree (toggles), none", "click", { ...pos('CheckBox "Agree"'), after: "none" }, 4);
  await time("click x,y Agree (toggles), auto", "click", pos('CheckBox "Agree"'), 4);
  await time("set_value Agree true (no-op twice), none", "set_value", { target: ref('CheckBox "Agree"'), value: true, after: "none" });
  await time("set_value Name, after none", "set_value", { target: ref('Edit "Name"'), value: "Zed", after: "none" });
  await time("type_text 20 chars into Notes, none", "type_text", { text: "hello world 1234567 ", target: ref('Edit "Notes"'), after: "none" });
  await time("type_text 300 chars (paste), none", "type_text", { text: "x".repeat(300), target: ref('Edit "Notes"'), replace: true, after: "none" });
  // What a paste leaves for the next action to wait on: the clipboard going back, which happens as soon as the
  // field is seen to hold the new text
  {
    const ms: number[] = [];
    for (let i = 0; i < 3; i++) {
      await tech.call("type_text", { text: String(i).repeat(300), target: ref('Edit "Notes"'), replace: true, after: "none" });
      const t = performance.now();
      await tech.call("set_value", { target: ref('CheckBox "Agree"'), value: i % 2 === 1, after: "none" });
      ms.push(performance.now() - t);
    }
    console.log(`${"set_value right after a paste, none".padEnd(46)} ${ms.map((m) => String(Math.round(m)).padStart(4)).join(" ")} ms`);
  }
  await time("press_keys shift, none", "press_keys", { keys: "shift", window: WIN, after: "none" });
  await time("read_text Notes", "read_text", { target: ref('Edit "Notes"') });
  await time("observe query CANVAS (OCR)", "observe", { query: "CANVAS-7391", window: WIN });
  const clicks = [pos('Edit "Name"'), pos('Edit "Notes"'), pos('Edit "Name"'), pos('Edit "Notes"')].map((p) => ({ tool: "click", args: p }));
  const plan = { steps: [...clicks, { tool: "press_keys", args: { keys: "a b c" } }] };
  await time("a plan of 4 clicks x,y + keys, fast", "exec", { plan, pace: "fast", learn: false, waitSec: 30 });
  await time("the same plan, reacting (the default)", "exec", { plan, learn: false, waitSec: 30 });
});
