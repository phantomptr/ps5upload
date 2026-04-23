# uninstall-windows.ps1 — remove the ps5upload-engine scheduled task.
$ErrorActionPreference = "Stop"
$TaskName = "ps5upload-engine"

Stop-ScheduledTask        -TaskName $TaskName -ErrorAction SilentlyContinue
Unregister-ScheduledTask  -TaskName $TaskName -Confirm:$false -ErrorAction SilentlyContinue

Write-Host "OK  $TaskName removed"
