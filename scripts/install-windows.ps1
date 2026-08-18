[CmdletBinding()]
param(
  [switch]$Release,
  [switch]$Global
)

# 构建并安装 AskHuman 到用户目录（Windows）。
$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Split-Path -Parent $ScriptDir
$ExplicitInstallDir = -not [string]::IsNullOrWhiteSpace($env:INSTALL_DIR)
$DefaultInstallDir = Join-Path $env:LOCALAPPDATA "Programs\AskHuman"
$DevRoot = $null
$Cursor = [IO.Path]::GetFullPath($RepoRoot)
while (-not [string]::IsNullOrWhiteSpace($Cursor)) {
  if (Test-Path -LiteralPath (Join-Path $Cursor ".askhuman-dev\enabled") -PathType Leaf) {
    $DevRoot = $Cursor
    break
  }
  $Parent = Split-Path -Parent $Cursor
  if ([string]::IsNullOrWhiteSpace($Parent) -or $Parent -eq $Cursor) { break }
  $Cursor = $Parent
}
$IsDevInstall = -not $ExplicitInstallDir -and -not $Global -and $null -ne $DevRoot
$InstallDir = if ($ExplicitInstallDir) {
  $env:INSTALL_DIR
} elseif ($IsDevInstall) {
  Join-Path $DevRoot ".askhuman-dev\bin"
} else {
  $DefaultInstallDir
}
$UsesDefaultInstallDir = -not $ExplicitInstallDir -and -not $IsDevInstall
$InstalledBin = Join-Path $InstallDir "AskHuman.exe"
$BuildProfile = if ($Release) { "release" } else { "local-install" }
Set-Location $RepoRoot
. (Join-Path $ScriptDir "windows-user-path.ps1")

if ($IsDevInstall) {
  $InstanceHome = Join-Path $DevRoot ".askhuman-dev\home"
  New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
  New-Item -ItemType Directory -Force -Path $InstanceHome | Out-Null
  $env:ASKHUMAN_HOME = $InstanceHome
  $env:ASKHUMAN_NO_KEYCHAIN = "1"
  Write-Host "==> Dev Instance 检测到: $DevRoot"
  Write-Host "    安装目标: $InstallDir"
}

if (-not (Get-Command pnpm -ErrorAction SilentlyContinue)) {
  Write-Error "需要 pnpm（npm i -g pnpm）"; exit 1
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  Write-Error "需要 Rust 工具链（https://rustup.rs）"; exit 1
}

# Show the same in-flight request warning as install.sh before replacing the binary.
if (Test-Path -LiteralPath $InstalledBin) {
  $StatusOut = & $InstalledBin daemon status 2>$null
  $StatusExitCode = $LASTEXITCODE
  $StatusMatch = [regex]::Match(($StatusOut | Out-String), 'requests\s+(\d+) active')
  if ($StatusExitCode -eq 0 -and $StatusMatch.Success -and [int]$StatusMatch.Groups[1].Value -gt 0) {
    $ActiveRequests = $StatusMatch.Groups[1].Value
    Write-Host "提示: daemon 当前有 $ActiveRequests 个在途请求；安装后将在它们完结后自动换新（期间新提问会等待）。"
    Write-Host "      立即换新: AskHuman daemon restart --force（会打断在途请求）"
  }
}

Write-Host "==> 安装前端依赖"
pnpm install
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

node scripts/build-frontend-if-needed.mjs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "==> 编译 $BuildProfile（前端资源在此步骤被嵌入）"
# --features custom-protocol：生产构建必须启用，否则二进制以 dev 模式连 devUrl 导致白屏。
cargo build --profile $BuildProfile --manifest-path src-tauri/Cargo.toml --features custom-protocol
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$Bin = "src-tauri\target\$BuildProfile\AskHuman.exe"
if (-not (Test-Path $Bin)) { Write-Error "未找到编译产物 $Bin"; exit 1 }

Write-Host "==> 安装到 $InstallDir"
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$InstallState = Join-Path $InstallDir ".askhuman-install-state"
$SourceHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $Bin).Hash.ToLowerInvariant()
$SkipCopy = $false
if ((Test-Path -LiteralPath $InstalledBin) -and (Test-Path -LiteralPath $InstallState)) {
  $state = @{}
  Get-Content -LiteralPath $InstallState | ForEach-Object {
    if ($_ -match '^([^=]+)=(.*)$') { $state[$Matches[1]] = $Matches[2] }
  }
  $InstalledHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $InstalledBin).Hash.ToLowerInvariant()
  if ($state.source -eq $SourceHash -and $state.installed -eq $InstalledHash) {
    $SkipCopy = $true
    Write-Host "    已安装二进制内容未变化，跳过复制"
  }
}

