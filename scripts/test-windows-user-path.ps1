[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "windows-user-path.ps1")

function Assert-Equal([string]$Expected, [string]$Actual, [string]$Case) {
  if ($Expected -cne $Actual) {
    throw "$Case failed: expected '$Expected', got '$Actual'"
  }
}

function Assert-True([bool]$Actual, [string]$Case) {
  if (-not $Actual) { throw "$Case failed" }
}

$Target = "C:\Users\Test User\AppData\Local\Programs\AskHuman"
$Other = "C:\Tools"
$OriginalProbeRoot = $env:ASKHUMAN_PATH_TEST_ROOT
$LauncherTestDirectory = Join-Path ([IO.Path]::GetTempPath()) "askhuman-launcher-test-$PID"

try {
  $env:ASKHUMAN_PATH_TEST_ROOT = "C:\Users\Test User\AppData\Local"

  Assert-Equal $Target (Add-AskHumanPathEntry "" $Target) "add to empty PATH"
  Assert-Equal "$Other;$Target" (Add-AskHumanPathEntry $Other $Target) "append entry"
  Assert-Equal "$Other;$Target" (Add-AskHumanPathEntry "$Other;$Target" $Target) "idempotent add"
  $ExpandedEntryPath = "$Other;%ASKHUMAN_PATH_TEST_ROOT%\Programs\AskHuman\"
  Assert-Equal $ExpandedEntryPath (
    Add-AskHumanPathEntry $ExpandedEntryPath $Target
  ) "expanded and trailing-slash duplicate"
  Assert-Equal "$Other;`"$Target`"" (
    Add-AskHumanPathEntry "$Other;`"$Target`"" $Target
  ) "quoted duplicate"
  Assert-Equal "$Other;$Target" (Add-AskHumanPathEntry "$Other;" $Target) "preserve separator"
  Assert-Equal $Other (
    Remove-AskHumanPathEntry "$Target;$Other;$($Target.ToUpperInvariant())\" $Target
  ) "remove all normalized matches"
  Assert-Equal $Other (
    Remove-AskHumanPathEntry "%ASKHUMAN_PATH_TEST_ROOT%\Programs\AskHuman;$Other" $Target
  ) "remove expanded match"
  Assert-Equal $Other (Remove-AskHumanPathEntry $Other $Target) "preserve unrelated PATH"

  New-Item -ItemType Directory -Path $LauncherTestDirectory | Out-Null
  $DefaultTarget = Join-Path $env:LOCALAPPDATA "Programs\AskHuman"
  $LauncherPath = Install-AskHumanCommandLauncher $DefaultTarget $LauncherTestDirectory
  Assert-True (Test-Path -LiteralPath $LauncherPath -PathType Leaf) "create managed launcher"
  $LauncherContent = [IO.File]::ReadAllText($LauncherPath)
  Assert-True (Test-AskHumanManagedCommandLauncher $LauncherContent) "recognize managed launcher"
  Assert-True ($LauncherContent.Contains('"%LOCALAPPDATA%\Programs\AskHuman\AskHuman.exe" %*')) (
    "launcher target and argument forwarding"
  )
  Install-AskHumanCommandLauncher $DefaultTarget $LauncherTestDirectory | Out-Null
  Remove-AskHumanCommandLauncher $LauncherTestDirectory
  Assert-True (-not (Test-Path -LiteralPath $LauncherPath)) "remove managed launcher"

  [IO.File]::WriteAllText($LauncherPath, "@echo off`r`necho unrelated`r`n")
  Remove-AskHumanCommandLauncher $LauncherTestDirectory 3>$null
  Assert-True (Test-Path -LiteralPath $LauncherPath -PathType Leaf) "preserve unmanaged launcher"

  $InstallWrapper = [IO.File]::ReadAllText((Join-Path $PSScriptRoot "install-windows.cmd"))
  Assert-True ($InstallWrapper.Contains('-ExecutionPolicy Bypass')) "installer execution-policy wrapper"
  $UninstallWrapper = [IO.File]::ReadAllText((Join-Path $PSScriptRoot "uninstall-windows.cmd"))
  Assert-True ($UninstallWrapper.Contains('-ExecutionPolicy Bypass')) "uninstaller execution-policy wrapper"
} finally {
  $env:ASKHUMAN_PATH_TEST_ROOT = $OriginalProbeRoot
  Remove-Item -LiteralPath $LauncherTestDirectory -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "Windows user PATH tests passed."
