# One full measured run of SONIX (release build): time to visible, private WS idle /
# after a full scroll of the Tracks list / Now Playing 40 s / minimized 35 s, CPU in each phase, restore latency,
# frame pacing. The app drives its own timeline (SONIX_SCRIPT=1) and logs `phase ...` and
# `frames ...` lines to SONIX_LOG; this script samples the process tree around them.
# Processes are only ever stopped by the exact PID this script started.
# Needs a signed-in profile with a library (the script plays tracks from it).
# Usage: powershell -File scriptsperf.ps1 [-Runs 3] [-OutDir perf-out]
param(
  [string]$Exe = "$PSScriptRoot\..\target\release\sonix.exe",
  [int]$Runs = 3,
  [string]$OutDir = "$PSScriptRoot\..\perf-out"
)
. "$PSScriptRoot\common.ps1"
$Exe = (Resolve-Path $Exe).Path
New-Item -ItemType Directory -Force $OutDir | Out-Null

function One([int]$n) {
  $log = "$env:TEMP\sonix-perf.log"
  $h = Start-Sonix $Exe @{ SONIX_SCRIPT = "1" } $log
  $pid_ = [uint32]$h.Proc.Id
  $res = [ordered]@{ run = $n }
  function Snap { $t = [SonixPerf]::Tree($pid_); @{ pws = (Sum $t 'PrivateWs'); commit = (Sum $t 'PrivateCommit'); n = $t.Count } }
  function CpuSec { (Get-Process -Id $pid_ -ErrorAction SilentlyContinue).TotalProcessorTime.TotalSeconds }
  function Has([string]$pat) { (Test-Path $log) -and (Select-String -Path $log -Pattern $pat -Quiet) }
  function WaitPhase([string]$name, [int]$MaxMs = 150000) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $MaxMs) { if (Has "phase $name") { return $true }; if ($h.Proc.HasExited) { return $false }; Start-Sleep -Milliseconds 50 }
    return $false
  }
  try {
    $res.visibleMs = Wait-Visible $h
    $hwnd = [SonixPerf]::LastHwnd
    [void](WaitPhase "idle_start")
    Start-Sleep -Milliseconds 800
    $c0 = CpuSec; $t0 = [Diagnostics.Stopwatch]::StartNew(); $s = @()
    while (-not (Has "phase idle_end")) { $s += (Snap).pws; Start-Sleep -Milliseconds 100 }
    $c1 = CpuSec
    $res.idle_pws_mb = MB ($s | Sort-Object)[[int]($s.Count / 2)]
    $res.idle_cpu_pct_of_one_core = [math]::Round(($c1 - $c0) / $t0.Elapsed.TotalSeconds * 100, 2)
    [void](WaitPhase "scroll_start")
    $c0 = CpuSec; $t0 = [Diagnostics.Stopwatch]::StartNew(); $sp = 0
    while (-not (Has "phase scroll_done")) { $x = (Snap).pws; if ($x -gt $sp) { $sp = $x }; Start-Sleep -Milliseconds 100 }
    $c1 = CpuSec
    $res.scroll_peak_pws_mb = MB $sp
    $res.scroll_cpu_pct_of_one_core = [math]::Round(($c1 - $c0) / $t0.Elapsed.TotalSeconds * 100, 1)
    [void](WaitPhase "post_scroll_idle"); Start-Sleep -Milliseconds 500
    $x = Snap; $res.after_scroll_pws_mb = MB $x.pws; $res.after_scroll_commit_mb = MB $x.commit
    [void](WaitPhase "np_start")
    Start-Sleep -Milliseconds 12000
    $c0 = CpuSec; $t0 = [Diagnostics.Stopwatch]::StartNew(); $npmax = 0
    while (-not (Has "phase np_end")) {
      $x = (Snap).pws; if ($x -gt $npmax) { $npmax = $x }
      Start-Sleep -Milliseconds 200
    }
    $res.np_cpu_pct_of_one_core = [math]::Round(((CpuSec) - $c0) / $t0.Elapsed.TotalSeconds * 100, 2)
    $x = Snap; $res.np_end_pws_mb = MB $x.pws; $res.np_peak_pws_mb = MB $npmax
    [void](WaitPhase "done")
    # minimize 35 s
    [void][SonixPerf]::ShowWindow($hwnd, 6)
    Start-Sleep -Milliseconds 5000
    $x = Snap; $res.min_5s_pws_mb = MB $x.pws
    $c0 = CpuSec; Start-Sleep -Milliseconds 4000; $res.min_cpu_pct_of_one_core = [math]::Round(((CpuSec) - $c0) / 4 * 100, 2)
    Start-Sleep -Milliseconds 26000
    $x = Snap; $res.min_35s_pws_mb = MB $x.pws
    # restore latency: restore command -> first render after resume (log epochs)
    $cmd = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    [void][SonixPerf]::ShowWindow($hwnd, 9)
    Start-Sleep -Milliseconds 1500
    $line = Select-String -Path $log -Pattern "restore_first_render" | Select-Object -Last 1
    if ($line -and $line.Line -match '@(\d+)') { $res.restore_to_first_render_ms = [int64]$Matches[1] - $cmd }
    $x = Snap; $res.after_restore_pws_mb = MB $x.pws
    $res.frames = @(Select-String -Path $log -Pattern "^\S+ frames " | ForEach-Object { $_.Line })
  } finally { Stop-Sonix $h }
  [pscustomobject]$res
}

$all = @()
for ($i = 1; $i -le $Runs; $i++) { $r = One $i; $all += $r; ($r | ConvertTo-Json -Depth 3) | Set-Content "$OutDir\run-$i.json"; $r | Format-List | Out-String | Write-Host }
function Med($f) { $v = $all | ForEach-Object { $_.$f } | Where-Object { $_ -ne $null } | Sort-Object; if ($v.Count) { $v[[int]($v.Count / 2)] } }
$sum = [ordered]@{}
foreach ($f in 'visibleMs','idle_pws_mb','idle_cpu_pct_of_one_core','scroll_peak_pws_mb','scroll_cpu_pct_of_one_core','after_scroll_pws_mb','np_end_pws_mb','np_peak_pws_mb','np_cpu_pct_of_one_core','min_5s_pws_mb','min_cpu_pct_of_one_core','min_35s_pws_mb','restore_to_first_render_ms','after_restore_pws_mb') { $sum[$f] = Med $f }
$sum.exe_mb = [math]::Round((Get-Item $Exe).Length / 1MB, 2)
"MEDIANS over $Runs runs:"; $sum | ConvertTo-Json | Tee-Object "$OutDir\summary.json"
