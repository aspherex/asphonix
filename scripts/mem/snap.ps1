# Memory snapshots of a scripted run (SONIX_SCRIPT=1): dumps the largest virtual-memory regions (vmm.ps1)
# at three points of the timeline (idle, after the scroll, end of Now Playing) into -OutDir.
# With -ProfileDir the app runs on a separate APPDATA/LOCALAPPDATA profile (sign in there once first).
# Usage: powershell -File scripts\mem\snap.ps1 -Exe target\release\sonix.exe [-OutDir $env:TEMP\sonix-mem] [-ProfileDir <dir>]
param(
  [Parameter(Mandatory = $true)][string]$Exe,
  [string]$OutDir = "$env:TEMP\sonix-mem",
  [string]$ProfileDir = ""
)
New-Item -ItemType Directory -Force $OutDir | Out-Null
$log = Join-Path $OutDir "snap.log"; Remove-Item $log -ErrorAction SilentlyContinue
if ($ProfileDir) { $env:APPDATA = Join-Path $ProfileDir "roaming"; $env:LOCALAPPDATA = Join-Path $ProfileDir "local" }
$env:SONIX_LOG = $log; $env:SONIX_SCRIPT = "1"
$p = Start-Process -FilePath (Resolve-Path $Exe).Path -PassThru
function Has([string]$pat) { (Test-Path $log) -and (Select-String -Path $log -Pattern $pat -Quiet) }
try {
  foreach ($ph in "idle_end", "post_scroll_idle", "np_end") {
    while (-not (Has "phase $ph")) { if ($p.HasExited) { throw "sonix exited early" }; Start-Sleep -Milliseconds 100 }
    Start-Sleep -Milliseconds 700
    & "$PSScriptRoot\vmm.ps1" -ProcId $p.Id -Top 6 | Out-File (Join-Path $OutDir "vmm-$ph.txt") -Encoding utf8
  }
} finally { if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force } }
