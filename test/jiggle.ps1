# Someone else using the mouse, for test/p6.ts: moves the pointer a pixel back and forth with SendInput (as a
# person's mouse or a remote desktop tool would, without Asctrl's mark) for -Seconds, starting after -After.
param([double]$Seconds = 2, [double]$After = 0, [string]$Signal = '')
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class Jiggle {
  [StructLayout(LayoutKind.Sequential)] struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] struct INPUT { public uint type; public MOUSEINPUT mi; }
  [DllImport("user32.dll")] static extern uint SendInput(uint n, INPUT[] inputs, int size);
  public static void Move(int dx) {
    var i = new INPUT[1];
    i[0].type = 0; i[0].mi.dx = dx; i[0].mi.dwFlags = 1;
    SendInput(1, i, Marshal.SizeOf(typeof(INPUT)));
  }
}
'@
Start-Sleep -Milliseconds ([int]($After * 1000))
# Tells the test the moving has begun (compiling the above takes a moment); a file, since what this writes to
# a pipe only arrives when it exits
if ($Signal) { Set-Content -Path $Signal -Value go }
$end = (Get-Date).AddSeconds($Seconds)
$d = 1
while ((Get-Date) -lt $end) {
  [Jiggle]::Move($d)
  $d = -$d
  Start-Sleep -Milliseconds 50
}