if (-not $SkipCopy) {
  $WorkerBin = [IO.Path]::GetFullPath((Resolve-Path -LiteralPath $Bin).Path)
  $TargetBin = [IO.Path]::GetFullPath($InstalledBin)
  if ($WorkerBin.Equals($TargetBin, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Build output and install target must be different files"
  }
  $VersionOut = (& $WorkerBin --version | Out-String).Trim()
  $VersionExitCode = $LASTEXITCODE
  $VersionMatch = [regex]::Match($VersionOut, '(\d+\.\d+\.\d+)')
  if ($VersionExitCode -ne 0 -or -not $VersionMatch.Success) {
    throw "Built AskHuman.exe did not report a valid version"
  }
  $ExpectedVersion = $VersionMatch.Groups[1].Value
  $TransactionRoot = Join-Path ([IO.Path]::GetTempPath()) ("askhuman_install_" + [guid]::NewGuid().ToString("N"))
  $TransactionPath = Join-Path $TransactionRoot "transaction.json"
  try {
    New-Item -ItemType Directory -Force -Path $TransactionRoot | Out-Null
    $Transaction = [ordered]@{
      target = $TargetBin
      worker = $WorkerBin
      expectedVersion = $ExpectedVersion
      expectedSha256 = $SourceHash
      preserveRuntimeState = $true
      restartDaemon = $false
      restartGuiHost = $false
      parentPid = 0
    }
    $Json = $Transaction | ConvertTo-Json -Compress
    [IO.File]::WriteAllText($TransactionPath, $Json, (New-Object Text.UTF8Encoding($false)))
    Write-Host "    正在安全排空后台进程并事务式替换二进制"
    & $WorkerBin __update-worker $TransactionPath
    if ($LASTEXITCODE -ne 0) {
      throw "AskHuman Windows install worker failed with exit code $LASTEXITCODE"
    }
  } finally {
    Remove-Item -LiteralPath $TransactionRoot -Recurse -Force -ErrorAction SilentlyContinue
  }
  $InstalledHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $InstalledBin).Hash.ToLowerInvariant()
  if ($InstalledHash -ne $SourceHash) {
    throw "Installed AskHuman.exe hash does not match the build output"
  }
  $StateTemp = "$InstallState.tmp.$PID"
  @("source=$SourceHash", "installed=$InstalledHash") | Set-Content -LiteralPath $StateTemp -Encoding ASCII
  Move-Item -LiteralPath $StateTemp -Destination $InstallState -Force
}

if (Get-Command cargo-sweep -ErrorAction SilentlyContinue) {
  Write-Host "==> 清理 7 天未使用的 target 依赖残留"
  Push-Location src-tauri
  cargo sweep --time 7
  if ($LASTEXITCODE -ne 0) { Write-Warning "cargo-sweep 清理失败，继续执行 profile 预算检查" }
  Pop-Location
}

function Get-ProfileSizeMB([string]$Path) {
  if (-not (Test-Path -LiteralPath $Path)) { return 0 }
  $sum = (Get-ChildItem -LiteralPath $Path -Recurse -File -Force -ErrorAction SilentlyContinue |
    Measure-Object -Property Length -Sum).Sum
  if ($null -eq $sum) { return 0 }
  return [math]::Ceiling($sum / 1MB)
}

function Enforce-ProfileBudget([string]$Profile, [string]$Path, [int]$LimitMB) {
  $before = Get-ProfileSizeMB $Path
  if ($before -le $LimitMB) { return }

  Write-Host "==> $Profile 缓存 ${before}MB 超过预算 ${LimitMB}MB；清理本项目产物"
  cargo clean --manifest-path src-tauri/Cargo.toml -p humaninloop --profile $Profile
  if ($LASTEXITCODE -ne 0) { Write-Warning "无法清理 $Profile 本项目缓存"; return }

  $after = Get-ProfileSizeMB $Path
  if ($after -gt $LimitMB) {
    Write-Host "==> $Profile 三方依赖缓存仍有 ${after}MB；执行 profile 级清理"
    cargo clean --manifest-path src-tauri/Cargo.toml --profile $Profile
    if ($LASTEXITCODE -ne 0) { Write-Warning "无法完成 $Profile profile 级清理"; return }
    $after = Get-ProfileSizeMB $Path
  }
  Write-Host "   $Profile 缓存: ${before}MB -> ${after}MB"
}

Enforce-ProfileBudget "local-install" "src-tauri\target\local-install" 4096
Enforce-ProfileBudget "dev" "src-tauri\target\debug" 6144
Enforce-ProfileBudget "full-debug" "src-tauri\target\full-debug" 6144
Enforce-ProfileBudget "release" "src-tauri\target\release" 4096

if (-not $IsDevInstall) {
  Add-AskHumanUserPath $InstallDir
}
if ($UsesDefaultInstallDir) {
  $LauncherDir = Join-Path $env:LOCALAPPDATA "Microsoft\WindowsApps"
  $KnownPath = "$env:Path;$([Environment]::GetEnvironmentVariable('Path', [EnvironmentVariableTarget]::User))"
  if (-not (Test-AskHumanPathContains $KnownPath $LauncherDir)) {
    throw "The standard Windows command-launcher directory is not on PATH: $LauncherDir"
  }
  Install-AskHumanCommandLauncher $InstallDir $LauncherDir | Out-Null
}
Write-Host "==> 完成：$InstallDir\AskHuman.exe"
if ($IsDevInstall) {
  Write-Host "==> Dev Instance 已安装；在本 worktree 内运行 AskHuman 会自动改道到此二进制。"
} else {
  Write-Host "==> AskHuman 命令已就绪；可直接运行 AskHuman --version。"
}
