#!/usr/bin/env bash
# 构建并安装 AskHuman 到 ~/.local/bin（macOS / Linux）。
# 若 cwd 在已 `dev enable` 的 worktree 内且未传 --global，则装到该树
# `.askhuman-dev/bin`（见 docs/specs/dev-instance-parallel.md）。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

FORCE_GLOBAL=0
BUILD_PROFILE="local-install"
for arg in "$@"; do
  case "$arg" in
    --global) FORCE_GLOBAL=1 ;;
    --release) BUILD_PROFILE="release" ;;
    -h|--help)
      cat <<'EOF'
Usage: ./scripts/install.sh [--global] [--release]

  (default)  If the current directory is under a Dev Instance
             (worktree with .askhuman-dev/enabled), install into
             <root>/.askhuman-dev/bin; otherwise ~/.local/bin. Uses the
             fast local `local-install` Cargo profile.
  --global   Always install to ~/.local/bin (or $INSTALL_DIR if set).
  --release  Build with the production `release` profile instead.

Environment:
  INSTALL_DIR   Explicit install directory (wins over auto-detect;
                with --global, still defaults to ~/.local/bin when unset).
EOF
      exit 0
      ;;
    *)
      echo "错误: 未知参数 $arg（可用 --global、--release 或 --help）" >&2
      exit 1
      ;;
  esac
done

find_dev_enabled_root() {
  local dir
  dir="$(pwd)"
  while [ -n "$dir" ] && [ "$dir" != "/" ]; do
    if [ -f "$dir/.askhuman-dev/enabled" ]; then
      printf '%s\n' "$dir"
      return 0
    fi
    dir="$(dirname "$dir")"
  done
  return 1
}

DEFAULT_INSTALL_DIR="${HOME}/.local/bin"
if [ -n "${INSTALL_DIR:-}" ]; then
  : # explicit env wins
elif [ "$FORCE_GLOBAL" -eq 1 ]; then
  INSTALL_DIR="$DEFAULT_INSTALL_DIR"
elif DEV_ROOT="$(find_dev_enabled_root)"; then
  INSTALL_DIR="${DEV_ROOT}/.askhuman-dev/bin"
  mkdir -p "$INSTALL_DIR" "${DEV_ROOT}/.askhuman-dev/home"
  echo "==> Dev Instance 检测到: $DEV_ROOT"
  echo "    安装目标: $INSTALL_DIR"
else
  INSTALL_DIR="$DEFAULT_INSTALL_DIR"
fi

sign_via_gui_launchd() {
  local identity="$1"
  local target="$2"
  local sign_dir status_file log_file runner_file plist_file label service gui_rc

  sign_dir="$(mktemp -d "${TMPDIR:-/tmp}/askhuman-sign.XXXXXX")"
  status_file="$sign_dir/status"
  log_file="$sign_dir/codesign.log"
  runner_file="$sign_dir/sign.sh"
  plist_file="$sign_dir/sign.plist"
  label="com.naituw.askhuman-sign.$$"
  service="gui/$(id -u)/$label"

  {
    echo '#!/usr/bin/env bash'
    printf '/usr/bin/codesign -i %q --force --timestamp=none --sign %q %q > %q 2>&1\n' \
      "com.naituw.humaninloop" "$identity" "$target" "$log_file"
    printf 'rc=$?\nprintf "%%s\\n" "$rc" > %q\nexit "$rc"\n' "$status_file"
  } > "$runner_file"
  chmod 0700 "$runner_file"

  cat > "$plist_file" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>$label</string>
  <key>ProgramArguments</key>
  <array>
    <string>$runner_file</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
PLIST

  if ! launchctl bootstrap "gui/$(id -u)" "$plist_file"; then
    rm -rf "$sign_dir"
    return 1
  fi

  # A one-shot agent in the user's GUI launchd domain retains keychain access without opening
  # Terminal, even when the installer itself runs under a background Codex app-server.
  for _ in $(seq 1 300); do
    [ -f "$status_file" ] && break
    sleep 0.1
  done
  if [ ! -f "$status_file" ]; then
    echo "错误: 等待 GUI 会话正式签名超时" >&2
    launchctl bootout "$service" 2>/dev/null || true
    rm -rf "$sign_dir"
    return 1
  fi

  gui_rc="$(cat "$status_file")"
  cat "$log_file"
  launchctl bootout "$service" 2>/dev/null || true
  rm -rf "$sign_dir"
  [ "$gui_rc" = "0" ]
}

