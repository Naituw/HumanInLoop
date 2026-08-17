[CmdletBinding(SupportsShouldProcess = $true)]
param(
  [switch]$PurgeData
)

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$UsesDefaultInstallDir = [string]::IsNullOrWhiteSpace($env:INSTALL_DIR)
$InstallDir = if ($env:INSTALL_DIR) { $env:INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "Programs\AskHuman" }
$InstalledBin = Join-Path $InstallDir "AskHuman.exe"
$RunKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
. (Join-Path $ScriptDir "windows-user-path.ps1")

if (Test-Path -LiteralPath $InstalledBin) {
  Write-Host "==> Removing managed Agent integrations"
  & $InstalledBin agents cleanup
  if ($LASTEXITCODE -ne 0) {
    Write-Warning "Some managed Agent integrations could not be removed. Review the output above."
  }

  Write-Host "==> Stopping AskHuman background processes"
  & $InstalledBin daemon stop --force 2>$null
}

Start-Sleep -Milliseconds 500
Get-Process -Name "AskHuman" -ErrorAction SilentlyContinue | ForEach-Object {
  try {
    if ($_.Path -and [IO.Path]::GetFullPath($_.Path).StartsWith(
      [IO.Path]::GetFullPath($InstallDir),
      [StringComparison]::OrdinalIgnoreCase
    )) {
      Stop-Process -Id $_.Id -Force -ErrorAction Stop
    }
  } catch {
    Write-Warning "Could not stop AskHuman process $($_.Id): $($_.Exception.Message)"
  }
}

if (Test-Path $RunKey) {
  Remove-ItemProperty -Path $RunKey -Name "AskHuman GUI Host" -ErrorAction SilentlyContinue
  Remove-ItemProperty -Path $RunKey -Name "AskHuman Daemon" -ErrorAction SilentlyContinue
}
$LoginLauncher = Join-Path $env:USERPROFILE ".askhuman\askhuman-login.vbs"
Remove-Item -LiteralPath $LoginLauncher -Force -ErrorAction SilentlyContinue

foreach ($pattern in @("askhuman_update_*", "askhuman_npm_update_*")) {
  Get-ChildItem -LiteralPath ([IO.Path]::GetTempPath()) -Directory -Filter $pattern -ErrorAction SilentlyContinue |
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
}

if (Test-Path -LiteralPath $InstallDir) {
  Write-Host "==> Removing $InstallDir"
  Remove-Item -LiteralPath $InstallDir -Recurse -Force
}

if ($UsesDefaultInstallDir) {
  Remove-AskHumanCommandLauncher
}
Remove-AskHumanUserPath $InstallDir

if ($PurgeData) {
  $DataDir = Join-Path $env:USERPROFILE ".askhuman"
  if ([IO.Path]::GetFileName([IO.Path]::GetFullPath($DataDir)) -ne ".askhuman") {
    throw "Refusing to remove an unexpected data directory: $DataDir"
  }
  if (Test-Path -LiteralPath $DataDir) {
    Write-Host "==> Removing user data from $DataDir"
    Remove-Item -LiteralPath $DataDir -Recurse -Force
  }
} else {
  Write-Host "User configuration, history, and todos were preserved in %USERPROFILE%\.askhuman."
  Write-Host "Run this script again with -PurgeData to remove them."
}

Write-Host "==> AskHuman has been uninstalled."
