// Programs (exec / resume), on a real desktop.
// Usage: node test/programs.ts   (opens the bench window and Asctrl's browser for about 40 s; leave the computer alone)
//
// 1. A loop over rows in one call: fill the bench form for each name, submit, read what it says. A program past
//    its time limit is stopped.
// 2. An action that fails stops the program at its line (stuck); resume with another action in its place and it
//    goes on from there.
// 3. A program that asks, and goes on with the answer.
// 4. Abort: the program gets an error it can catch.
// 5. A running program stopped.
// 6. A program in Asctrl's own browser: the page channel from inside a program.
// 7. on(): a dialog that comes up in the middle of a program is closed in between its steps, and it goes on; ending
//    another program leaves this one's rule in place.
// 8. until(): waits for a condition the program checks itself.
// 9. Stuck on a button that isn't there: the report lists what windows came and went; the AI answers with code,
//    which does several steps in the stuck one's place, and the program goes on with what that code returned.
// 10. waitFor / expect / shows / waitGone see text in the window the moment it shows, and a window's title still
//    counts.

import path from "node:path";
import { pathToFileURL } from "node:url";
import { Suite, brief, prog, root } from "./desk.ts";
import type { ObserveData } from "../src/shared/protocol.ts";

const s = new Suite("Programs", { bench: true });
const { tech, say } = s;
const submitted = async () => /Submitted: \w*/.exec((await tech.call("observe", { window: "Asctrl Bench", query: "Submitted" })).text ?? "")?.[0] ?? "";

