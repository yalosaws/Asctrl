import { spawn } from "node:child_process";

export interface PsResult {
  stdout: string;
  stderr: string;
  exitCode: number | null;
  timedOut: boolean;
}

/** 运行一段 PowerShell 脚本，输出统一用 UTF-8（避免中文乱码） */
export function runPowerShell(script: string, timeoutMs = 30_000): Promise<PsResult> {
  const full =
    "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8;" +
    "$ProgressPreference='SilentlyContinue';\n" +
    script;
  // -EncodedCommand 需要 UTF-16LE 的 base64，可以避开所有引号转义问题
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
