// Skills (exec with learn / skill, and the skills tool), on a real desktop.
// Usage: node test/skills.ts   (opens the bench window and Notepad for about 40 s; leave the computer alone)
// Skills go to a file of the suite's own, not the computer's.
//
// 1. A program given learn is kept as a skill.
// 2. It is found by the window in front (no query) and by words; a query about something else finds nothing.
// 3. Run again with new args and no script, in one call.
// 4. Without its args it is refused, saying which are missing.
// 5. A skill that no longer fits gets stuck and says where; learned again under its name, the new one replaces it,
//    keeping its record.
// 6. show gives its script and args; forget drops one.
// 7. A program that works, sent without learn, is kept by itself, and found by what it did.
// 8. With another window in front, running it is refused (it was made for the bench window); with the bench
//    window in front it runs.
// 9. import takes a skill (as export gives one) whose button has been renamed: it gets stuck; put right with
//    instead, it ends well, and its script now names the new button. export gives it back with the change.

import { Suite, brief, prog } from "./desk.ts";

const s = new Suite("Skills", { bench: true });
const { tech, say } = s;
type Found = { skills?: { name: string; fits: number; stuck: number; problem: string | null; runs: number }[] };

const SUBMIT = `
fill({ Name: args.name }, { window: "Asctrl Bench" });
click('Submit', { window: "Asctrl Bench" });
return look({ query: 'Submitted:', window: "Asctrl Bench" }).text.match(/Submitted: \\w+/)[0];`;

