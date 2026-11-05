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
#   - Repository-pinned PS5 Payload SDK (currently v0.45)
#     → $env:USERPROFILE\ps5-payload-sdk
#   - Its portable prospero-nid, and the pinned SQLite sources the payload compiles
#     → payload\third_party\sqlite3
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
  [string]$SdkTag = '',
  [string]$SdkSha256 = ''
)

$ErrorActionPreference = 'Stop'

function Log    { param($m) Write-Host "`n==> $m" -ForegroundColor Cyan }
function Ok     { param($m) Write-Host "✓ $m" -ForegroundColor Green }
function Warn   { param($m) Write-Host "! $m" -ForegroundColor Yellow }
function Die    { param($m) Write-Host "ERROR: $m" -ForegroundColor Red; exit 1 }

function Has-Cmd { param($name) (Get-Command $name -ErrorAction SilentlyContinue) -ne $null }

# Load the same immutable SDK tag and release checksum used by the POSIX
# installer and GitHub Actions. Callers may override both parameters together
# for local experiments; release builds always use these repository values.
$repoRoot = Split-Path -Parent $PSScriptRoot
$sdkMetadataPath = Join-Path $PSScriptRoot 'ps5-sdk.env'
if (-not (Test-Path $sdkMetadataPath)) { Die "missing SDK metadata: $sdkMetadataPath" }
$sdkMetadata = @{}
Get-Content $sdkMetadataPath | ForEach-Object {
  if ($_ -match '^([A-Z0-9_]+)=(.+)$') { $sdkMetadata[$Matches[1]] = $Matches[2].Trim() }
}
$pinnedSdkTag = $sdkMetadata['PS5_SDK_TAG']
$pinnedSdkSha256 = $sdkMetadata['PS5_SDK_SHA256']
if ($pinnedSdkTag -notmatch '^v\d+\.\d+(?:\.\d+)?$') {
  Die 'PS5_SDK_TAG in scripts\ps5-sdk.env is invalid'
}
if ($pinnedSdkSha256 -notmatch '^[0-9a-f]{64}$') {
  Die 'PS5_SDK_SHA256 in scripts\ps5-sdk.env must be a lowercase SHA-256'
}
if ([string]::IsNullOrWhiteSpace($SdkTag)) { $SdkTag = $pinnedSdkTag }
if ([string]::IsNullOrWhiteSpace($SdkSha256)) {
  if ($SdkTag -ne $pinnedSdkTag) {
    Die '-SdkSha256 is required when overriding -SdkTag'
  }
  $SdkSha256 = $pinnedSdkSha256
}
if ($SdkSha256 -notmatch '^[0-9a-fA-F]{64}$') { Die 'SDK SHA-256 must contain 64 hex characters' }
$SdkUrl = "https://github.com/ps5-payload-dev/sdk/releases/download/$SdkTag/ps5-payload-sdk.zip"
$sdkVersionMarker = '.ps5upload-sdk-version'

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