if ! command -v pnpm >/dev/null 2>&1; then
  echo "错误: 需要 pnpm（npm i -g pnpm）" >&2
  exit 1
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "错误: 需要 Rust 工具链（https://rustup.rs）" >&2
  exit 1
fi

# 在途请求提示：daemon 正服务中的提问不会被安装打断——换新会在它们完结后自动发生（graceful drain），
# 期间新提问会等待。此处只提示，不强杀。
if command -v AskHuman >/dev/null 2>&1; then
  ACTIVE="$(AskHuman daemon status 2>/dev/null | sed -n 's/.*requests[[:space:]]*\([0-9][0-9]*\) active.*/\1/p' | head -n1 || true)"
  if [ -n "${ACTIVE:-}" ] && [ "$ACTIVE" -gt 0 ] 2>/dev/null; then
    echo "提示: daemon 当前有 $ACTIVE 个在途请求；安装后将在它们完结后自动换新（期间新提问会等待）。"
    echo "      立即换新: AskHuman daemon restart --force（会打断在途请求）"
  fi
fi

echo "==> 安装前端依赖"
pnpm install

node scripts/build-frontend-if-needed.mjs

echo "==> 编译 $BUILD_PROFILE 二进制（前端资源在此步骤被嵌入）"
# --features custom-protocol：生产构建必须启用，否则二进制以 dev 模式连 devUrl 导致白屏。
cargo build --profile "$BUILD_PROFILE" --manifest-path src-tauri/Cargo.toml --features custom-protocol

BIN_PATH="src-tauri/target/$BUILD_PROFILE/AskHuman"
if [ ! -f "$BIN_PATH" ]; then
  echo "错误: 未找到编译产物 $BIN_PATH" >&2
  exit 1
fi

_file_sha256() {
  local path="$1"
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{print $1}'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
  else
    return 1
  fi
}

echo "==> 安装到 $INSTALL_DIR"
mkdir -p "$INSTALL_DIR"
INSTALLED_BIN="$INSTALL_DIR/AskHuman"
INSTALL_STATE="$INSTALL_DIR/.askhuman-install-state"
SOURCE_HASH="$(_file_sha256 "$BIN_PATH" 2>/dev/null || true)"
SKIP_COPY=0
if [ -n "$SOURCE_HASH" ] && [ -f "$INSTALLED_BIN" ] && [ -f "$INSTALL_STATE" ]; then
  STATE_SOURCE="$(sed -n 's/^source=//p' "$INSTALL_STATE" | head -n1)"
  STATE_INSTALLED="$(sed -n 's/^installed=//p' "$INSTALL_STATE" | head -n1)"
  INSTALLED_HASH="$(_file_sha256 "$INSTALLED_BIN" 2>/dev/null || true)"
  if [ "$STATE_SOURCE" = "$SOURCE_HASH" ] && [ "$STATE_INSTALLED" = "$INSTALLED_HASH" ]; then
    SKIP_COPY=1
    echo "    已安装二进制内容未变化，跳过复制与签名"
  fi
fi

