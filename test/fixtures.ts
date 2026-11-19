import { randomUUID } from "node:crypto";
import type { ChildProcess } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";

// A live runner cleans up its own fixtures. The journal is for interrupted runs.
export const driverExecutable = (root: string) => process.env.ASCTRL_TEST_DRIVER ?? path.join(root, "agent-app/src-tauri/target/release/asctrl-agent.exe");

const journal = path.join(import.meta.dirname, "out", "owners");
function record(id: string, entry: object) {
  mkdirSync(journal, { recursive: true });
  const file = path.join(journal, id + ".json");
  writeFileSync(file, JSON.stringify({ owner: process.pid, ...entry }), { flag: "wx" });
  return () => rmSync(file, { force: true });
}

export function owned<T extends ChildProcess>(launch: () => T): T {
  const before = Date.now();
  const child = launch();
  if (child.pid) {
    const forget = record(randomUUID(), { pid: child.pid, before, after: Date.now() });
    child.once("exit", forget);
  }
  return child;
}

export function fixtureDirectory(prefix: string): string {
  const directory = mkdtempSync(prefix);
  const id = randomUUID();
  writeFileSync(path.join(directory, ".asctrl-test-owner"), id, { flag: "wx" });
  const forget = record(id, { directory });
  process.once("exit", () => { if (!existsSync(directory)) forget(); });
  return directory;
}
