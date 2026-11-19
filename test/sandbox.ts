// Run a desktop test suite in Windows Sandbox, so it never takes the person's mouse, keyboard or windows.
//
//   node test/sandbox.ts [suite] [args...]      (default suite: test/e2e.ts)
//
// The sandbox is a throwaway Windows desktop. It sees this repository and Node read-only and writes only to
// test/out/sandbox-<stamp>/; it has no network (the tests use a relay on its own loopback). The suite runs
// against the driver built here (agent-app/src-tauri/target/release/asctrl-agent.exe), so build it first.
// The sandbox closes itself when the suite ends; its log and exit code come back here.
//
// Windows Sandbox is an optional Windows feature (Pro, Enterprise, Education). It is turned on once, by the
// person, in an administrator PowerShell, followed by a restart:
//   Enable-WindowsOptionalFeature -Online -FeatureName Containers-DisposableClientVM -All

import { spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const sandboxExe = path.join(process.env.WINDIR ?? "C:\\Windows", "System32", "WindowsSandbox.exe");
if (!existsSync(sandboxExe)) {
  console.error("Windows Sandbox is not turned on. In an administrator PowerShell, then restart:\n  Enable-WindowsOptionalFeature -Online -FeatureName Containers-DisposableClientVM -All");
  process.exit(2);
}
const driver = path.join(root, "agent-app/src-tauri/target/release/asctrl-agent.exe");
if (!existsSync(driver)) {
  console.error(`Build the driver first (cd agent-app && npx tauri build --no-bundle): ${driver} is missing`);
  process.exit(2);
}

const [suite = "test/e2e.ts", ...rest] = process.argv.slice(2);
if (!existsSync(path.join(root, suite))) {
  console.error(`No such suite: ${suite}`);
  process.exit(2);
}
const stamp = new Date().toISOString().replace(/[-:]/g, "").replace(/\..*/, "").replace("T", "-");
const out = path.join(root, "test", "out", `sandbox-${stamp}`);
mkdirSync(out, { recursive: true });

const xml = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const folder = (host: string, inside: string, readOnly: boolean) =>
  `    <MappedFolder><HostFolder>${xml(host)}</HostFolder><SandboxFolder>${xml(inside)}</SandboxFolder><ReadOnly>${readOnly}</ReadOnly></MappedFolder>`;
const command = [
  "powershell.exe -NoProfile -ExecutionPolicy Bypass -File C:\\repo\\test\\sandbox-inside.ps1",
  `-Suite "${suite}"`,
  rest.length ? `-SuiteArgs "${rest.join(" ")}"` : "",
].filter(Boolean).join(" ");
const wsb = `<Configuration>
  <Networking>Disable</Networking>
  <MemoryInMB>3072</MemoryInMB>
  <ClipboardRedirection>Disable</ClipboardRedirection>
  <AudioInput>Disable</AudioInput>
  <VideoInput>Disable</VideoInput>
  <PrinterRedirection>Disable</PrinterRedirection>
  <MappedFolders>
${folder(root, "C:\\repo", true)}
${folder(path.dirname(process.execPath), "C:\\node", true)}
${folder(out, "C:\\out", false)}
  </MappedFolders>
  <LogonCommand><Command>${xml(command)}</Command></LogonCommand>
</Configuration>
`;
const config = path.join(out, "run.wsb");
writeFileSync(config, wsb);
console.log(`Starting Windows Sandbox for ${suite}; results in ${out}`);
spawn(sandboxExe, [config], { detached: true, stdio: "ignore" }).unref();

// The suite writes done.json last; the sandbox shuts itself down after that
const limitMin = Number(process.env.ASCTRL_SANDBOX_MINUTES ?? 45);
const deadline = Date.now() + limitMin * 60_000;
const done = path.join(out, "done.json");
while (!existsSync(done)) {
  if (Date.now() > deadline) {
    console.error(`No result after ${limitMin} min (see ${out}); close the sandbox window by hand.`);
    process.exit(1);
  }
  await new Promise((r) => setTimeout(r, 2000));
}
const log = path.join(out, "log.txt");
if (existsSync(log)) process.stdout.write(readFileSync(log, "utf8"));
const { code } = JSON.parse(readFileSync(done, "utf8")) as { code: number };
console.log(`\n${suite} in the sandbox exited with ${code}`);
process.exit(code);