if [ "$SKIP_COPY" -eq 0 ]; then
  cp "$BIN_PATH" "$INSTALLED_BIN"
  chmod 0755 "$INSTALLED_BIN"

  if [ "$(uname)" = "Darwin" ]; then
    # 清除 quarantine，降低拷贝后被 Gatekeeper 拦截的概率
    xattr -d com.apple.quarantine "$INSTALL_DIR/AskHuman" 2>/dev/null || true
    # Sign with a stable identity + fixed identifier so the OS keychain trusts the binary across
    # rebuilds (its designated requirement is cdhash-independent) → secret reads stay prompt-free.
    # Identity: $CODESIGN_IDENTITY if set, else prefer a "Developer ID Application" cert, else the
    # first available codesigning cert, else ad-hoc (per-build keychain prompts).
    #
    # Prefer Developer ID *deterministically*: `find-identity` order is not stable, and when both a
    # "Developer ID Application" and an "Apple Development" cert exist, picking whichever lands first
    # flips the binary's designated requirement between installs → the keychain ACL stops trusting it
    # → silent secret reads break (esp. for the background daemon). Developer ID's DR is also cdhash-
    # independent and non-expiring, so pinning it keeps the ACL valid across rebuilds.
    IDENTITY="${CODESIGN_IDENTITY:-}"
    if [ -z "$IDENTITY" ]; then
      IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null | awk '/Developer ID Application/{print $2; exit}')"
    fi
    if [ -z "$IDENTITY" ]; then
      IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null | awk '/^[[:space:]]*[0-9]+\)/{print $2; exit}')"
    fi
    [ -z "$IDENTITY" ] && IDENTITY="-"
    if [ "$IDENTITY" = "-" ]; then
      echo "==> 签名 (ad-hoc; 设置 CODESIGN_IDENTITY 可避免每次重装的钥匙串弹框)"
      codesign -i com.naituw.humaninloop --force --timestamp=none --sign "$IDENTITY" "$INSTALLED_BIN"
    else
      echo "==> 签名 (identity: $IDENTITY, identifier: com.naituw.humaninloop)"
      if ! codesign -i com.naituw.humaninloop --force --timestamp=none --sign "$IDENTITY" "$INSTALLED_BIN"; then
        echo "==> 后台进程无法使用正式签名私钥，改由用户 GUI 会话完成签名"
        sign_via_gui_launchd "$IDENTITY" "$INSTALLED_BIN" || {
          echo "错误: 正式签名失败，安装已中止" >&2
          exit 1
        }
      fi
    fi
    codesign --verify --strict "$INSTALLED_BIN"
  fi

  if [ -n "$SOURCE_HASH" ]; then
    INSTALLED_HASH="$(_file_sha256 "$INSTALLED_BIN" 2>/dev/null || true)"
    if [ -n "$INSTALLED_HASH" ]; then
      STATE_TMP="$INSTALL_STATE.tmp.$$"
      {
        printf 'source=%s\n' "$SOURCE_HASH"
        printf 'installed=%s\n' "$INSTALLED_HASH"
      } > "$STATE_TMP"
      mv "$STATE_TMP" "$INSTALL_STATE"
    fi
  fi
fi

# --- target/ 缓存清理 ---

# cargo-sweep 回收长期未使用的依赖 hash；profile 预算（下方）不依赖它存在。
if command -v cargo-sweep >/dev/null 2>&1; then
  echo "==> 清理 7 天未使用的 target 依赖残留"
  if ! ( cd src-tauri && cargo sweep --time 7 ); then
    echo "警告: cargo-sweep 清理失败，继续执行 profile 预算检查" >&2
  fi
fi

# Cargo 自己执行 package/profile 清理并持有 target lock，避免裸 rm 与并发构建竞争。
# 先只移除本项目产物、保留三方依赖；仍超预算才清整个 profile。
_profile_size_mb() {
  local dir="$1"
  [ -d "$dir" ] || { echo 0; return; }
  du -sk "$dir" 2>/dev/null | awk '{print int(($1 + 1023) / 1024)}'
}

_enforce_profile_budget() {
  local profile="$1" dir="$2" limit_mb="$3" size_mb after_mb
  size_mb="$(_profile_size_mb "$dir")"
  [ "$size_mb" -le "$limit_mb" ] && return 0

  echo "==> $profile 缓存 ${size_mb}MB 超过预算 ${limit_mb}MB；清理本项目产物"
  if ! cargo clean --manifest-path src-tauri/Cargo.toml -p humaninloop --profile "$profile"; then
    echo "警告: 无法清理 $profile 本项目缓存" >&2
    return 0
  fi
  after_mb="$(_profile_size_mb "$dir")"
  if [ "$after_mb" -gt "$limit_mb" ]; then
    echo "==> $profile 三方依赖缓存仍有 ${after_mb}MB；执行 profile 级清理"
    if ! cargo clean --manifest-path src-tauri/Cargo.toml --profile "$profile"; then
      echo "警告: 无法完成 $profile profile 级清理" >&2
      return 0
    fi
    after_mb="$(_profile_size_mb "$dir")"
  fi
  echo "   $profile 缓存: ${size_mb}MB → ${after_mb}MB"
}

_enforce_profile_budget "local-install" "src-tauri/target/local-install" 4096
_enforce_profile_budget "dev" "src-tauri/target/debug" 6144
_enforce_profile_budget "full-debug" "src-tauri/target/full-debug" 6144
_enforce_profile_budget "release" "src-tauri/target/release" 4096

echo "==> 完成：$INSTALL_DIR/AskHuman"
if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
  echo "提示: $INSTALL_DIR 不在 PATH 中，请将其加入 PATH。"
fi