# ─── 5b. LLVM (prospero-clang dependency) ────────────────────────────────────
# The PS5 payload toolchain (prospero-clang + ld.lld) needs LLVM with
# lld included. Without this `make payload` fails on a fresh Windows
# checkout — the SDK ships its own clang shim but expects host lld to be
# discoverable. macOS discovers Homebrew's current llvm, Ubuntu pins 22; do the
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
$prosperoMk = Join-Path $SdkDir 'toolchain\prospero.mk'
$versionMarkerPath = Join-Path $SdkDir $sdkVersionMarker
$installedSdkTag = if (Test-Path $versionMarkerPath) { (Get-Content $versionMarkerPath -Raw).Trim() } else { '' }
if ((Test-Path $prosperoMk) -and $installedSdkTag -eq $SdkTag) {
  Ok "PS5 SDK $SdkTag already installed at $SdkDir"
} else {
  Log "Downloading PS5 Payload SDK $SdkTag → $SdkDir"
  $tmp = Join-Path $env:TEMP "ps5sdk-$(Get-Random)"
  New-Item -ItemType Directory -Path $tmp | Out-Null
  try {
    $zip = Join-Path $tmp 'sdk.zip'
    Invoke-WebRequest -UseBasicParsing -Uri $SdkUrl -OutFile $zip
    $actualSha256 = (Get-FileHash -Algorithm SHA256 -Path $zip).Hash.ToLowerInvariant()
    if ($actualSha256 -ne $SdkSha256.ToLowerInvariant()) {
      Die "SDK checksum mismatch: expected $SdkSha256, got $actualSha256"
    }
    Ok "Verified official archive SHA-256 $actualSha256"

    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    $extracted = Join-Path $tmp 'ps5-payload-sdk'
    if (-not (Test-Path (Join-Path $extracted 'toolchain\prospero.mk'))) {
      Die "SDK zip did not contain the expected ps5-payload-sdk\ toolchain"
    }
    if (-not (Test-Path (Join-Path $extracted 'target\lib\crt1.o'))) {
      Die 'SDK zip is missing target\lib\crt1.o'
    }
    Set-Content -Path (Join-Path $extracted $sdkVersionMarker) -Value $SdkTag -Encoding ascii

    $parent = Split-Path $SdkDir -Parent
    if (-not (Test-Path $parent)) { New-Item -ItemType Directory -Path $parent | Out-Null }
    $backupDir = ''
    if (Test-Path $SdkDir) {
      $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
      $backupDir = "$SdkDir.backup-$stamp-$PID"
      Move-Item -Path $SdkDir -Destination $backupDir
      Ok "Moved the previous SDK to $backupDir"
    }
    try {
      Move-Item -Path $extracted -Destination $SdkDir
    } catch {
      if ($backupDir -and (Test-Path $backupDir) -and -not (Test-Path $SdkDir)) {
        Move-Item -Path $backupDir -Destination $SdkDir
      }
      throw
    }
  } finally {
    if (Test-Path $tmp) { Remove-Item $tmp -Recurse -Force }
  }
  Ok "PS5 SDK $SdkTag installed at $SdkDir"
}

# ─── 6b. Portable prospero-nid ─────────────────────────────────────────────────
# The SDK archive carries a Linux/x86-64 prospero-nid; the tagged source is portable Python.
# Same pinned copy as scripts/install-ps5-sdk.sh. Nothing in `make payload` runs it, so Python is
# not installed for it: only used to check it when it is already here.
$portableNid = Join-Path $PSScriptRoot 'ps5-sdk\prospero-nid'
if (-not (Test-Path $portableNid)) { Die "missing portable prospero-nid at $portableNid" }
$nidSha256 = (Get-FileHash -Algorithm SHA256 -Path $portableNid).Hash.ToLowerInvariant()
if ($nidSha256 -ne $sdkMetadata['PS5_SDK_NID_SHA256']) {
  Die 'portable prospero-nid checksum does not match scripts\ps5-sdk.env'
}
$installedNid = Join-Path $SdkDir 'bin\prospero-nid'
Copy-Item -Path $portableNid -Destination $installedNid -Force
Ok "Installed portable prospero-nid at $installedNid"
foreach ($python in @('python3', 'python')) {
  if (-not (Has-Cmd $python)) { continue }
  # Windows' Store stubs answer to these names and fail.
  & $python --version *> $null
  if ($LASTEXITCODE -ne 0) { continue }
  $nid = & $python $installedNid sceKernelGetProcessTime
  if ($nid -ne '4J2sUJmuHZQ') { Die "prospero-nid self-test failed (got $nid)" }
  Ok 'prospero-nid self-test passed'
  break
}

