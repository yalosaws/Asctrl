# Closes what desktop tests leave behind when they are cut short (stopped mid-run, so their own clean-up never ran):
# their drivers, bench windows, mouse-moving helpers, browsers with a test profile and copies of VS Code, and the
# temporary profiles. Only processes started by the tests are touched (matched by what only tests put on their
# command lines); the person's own browsers, editors and driver are left alone.
# Usage: powershell -ExecutionPolicy Bypass -File test/cleanup.ps1
$stopped = @()
foreach ($p in Get-CimInstance Win32_Process) {
    $c = [string]$p.CommandLine
    $test =
        ($p.Name -eq 'node.exe' -and $c -match 'test[\\/][\w\\/]+\.ts') -or
        ($p.Name -in 'msedge.exe', 'chrome.exe' -and $c -match 'asctrl-(ext|probe)-profile|occl-') -or
        ($p.Name -eq 'asctrl-agent.exe' -and $c -match 'ws://127\.0\.0\.1:1') -or
        ($p.Name -eq 'powershell.exe' -and $c -match 'bench-app\.ps1|jiggle\.ps1|answer\.ps1|popup\.ps1') -or
        ($p.Name -eq 'Code.exe' -and $c -match 'asctrl-electron')
    if ($test) {
        Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue
        $stopped += $p.Name
    }
}
Start-Sleep -Milliseconds 800
foreach ($pattern in 'asctrl-ext-profile-*', 'asctrl-electron-*', 'asctrl-probe-profile-*', 'occl-*') {
    Get-ChildItem $env:TEMP -Directory -Filter $pattern -ErrorAction SilentlyContinue | ForEach-Object { Remove-Item $_.FullName -Recurse -Force -ErrorAction SilentlyContinue }
}
if ($stopped) { "Stopped: " + (($stopped | Group-Object | ForEach-Object { "$($_.Name) x$($_.Count)" }) -join ', ') } else { "Nothing was left" }
