// 已支持「聚焦终端 / 激活 Tab」的终端类型（标识与后端 `agents::detect::terminal_kind` 一致）。
// 加新终端支持时：这里加标识 + 后端 `integrations/terminal_focus.rs` 补对应聚焦实现。
export const SUPPORTED_TERMINALS = new Set<string>([
  "apple-terminal",
  "iterm2",
  "windows-terminal",
]);

/** 该终端类型是否可激活到 Tab（决定「聚焦终端」按钮 / 箭头是否出现）。 */
export function isFocusableTerminal(kind?: string | null): boolean {
  return !!kind && SUPPORTED_TERMINALS.has(kind);
}
