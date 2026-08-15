[CmdletBinding()]
param(
  [switch]$Release
)

# 构建并安装 AskHuman 到用户目录（Windows）。
$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Split-Path -Parent $ScriptDir
$InstallDir = if ($env:INSTALL_DIR) { $env:INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "Programs\AskHuman" }
$BuildProfile = if ($Release) { "release" } else { "local-install" }
Set-Location $RepoRoot

if (-not (Get-Command pnpm -ErrorAction SilentlyContinue)) {
  Write-Error "需要 pnpm（npm i -g pnpm）"; exit 1
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  Write-Error "需要 Rust 工具链（https://rustup.rs）"; exit 1
}

# Show the same in-flight request warning as install.sh before replacing the binary.
if (Get-Command AskHuman -ErrorAction SilentlyContinue) {
  $StatusOut = & AskHuman daemon status 2>$null
  if ($LASTEXITCODE -eq 0 -and $StatusOut -match 'requests\s+(\d+) active' -and [int]$Matches[1] -gt 0) {
    Write-Host "提示: daemon 当前有 $($Matches[1]) 个在途请求；安装后将在它们完结后自动换新（期间新提问会等待）。"
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
$InstalledBin = Join-Path $InstallDir "AskHuman.exe"
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
  $StagedBin = Join-Path $InstallDir ".AskHuman.new.$PID.exe"
  try {
    Copy-Item -LiteralPath $Bin -Destination $StagedBin -Force
    $StagedHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $StagedBin).Hash.ToLowerInvariant()
    if ($StagedHash -ne $SourceHash) {
      throw "Staged AskHuman.exe hash does not match the build output"
    }
    Move-Item -LiteralPath $StagedBin -Destination $InstalledBin -Force
  } finally {
    Remove-Item -LiteralPath $StagedBin -Force -ErrorAction SilentlyContinue
  }
  $InstalledHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $InstalledBin).Hash.ToLowerInvariant()
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

Write-Host "==> 完成：$InstallDir\AskHuman.exe"
Write-Host "提示: 请将 $InstallDir 加入 PATH 后即可在终端使用 AskHuman。"
