param([string]$Exe)
$log = "<build-dir>\mem-snap.log"; Remove-Item $log -ErrorAction SilentlyContinue
$env:APPDATA = "<build-dir>\mem-profile\roaming"; $env:LOCALAPPDATA = "<build-dir>\mem-profile\local"; $env:SONIX_LOG = $log; $env:SONIX_SCRIPT = "real"; $env:SONIX_NO_IDLE_TRIM = "1"
$p = Start-Process -FilePath (Resolve-Path $Exe).Path -PassThru
function Has([string]$pat) { (Test-Path $log) -and (Select-String -Path $log -Pattern $pat -Quiet) }
try {
  foreach ($ph in "idle_end", "post_scroll_idle", "np_end") {
    while (-not (Has "phase $ph")) { Start-Sleep -Milliseconds 100 }
    Start-Sleep -Milliseconds 700
    & "<build-dir>/memtools/vmm.ps1" -ProcId $p.Id -Top 6 | Out-File "<build-dir>\mem-vmm-$ph.txt" -Encoding utf8
  }
} finally { if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force } }
