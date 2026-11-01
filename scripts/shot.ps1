# Screenshot of the real window (CopyFromScreen of our own HWND's frame). Starts the exe,
# captures, stops ONLY that PID. Env hooks: -Route, -Light, -Size WxH, -Play, -Menu <row>.
param(
  [string]$Exe = "$PSScriptRoot\..\target\release\sonix-native.exe",
  [string]$Out = "$PSScriptRoot\..\docs\screenshots",
  [string]$Name = "tracks",
  [string]$Route = "",
  [switch]$Light,
  [switch]$Play,
  [string]$Size = "",
  [string]$Menu = "",
  [int]$WaitMs = 3500
)
. "$PSScriptRoot\common.ps1"
New-Item -ItemType Directory -Force $Out | Out-Null
$Exe = (Resolve-Path $Exe).Path
$envv = @{}
if ($Route) { $envv.SONIX_ROUTE = $Route }
if ($Light) { $envv.SONIX_LIGHT = "1" }
if ($Play) { $envv.SONIX_PLAY = "1" }
if ($Size) { $envv.SONIX_SIZE = $Size }
if ($Menu) { $envv.SONIX_MENU = $Menu }
$log = "$env:TEMP\sonix-shot.log"
$h = Start-Sonix $Exe $envv $log
try {
  $v = Wait-Visible $h
  "visible after $v ms"
  Start-Sleep -Milliseconds $WaitMs
  [void][SonixPerf]::SetForegroundWindow([SonixPerf]::LastHwnd)
  Start-Sleep -Milliseconds 400
  Save-WindowShot ([SonixPerf]::LastHwnd) (Join-Path $Out "$Name.png")
} finally { Stop-Sonix $h }
Get-Content $log
