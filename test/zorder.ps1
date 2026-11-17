# How long the overlay is covered: samples the window order every millisecond for -Seconds and counts the
# moments a window that overlaps it (by default "Asctrl Bench", a topmost window) is above Asctrl's overlay, which
# hides the overlay's pointers and frame there. Writes one line: covered episodes, total ms, longest ms.
# Usage: powershell -File test/zorder.ps1 -Seconds 25 [-Title "Asctrl Bench"] [-Out file]
param([int]$Seconds = 25, [string]$Title = "Asctrl Bench", [string]$Out = "")

Add-Type @'
using System;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
public static class ZOrder {
  [DllImport("user32.dll")] static extern IntPtr GetTopWindow(IntPtr h);
  [DllImport("user32.dll")] static extern IntPtr GetWindow(IntPtr h, uint cmd);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("winmm.dll")] static extern uint timeBeginPeriod(uint p);
  static string Title(IntPtr h) { var s = new StringBuilder(256); GetWindowText(h, s, 256); return s.ToString(); }
  static bool IsOverlay(IntPtr h, uint[] pids) {
    uint pid; GetWindowThreadProcessId(h, out pid);
    if (Array.IndexOf(pids, pid) < 0) return false;
    var c = new StringBuilder(64); GetClassName(h, c, 64);
    return c.ToString() == "Tauri Window" && IsWindowVisible(h);
  }
  public static string Run(int seconds, string title) {
    timeBeginPeriod(1);
    var sw = Stopwatch.StartNew();
    int episodes = 0, samples = 0, seen = 0; double total = 0, longest = 0, since = -1;
    var list = new StringBuilder();
    uint[] pids = new uint[0]; long pidsAt = -1000;
    while (sw.ElapsedMilliseconds < seconds * 1000L) {
      if (sw.ElapsedMilliseconds - pidsAt > 300) { pids = Array.ConvertAll(Process.GetProcessesByName("asctrl-agent"), p => (uint)p.Id); pidsAt = sw.ElapsedMilliseconds; }
      // Front to back: is the bench met before the overlay?
      bool above = false, both = false; IntPtr overlay = IntPtr.Zero;
      for (IntPtr h = GetTopWindow(IntPtr.Zero); h != IntPtr.Zero; h = GetWindow(h, 2)) {
        if (!IsWindowVisible(h)) continue;
        if (overlay == IntPtr.Zero && IsOverlay(h, pids)) { overlay = h; break; }
        if (Title(h) == title) above = true;
      }
      if (overlay != IntPtr.Zero) {
        // Still need to know the bench exists at all (below the overlay)
        both = above;
        if (!above) for (IntPtr h = GetWindow(overlay, 2); h != IntPtr.Zero; h = GetWindow(h, 2)) if (IsWindowVisible(h) && Title(h) == title) { both = true; break; }
      }
      double now = sw.Elapsed.TotalMilliseconds;
      if (both) { samples++; if (above) seen++; }
      above = above && overlay != IntPtr.Zero;
      if (above && since < 0) { since = now; episodes++; }
      if (!above && since >= 0) { double d = now - since; total += d; if (d > longest) longest = d; list.AppendFormat(" [{0:0}+{1:0}]", since, d); since = -1; }
      System.Threading.Thread.Sleep(1);
    }
    if (since >= 0) { double d = sw.Elapsed.TotalMilliseconds - since; total += d; if (d > longest) longest = d; }
    return String.Format("covered {0} times, {1:0} ms in all, longest {2:0} ms ({3} of {4} samples with both on screen); at ms+ms:{5}", episodes, total, longest, seen, samples, list);
  }
}
'@
$r = [ZOrder]::Run($Seconds, $Title)
if ($Out) { Set-Content -Path $Out -Value $r -Encoding utf8 } else { $r }
