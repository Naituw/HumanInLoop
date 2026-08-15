function Get-AskHumanNormalizedPathEntry([string]$Entry) {
  if ([string]::IsNullOrWhiteSpace($Entry)) { return $null }

  $Expanded = [Environment]::ExpandEnvironmentVariables($Entry.Trim().Trim('"'))
  try { $Expanded = [IO.Path]::GetFullPath($Expanded) } catch { }
  return $Expanded.TrimEnd('\', '/').ToLowerInvariant()
}

function Test-AskHumanPathContains([string]$PathValue, [string]$Directory) {
  $NormalizedTarget = Get-AskHumanNormalizedPathEntry $Directory
  foreach ($Entry in @($PathValue -split ';')) {
    if ((Get-AskHumanNormalizedPathEntry $Entry) -eq $NormalizedTarget) {
      return $true
    }
  }
  return $false
}

function Add-AskHumanPathEntry([string]$PathValue, [string]$Directory) {
  $Target = [IO.Path]::GetFullPath($Directory).TrimEnd('\', '/')
  if (Test-AskHumanPathContains $PathValue $Target) { return $PathValue }
  if ([string]::IsNullOrWhiteSpace($PathValue)) { return $Target }
  if ($PathValue.EndsWith(';')) { return "$PathValue$Target" }
  return "$PathValue;$Target"
}

function Remove-AskHumanPathEntry([string]$PathValue, [string]$Directory) {
  if ([string]::IsNullOrEmpty($PathValue)) { return $PathValue }

  $NormalizedTarget = Get-AskHumanNormalizedPathEntry ([IO.Path]::GetFullPath($Directory))
  $Kept = @()
  foreach ($Entry in ($PathValue -split ';')) {
    if ((Get-AskHumanNormalizedPathEntry $Entry) -ne $NormalizedTarget) {
      $Kept += $Entry
    }
  }
  return ($Kept -join ';')
}

function Add-AskHumanUserPath([string]$Directory) {
  $Target = [IO.Path]::GetFullPath($Directory).TrimEnd('\', '/')
  $UserPath = [Environment]::GetEnvironmentVariable(
    "Path", [EnvironmentVariableTarget]::User
  )

  if (-not (Test-AskHumanPathContains $UserPath $Target)) {
    $NewPath = Add-AskHumanPathEntry $UserPath $Target
    if ($NewPath.Length -gt 32767) {
      throw "The user PATH would exceed the Windows environment-variable limit"
    }

    # User-scoped updates persist under HKCU and notify the Windows shell.
    [Environment]::SetEnvironmentVariable(
      "Path", $NewPath, [EnvironmentVariableTarget]::User
    )
    Write-Host "==> Added $Target to the current user's PATH"
  } else {
    Write-Host "==> The current user's PATH already contains $Target"
  }

  if (-not (Test-AskHumanPathContains $env:Path $Target)) {
    $env:Path = Add-AskHumanPathEntry $env:Path $Target
  }
}

function Remove-AskHumanUserPath([string]$Directory) {
  $UserPath = [Environment]::GetEnvironmentVariable(
    "Path", [EnvironmentVariableTarget]::User
  )
  if ([string]::IsNullOrEmpty($UserPath)) { return }

  if (-not (Test-AskHumanPathContains $UserPath $Directory)) { return }

  # User-scoped updates persist under HKCU and notify the Windows shell.
  [Environment]::SetEnvironmentVariable(
    "Path", (Remove-AskHumanPathEntry $UserPath $Directory),
    [EnvironmentVariableTarget]::User
  )
  Write-Host "==> Removed $Directory from the current user's PATH"
}

function Get-AskHumanCommandLauncherContent([string]$InstallDirectory) {
  $DefaultInstallDirectory = Join-Path $env:LOCALAPPDATA "Programs\AskHuman"
  if ((Get-AskHumanNormalizedPathEntry $InstallDirectory) -ne
      (Get-AskHumanNormalizedPathEntry $DefaultInstallDirectory)) {
    throw "The global command launcher only supports the default install directory"
  }

  return "@echo off`r`nrem AskHuman managed command launcher v1`r`n`"%LOCALAPPDATA%\Programs\AskHuman\AskHuman.exe`" %*`r`n"
}

function Test-AskHumanManagedCommandLauncher([string]$Content) {
  return $Content.StartsWith(
    "@echo off`r`nrem AskHuman managed command launcher v1`r`n",
    [StringComparison]::Ordinal
  )
}

function Install-AskHumanCommandLauncher(
  [string]$InstallDirectory,
  [string]$LauncherDirectory = (Join-Path $env:LOCALAPPDATA "Microsoft\WindowsApps")
) {
  if (-not (Test-Path -LiteralPath $LauncherDirectory -PathType Container)) {
    throw "The standard Windows command-launcher directory is missing: $LauncherDirectory"
  }

  $LauncherPath = Join-Path $LauncherDirectory "AskHuman.cmd"
  $Content = Get-AskHumanCommandLauncherContent $InstallDirectory
  if (Test-Path -LiteralPath $LauncherPath) {
    $Existing = [IO.File]::ReadAllText($LauncherPath)
    if (-not (Test-AskHumanManagedCommandLauncher $Existing)) {
      throw "Refusing to overwrite an unmanaged command launcher: $LauncherPath"
    }
    if ($Existing -ceq $Content) {
      Write-Host "==> Command launcher is already current: $LauncherPath"
      return $LauncherPath
    }
  }

  $StagedLauncher = Join-Path $LauncherDirectory ".AskHuman.cmd.new.$PID"
  try {
    [IO.File]::WriteAllText($StagedLauncher, $Content, [Text.Encoding]::ASCII)
    Move-Item -LiteralPath $StagedLauncher -Destination $LauncherPath -Force
  } finally {
    Remove-Item -LiteralPath $StagedLauncher -Force -ErrorAction SilentlyContinue
  }
  Write-Host "==> Installed command launcher: $LauncherPath"
  return $LauncherPath
}

function Remove-AskHumanCommandLauncher(
  [string]$LauncherDirectory = (Join-Path $env:LOCALAPPDATA "Microsoft\WindowsApps")
) {
  $LauncherPath = Join-Path $LauncherDirectory "AskHuman.cmd"
  if (-not (Test-Path -LiteralPath $LauncherPath -PathType Leaf)) { return }

  $Content = [IO.File]::ReadAllText($LauncherPath)
  if (-not (Test-AskHumanManagedCommandLauncher $Content)) {
    Write-Warning "Preserving unmanaged command launcher: $LauncherPath"
    return
  }
  Remove-Item -LiteralPath $LauncherPath -Force
  Write-Host "==> Removed command launcher: $LauncherPath"
}
