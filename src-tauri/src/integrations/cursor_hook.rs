//! Cursor Hook 集成：安装/移除/状态 + 跨平台打开 hooks.json。
//!
//! hooks.json 被 cursor-agent 当作 **JSONC** 解析，且用户可能写有注释 / 转义斜杠
//! （如 `http:\/\/...`，借此规避 `//` 被当行注释）。因此本模块以 `jsonc-parser` 的
//! **CST 做“保留格式的最小化编辑”**：只增删本应用注入的条目，其余字节（注释、缩进、
//! 转义、键序）原样保留；解析失败立即返回 Err 中止，**绝不整份重写覆盖用户文件**。

use crate::paths;
use anyhow::{anyhow, Context, Result};
use jsonc_parser::cst::{CstNode, CstRootNode};
use jsonc_parser::json;
use jsonc_parser::ParseOptions;
use serde_json::Value;

/// 识别本应用条目的标记（脚本文件名）。
pub const MARKER: &str = "askhuman-timeout";

/// 旧版标记，用于识别 / 清理历史安装（向后兼容）。
pub const LEGACY_MARKER: &str = "humaninloop-timeout";

/// 钩子脚本内容（安装时写入并 chmod 0755）。grep 正则与 Swift 版逐字一致。
pub const SCRIPT_CONTENT: &str = r##"#!/usr/bin/env bash
# askhuman-timeout.sh
# 由 AskHuman 自动安装 / 移除，请勿手动编辑。
#
# 作用：作为 Cursor 的 preToolUse 钩子，检测 Shell 工具调用是否会执行 AskHuman
# 命令；命中时将工具调用 timeout 提升至 24 小时（86400000ms），防止等待用户回应
# 时被强制取消。未命中或解析失败时输出空对象 `{}`，保持 fail-open。

set -u

# 从 stdin 读取 JSON 输入
input=$(cat)

# 解析 .tool_input.command，按 python3 -> jq -> 简单 grep 顺序回退
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
else
  command="$input"
fi

# 匹配 AskHuman 调用：兼顾行内任意位置 / 链式命令 / 引号包裹 / 绝对路径前缀
if [ -n "$command" ] && printf '%s' "$command" \
     | grep -Eq "(^|[[:space:];&|()\`\"'\\]|/)AskHuman([[:space:]]|$|[\"'\\])"; then
  output='{"updated_input": {"timeout": 86400000}}'
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
    @{ updated_input = @{ timeout = 86400000 } } | ConvertTo-Json -Compress -Depth 10
  } else { '{}' }
} catch { '{}' }
"#;

pub fn supported() -> bool {
    cfg!(any(unix, windows))
}

/// hooks.json 是否存在。
pub fn hooks_json_exists() -> bool {
    paths::cursor_hooks_json().exists()
}

/// 是否已安装本应用条目。
pub fn is_installed() -> bool {
    read_value().map(|r| has_marker(&r)).unwrap_or(false)
}

/// 已安装但磁盘脚本与内置脚本不一致（或脚本缺失 / 仅旧版脚本）→ 需更新。
pub fn needs_update() -> bool {
    if !is_installed() {
        return false;
    }
    match std::fs::read_to_string(paths::cursor_hook_script()) {
        Ok(content) => content != desired_script_content(),
        Err(_) => true,
    }
}