await s.run(async () => {
  const B = (await s.connect())!;
  await tech.call("focus_window", { window: B, after: "none" });

  // 1. Rows in one call
  let t = performance.now();
  const r1 = await tech.call("exec", {
    script: `
      const seen = [];
      for (const name of args.names) {
        fill({ Name: name }, { window: args.w });
        click('Submit', { window: args.w });
        seen.push(look({ window: args.w, query: 'Submitted:' }).text.match(/Submitted: \\w+/)[0]);
      }
      return seen;`,
    args: { names: ["Ann", "Bob", "Cy"], w: B },
  });
  const p1 = prog(r1);
  say(p1?.state === "done" && JSON.stringify(p1.result) === JSON.stringify(["Submitted: Ann", "Submitted: Bob", "Submitted: Cy"]), `three rows in one call, ${p1?.actions} actions in ${Math.round(performance.now() - t)} ms: ${JSON.stringify(p1?.result ?? brief(r1.text ?? r1.error))}`);
  const expired = await tech.call("exec", { script: "sleep(2500); return 17;", timeoutSec: 1, waitSec: 4, learn: false });
  say(prog(expired)?.state === "stopped" && /out of time/.test(prog(expired)?.error ?? ""), "a program past its time limit is stopped, sleeping or not");

  // 2. Stuck, then something else in its place
  t = performance.now();
  const r2 = await tech.call("exec", {
    script: `fill({ Name: 'Dee' }, { window: args.w });\nclick('Send it', { window: args.w, timeoutSec: 0 });\nreturn look({ window: args.w, query: 'Submitted:' }).text.match(/Submitted: \\w+/)[0];`,
    args: { w: B },
  });
  const p2 = prog(r2);
  say(p2?.state === "stuck" && p2.line === 2 && p2.call?.tool === "click", `a failed action stopped the program at its line: ${brief(r2.text)}`);
  const r2b = await tech.call("resume", { id: p2?.id, instead: { tool: "click", args: { target: "Submit", window: B, after: "none" } } });
  say(prog(r2b)?.state === "done" && prog(r2b)?.result === "Submitted: Dee", `resumed with another action in its place, it went on: ${JSON.stringify(prog(r2b)?.result ?? brief(r2b.text ?? r2b.error))} (${Math.round(performance.now() - t)} ms)`);

  // 3. Asking
  const r3 = await tech.call("exec", { script: `const who = ask('Which name?', { choices: ['Eve', 'Fay'] });\nfill({ Name: who }, { window: args.w });\nclick('Submit', { window: args.w });\nreturn who;`, args: { w: B } });
  const p3 = prog(r3);
  say(p3?.state === "asking" && /Which name/.test(r3.text ?? ""), `the program asked: ${brief(r3.text)}`);
  const r3b = await tech.call("resume", { id: p3?.id, answer: "Fay" });
  const shown = (await tech.call("observe", { window: B, query: "Submitted: Fay" })).text ?? "";
  say(prog(r3b)?.state === "done" && /Submitted: Fay/.test(shown), `answered, it went on with the answer: ${JSON.stringify(prog(r3b)?.result)}`);

  // 4. Abort, caught
  const r4 = await tech.call("exec", { script: `try { click('No such thing', { window: args.w, timeoutSec: 0 }); } catch (e) { return 'caught: ' + e.message; }`, args: { w: B } });
  const r4b = await tech.call("resume", { id: prog(r4)?.id, abort: "not there today" });
  say(prog(r4b)?.result === "caught: not there today", `aborted, the program caught it: ${JSON.stringify(prog(r4b)?.result ?? brief(r4b.text ?? r4b.error))}`);

  // 5. Stop
  const r5 = await tech.call("exec", { script: `let n = 0;\nwhile (true) { n++; sleep(50); }`, waitSec: 0.5 });
  const r5b = await tech.call("resume", { id: prog(r5)?.id, stop: true });
  say(prog(r5)?.state === "running" && prog(r5b)?.state === "stopped", `a running program was stopped: ${brief(r5b.text)}`);

  // 6. In Asctrl's browser
  t = performance.now();
  const url = pathToFileURL(path.join(root, "test/web/p3.html")).href;
  const r6 = await tech.call("exec", {
    script: `
      const o = open(args.url, { browser: 'asctrl' });
      const w = o.data.handle;
      for (let i = 0; i < 3; i++) click('Save', { window: w });
      type('Search here', 'from a program', { window: w });
      return { w, saved: (read(w).match(/saved \\d+/g) || []).pop() };`,
    args: { url },
  });
  const p6 = prog(r6);
  const W = (p6?.result as { w?: number } | undefined)?.w;
  if (W) s.after(() => tech.call("focus_window", { window: W, action: "close", after: "none" }));
  say(p6?.state === "done" && (p6.result as { saved?: string }).saved === "saved 3", `a program in Asctrl's browser: ${JSON.stringify(p6?.result ?? brief(r6.text ?? r6.error))}, ${p6?.actions} actions in ${Math.round(performance.now() - t)} ms`);
  await tech.call("focus_window", { window: B, after: "none" });

  // 7. on()
  const r7 = await tech.call("exec", {
    script: `on('Close me', 'Close me');\nclick('Open dialog', { window: 'Asctrl Bench' });\nsleep(1500);\nfill({ Name: 'onward' }, { window: 'Asctrl Bench' });\nclick('Submit', { window: 'Asctrl Bench' });\nreturn 'ok';`,
    learn: false,
    waitSec: 40,
  });
  const wins = ((await tech.call("observe", { max: 1 })).data as ObserveData).windows.map((w) => w.title);
  say(prog(r7)?.state === "done" && (await submitted()) === "Submitted: onward" && !wins.includes("Bench Dialog") && /Popups dismissed/.test((prog(r7)?.log ?? []).join("\n")), `on(): the dialog that came up was closed between steps, and the program went on: ${brief(prog(r7)?.log.join(" "))}`);
  const a = prog(await tech.call("exec", { script: "on('Close me', 'Close me'); return ask('Continue A?');", learn: false }));
  const b = prog(await tech.call("exec", { script: "on('Separate fixture', 'Dismiss'); return ask('Continue B?');", learn: false }));
  if (!a?.id || !b?.id || a.state !== "asking" || b.state !== "asking") throw new Error("Popup fixtures did not reach their questions");
  const finishedB = prog(await tech.call("resume", { id: b.id, answer: "B", waitSec: 5 }));
  await tech.call("click", { target: "Open dialog", window: "Asctrl Bench", after: "none" });
  await new Promise((r) => setTimeout(r, 1500));
  const finishedA = prog(await tech.call("resume", { id: a.id, answer: "A", waitSec: 5 }));
  const openDialogs = ((await tech.call("observe", { max: 1 })).data as ObserveData).windows;
  say(finishedA?.state === "done" && finishedB?.state === "done" && !openDialogs.some((w) => w.title === "Bench Dialog")
    && /Popups dismissed/.test(finishedA.log.join(" ")) && !/Popups dismissed/.test(finishedB.log.join(" ")), "ending a second program preserves the first program's popup rule and log");

  // 8. until()
  const r8 = await tech.call("exec", { script: `fill({ Name: 'later' }, { window: 'Asctrl Bench' }); click('Submit', { window: 'Asctrl Bench' }); return until(() => read('Asctrl Bench').match(/Submitted: later/), { timeoutSec: 5 })[0];`, learn: false, waitSec: 30 });
  say(prog(r8)?.state === "done" && prog(r8)?.result === "Submitted: later", `until(): waited for what the program looked for: ${JSON.stringify(prog(r8)?.result)}`);

  // 9. Stuck, answered with code
  let r = await tech.call("exec", { script: `click('Open dialog', { window: 'Asctrl Bench' });\nwaitFor('Bench Dialog');\nclick('Close me', { window: 'Bench Dialog' });\nconst r = click('Send everything', { window: 'Asctrl Bench' });\nreturn 'went on with ' + r;`, learn: false, waitSec: 30 });
  const p9 = prog(r);
  const events = (p9 as unknown as { events?: { kind: string; title: string }[] } | undefined)?.events ?? [];
  say(p9?.state === "stuck" && p9?.line === 4 && events.some((e) => e.title === "Bench Dialog") && !!r.image, `stuck at line ${p9?.line}; the report has a picture of the window and lists the windows that came and went: ${events.map((e) => `${e.kind} ${e.title}`).join(", ")}`);
  r = await tech.call("resume", { id: p9?.id, code: `fill({ Name: 'patched' }, { window: 'Asctrl Bench' }); click('Submit', { window: 'Asctrl Bench' }); return 'the patch';`, waitSec: 30 });
  say(prog(r)?.state === "done" && prog(r)?.result === "went on with the patch" && (await submitted()) === "Submitted: patched", `answered with code: it ran in the step's place and the program went on: ${JSON.stringify(prog(r)?.result)}`);

  // 10. Waiting on text, and on a window's title
  const r10 = prog(await tech.call("exec", {
    script: `fill({ Name: 'seen' }, { window: 'Asctrl Bench' });\nclick('Submit', { window: 'Asctrl Bench' });\nconst t = Date.now();\nwaitFor('Submitted: seen', { window: 'Asctrl Bench' });\nexpect('Submitted: seen', { window: 'Asctrl Bench' });\nconst yes = shows('Submitted: seen', { window: 'Asctrl Bench' });\nconst no = shows('Nothing says this', { window: 'Asctrl Bench' });\nconst text = Date.now() - t;\nclick('Open dialog', { window: 'Asctrl Bench' });\nwaitFor('Bench Dialog');\nclick('Close me', { window: 'Bench Dialog' });\nwaitGone('Bench Dialog');\nreturn { text, yes, no };`,
    learn: false,
    waitSec: 30,
  }));
  const v10 = r10?.result as { text?: number; yes?: boolean; no?: boolean } | undefined;
  say(r10?.state === "done" && v10?.yes === true && v10?.no === false && (v10?.text ?? 1e9) < 3000, `waitFor, expect and shows see text in the window at once (${v10?.text} ms for all four), and a window's title still counts: ${r10?.state} ${brief(r10?.failure?.error ?? r10?.error ?? "")}`);
});
