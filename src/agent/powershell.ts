import { spawn } from "node:child_process";

export interface PsResult {
  stdout: string;
  stderr: string;
  exitCode: number | null;
  timedOut: boolean;
}

/** Run a PowerShell script with UTF-8 output (so non-ASCII text isn't garbled) */
export function runPowerShell(script: string, timeoutMs = 30_000): Promise<PsResult> {
  const full =
    "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8;" +
    "$ProgressPreference='SilentlyContinue';\n" +
    script;
  // -EncodedCommand takes UTF-16LE base64, which sidesteps every quoting problem
  const encoded = Buffer.from(full, "utf16le").toString("base64");

  return new Promise((resolve) => {
    const child = spawn(
      "powershell.exe",
      ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", encoded],
      { windowsHide: true },
    );
    let stdout = "";
    let stderr = "";
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill();
    }, timeoutMs);

    child.stdout.on("data", (d) => (stdout += d.toString("utf8")));
    child.stderr.on("data", (d) => (stderr += d.toString("utf8")));
    child.on("close", (exitCode) => {
      clearTimeout(timer);
      resolve({ stdout, stderr, exitCode, timedOut });
    });
    child.on("error", (err) => {
      clearTimeout(timer);
      resolve({ stdout, stderr: String(err), exitCode: -1, timedOut });
    });
  });
}
