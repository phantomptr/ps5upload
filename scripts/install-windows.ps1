# install-windows.ps1 — one-shot dev environment bootstrap for Windows 11.
#
# Idempotent: safe to re-run.
#
# Installs (via winget where possible):
#   - Node.js LTS
#   - Rust (rustup-init)
#   - Visual Studio 2022 Build Tools w/ C++ workload (MSVC + Windows SDK)
#   - 7zip (used to extract the PS5 SDK zip in CI-clean way)
#   - Microsoft Edge WebView2 Runtime (preinstalled on Win11; verified)
#   - PS5 Payload SDK v0.38 → $env:USERPROFILE\ps5-payload-sdk
#
# Run from an elevated PowerShell:
#   PS> Set-ExecutionPolicy -Scope Process Bypass -Force
#   PS> .\scripts\install-windows.ps1
#
# After it finishes, the script prints the env exports you need to persist via
# `setx` (or just paste into the current shell) so `make build` and `make run-client`
# work in any new terminal — assuming you have GNU Make available (Git Bash, MSYS2,
# or `winget install ezwinports.make`). Tauri itself doesn't need Make on Windows;
# the Make targets are convenience wrappers that shell out to npm/cargo.

[CmdletBinding()]
param(
  # Default installs to %USERPROFILE%\ps5-payload-sdk — user-writable, no admin
  # needed. We deliberately do NOT read $env:PS5_PAYLOAD_SDK: that's the
  # *build-time* SDK path and may point at somewhere the current user can't
  # write to. Pass -SdkDir or set $env:PS5_SDK_INSTALL_DIR to change.
  [string]$SdkDir = $(if ($env:PS5_SDK_INSTALL_DIR) { $env:PS5_SDK_INSTALL_DIR } else { Join-Path $env:USERPROFILE 'ps5-payload-sdk' }),
  [string]$SdkTag = 'v0.38'
)

$ErrorActionPreference = 'Stop'
$SdkUrl = "https://github.com/ps5-payload-dev/sdk/releases/download/$SdkTag/ps5-payload-sdk.zip"

function Log    { param($m) Write-Host "`n==> $m" -ForegroundColor Cyan }
function Ok     { param($m) Write-Host "✓ $m" -ForegroundColor Green }
function Warn   { param($m) Write-Host "! $m" -ForegroundColor Yellow }
function Die    { param($m) Write-Host "ERROR: $m" -ForegroundColor Red; exit 1 }

function Has-Cmd { param($name) (Get-Command $name -ErrorAction SilentlyContinue) -ne $null }

function Winget-Install {
  param([string]$Id, [string]$Name, [string[]]$Override = @())
  Log "winget install $Name ($Id)"
  $args = @('install', '--id', $Id, '-e', '--accept-source-agreements', '--accept-package-agreements')
  if ($Override.Count -gt 0) { $args += @('--override', ($Override -join ' ')) }
  & winget @args
  if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne -1978335189) {
    # -1978335189 = APPINSTALLER_CLI_ERROR_PACKAGE_ALREADY_INSTALLED
    Warn "winget exited $LASTEXITCODE for $Id (continuing — may already be installed)"
  } else {
    Ok "$Name installed (or already present)"
  }
}

# ─── pre-flight ────────────────────────────────────────────────────────────────
if (-not [System.Environment]::OSVersion.VersionString.Contains('Windows')) {
  Die "This script targets Windows. For Linux use scripts/install-ubuntu.sh; for macOS scripts/install-macos.sh."
}
if (-not (Has-Cmd winget)) {
  Die "winget not found. Install 'App Installer' from the Microsoft Store, then re-run."
}

# ─── 1. Node.js LTS ────────────────────────────────────────────────────────────
if (Has-Cmd node) {
  Ok "Node.js already installed: $(node --version)"
} else {
  Winget-Install -Id 'OpenJS.NodeJS.LTS' -Name 'Node.js LTS'
}

# ─── 2. Rust toolchain ─────────────────────────────────────────────────────────
if ((Has-Cmd rustc) -and (Has-Cmd cargo)) {
  Ok "Rust already installed: $(rustc --version)"
} else {
  Log "Installing Rust via rustup-init"
  $tmpDir = Join-Path $env:TEMP "rustup-$(Get-Random)"
  New-Item -ItemType Directory -Path $tmpDir | Out-Null
  $rustupExe = Join-Path $tmpDir 'rustup-init.exe'
  Invoke-WebRequest -UseBasicParsing -Uri 'https://win.rustup.rs/x86_64' -OutFile $rustupExe
  & $rustupExe -y --default-toolchain stable --profile default
  if ($LASTEXITCODE -ne 0) { Die "rustup-init failed (exit $LASTEXITCODE)" }
  Remove-Item $tmpDir -Recurse -Force
  # Make cargo discoverable in this session
  $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
  Ok "Rust installed: $(rustc --version)"
}

