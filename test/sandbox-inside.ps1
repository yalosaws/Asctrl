# Runs inside Windows Sandbox (started by test/sandbox.ts): the repository is at C:\repo (read-only), Node at
# C:\node (read-only), and C:\out is the only folder that goes back to the host. Shuts the sandbox down at the end.
param([string]$Suite = "test/e2e.ts", [string]$SuiteArgs = "")

$ErrorActionPreference = "Continue"
$log = "C:\out\log.txt"
$code = 1
try {
    # A writable copy for the suite's own output; the driver's build stays where it is
    robocopy C:\repo C:\work /E /NFL /NDL /NJH /NJS /NP /XD .git target "test\out" | Out-Null
    $env:PATH = "C:\node;$env:PATH"
    $env:ASCTRL_TEST_DRIVER = "C:\repo\agent-app\src-tauri\target\release\asctrl-agent.exe"
    Set-Location C:\work
    $suiteArgs = if ($SuiteArgs) { $SuiteArgs -split " " } else { @() }
    & node $Suite @suiteArgs *> $log
    $code = $LASTEXITCODE
    if (Test-Path C:\work\test\out) { Copy-Item C:\work\test\out\* C:\out -Recurse -Force -ErrorAction SilentlyContinue }
} catch {
    "sandbox-inside: $_" | Out-File $log -Append
} finally {
    Set-Content -Path C:\out\done.json -Value ("{""code"": $code}") -Encoding ascii
    shutdown.exe /s /t 5
}
