//! Claude Code Hook 集成：安装/移除/状态 + 跨平台打开 settings.json。
//!
//! 作用与 Cursor Hook 对称：在 `~/.claude/settings.json` 注册一条 `PreToolUse`（matcher=`Bash`）
//! 命令钩子，检测到 Shell 调用 AskHuman 时把该次工具调用的 `timeout` 提升到 24 小时；同时把
//! `env.BASH_MAX_TIMEOUT_MS` 抬高到 24 小时，否则钩子设置的超时会被 Claude 默认上限（10 分钟）钳掉。
//!
//! settings.json 的增删为纯函数，便于单测（幂等、保留用户其它条目）。卸载只移除本应用注入的
//! `PreToolUse` 条目与脚本文件，**不动 `env`**（用户可能依赖该上限）。

use crate::paths;
use anyhow::{anyhow, Context, Result};
use jsonc_parser::cst::{CstNode, CstRootNode};
use jsonc_parser::json;
use jsonc_parser::ParseOptions;
use serde_json::Value;

/// 识别本应用条目的标记（脚本文件名）。
pub const MARKER: &str = "askhuman-timeout";

/// 抬高的 Bash 超时上限环境变量名。
pub const BASH_MAX_KEY: &str = "BASH_MAX_TIMEOUT_MS";

/// 目标超时上限（24 小时，毫秒），与脚本写入的 timeout 一致。
pub const BASH_MAX_MS: u64 = 86_400_000;

/// 钩子脚本内容（安装时写入并 chmod 0755）。
///
/// 读取 stdin 的 `tool_input`，命中 AskHuman 调用时返回 `permissionDecision:allow` + `updatedInput`
/// （把原 `tool_input` 原样回写、仅覆盖 `timeout`，规避 Claude「整体替换」语义丢字段）；未命中或
/// 解析失败时输出 `{}`，保持 fail-open。AskHuman 匹配正则与 Cursor 脚本逐字一致。
pub const SCRIPT_CONTENT: &str = r##"#!/usr/bin/env bash
# askhuman-timeout.sh
# 由 AskHuman 自动安装 / 移除，请勿手动编辑。
#
# 作用：作为 Claude Code 的 PreToolUse 钩子，检测 Bash 工具调用是否会执行 AskHuman
# 命令；命中时把该次工具调用 timeout 提升至 24 小时（86400000ms），防止等待用户回应
# 时被强制取消。未命中或解析失败时输出空对象 `{}`，保持 fail-open。

set -u

# 从 stdin 读取 JSON 输入
input=$(cat)

