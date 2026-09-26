# install-windows.ps1 — register ps5upload-engine as a Windows Task Scheduler
# task that starts on user login.
#
# Run in PowerShell (as normal user, no elevation needed for Task Scheduler
# user tasks):
#   .\scripts\autolaunch\install-windows.ps1
#   .\scripts\autolaunch\install-windows.ps1 -Ps5Addr 192.168.1.50:9113
#
# Requirements: cargo in PATH, PowerShell 5.1+

param(
    # Defaults via if/else rather than the `??` null-coalescing
    # operator — the latter is PowerShell 7.0+ only and the script
    # advertises PS 5.1+ compatibility. On Windows 10/11 with stock
    # Windows PowerShell, the `??` form fails to parse before the
    # script even runs.
    [string]$Ps5Addr    = $(if ($env:PS5_ADDR) { $env:PS5_ADDR } else { "192.168.137.2:9113" }),
    # Engine listens on 19113 by default; matches the desktop client's
    # hard-coded probe URL and the PS5UPLOAD_ENGINE_PORT env var the
    # engine reads at startup.
    [string]$EnginePort = $(if ($env:PS5UPLOAD_ENGINE_PORT) { $env:PS5UPLOAD_ENGINE_PORT } else { "19113" })
)

$ErrorActionPreference = "Stop"

$RepoDir    = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$TaskName   = "ps5upload-engine"

Write-Host "==> Building engine release binary..."
Push-Location (Join-Path $RepoDir "engine")
cargo build --release -p ps5upload-engine
Pop-Location

$EngineBin = Join-Path $RepoDir "engine\target\release\ps5upload-engine.exe"
if (-not (Test-Path $EngineBin)) {
    Write-Error "Engine binary not found: $EngineBin"
    exit 1
}

Write-Host "==> Registering Task Scheduler task '$TaskName'..."

# Remove old task if it exists
Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false -ErrorAction SilentlyContinue

$Action = New-ScheduledTaskAction `
    -Execute $EngineBin `
    -WorkingDirectory (Split-Path $EngineBin)

$Action.EnvironmentVariables = @(
    [PSCustomObject]@{ Name = "PS5_ADDR";                 Value = $Ps5Addr    }
    [PSCustomObject]@{ Name = "PS5UPLOAD_ENGINE_PORT";    Value = $EnginePort }
)

$Trigger   = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
$Settings  = New-ScheduledTaskSettingsSet `
    -ExecutionTimeLimit (New-TimeSpan -Hours 0) `
    -RestartCount 3 `
    -RestartInterval (New-TimeSpan -Minutes 1) `
    -StartWhenAvailable

Register-ScheduledTask `
    -TaskName  $TaskName `
    -Action    $Action `
    -Trigger   $Trigger `
    -Settings  $Settings `
    -RunLevel  Limited `
    -Force | Out-Null

# Start it immediately
Start-ScheduledTask -TaskName $TaskName

Write-Host ""
Write-Host "OK  $TaskName registered and started"
Write-Host "    dashboard: http://127.0.0.1:${EnginePort}"
Write-Host ""
Write-Host "Useful commands:"
Write-Host "  Get-ScheduledTask $TaskName"
Write-Host "  Stop-ScheduledTask $TaskName"
Write-Host "  Unregister-ScheduledTask $TaskName"