await s.run(async () => {
  const B = (await s.connect())!;
  await tech.call("focus_window", { window: B, after: "none" });

  // 1. Learned
  const r1 = await tech.call("exec", { script: SUBMIT, args: { name: "Ann" }, learn: { name: "bench-submit", description: "Submit the bench form with a name and read what it says" } });
  say(prog(r1)?.state === "done" && /Kept as the skill "bench-submit"/.test(r1.text ?? ""), `kept as a skill: ${brief(r1.text ?? r1.error)}`);

  // 2. Found by the window in front, and by words
  const byWindow = (await tech.call("skills", { window: B })).data as Found;
  const top = byWindow.skills?.[0];
  say(top?.name === "bench-submit" && top.fits >= 0.8, `with the bench in front and no query, it comes first, fitting ${Math.round((top?.fits ?? 0) * 100)}%`);
  const byWords = (await tech.call("skills", { query: "submit a name in the bench form" })).data as Found;
  const none = await tech.call("skills", { query: "send an email" });
  say(byWords.skills?.[0]?.name === "bench-submit" && none.ok && !(none.data as Found | undefined)?.skills?.length, `found by what it is for, and not for something else: ${brief(none.text ?? none.error)}`);

  // 3. Again, with new args and no script
  const t = performance.now();
  const r3 = await tech.call("exec", { skill: "bench-submit", args: { name: "Zoe" } });
  say(prog(r3)?.state === "done" && prog(r3)?.result === "Submitted: Zoe", `run again in one call with new args, in ${Math.round(performance.now() - t)} ms: ${JSON.stringify(prog(r3)?.result ?? brief(r3.text ?? r3.error))}`);

  // 4. Missing args
  const r4 = await tech.call("exec", { skill: "bench-submit", args: {} });
  say(!r4.ok && /missing name/.test(r4.error ?? r4.text ?? ""), `refused without its args: ${brief(r4.error ?? r4.text)}`);

  // 5. A skill that no longer fits: stuck, then learned again
  await tech.call("exec", { script: SUBMIT.replace("'Submit'", "args.button"), args: { name: "Bo", button: "Submit" }, learn: { name: "bench-old", description: "Submit the bench form, the old way" } });
  const r5 = await tech.call("exec", { skill: "bench-old", args: { name: "Cy", button: "Send now", timeoutSec: 0 } });
  const p5 = prog(r5);
  await tech.call("resume", { id: p5?.id, abort: true });
  const after5 = ((await tech.call("skills", { query: "old way" })).data as Found).skills?.[0];
  say(p5?.state === "stuck" && after5?.stuck === 1 && /at line 3: click failed/.test(after5.problem ?? ""), `a run that no longer fits got stuck, and the skill says where: ${after5?.problem}`);
  const r5b = await tech.call("exec", { script: SUBMIT, args: { name: "Cy" }, learn: { name: "bench-old", description: "Submit the bench form, the old way" } });
  const again = ((await tech.call("skills", { query: "old way" })).data as Found).skills?.[0];
  const r5c = await tech.call("exec", { skill: "bench-old", args: { name: "Di" } });
  say(/in place of the one of that name/.test(r5b.text ?? "") && again?.runs === 1 && again.problem === null && prog(r5c)?.result === "Submitted: Di", `learned again in its place (record kept: ${again?.runs} run), and the new one works: ${JSON.stringify(prog(r5c)?.result)}`);

  // 6. show, forget
  const shown6 = await tech.call("skills", { show: "bench-submit" });
  const gone = await tech.call("skills", { forget: "bench-old" });
  const left = ((await tech.call("skills", { query: "bench" })).data as Found).skills?.map((x) => x.name);
  say(/fill\(\{ Name: args.name \}/.test(shown6.text ?? "") && /Takes args: name/.test(shown6.text ?? "") && gone.ok && JSON.stringify(left) === '["bench-submit"]', `show gives its script and args; forget drops one (left: ${JSON.stringify(left)})`);

  // 7. Kept by itself
  const r7 = await tech.call("exec", { script: `fill({ Name: args.name }); click('Agree'); click('Submit'); return 'ok';`, args: { name: "Ann" }, waitSec: 30 });
  const kept = /Kept by itself as the skill "([^"]+)"/.exec(prog(r7)?.learned ?? "")?.[1];
  const found = await tech.call("skills", { query: "Agree Submit" });
  say(prog(r7)?.state === "done" && !!kept && found.ok && (found.text ?? "").includes(kept), `a program that worked was kept by itself as "${kept}", and found by what it did: ${brief(found.text, 2)}`);

  // 8. Checked against the window in front
  const n = await tech.call("open_app", { target: "notepad", after: "none" });
  const N = (n.data as { handle?: number } | undefined)?.handle;
  if (N) s.after(() => tech.call("focus_window", { window: N, action: "close", after: "none" }));
  await tech.call("wait_for", { stable: true, timeoutSec: 2, after: "none" });
  const r8 = await tech.call("exec", { skill: kept, args: { name: "Bob" }, waitSec: 30 });
  say(!r8.ok && (r8.data as { misfit?: boolean } | undefined)?.misfit === true, `with Notepad in front, the skill was not run: ${brief(r8.error, 2)}`);
  await tech.call("focus_window", { window: B, after: "none" });
  const r8b = await tech.call("exec", { skill: kept, args: { name: "Bob" }, waitSec: 30 });
  say(prog(r8b)?.state === "done", `with the bench window in front, it ran: ${prog(r8b)?.state}`);

  // 9. Imported, put right as it is used, exported
  const stale = { name: "submit a name", description: "Types a name into the bench window and submits it", script: `fill({ Name: args.name });\nclick('Send it');\nreturn 'sent';`, params: ["name"], apps: [], marks: [], runs: 0, done: 0, stuck: 0, learned: 0 };
  const imp = await tech.call("skills", { import: [stale] });
  say(imp.ok, `imported a skill: ${brief(imp.text ?? imp.error, 2)}`);
  let r = await tech.call("exec", { skill: "submit a name", args: { name: "Cy" }, waitSec: 30 });
  const id = prog(r)?.id;
  say(prog(r)?.state === "stuck" && prog(r)?.line === 2, `its renamed button made it stuck at line ${prog(r)?.line}`);
  r = await tech.call("resume", { id, instead: { tool: "click", args: { target: "Submit", window: "Asctrl Bench" } }, waitSec: 30 });
  const shown9 = await tech.call("skills", { show: "submit a name" });
  say(prog(r)?.state === "done" && /click\('Submit'\)/.test(shown9.text ?? "") && /changed to match/.test(prog(r)?.learned ?? ""), `put right with instead, it ended well and its script changed to match: ${brief(prog(r)?.learned ?? "", 2)}`);
  const ex = await tech.call("skills", { export: "submit a name" });
  say(ex.ok && /"script":"fill\(\{ Name: args.name \}\);\\nclick\('Submit'\)/.test(ex.text ?? ""), `export gives it as JSON, changed: ${(ex.text ?? ex.error ?? "").slice(0, 120)}`);
});