# ─── 3. MSVC Build Tools (C++ workload) ────────────────────────────────────────
$vsWherePath = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$hasVcTools = $false
if (Test-Path $vsWherePath) {
  $vsInstance = & $vsWherePath -products '*' -requires 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64' -format json | ConvertFrom-Json
  if ($vsInstance) { $hasVcTools = $true }
}
if ($hasVcTools) {
  Ok "MSVC C++ Build Tools already installed"
} else {
  Winget-Install -Id 'Microsoft.VisualStudio.2022.BuildTools' -Name 'VS 2022 Build Tools' `
    -Override @('--quiet','--wait','--norestart','--nocache',
                '--add Microsoft.VisualStudio.Workload.VCTools',
                '--includeRecommended')
}

# ─── 4. WebView2 Runtime ───────────────────────────────────────────────────────
$webview2Path = 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
if (Test-Path $webview2Path) {
  Ok "WebView2 Runtime already installed"
} else {
  Winget-Install -Id 'Microsoft.EdgeWebView2Runtime' -Name 'WebView2 Runtime'
}

# ─── 5. 7zip (for clean SDK extraction) ────────────────────────────────────────
if (-not (Has-Cmd 7z)) {
  Winget-Install -Id '7zip.7zip' -Name '7-Zip'
}

# ─── 5b. LLVM 18 (prospero-clang dependency) ──────────────────────────────────
# The PS5 payload toolchain (prospero-clang + ld.lld) needs LLVM 18 with
# lld included. Without this `make payload` fails on a fresh Windows
# checkout — the SDK ships its own clang shim but expects host lld to be
# discoverable. macOS pins llvm@18 via brew, Ubuntu via apt; do the
# equivalent here. winget's LLVM.LLVM tracks latest, which is fine —
# prospero-clang is forward-compatible with newer host LLVM.
if (Has-Cmd clang) {
  Ok "LLVM/clang already installed: $(clang --version | Select-Object -First 1)"
} else {
  Winget-Install -Id 'LLVM.LLVM' -Name 'LLVM (clang + lld)'
  # Make clang/lld discoverable in this session for any subsequent
  # `make payload` step the user runs from the same shell.
  $env:Path = "${env:ProgramFiles}\LLVM\bin;$env:Path"
}

# ─── 6. PS5 Payload SDK ────────────────────────────────────────────────────────
if (Test-Path (Join-Path $SdkDir 'toolchain\prospero.mk')) {
  Ok "PS5 SDK already present at $SdkDir"
} else {
  Log "Downloading PS5 Payload SDK $SdkTag → $SdkDir"
  $tmp = Join-Path $env:TEMP "ps5sdk-$(Get-Random)"
  New-Item -ItemType Directory -Path $tmp | Out-Null
  $zip = Join-Path $tmp 'sdk.zip'
  Invoke-WebRequest -UseBasicParsing -Uri $SdkUrl -OutFile $zip
  Expand-Archive -Path $zip -DestinationPath $tmp -Force
  $extracted = Join-Path $tmp 'ps5-payload-sdk'
  if (-not (Test-Path $extracted)) {
    Die "SDK zip did not contain expected ps5-payload-sdk\ directory"
  }
  $parent = Split-Path $SdkDir -Parent
  if (-not (Test-Path $parent)) { New-Item -ItemType Directory -Path $parent | Out-Null }
  Move-Item -Path $extracted -Destination $SdkDir -Force
  Remove-Item $tmp -Recurse -Force
  if (-not (Test-Path (Join-Path $SdkDir 'toolchain\prospero.mk'))) {
    Die "SDK extracted but prospero.mk missing"
  }
  Ok "PS5 SDK installed at $SdkDir"
}

# ─── 7. client npm deps ────────────────────────────────────────────────────────
$repoRoot = Split-Path -Parent $PSScriptRoot
$clientDir = Join-Path $repoRoot 'client'
if (Test-Path $clientDir) {
  Log "Installing client npm dependencies"
  Push-Location $clientDir
  try {
    & npm install --no-audit --no-fund
    if ($LASTEXITCODE -ne 0) { Die "npm install failed" }
    Ok "client/node_modules ready"
  } finally {
    Pop-Location
  }
}

# ─── 8. wrap up ────────────────────────────────────────────────────────────────
Write-Host ""
Write-Host "✓ Setup complete." -ForegroundColor Green
Write-Host ""
Write-Host "Persist PS5_PAYLOAD_SDK for future shells (run from any PowerShell):"
Write-Host "  setx PS5_PAYLOAD_SDK `"$SdkDir`""
Write-Host ""
Write-Host "Then in this terminal (or open a new one):"
Write-Host "  `$env:PS5_PAYLOAD_SDK = `"$SdkDir`""
Write-Host "  make build"
Write-Host "  make run-client"
Write-Host ""
Write-Host "Note: `make` is not part of Windows by default. If `make` isn't on PATH:"
Write-Host "  winget install ezwinports.make"
Write-Host "or use Git Bash / MSYS2, where `make` is bundled."
Write-Host ""
