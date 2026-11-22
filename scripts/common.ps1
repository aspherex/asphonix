# Shared helpers for the perf harness: private working set (GetProcessMemoryInfo ->
# PrivateWorkingSetSize) summed over the process tree rooted at OUR pid. Processes are only ever stopped by the
# exact PID this harness started.
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class AsphonixPerf {
  [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
  struct PROCESSENTRY32 {
    public uint dwSize; public uint cntUsage; public uint th32ProcessID; public IntPtr th32DefaultHeapID;
    public uint th32ModuleID; public uint cntThreads; public uint th32ParentProcessID; public int pcPriClassBase;
    public uint dwFlags; [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)] public string szExeFile;
  }
  [StructLayout(LayoutKind.Sequential)]
  struct PMC_EX2 {
    public uint cb; public uint PageFaultCount; public UIntPtr PeakWorkingSetSize; public UIntPtr WorkingSetSize;
    public UIntPtr QuotaPeakPagedPoolUsage; public UIntPtr QuotaPagedPoolUsage; public UIntPtr QuotaPeakNonPagedPoolUsage;
    public UIntPtr QuotaNonPagedPoolUsage; public UIntPtr PagefileUsage; public UIntPtr PeakPagefileUsage;
    public UIntPtr PrivateUsage; public UIntPtr PrivateWorkingSetSize; public ulong SharedCommitUsage;
  }
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr CreateToolhelp32Snapshot(uint f, uint pid);
  [DllImport("kernel32.dll", CharSet = CharSet.Unicode, EntryPoint = "Process32FirstW")] static extern bool Process32First(IntPtr h, ref PROCESSENTRY32 e);
  [DllImport("kernel32.dll", CharSet = CharSet.Unicode, EntryPoint = "Process32NextW")] static extern bool Process32Next(IntPtr h, ref PROCESSENTRY32 e);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
  [DllImport("kernel32.dll")] static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
  [DllImport("psapi.dll")] static extern bool GetProcessMemoryInfo(IntPtr h, ref PMC_EX2 c, uint cb);
  delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr hWnd);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr hWnd, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int cmd);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hWnd, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
  [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr hwnd, int attr, out RECT r, int size);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, UIntPtr extra);
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr hWnd, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  public static void MakeTopmost(IntPtr h) { SetWindowPos(h, new IntPtr(-1), 0, 0, 0, 0, 0x0001 | 0x0002 | 0x0010); }
  public static IntPtr LastHwnd;

  public static RECT FrameRect(IntPtr h) { RECT r; DwmGetWindowAttribute(h, 9, out r, Marshal.SizeOf(typeof(RECT))); return r; }

  public class Proc { public uint Pid; public uint Parent; public string Name; public ulong WorkingSet; public ulong PrivateWs; public ulong PrivateCommit; }

  public static List<Proc> Tree(uint root) {
    var all = new List<PROCESSENTRY32>();
    IntPtr snap = CreateToolhelp32Snapshot(2, 0);
    var e = new PROCESSENTRY32(); e.dwSize = (uint)Marshal.SizeOf(typeof(PROCESSENTRY32));
    if (Process32First(snap, ref e)) { do { all.Add(e); } while (Process32Next(snap, ref e)); }
    CloseHandle(snap);
    var ids = new HashSet<uint> { root };
    bool changed = true;
    while (changed) {
      changed = false;
      foreach (var p in all) if (ids.Contains(p.th32ParentProcessID) && !ids.Contains(p.th32ProcessID) && p.th32ProcessID != 0) { ids.Add(p.th32ProcessID); changed = true; }
    }
    var res = new List<Proc>();
    foreach (var p in all) {
      if (!ids.Contains(p.th32ProcessID)) continue;
      var r = new Proc { Pid = p.th32ProcessID, Parent = p.th32ParentProcessID, Name = p.szExeFile };
      IntPtr h = OpenProcess(0x1000, false, p.th32ProcessID);
      if (h != IntPtr.Zero) {
        var c = new PMC_EX2(); c.cb = (uint)Marshal.SizeOf(typeof(PMC_EX2));
        if (GetProcessMemoryInfo(h, ref c, c.cb)) { r.WorkingSet = (ulong)c.WorkingSetSize; r.PrivateWs = (ulong)c.PrivateWorkingSetSize; r.PrivateCommit = (ulong)c.PrivateUsage; }
        CloseHandle(h);
      }
      res.Add(r);
    }
    return res;
  }

  public static string VisibleWindowTitle(uint pid) {
    string found = null;
    EnumWindows((hWnd, l) => {
      uint wp; GetWindowThreadProcessId(hWnd, out wp);
      if (wp == pid && IsWindowVisible(hWnd)) {
        var sb = new StringBuilder(256); GetWindowText(hWnd, sb, 256);
        if (sb.Length > 0) { found = sb.ToString(); LastHwnd = hWnd; return false; }
      }
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
'@
[void][AsphonixPerf]::SetProcessDPIAware()

function Sum($procs, $field) { $s = [uint64]0; foreach ($p in $procs) { $s += [uint64]$p.$field }; $s }
function MB($b) { [math]::Round($b / 1MB, 1) }

function Start-Asphonix([string]$Exe, [hashtable]$EnvVars = @{}, [string]$Log = $null) {
    foreach ($k in $EnvVars.Keys) { Set-Item -Path "Env:$k" -Value $EnvVars[$k] | Out-Null }
    if ($Log) { Remove-Item $Log -ErrorAction SilentlyContinue; $env:ASPHONIX_LOG = $Log }
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $p = Start-Process -FilePath $Exe -PassThru
    foreach ($k in $EnvVars.Keys) { Remove-Item -Path "Env:$k" -ErrorAction SilentlyContinue }
    return @{ Proc = $p; Sw = $sw }
}

# Wait until our PID owns a visible titled window; returns elapsed ms (or $null)
function Wait-Visible($h, [int]$MaxMs = 20000) {
    while ($h.Sw.ElapsedMilliseconds -lt $MaxMs) {
        $t = [AsphonixPerf]::VisibleWindowTitle([uint32]$h.Proc.Id)
        if ($t) { return $h.Sw.ElapsedMilliseconds }
        if ($h.Proc.HasExited) { return $null }
        Start-Sleep -Milliseconds 2
    }
    return $null
}

function Stop-Asphonix($h) {
    # exact PID only
    if ($h -and $h.Proc -and -not $h.Proc.HasExited) { Stop-Process -Id $h.Proc.Id -Force -ErrorAction SilentlyContinue }
}

function Save-WindowShot([IntPtr]$Hwnd, [string]$Path) {
    Add-Type -AssemblyName System.Drawing
    [AsphonixPerf]::MakeTopmost($Hwnd); Start-Sleep -Milliseconds 300
    $r = [AsphonixPerf]::FrameRect($Hwnd)
    $w = $r.Right - $r.Left; $h = $r.Bottom - $r.Top
    $bmp = New-Object System.Drawing.Bitmap $w, $h
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.Left, $r.Top, 0, 0, (New-Object System.Drawing.Size $w, $h))
    $g.Dispose(); $bmp.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
    "$w x $h -> $Path"
}
