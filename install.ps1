# install.ps1 — One-line installer for oc-claw on Windows
# Usage:  irm https://raw.githubusercontent.com/rainnoon/oc-claw/main/install.ps1 | iex
$ErrorActionPreference = 'Stop'

$Repo     = "rainnoon/oc-claw"
$AppName  = "oc-claw"

Write-Host "Installing ${AppName}..." -ForegroundColor Cyan

# ── Get latest release installer URL (.msi or .exe) from GitHub ──────────────
$releaseJson = Invoke-RestMethod "https://api.github.com/repos/${Repo}/releases/latest"
$asset = $releaseJson.assets |
    Where-Object { $_.name -match '\.(msi|exe)$' -and $_.name -notmatch '\.blockmap$' } |
    Select-Object -First 1

if (-not $asset) {
    Write-Host "Error: could not find MSI/EXE download URL in latest release" -ForegroundColor Red
    exit 1
}

$downloadUrl  = $asset.browser_download_url
$fileName     = $asset.name
$isMsi        = $fileName -match '\.msi$'

# ── Download ─────────────────────────────────────────────────────────────────
$tempDir = Join-Path $env:TEMP "oc-claw-install-$(Get-Date -Format 'yyyyMMddHHmmss')"
New-Item -ItemType Directory -Path $tempDir -Force | Out-Null
$installerPath = Join-Path $tempDir $fileName

Write-Host "Downloading ${fileName}..."
Invoke-WebRequest -Uri $downloadUrl -OutFile $installerPath -UseBasicParsing

# ── Install ──────────────────────────────────────────────────────────────────
Write-Host "Installing..."
if ($isMsi) {
    # MSI — silent install
    $msiArgs = @('/i', "`"$installerPath`"", '/quiet', '/norestart')
    Start-Process msiexec.exe -ArgumentList $msiArgs -Wait -NoNewWindow
} else {
    # NSIS / Inno EXE — silent install (common flags: /S for NSIS, /VERYSILENT for Inno)
    Start-Process $installerPath -ArgumentList '/S', '/norestart' -Wait -NoNewWindow
}

# ── Clean up ─────────────────────────────────────────────────────────────────
Remove-Item -Recurse -Force $tempDir -ErrorAction SilentlyContinue

# ── Launch ───────────────────────────────────────────────────────────────────
Write-Host "Done! Launching ${AppName}..." -ForegroundColor Green
$appPath = Join-Path $env:LOCALAPPDATA "${AppName}\${AppName}.exe"
if (-not (Test-Path $appPath)) {
    $appPath = Join-Path $env:ProgramFiles "${AppName}\${AppName}.exe"
}
if (Test-Path $appPath) {
    Start-Process $appPath
} else {
    Write-Host "Installed successfully, but could not locate ${AppName}.exe to launch automatically." -ForegroundColor Yellow
    Write-Host "You can start it from the Start Menu." -ForegroundColor Yellow
}
