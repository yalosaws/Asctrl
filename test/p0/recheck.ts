// Check a finished P0 run again, without the model: for the tasks whose check itself failed (it could not read
// what the task left, e.g. a file Word still held open), run the check once more on what the task left behind,
// which is still in the run's folder. Writes <results>.rechecked.jsonl and prints the summary.
// Usage: node test/p0/recheck.ts test/out/p0-<stamp>.jsonl

import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { TASKS, type Area } from "./tasks.ts";

const file = process.argv[2];
if (!file || !existsSync(file)) {
  console.error("usage: node test/p0/recheck.ts test/out/p0-<stamp>.jsonl");
  process.exit(2);
}
const stamp = /p0-(.+)\.jsonl$/.exec(path.basename(file))?.[1];
const base = path.join(tmpdir(), `asctrl-p0-${stamp}`);
type Row = { id: string; area: Area; ok: boolean; why: string; answer: string; ms: number; calls: number; cost: number; tokens: Record<string, number>; recheck?: string };
const rows = readFileSync(file, "utf8").split(/\r?\n/).filter(Boolean).map((l) => JSON.parse(l) as Row);
for (const r of rows) {
  // Only checks that broke on their own (the checker's command failed), not tasks the model got wrong
  if (r.ok || !/^Command failed/.test(r.why)) continue;
  const t = TASKS.find((x) => x.id === r.id);
  const work = path.join(base, r.id);
  if (!t || !existsSync(work)) continue;
  let why: string;
  try {
    const v = t.check({ work, site: undefined as never }, r.answer);
    why = v === true ? "" : v;
  } catch (e) {
    why = (e as Error).message;
  }
  r.recheck = `was: ${r.why.slice(0, 120)}`;
  r.ok = !why;
  r.why = why;
  console.log(`${r.ok ? "✓" : "✗"} ${r.id.padEnd(20)} rechecked${r.ok ? "" : `: ${why.slice(0, 100)}`}`);
}
writeFileSync(file.replace(/\.jsonl$/, ".rechecked.jsonl"), rows.map((r) => JSON.stringify(r)).join("\n") + "\n");
const mean = (xs: number[]) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : 0);
const tok = (r: Row) => Object.values(r.tokens).reduce((a, b) => a + b, 0);
console.log(`\n${"area".padEnd(9)} ${"success".padStart(9)} ${"time".padStart(7)} ${"calls".padStart(6)} ${"tokens".padStart(8)} ${"cost".padStart(7)}   (means per task)`);
for (const a of ["browser", "settings", "office", "vscode", "all"]) {
  const rs = a === "all" ? rows : rows.filter((r) => r.area === a);
  if (!rs.length) continue;
  console.log(`${a.padEnd(9)} ${`${rs.filter((r) => r.ok).length}/${rs.length}`.padStart(9)} ${`${(mean(rs.map((r) => r.ms)) / 1000).toFixed(0)} s`.padStart(7)} ${mean(rs.map((r) => r.calls)).toFixed(1).padStart(6)} ${`${(mean(rs.map(tok)) / 1000).toFixed(0)}k`.padStart(8)} ${`$${mean(rs.map((r) => r.cost)).toFixed(2)}`.padStart(7)}`);
}
console.log(`total: ${(rows.reduce((a, r) => a + r.ms, 0) / 60000).toFixed(1)} min, ${rows.reduce((a, r) => a + r.calls, 0)} model calls, ${(rows.reduce((a, r) => a + tok(r), 0) / 1e6).toFixed(2)}M tokens, $${rows.reduce((a, r) => a + r.cost, 0).toFixed(2)}`);
