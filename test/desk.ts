// What every desktop suite starts with: a driver of its own on the local link (its own address file and skills
// file, so a driver the person already runs, and the skills they keep, are left alone), the bench window when
// asked for, a controller, and the ✓/✗ tally. `run` runs the suite's checks and then closes all of it, whatever
// happened.

import { driverExecutable, owned } from "./fixtures.ts";
import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { TechClient } from "../src/console/tech-client.ts";
import type { ObserveData, ProgramData } from "../src/shared/protocol.ts";

export const root = path.resolve(import.meta.dirname, "..");
export const exe = driverExecutable(root);
export const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
/** The first line of a reply */
export const first = (s?: string | null) => (s ?? "").split("\n")[0];
/** The first lines of a reply, on one line */
export const brief = (s?: string | null, lines = 3) => (s ?? "").split("\n").slice(0, lines).join(" | ");
/** A program's state, from exec's or resume's reply */
export const prog = (r: { data?: unknown }) => (r.data as ProgramData | undefined)?.program;
/** Where the real mouse pointer is, "x,y" */
export const cursor = () =>
  execFileSync("powershell.exe", ["-NoProfile", "-Command", "Add-Type -AssemblyName System.Windows.Forms; $p = [System.Windows.Forms.Cursor]::Position; \"$($p.X),$($p.Y)\""]).toString().trim();

/** The bench window (test/bench-app.ps1), started with no console of its own */
export const startBench = (args: string[] = []) =>
  owned(() => spawn("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(root, "test/bench-app.ps1"), ...args], { stdio: "ignore", windowsHide: true }));

export interface SuiteOptions {
  /** Added to the driver's environment */
  env?: Record<string, string>;
  /** Start the bench window (true), with these arguments (e.g. ["-NotOnTop"]), or not (false, the default) */
  bench?: boolean | string[];
  /** Start a driver of the suite's own (default true) */
  driver?: boolean;
}

export class Suite {
  readonly tech: TechClient;
  readonly localFile: string;
  readonly skillsFile: string;
  failures = 0;
  private readonly agent?: ChildProcess;
  private readonly bench?: ChildProcess;
  private readonly cleanups: (() => unknown)[] = [];

  constructor(name: string, o: SuiteOptions = {}) {
    const tag = name.toLowerCase().replace(/\W+/g, "-");
    this.localFile = path.join(tmpdir(), `asctrl-${tag}-local-${process.pid}.json`);
    this.skillsFile = path.join(tmpdir(), `asctrl-${tag}-skills-${process.pid}.json`);
    process.env.ASCTRL_LOCAL_FILE = this.localFile;
    process.env.ASCTRL_SKILLS_FILE ??= this.skillsFile;
    if (o.driver !== false) {
      this.agent = owned(() => spawn(exe, ["--relay", "ws://127.0.0.1:1"], { stdio: ["ignore", "ignore", "inherit"], env: { ...process.env, ...o.env } }));
    }
    if (o.bench) this.bench = startBench(Array.isArray(o.bench) ? o.bench : []);
    this.tech = new TechClient("ws://127.0.0.1:1", `${name}Test`);
  }

  /** One check: counted, and printed with ✓ or ✗ */
  say = (ok: boolean, line: string) => {
    if (!ok) this.failures++;
    console.log(`${ok ? "✓" : "✗"} ${line}`);
    return ok;
  };

  /** Something to undo at the end (close a window the suite opened, remove a file), before the driver goes */
  after(f: () => unknown) {
    this.cleanups.push(f);
  }

  /** Connect to the suite's driver and wait for the bench window, if there is one; its handle */
  async connect(): Promise<number | undefined> {
    await this.tech.connectLocalDriver({ launch: false, waitMs: 30_000 });
    if (!this.bench) return undefined;
    await this.tech.call("wait_for", { appears: "Asctrl Bench", timeoutSec: 15 });
    return this.windowNamed(/^Asctrl Bench$/);
  }

  /** The handle of the first window whose title matches */
  async windowNamed(title: RegExp): Promise<number | undefined> {
    const wins = ((await this.tech.call("observe", { max: 1 })).data as ObserveData | undefined)?.windows ?? [];
    return wins.find((x) => title.test(x.title))?.handle;
  }

  /** Run the checks, then close everything; the process exits 1 when any check failed */
  async run(body: () => Promise<void>): Promise<void> {
    try {
      await body();
    } catch (e) {
      console.error(e);
      this.failures++;
    } finally {
      for (const f of this.cleanups.reverse()) {
        try {
          await f();
        } catch {}
      }
      console.log(this.failures ? `\n${this.failures} check(s) failed` : "\nAll checks passed");
      process.exitCode = this.failures ? 1 : 0;
      this.tech.disconnect();
      this.bench?.kill();
      this.agent?.kill();
      rmSync(this.localFile, { force: true });
      rmSync(this.skillsFile, { force: true });
      setTimeout(() => process.exit(), 300);
    }
  }
}
