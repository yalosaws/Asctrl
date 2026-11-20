// Windows, on a real desktop: the window timeline and reading a window that something covers.
// Usage: node test/windows.ts [--no-events]   (opens the bench window and Calculator; leave the computer alone ~20 s)
//   --no-events runs the driver with the timeline off (ASCTRL_NO_EVENTS=1), to compare with how it was
//
// 1. A window that opens late is noticed as it opens: the dialog opens 800 ms after its button is pressed, and
//    wait_for should return soon after that, not up to a poll later.
// 2. The timeline says which windows came and went.
// 3. Text drawn in a window that another program's window covers is still read (Windows Graphics Capture), and
//    a click on it lands in that window.
// 4. A screenshot of a covered window shows it as it draws itself without moving anything, and a click at a
//    point on that picture lands in it.

import path from "node:path";
import { Suite } from "./desk.ts";
import type { ObserveData } from "../src/shared/protocol.ts";

const off = process.argv.includes("--no-events");
const s = new Suite("Windows", { bench: ["-NotOnTop"], env: off ? { ASCTRL_NO_EVENTS: "1" } : {} });
const { tech, say } = s;

async function windowNamed(title: RegExp) {
  const wins = ((await tech.call("observe", { max: 1 })).data as ObserveData | undefined)?.windows ?? [];
  return wins.find((x) => title.test(x.title))?.handle;
}

let C: number | undefined;
s.after(async () => {
  // Restored before it closes: Calculator opens next time the way it was closed, and other tests expect its usual size
  if (C) await tech.call("focus_window", { window: C, action: "restore", after: "none" });
  if (C) await tech.call("focus_window", { window: C, action: "close", after: "none" });
});

await s.run(async () => {
  await tech.connectLocalDriver({ launch: false, waitMs: 30_000 });
  console.log(`timeline ${off ? "off" : "on"}`);
  await tech.call("wait_for", { appears: "Asctrl Bench", timeoutSec: 15 });
  const B = (await windowNamed(/^Asctrl Bench$/))!;

  for (const condition of [{ text: "Submit" }, { stable: true }]) {
    const absent = await tech.call("wait_for", { window: "Asctrl missing fixture 38721", ...condition, timeoutSec: 0.3, after: "none" });
    say(!absent.ok && /specified window/.test(absent.error ?? ""), "a missing explicit window cannot satisfy " + JSON.stringify(condition));
  }

  // 1. How long after the dialog opens wait_for notices it
  const late: number[] = [];
  for (let n = 0; n < 5; n++) {
    await tech.call("click", { window: B, target: "Open dialog", after: "none" });
    const t = performance.now();
    const r = await tech.call("wait_for", { appears: "Bench Dialog", timeoutSec: 5, after: "none" });
    const ms = performance.now() - t;
    if (!r.ok) {
      say(false, `dialog ${n + 1} not noticed: ${r.error}`);
      continue;
    }
    late.push(ms);
    await tech.call("click", { window: "Bench Dialog", target: "Close me", after: "none" });
    await tech.call("wait_for", { appears: "Bench Dialog", gone: true, timeoutSec: 5, after: "none" });
  }
  const avg = late.reduce((a, b) => a + b, 0) / Math.max(late.length, 1);
  // The click returns once the screen has settled (a few hundred ms), so the wait is shorter than 800 ms: what
  // counts is how it compares with the timeline off
  say(late.length === 5, `wait_for noticed the late dialog after ${late.map((x) => Math.round(x)).join(", ")} ms (average ${Math.round(avg)} ms)`);

  // 2. The timeline says which windows came and went
  await tech.call("click", { window: B, target: "Open dialog", after: "none" });
  await tech.call("wait_for", { appears: "Bench Dialog", timeoutSec: 5, after: "none" });
  await tech.call("click", { window: "Bench Dialog", target: "Close me", after: "none" });
  await tech.call("wait_for", { appears: "Bench Dialog", gone: true, timeoutSec: 5, after: "none" });
  const tl = await tech.call("timeline", { seconds: 15, window: "Bench Dialog" });
  type Ev = { kind: string; title: string; ago: number };
  const evs = ((tl.data as { events?: Ev[] } | undefined)?.events ?? []);
  if (off) say(!tl.ok, "without the timeline, timeline says it is off");
  else say(evs.some((e) => e.kind === "shown") && evs.some((e) => e.kind === "destroyed" || e.kind === "hidden"), `the timeline saw the dialog come and go: ${evs.map((e) => `${e.ago} s ago ${e.kind}`).join("; ")}`);

  // 3. Cover the bench window with Calculator, maximized, and read the canvas text underneath
  await tech.call("open_app", { target: "calc", after: "none" });
  await tech.call("wait_for", { appears: "计算器|Calculator", timeoutSec: 10, after: "none" });
  C = await windowNamed(/^(计算器|Calculator)$/);
  await tech.call("focus_window", { window: C, action: "maximize", after: "none" });
  await tech.call("wait_for", { stable: true, timeoutSec: 3, after: "none" });
  const t3 = performance.now();
  const q = await tech.call("observe", { window: B, query: "CANVAS-7391", ocr: true });
  const read3 = Math.round(performance.now() - t3);
  say(!!q.ok && /CANVAS-7391/.test(q.text ?? "") && /pixels|on screen/i.test(q.text ?? ""), `text on the covered window's canvas read in ${read3} ms:\n    ${(q.text ?? q.error ?? "").split("\n").slice(0, 3).join("\n    ")}`);
  const c = await tech.call("click", { window: B, target: "CANVAS-7391", after: "none" });
  const n = await tech.call("observe", { window: B, query: "Canvas clicks: 1" });
  say(c.ok && /Canvas clicks: 1/.test(n.text ?? ""), `a click on text in the covered window landed there: ${(c.text ?? c.error ?? "").split("\n")[0]}`);

  // 4. Cover it again: a screenshot of it shows it as it draws itself and moves nothing, and a click at a point
  //    on that picture brings it forward and lands there
  await tech.call("focus_window", { window: C, after: "none" });
  await tech.call("wait_for", { stable: true, timeoutSec: 3, after: "none" });
  const front = async () => ((await tech.call("observe", { max: 1 })).data as ObserveData | undefined)?.windows?.find((w) => w.front);
  const before = await front();
  const marked = await tech.call("screenshot", { window: B, marks: true });
  say(marked.ok && !!marked.image && /\be\d+ Button "Submit"/.test(marked.text ?? "") && (await front())?.handle === C, "covered screenshot preserves control references and foreground");
  const s = await tech.call("screenshot", { window: B });
  const still = (await front())?.handle;
  say(
    !!s.ok && !!s.image && /draws itself/.test(s.text ?? "") && still === C,
    `screenshot of the covered window, Calculator still in front (in front before: "${before?.title}", after: ${still === C ? "Calculator" : still}): ${(s.text ?? s.error ?? "").split("\n")[0]}`,
  );
  const at = /@\((\d+),(\d+)\) in "CANVAS-7391"/.exec((await tech.call("observe", { window: B, query: "CANVAS-7391", ocr: true })).text ?? "");
  const c2 = at ? await tech.call("click", { x: Number(at[1]), y: Number(at[2]), after: "none" }) : undefined;
  const n2 = await tech.call("observe", { window: B, query: "Canvas clicks: 2" });
  say(!!c2?.ok && /Canvas clicks: 2/.test(n2.text ?? ""), `a click at a point on that picture landed in it: ${at ? (c2?.text ?? c2?.error ?? "").split("\n")[0] : "the text's position was not given"}`);
});
