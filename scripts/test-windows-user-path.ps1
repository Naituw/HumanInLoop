[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "windows-user-path.ps1")

function Assert-Equal([string]$Expected, [string]$Actual, [string]$Case) {
  if ($Expected -cne $Actual) {
    throw "$Case failed: expected '$Expected', got '$Actual'"
  }
}

$Target = "C:\Users\Test User\AppData\Local\Programs\AskHuman"
$Other = "C:\Tools"
$OriginalProbeRoot = $env:ASKHUMAN_PATH_TEST_ROOT

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
} finally {
  $env:ASKHUMAN_PATH_TEST_ROOT = $OriginalProbeRoot
}

Write-Host "Windows user PATH tests passed."