# 解析 .tool_input.command，按 python3 -> jq 顺序回退
command=""
if command -v python3 >/dev/null 2>&1; then
  command=$(printf '%s' "$input" | python3 -c '
import json
import sys

try:
    data = json.load(sys.stdin)
    print(data.get("tool_input", {}).get("command", ""))
except Exception:
    print("")
')
elif command -v jq >/dev/null 2>&1; then
  command=$(printf '%s' "$input" | jq -r '.tool_input.command // empty' 2>/dev/null)
fi

# 匹配 AskHuman 调用：兼顾行内任意位置 / 链式命令 / 引号包裹 / 绝对路径前缀
if [ -n "$command" ] && printf '%s' "$command" \
     | grep -Eq "(^|[[:space:];&|()\`\"'\\]|/)AskHuman([[:space:]]|$|[\"'\\])"; then
  # 命中：把原 tool_input 原样回写并覆盖 timeout
  if command -v python3 >/dev/null 2>&1; then
    output=$(printf '%s' "$input" | python3 -c '
import json
import sys

try:
    data = json.load(sys.stdin)
    ti = data.get("tool_input") or {}
    if not isinstance(ti, dict):
        ti = {}
    ti["timeout"] = 86400000
    print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": ti}}))
except Exception:
    print("{}")
')
  elif command -v jq >/dev/null 2>&1; then
    output=$(printf '%s' "$input" | jq -c '{hookSpecificOutput:{hookEventName:"PreToolUse",permissionDecision:"allow",updatedInput:((.tool_input // {}) + {timeout:86400000})}}' 2>/dev/null)
    [ -z "$output" ] && output='{}'
  else
    output='{}'
  fi
else
  output='{}'
fi

printf '%s\n' "$output"

exit 0
"##;

pub const SCRIPT_CONTENT_WINDOWS: &str = r#"# Managed by AskHuman. Do not edit.
$ErrorActionPreference = 'Stop'
try {
  $inputJson = [Console]::In.ReadToEnd()
  $data = $inputJson | ConvertFrom-Json
  $command = [string]$data.tool_input.command
  if ($command -match '(^|[\s;&|()`"''\\/])AskHuman(?=\s|$|["''\\])') {
    $toolInput = $data.tool_input
    if ($null -eq $toolInput) { $toolInput = [pscustomobject]@{} }
    $toolInput | Add-Member -NotePropertyName timeout -NotePropertyValue 86400000 -Force
    $result = @{ hookSpecificOutput = @{
      hookEventName = 'PreToolUse'
      permissionDecision = 'allow'
      updatedInput = $toolInput
    }}
    $result | ConvertTo-Json -Compress -Depth 100
  } else { '{}' }
} catch { '{}' }
"#;

pub fn supported() -> bool {
    cfg!(any(unix, windows))
}

/// settings.json 是否存在。
pub fn settings_exists() -> bool {
    paths::claude_settings_json().exists()
}

/// 是否已安装本应用条目。
pub fn is_installed() -> bool {
    read_value().map(|r| has_marker(&r)).unwrap_or(false)
}

/// 已安装但磁盘脚本与内置脚本不一致（或脚本缺失）→ 需更新。
pub fn needs_update() -> bool {
    if !is_installed() {
        return false;
    }
    match std::fs::read_to_string(paths::claude_hook_script()) {
        Ok(content) => content != desired_script_content(),
        Err(_) => true,
    }
}

/// 安装：写脚本（chmod 0755）+ 注册/更新 PreToolUse 条目 + 抬高 BASH_MAX_TIMEOUT_MS。
pub fn install() -> Result<String> {
    let script = paths::claude_hook_script();
    if let Some(dir) = script.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create script directory: {}", dir.display()))?;
    }
    atomic_write(&script, desired_script_content().as_bytes())
        .with_context(|| format!("failed to write script: {}", script.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .with_context(|| "failed to set script executable permission")?;
    }

    let text = read_text_or_default();
    let updated = apply_install(&text, &script_command(&script))?;
    write_text(&updated)?;

    let lang = crate::i18n::Lang::current();
    Ok(crate::i18n::tr(lang, "cmd.hookInstalled").to_string())
}

fn desired_script_content() -> &'static str {
    if cfg!(windows) {
        SCRIPT_CONTENT_WINDOWS
    } else {
        SCRIPT_CONTENT
    }
}

fn script_command(script: &std::path::Path) -> String {
    if cfg!(windows) {
        format!(
            "powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"{}\"",
            script.to_string_lossy()
        )
    } else {
        script.to_string_lossy().to_string()
    }
}

/// 更新：用最新脚本与条目覆盖（复用 install），同时确保 env 上限 ≥ 24h。
pub fn update() -> Result<String> {
    install()?;
    let lang = crate::i18n::Lang::current();
    Ok(crate::i18n::tr(lang, "cmd.hookUpdated").to_string())
}

/// 移除：删除本应用 PreToolUse 条目 + 删除脚本文件；保留 `env` 与其它条目。
pub fn uninstall() -> Result<String> {
    if let Ok(text) = std::fs::read_to_string(paths::claude_settings_json()) {
        let updated = apply_uninstall(&text)?;
        write_text(&updated)?;
    }
    let script = paths::claude_hook_script();
    if script.exists() {
        let _ = std::fs::remove_file(&script);
    }
    let lang = crate::i18n::Lang::current();
    Ok(crate::i18n::tr(lang, "cmd.hookRemoved").to_string())
}

/// 在文件管理器中定位 settings.json。
pub fn reveal() {
    let path = paths::claude_settings_json();
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .args(["-R", &path.to_string_lossy()])
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let dir = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or(path.clone());
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.to_string_lossy()))
            .spawn();
    }
}

/// 用系统默认程序打开 settings.json。
pub fn open() {
    let path = paths::claude_settings_json();
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&path).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(&path)
            .spawn();
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(&path).spawn();
    }
}

// MARK: - 标记判定（serde 值，供状态查询与测试）