/// 安装：写脚本（chmod 0755）+ 在 hooks.json 注册/更新条目。
pub fn install() -> Result<String> {
    let script = paths::cursor_hook_script();
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

/// 更新：用最新脚本与条目覆盖（复用 install），并清理旧版脚本文件。
pub fn update() -> Result<String> {
    install()?;
    let legacy = paths::legacy_cursor_hook_script();
    if legacy.exists() {
        let _ = std::fs::remove_file(&legacy);
    }
    let lang = crate::i18n::Lang::current();
    Ok(crate::i18n::tr(lang, "cmd.hookUpdated").to_string())
}

/// 移除：删除本应用条目 + 删除脚本文件（保留他人条目）。
pub fn uninstall() -> Result<String> {
    if let Ok(text) = std::fs::read_to_string(paths::cursor_hooks_json()) {
        let updated = apply_uninstall(&text)?;
        write_text(&updated)?;
    }
    let script = paths::cursor_hook_script();
    if script.exists() {
        let _ = std::fs::remove_file(&script);
    }
    // 向后兼容：清理旧版脚本文件。
    let legacy = paths::legacy_cursor_hook_script();
    if legacy.exists() {
        let _ = std::fs::remove_file(&legacy);
    }
    let lang = crate::i18n::Lang::current();
    Ok(crate::i18n::tr(lang, "cmd.hookRemoved").to_string())
}

/// 在文件管理器中定位 hooks.json。
pub fn reveal() {
    let path = paths::cursor_hooks_json();
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .args(["-R", &path.to_string_lossy()])
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        // 无“选中”语义，定位到所在目录。
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

/// 用系统默认程序打开 hooks.json。
pub fn open() {
    let path = paths::cursor_hooks_json();
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

fn entry_has_marker(entry: &Value) -> bool {
    entry
        .get("command")
        .and_then(|c| c.as_str())
        .map(|c| c.contains(MARKER) || c.contains(LEGACY_MARKER))
        .unwrap_or(false)
}

/// 任意 preToolUse 条目含 marker → 已安装。
pub fn has_marker(root: &Value) -> bool {
    root.get("hooks")
        .and_then(|h| h.get("preToolUse"))
        .and_then(|p| p.as_array())
        .map(|arr| arr.iter().any(entry_has_marker))
        .unwrap_or(false)
}

/// CST 数组元素（preToolUse 条目）是否为本应用条目。
fn element_has_marker(node: &CstNode) -> bool {
    node.to_serde_value()
        .map(|v| entry_has_marker(&v))
        .unwrap_or(false)
}

// MARK: - 文本变换（CST 保留格式最小化编辑，可测试）

/// 在 hooks.json 文本中插入 / 更新本应用 preToolUse 条目；**仅触碰本应用条目**，
/// 其余内容（注释、缩进、转义、键序）逐字节保留。解析失败返回 Err（调用方据此中止）。
fn apply_install(text: &str, script_path: &str) -> Result<String> {
    let source = if text.trim().is_empty() { "{}" } else { text };
    let root = CstRootNode::parse(source, &ParseOptions::default())
        .map_err(|e| anyhow!("解析 hooks.json 失败，已中止（不覆盖原文件）：{e}"))?;
    let root_obj = root
        .object_value_or_create()
        .ok_or_else(|| anyhow!("hooks.json 根不是 JSON 对象，已中止"))?;
    let hooks = root_obj
        .object_value_or_create("hooks")
        .ok_or_else(|| anyhow!("hooks.json 的 'hooks' 不是对象，已中止"))?;
    let pre = hooks
        .array_value_or_create("preToolUse")
        .ok_or_else(|| anyhow!("hooks.json 的 'preToolUse' 不是数组，已中止"))?;

    // 已存在则就地替换（保留位置），多余的重复条目一并清除；不存在则末尾追加。
    let mut replaced = false;
    for el in pre.elements() {
        if !element_has_marker(&el) {
            continue;
        }
        if !replaced {
            if let Some(obj) = el.as_object() {
                obj.replace_with(json!({
                    "command": script_path,
                    "matcher": "Shell",
                }));
                replaced = true;
                continue;
            }
        }
        el.remove();
    }
    if !replaced {
        pre.ensure_multiline();
        pre.append(json!({
            "command": script_path,
            "matcher": "Shell",
        }));
    }
    Ok(root.to_string())
}

/// 在 hooks.json 文本中移除本应用 preToolUse 条目；若数组变空则删除该键。
/// 其余内容原样保留。解析失败返回 Err。
fn apply_uninstall(text: &str) -> Result<String> {
    let root = CstRootNode::parse(text, &ParseOptions::default())
        .map_err(|e| anyhow!("解析 hooks.json 失败，已中止（不覆盖原文件）：{e}"))?;
    let Some(root_obj) = root.object_value() else {
        return Ok(root.to_string());
    };
    let Some(hooks) = root_obj.object_value("hooks") else {
        return Ok(root.to_string());
    };
    if let Some(pre) = hooks.array_value("preToolUse") {
        for el in pre.elements() {
            if element_has_marker(&el) {
                el.remove();
            }
        }
        if pre.elements().is_empty() {
            if let Some(prop) = hooks.get("preToolUse") {
                prop.remove();
            }
        }
    }
    Ok(root.to_string())
}

// MARK: - 私有 IO

/// 读取 hooks.json 文本；不存在 / 读失败 → 返回 "{}"（供安装时新建）。
fn read_text_or_default() -> String {
    std::fs::read_to_string(paths::cursor_hooks_json()).unwrap_or_else(|_| "{}".to_string())
}

/// 以 JSONC 解析 hooks.json 为 serde 值，供状态查询（与 cursor-agent 解析语义一致）。
fn read_value() -> Option<Value> {
    let text = std::fs::read_to_string(paths::cursor_hooks_json()).ok()?;
    let value: Value = jsonc_parser::parse_to_serde_value(&text, &ParseOptions::default()).ok()?;
    Some(value)
}

fn write_text(text: &str) -> Result<()> {
    let path = paths::cursor_hooks_json();
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

    const SCRIPT: &str = "/home/u/.cursor/hooks/askhuman-timeout.sh";

    fn to_value(text: &str) -> Value {
        jsonc_parser::parse_to_serde_value(text, &ParseOptions::default()).unwrap()
    }

    #[test]
    fn install_into_empty_creates_entry() {
        let out = apply_install("{}", SCRIPT).unwrap();
        let v = to_value(&out);
        assert!(has_marker(&v));
        assert_eq!(v["hooks"]["preToolUse"][0]["matcher"], "Shell");
        assert_eq!(v["hooks"]["preToolUse"][0]["command"], SCRIPT);
    }

    #[test]
    fn install_into_blank_text_creates_entry() {
        let out = apply_install("   \n", SCRIPT).unwrap();
        assert!(has_marker(&to_value(&out)));
    }

    #[test]
    fn install_is_idempotent() {
        let a = apply_install("{}", SCRIPT).unwrap();
        let b = apply_install(&a, SCRIPT).unwrap();
        let v = to_value(&b);
        assert_eq!(v["hooks"]["preToolUse"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn install_preserves_user_comment_and_escaped_slashes() {
        // 用户已有 afterFileEdit（含 http:\/\/，借转义规避 // 被当行注释）+ 注释。
        // 安装本应用条目时，这些字节必须原样保留。
        let input = "{\n  // 用户注释，勿动\n  \"hooks\": {\n    \"afterFileEdit\": [\n      { \"command\": \"curl http:\\/\\/127.0.0.1:8080\\/x\" }\n    ]\n  }\n}";
        let out = apply_install(input, SCRIPT).unwrap();
        assert!(out.contains("// 用户注释，勿动"), "注释应原样保留");
        assert!(
            out.contains("http:\\/\\/127.0.0.1:8080\\/x"),
            "用户的转义斜杠应逐字节保留，实际：{out}"
        );
        assert!(out.contains("afterFileEdit"));
        assert!(has_marker(&to_value(&out)));
    }

    #[test]
    fn install_preserves_other_pretooluse_entries() {
        let input = "{ \"hooks\": { \"preToolUse\": [ { \"command\": \"other.sh\", \"matcher\": \"Shell\" } ] } }";
        let out = apply_install(input, SCRIPT).unwrap();
        let v = to_value(&out);
        let arr = v["hooks"]["preToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr.iter().any(|e| e["command"] == "other.sh"));
        assert!(has_marker(&v));
    }

    #[test]
    fn install_replaces_legacy_entry_in_place() {
        let input =
            "{ \"hooks\": { \"preToolUse\": [ { \"command\": \"x/humaninloop-timeout.sh\", \"matcher\": \"Shell\" } ] } }";
        let out = apply_install(input, SCRIPT).unwrap();
        let v = to_value(&out);
        let arr = v["hooks"]["preToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "旧条目应被替换而非新增");
        assert_eq!(arr[0]["command"], SCRIPT);
    }

    #[test]
    fn install_collapses_duplicate_entries() {
        // 历史脏数据：两条本应用条目 → 安装后收敛为一条。
        let input = "{ \"hooks\": { \"preToolUse\": [ { \"command\": \"a/askhuman-timeout.sh\", \"matcher\": \"Shell\" }, { \"command\": \"b/askhuman-timeout.sh\", \"matcher\": \"Shell\" } ] } }";
        let out = apply_install(input, SCRIPT).unwrap();
        let v = to_value(&out);
        let arr = v["hooks"]["preToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "重复的本应用条目应被收敛为一条");
        assert_eq!(arr[0]["command"], SCRIPT);
    }

    #[test]
    fn install_aborts_on_non_object_root() {
        assert!(apply_install("[]", SCRIPT).is_err());
    }

    #[test]
    fn install_aborts_on_wrong_type_hooks() {
        // 解析成功但 hooks 不是对象 → 中止，绝不破坏用户结构。
        assert!(apply_install("{ \"hooks\": [] }", SCRIPT).is_err());
    }

    #[test]
    fn install_aborts_on_wrong_type_pretooluse() {
        assert!(apply_install("{ \"hooks\": { \"preToolUse\": {} } }", SCRIPT).is_err());
    }

    #[test]
    fn install_is_byte_stable_fixpoint() {
        // 已安装态再次安装应收敛到稳定不动点（就地替换不再改变字节）。
        let a = apply_install("{}", SCRIPT).unwrap();
        let b = apply_install(&a, SCRIPT).unwrap();
        let c = apply_install(&b, SCRIPT).unwrap();
        assert_eq!(b, c, "已安装态再安装应为稳定不动点");
    }

    #[test]
    fn uninstall_noop_when_absent() {
        // 未安装本应用条目时卸载：不报错、不动他人条目。
        let input = "{ \"hooks\": { \"preToolUse\": [ { \"command\": \"other.sh\", \"matcher\": \"Shell\" } ] } }";
        let out = apply_uninstall(input).unwrap();
        let v = to_value(&out);
        assert!(!has_marker(&v));
        assert_eq!(v["hooks"]["preToolUse"][0]["command"], "other.sh");
    }

    #[test]
    fn uninstall_removes_only_ours() {
        let input = "{ \"hooks\": { \"preToolUse\": [ { \"command\": \"other.sh\", \"matcher\": \"Shell\" }, { \"command\": \"x/askhuman-timeout.sh\", \"matcher\": \"Shell\" } ] } }";
        let out = apply_uninstall(input).unwrap();
        let v = to_value(&out);
        let arr = v["hooks"]["preToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["command"], "other.sh");
        assert!(!has_marker(&v));
    }

    #[test]
    fn uninstall_drops_empty_pretooluse_key() {
        let input = "{ \"hooks\": { \"preToolUse\": [ { \"command\": \"x/askhuman-timeout.sh\", \"matcher\": \"Shell\" } ] } }";
        let out = apply_uninstall(input).unwrap();
        let v = to_value(&out);
        assert!(v["hooks"].get("preToolUse").is_none(), "空数组应删除该键");
    }

    #[test]
    fn uninstall_preserves_user_content() {
        let input = "{\n  // 用户注释\n  \"hooks\": {\n    \"afterFileEdit\": [ { \"command\": \"curl http:\\/\\/x\" } ],\n    \"preToolUse\": [ { \"command\": \"x/askhuman-timeout.sh\", \"matcher\": \"Shell\" } ]\n  }\n}";
        let out = apply_uninstall(input).unwrap();
        assert!(out.contains("// 用户注释"));
        assert!(out.contains("http:\\/\\/x"));
        assert!(out.contains("afterFileEdit"));
        assert!(!has_marker(&to_value(&out)));
    }

    #[test]
    fn parse_error_aborts_without_overwrite() {
        // 非法 JSON（未闭合）→ Err，调用方据此中止、不写文件。
        assert!(apply_install("{ \"hooks\": ", SCRIPT).is_err());
        assert!(apply_uninstall("{ \"hooks\": ").is_err());
    }

    #[test]
    fn script_targets_askhuman() {
        assert!(SCRIPT_CONTENT.contains("AskHuman"));
        assert!(SCRIPT_CONTENT.contains("86400000"));
    }
}
