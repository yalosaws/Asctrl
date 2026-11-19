# Recover fixtures recorded by interrupted test runners in this checkout.
[CmdletBinding(SupportsShouldProcess)]
param()
$journal = Join-Path $PSScriptRoot 'out\owners'
if (!(Test-Path -LiteralPath $journal -PathType Container)) { return }
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
foreach ($file in Get-ChildItem -LiteralPath $journal -File -Filter '*.json') {
    $id = [guid]::Empty
    if (![guid]::TryParseExact($file.BaseName, 'D', [ref]$id)) { continue }
    $item = Get-Content -LiteralPath $file.FullName -Raw | ConvertFrom-Json
    if (Get-Process -Id $item.owner -ErrorAction SilentlyContinue) { continue }
    if ($item.pid) {
        $child = Get-Process -Id $item.pid -ErrorAction SilentlyContinue
        if ($child) {
            try {
                $null = $child.Handle
                $created = ([DateTimeOffset]$child.StartTime).ToUnixTimeMilliseconds()
                $parent = (Get-CimInstance Win32_Process -Filter "ProcessId=$($child.Id)").ParentProcessId
                if ($parent -eq $item.owner -and $created -ge $item.before -and $created -le $item.after) {
                    if (!$PSCmdlet.ShouldProcess($child.ProcessName, 'Stop orphaned test child')) { continue }
                    $child.Kill()
                    $child.WaitForExit()
                }
            } finally { $child.Dispose() }
        }
    } elseif ($item.directory -and (Test-Path -LiteralPath $item.directory -PathType Container)) {
        $dir = Get-Item -LiteralPath $item.directory
        if ($dir.Parent.FullName -ine $tempRoot -or $dir.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) { continue }
        $marker = Join-Path $dir.FullName '.asctrl-test-owner'
        if (!(Test-Path -LiteralPath $marker -PathType Leaf) -or (Get-Content -LiteralPath $marker -Raw) -ne $file.BaseName) { continue }
        if (!$PSCmdlet.ShouldProcess($dir.FullName, 'Remove owned test profile')) { continue }
        Remove-Item -LiteralPath $dir.FullName -Recurse -Force -ErrorAction Stop
    }
    if ($PSCmdlet.ShouldProcess($file.FullName, 'Retire test record')) { Remove-Item -LiteralPath $file.FullName }
}
