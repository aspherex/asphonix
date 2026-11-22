param([uint32]$ProcId, [int]$Sec = 8)
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices; using System.Text;
public static class Thr {
  [DllImport("kernel32.dll")] public static extern IntPtr OpenThread(uint a, bool i, uint tid);
  [DllImport("kernel32.dll")] public static extern int GetThreadDescription(IntPtr h, out IntPtr name);
  [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
  [DllImport("kernel32.dll")] public static extern bool GetThreadTimes(IntPtr h, out long c, out long e, out long k, out long u);
  public static string Name(uint tid) { IntPtr h = OpenThread(0x0400 | 0x0040, false, tid); if (h == IntPtr.Zero) return "?"; IntPtr p; string s = ""; if (GetThreadDescription(h, out p) >= 0 && p != IntPtr.Zero) s = Marshal.PtrToStringUni(p); CloseHandle(h); return s; }
  public static double Cpu(uint tid) { IntPtr h = OpenThread(0x0400 | 0x0040, false, tid); if (h == IntPtr.Zero) return 0; long c, e, k, u; GetThreadTimes(h, out c, out e, out k, out u); CloseHandle(h); return (k + u) / 1e7; }
}
'@
function Take { $h = @{}; foreach ($t in (Get-Process -Id $ProcId).Threads) { $h[$t.Id] = [Thr]::Cpu([uint32]$t.Id) }; $h }
$a = Take; Start-Sleep -Seconds $Sec; $b = Take
$b.Keys | ForEach-Object { $d = $b[$_] - $(if ($a.ContainsKey($_)) { $a[$_] } else { 0 }); [pscustomobject]@{ tid = $_; name = [Thr]::Name([uint32]$_); pct = [math]::Round($d / $Sec * 100, 2) } } | Where-Object { $_.pct -gt 0.01 } | Sort-Object pct -Descending | Format-Table -AutoSize | Out-String