fn group_has_marker(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|arr| {
            arr.iter().any(|entry| {
                entry
                    .get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| c.contains(MARKER))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// 任意 PreToolUse matcher 组含本应用脚本 → 已安装。
pub fn has_marker(root: &Value) -> bool {
    root.get("hooks")
        .and_then(|h| h.get("PreToolUse"))
        .and_then(|p| p.as_array())
        .map(|arr| arr.iter().any(group_has_marker))
        .unwrap_or(false)
}

/// CST 数组元素（PreToolUse 组）是否含本应用脚本。
fn group_node_has_marker(node: &CstNode) -> bool {
    node.to_serde_value()
        .map(|v| group_has_marker(&v))
        .unwrap_or(false)
}

/// 解析环境变量值（字符串或数字）为毫秒数。
fn parse_ms(v: &Value) -> Option<u64> {
    match v {
        Value::String(s) => s.trim().parse::<u64>().ok(),
        Value::Number(n) => n.as_u64(),
        _ => None,
    }
}

// MARK: - 文本变换（CST 保留格式最小化编辑，可测试）

/// 在 settings.json 文本中插入 / 更新本应用 PreToolUse 组（matcher=Bash），并确保
/// `env.BASH_MAX_TIMEOUT_MS` ≥ 24h。**仅触碰本应用注入的内容**，其余字节（缩进、键序、
/// 用户其它条目与 env）原样保留。解析失败返回 Err（调用方据此中止，不覆盖原文件）。
fn apply_install(text: &str, script_path: &str) -> Result<String> {
    let source = if text.trim().is_empty() { "{}" } else { text };
    let root = CstRootNode::parse(source, &ParseOptions::default())
        .map_err(|e| anyhow!("解析 settings.json 失败，已中止（不覆盖原文件）：{e}"))?;
    let root_obj = root
        .object_value_or_create()
        .ok_or_else(|| anyhow!("settings.json 根不是 JSON 对象，已中止"))?;

    // hooks.PreToolUse：就地替换本应用组（保留位置），多余重复清除；否则末尾追加。
    let hooks = root_obj
        .object_value_or_create("hooks")
        .ok_or_else(|| anyhow!("settings.json 的 'hooks' 不是对象，已中止"))?;
    let pre = hooks
        .array_value_or_create("PreToolUse")
        .ok_or_else(|| anyhow!("settings.json 的 'PreToolUse' 不是数组，已中止"))?;
    let mut replaced = false;
    for g in pre.elements() {
        if !group_node_has_marker(&g) {
            continue;
        }
        if !replaced {
            if let Some(obj) = g.as_object() {
                obj.replace_with(json!({
                    "matcher": "Bash",
                    "hooks": [ { "type": "command", "command": script_path } ],
                }));
                replaced = true;
                continue;
            }
        }
        g.remove();
    }
    if !replaced {
        pre.ensure_multiline();
        pre.append(json!({
            "matcher": "Bash",
            "hooks": [ { "type": "command", "command": script_path } ],
        }));
    }

    // env.BASH_MAX_TIMEOUT_MS ≥ 24h：缺失或现值更小则设为 24h；更大则保留。
    let env = root_obj
        .object_value_or_create("env")
        .ok_or_else(|| anyhow!("settings.json 的 'env' 不是对象，已中止"))?;
    let current = env.get(BASH_MAX_KEY).and_then(|p| p.to_serde_value());
    let need_set = current
        .and_then(|v| parse_ms(&v))
        .map(|v| v < BASH_MAX_MS)
        .unwrap_or(true);
    if need_set {
        if let Some(prop) = env.get(BASH_MAX_KEY) {
            prop.replace_with(BASH_MAX_KEY, json!(BASH_MAX_MS.to_string()));
        } else {
            env.append(BASH_MAX_KEY, json!(BASH_MAX_MS.to_string()));
        }
    }

    Ok(root.to_string())
}

/// 在 settings.json 文本中移除本应用 PreToolUse 组；若数组变空则删除该键。
/// **不动 `env`** 与用户其它条目。解析失败返回 Err。
fn apply_uninstall(text: &str) -> Result<String> {
    let root = CstRootNode::parse(text, &ParseOptions::default())
        .map_err(|e| anyhow!("解析 settings.json 失败，已中止（不覆盖原文件）：{e}"))?;
    let Some(root_obj) = root.object_value() else {
        return Ok(root.to_string());
    };
    let Some(hooks) = root_obj.object_value("hooks") else {
        return Ok(root.to_string());
    };
    if let Some(pre) = hooks.array_value("PreToolUse") {
        for g in pre.elements() {
            if group_node_has_marker(&g) {
                g.remove();
            }
        }
        if pre.elements().is_empty() {
            if let Some(prop) = hooks.get("PreToolUse") {
                prop.remove();
            }
        }
    }
    Ok(root.to_string())
}

// MARK: - 私有 IO

/// 读取 settings.json 文本；不存在 / 读失败 → 返回 "{}"（供安装时新建）。
fn read_text_or_default() -> String {
    std::fs::read_to_string(paths::claude_settings_json()).unwrap_or_else(|_| "{}".to_string())
}

/// 以 JSONC 解析 settings.json 为 serde 值，供状态查询。
fn read_value() -> Option<Value> {
    let text = std::fs::read_to_string(paths::claude_settings_json()).ok()?;
    let value: Value = jsonc_parser::parse_to_serde_value(&text, &ParseOptions::default()).ok()?;
    Some(value)
}

fn write_text(text: &str) -> Result<()> {
    let path = paths::claude_settings_json();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    atomic_write(&path, text.as_bytes())
}

fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = "/home/u/.claude/hooks/askhuman-timeout.sh";

    fn to_value(text: &str) -> Value {
        jsonc_parser::parse_to_serde_value(text, &ParseOptions::default()).unwrap()
    }

    #[test]
    fn install_into_empty_creates_group_and_env() {
        let out = apply_install("{}", SCRIPT).unwrap();
        let v = to_value(&out);
        assert!(has_marker(&v));
        let arr = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr[0]["matcher"], "Bash");
        assert_eq!(arr[0]["hooks"][0]["type"], "command");
        assert_eq!(arr[0]["hooks"][0]["command"], SCRIPT);
        assert_eq!(v["env"][BASH_MAX_KEY], BASH_MAX_MS.to_string());
    }

    #[test]
    fn install_is_idempotent() {
        let a = apply_install("{}", SCRIPT).unwrap();
        let b = apply_install(&a, SCRIPT).unwrap();
        let v = to_value(&b);
        assert_eq!(v["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(v["env"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn install_preserves_other_groups() {
        let input = "{ \"hooks\": { \"PreToolUse\": [ { \"matcher\": \"Edit\", \"hooks\": [ { \"type\": \"command\", \"command\": \"other.sh\" } ] } ] } }";
        let out = apply_install(input, SCRIPT).unwrap();
        let v = to_value(&out);
        let arr = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr.iter().any(|g| g["hooks"][0]["command"] == "other.sh"));
        assert!(has_marker(&v));
    }

    #[test]
    fn install_keeps_larger_env_value() {
        let bigger = (BASH_MAX_MS + 1).to_string();
        let input = format!("{{ \"env\": {{ \"{BASH_MAX_KEY}\": \"{bigger}\" }} }}");
        let out = apply_install(&input, SCRIPT).unwrap();
        let v = to_value(&out);
        assert_eq!(v["env"][BASH_MAX_KEY], bigger);
    }

    #[test]
    fn install_raises_smaller_env_value() {
        let input =
            format!("{{ \"env\": {{ \"{BASH_MAX_KEY}\": \"600000\", \"FOO\": \"bar\" }} }}");
        let out = apply_install(&input, SCRIPT).unwrap();
        let v = to_value(&out);
        assert_eq!(v["env"][BASH_MAX_KEY], BASH_MAX_MS.to_string());
        assert_eq!(v["env"]["FOO"], "bar", "env 其它键应保留");
    }

    #[test]
    fn install_preserves_user_comment_and_escaped_slashes() {
        let input = "{\n  // 用户注释，勿动\n  \"hooks\": {\n    \"PostToolUse\": [ { \"matcher\": \"Bash\", \"hooks\": [ { \"type\": \"command\", \"command\": \"curl http:\\/\\/x\" } ] } ]\n  }\n}";
        let out = apply_install(input, SCRIPT).unwrap();
        assert!(out.contains("// 用户注释，勿动"), "注释应原样保留");
        assert!(out.contains("http:\\/\\/x"), "转义斜杠应逐字节保留");
        assert!(out.contains("PostToolUse"));
        assert!(has_marker(&to_value(&out)));
    }

    #[test]
    fn install_replaces_existing_group_in_place() {
        let input = "{ \"hooks\": { \"PreToolUse\": [ { \"matcher\": \"Bash\", \"hooks\": [ { \"type\": \"command\", \"command\": \"old/askhuman-timeout.sh\" } ] } ] } }";
        let out = apply_install(input, SCRIPT).unwrap();
        let v = to_value(&out);
        let arr = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "本应用组应被替换而非新增");
        assert_eq!(arr[0]["hooks"][0]["command"], SCRIPT);
    }

    #[test]
    fn install_collapses_duplicate_groups() {
        let input = "{ \"hooks\": { \"PreToolUse\": [ { \"matcher\": \"Bash\", \"hooks\": [ { \"type\": \"command\", \"command\": \"a/askhuman-timeout.sh\" } ] }, { \"matcher\": \"Bash\", \"hooks\": [ { \"type\": \"command\", \"command\": \"b/askhuman-timeout.sh\" } ] } ] } }";
        let out = apply_install(input, SCRIPT).unwrap();
        let v = to_value(&out);
        let arr = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "重复的本应用组应被收敛为一条");
        assert_eq!(arr[0]["hooks"][0]["command"], SCRIPT);
    }

    #[test]
    fn install_aborts_on_non_object_root() {
        assert!(apply_install("[]", SCRIPT).is_err());
    }

    #[test]
    fn install_aborts_on_wrong_type_env() {
        // 解析成功但 env 不是对象 → 中止，绝不破坏。
        assert!(apply_install("{ \"env\": [] }", SCRIPT).is_err());
    }

    #[test]
    fn install_raises_numeric_env_value() {
        // 现值为数字且更小 → 抬高为 24h（统一存为字符串）。
        let input = "{ \"env\": { \"BASH_MAX_TIMEOUT_MS\": 600000 } }";
        let out = apply_install(input, SCRIPT).unwrap();
        let v = to_value(&out);
        assert_eq!(v["env"][BASH_MAX_KEY], BASH_MAX_MS.to_string());
    }

    #[test]
    fn install_is_byte_stable_fixpoint() {
        let a = apply_install("{}", SCRIPT).unwrap();
        let b = apply_install(&a, SCRIPT).unwrap();
        let c = apply_install(&b, SCRIPT).unwrap();
        assert_eq!(b, c, "已安装态再安装应为稳定不动点");
    }

    #[test]
    fn uninstall_noop_when_absent() {
        let input = "{ \"hooks\": { \"PreToolUse\": [ { \"matcher\": \"Edit\", \"hooks\": [ { \"type\": \"command\", \"command\": \"other.sh\" } ] } ] } }";
        let out = apply_uninstall(input).unwrap();
        let v = to_value(&out);
        assert!(!has_marker(&v));
        assert_eq!(
            v["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "other.sh"
        );
    }

    #[test]
    fn uninstall_removes_only_ours_and_keeps_env() {
        let input = "{ \"env\": { \"BASH_MAX_TIMEOUT_MS\": \"86400000\", \"FOO\": \"bar\" }, \"hooks\": { \"PreToolUse\": [ { \"matcher\": \"Edit\", \"hooks\": [ { \"type\": \"command\", \"command\": \"other.sh\" } ] }, { \"matcher\": \"Bash\", \"hooks\": [ { \"type\": \"command\", \"command\": \"x/askhuman-timeout.sh\" } ] } ] } }";
        let out = apply_uninstall(input).unwrap();
        let v = to_value(&out);
        let arr = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["hooks"][0]["command"], "other.sh");
        assert!(!has_marker(&v));
        assert_eq!(
            v["env"][BASH_MAX_KEY],
            BASH_MAX_MS.to_string(),
            "env 应不动"
        );
        assert_eq!(v["env"]["FOO"], "bar");
    }

    #[test]
    fn uninstall_drops_empty_pretooluse_key() {
        let input = "{ \"hooks\": { \"PreToolUse\": [ { \"matcher\": \"Bash\", \"hooks\": [ { \"type\": \"command\", \"command\": \"x/askhuman-timeout.sh\" } ] } ] } }";
        let out = apply_uninstall(input).unwrap();
        let v = to_value(&out);
        assert!(v["hooks"].get("PreToolUse").is_none(), "空数组应删除该键");
    }

    #[test]
    fn parse_error_aborts_without_overwrite() {
        assert!(apply_install("{ \"hooks\": ", SCRIPT).is_err());
        assert!(apply_uninstall("{ \"hooks\": ").is_err());
    }

    #[test]
    fn script_targets_askhuman() {
        assert!(SCRIPT_CONTENT.contains("AskHuman"));
        assert!(SCRIPT_CONTENT.contains("86400000"));
        assert!(SCRIPT_CONTENT.contains("permissionDecision"));
    }
}