# ─── 6c. SQLite amalgamation ───────────────────────────────────────────────────
# The payload compiles a real SQLite from these sources. Fetched rather than committed, pinned by
# version and SHA-256 in ps5-sdk.env; same steps and archive-layout guard as install-ps5-sdk.sh.
$sqliteVersion = $sdkMetadata['PS5_SQLITE_VERSION']
$sqliteYear = $sdkMetadata['PS5_SQLITE_YEAR']
$sqliteSha256 = $sdkMetadata['PS5_SQLITE_SHA256']
if ($sqliteVersion -notmatch '^\d{7}$') { Die 'PS5_SQLITE_VERSION in scripts\ps5-sdk.env is invalid' }
if ($sqliteYear -notmatch '^\d{4}$') { Die 'PS5_SQLITE_YEAR in scripts\ps5-sdk.env is invalid' }
if ($sqliteSha256 -notmatch '^[0-9a-f]{64}$') { Die 'PS5_SQLITE_SHA256 in scripts\ps5-sdk.env must be a lowercase SHA-256' }
$sqliteDir = Join-Path $repoRoot 'payload\third_party\sqlite3'
$sqliteStamp = Join-Path $sqliteDir '.version'
if ((Test-Path (Join-Path $sqliteDir 'sqlite3.c')) -and (Test-Path (Join-Path $sqliteDir 'sqlite3.h')) -and
    (Test-Path $sqliteStamp) -and ((Get-Content $sqliteStamp -Raw).Trim() -eq $sqliteVersion)) {
  Ok "SQLite $sqliteVersion amalgamation already present"
} else {
  Log "Downloading SQLite $sqliteVersion amalgamation"
  $tmp = Join-Path $env:TEMP "ps5sqlite-$(Get-Random)"
  New-Item -ItemType Directory -Path $tmp | Out-Null
  try {
    $zip = Join-Path $tmp 'sqlite-amalgamation.zip'
    Invoke-WebRequest -UseBasicParsing -Uri "https://www.sqlite.org/$sqliteYear/sqlite-amalgamation-$sqliteVersion.zip" -OutFile $zip
    $actualSha256 = (Get-FileHash -Algorithm SHA256 -Path $zip).Hash.ToLowerInvariant()
    if ($actualSha256 -ne $sqliteSha256) {
      Die "SQLite checksum mismatch: expected $sqliteSha256, got $actualSha256"
    }
    Ok "Verified SQLite archive SHA-256 $actualSha256"

    # Every entry must live under the one documented top-level folder, with no traversal.
    $top = "sqlite-amalgamation-$sqliteVersion"
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [System.IO.Compression.ZipFile]::OpenRead($zip)
    try {
      foreach ($entry in $archive.Entries) {
        $name = $entry.FullName -replace '\\', '/'
        if (-not $name.StartsWith("$top/")) { Die "unexpected SQLite archive entry: $name" }
        if ("/$name/" -match '/\.\.?/') { Die "unsafe SQLite archive entry: $name" }
      }
    } finally {
      $archive.Dispose()
    }

    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    foreach ($file in @('sqlite3.c', 'sqlite3.h')) {
      if (-not (Test-Path (Join-Path $tmp "$top\$file"))) { Die "SQLite archive is missing $file" }
    }
    New-Item -ItemType Directory -Path $sqliteDir -Force | Out-Null
    foreach ($file in @('sqlite3.c', 'sqlite3.h')) {
      Copy-Item -Path (Join-Path $tmp "$top\$file") -Destination (Join-Path $sqliteDir $file) -Force
    }
    Set-Content -Path $sqliteStamp -Value $sqliteVersion -Encoding ascii
  } finally {
    if (Test-Path $tmp) { Remove-Item $tmp -Recurse -Force }
  }
  Ok "Installed SQLite $sqliteVersion sources at $sqliteDir"
}

# ─── 7. client npm deps ────────────────────────────────────────────────────────
$clientDir = Join-Path $repoRoot 'client'
if (Test-Path $clientDir) {
  Log "Installing client npm dependencies"
  Push-Location $clientDir
  try {
    # `npm ci`, never `npm install`: see NPM_INSTALL in the root Makefile (npm 10
    # strips the lockfile's libc fields that the Alpine web UI image relies on).
    & npm ci --no-audit --no-fund
    if ($LASTEXITCODE -ne 0) { Die "npm ci failed" }
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
