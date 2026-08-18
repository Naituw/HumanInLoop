# 需求：按 Agent 分组的「全局提示词（Rules）」配置入口

> 状态：待确认（按计划实现前需我确认）
> 关联计划：`docs/plans/agent-rules-config.md`

## 1. 背景

设置页现有「集成」Tab 里只有两块：

- **参考提示词**：只读展示 `prompts::cli_reference()` + 复制按钮；用户需自己把这段提示词粘到各自 AI 工具的规则里。
- **Cursor Hook**：一键写入 `~/.cursor/hooks.json` 的 `preToolUse` 钩子，命中 Shell 调用 `AskHuman` 时把超时拉到 24h。

痛点：用户拿到提示词后，还得自己知道「该粘到哪个文件 / 哪个设置项」。我们希望在这一页直接给出 **Cursor、Claude Code、Codex** 三个 Agent 的配置入口，让用户一键把推荐提示词装进各 Agent 的**全局规则**里。

经核查（含官方文档、社区与已安装二进制源码）确定三个 Agent 的全局（用户级、跨项目）提示词落点：

- **Cursor**：全局文件规则放 `~/.cursor/rules/*.mdc`。已读 **Cursor Agent CLI（2026.06.04）** 与 **Cursor.app IDE（2026-06-07 构建）** 的打包代码，二者都：
  - 规则目录加载器只认 `**/.cursor/rules/**/*.mdc`（`includeGlobs:["**/*.mdc"]`），**整个包里 `RULE.md` 出现 0 次**；论坛官方员工所说的「RULE.md 格式」与实际二进制不符。
  - 规则加载是「从工作区目录**逐级父目录回溯到文件系统根 `/`**」，每层读 `<dir>/.cursor/rules/**/*.mdc` 与 `<dir>/AGENTS.md`（及开启 third-party 时的 CLAUDE.md）。因此当**项目位于 `~` 之下**时，途经 `~` 即会加载 `~/.cursor/rules/**/*.mdc`；项目在 `~` 之外（/tmp、/opt、外置盘）则不会被加载。
- **Claude Code**：全局自动加载的只有共享文件 `~/.claude/CLAUDE.md`（用户级 memory）。`.claude/rules/` 是**项目级**，无受支持的全局 `~/.claude/rules/` 自动加载。
- **Codex**：全局自动加载的只有共享文件 `~/.codex/AGENTS.md`。`@include` / `~/.codex/instructions/` 目前仍是未实现的社区提案；`AGENTS.override.md` 会整体顶替 `AGENTS.md`（会盖掉用户已有全局内容），不可用。

## 2. 目标

- 把原「集成」Tab 重做成**按 Agent 分组**的配置页：顶部一张共用「参考提示词」卡，其下 Cursor / Claude Code / Codex 三组，每组含「Rules」（及 Cursor 的「Hook」）子项。
- 三个 Agent 的 Rules 都用**同一份**推荐提示词（即 `cli_reference()`），不做专属提示词。
- 每个 Rules 子项提供与现有 Cursor Hook 一致的「**安装 / 卸载 / 定位**」交互：未安装→显示「安装」；已安装→显示「卸载」+「定位/打开」。
- 安装行为对**独占文件**直接写完整内容、对**共享文件**用带标记的托管区块写入，**绝不破坏用户已有内容**，且卸载可干净移除。
- 紧凑布局，压缩留白，避免页面过高。

## 3. 已确认决策

| 编号 | 决策项 | 结论 |
|---|---|---|
| D1 | Tab 名称 | 「集成」改名 **Agent**；中英文都用 `Agent`，**不翻译** |
| D2 | 顶部卡片 | 保留「参考提示词」卡（完整 `cli_reference()` + 复制）；`promptDesc` 文案改为「复制后添加到你的 Agent 的 Rules 中」/ EN：`Copy and add it to your Agent's rules.` |
| D3 | 提示词内容 | 三个 Agent 共用同一份 `cli_reference()`，**无专属提示词** |
| D4 | 统一交互 | 三个 Agent 的 Rules 子项统一「安装 / 卸载 / 定位」，与现有 Cursor Hook 卡一致（未装→安装；已装→卸载 + 定位/打开） |
| D5 | Cursor Rules 落点 | **独占文件** `~/.cursor/rules/askhuman.mdc`：内容 = `alwaysApply:true` frontmatter +「独占文件头标记」+ 完整 `cli_reference()`。安装=创建/覆盖该文件；卸载=**仅当文件含本应用头标记时**删除该文件 |
| D6 | Claude Code Rules 落点 | **共享文件** `~/.claude/CLAUDE.md` 内的「托管区块」（完整 `cli_reference()`），**不用 import / 不建独立文件**（见 §6 决策依据） |
| D7 | Codex Rules 落点 | **共享文件** `~/.codex/AGENTS.md` 内的「托管区块」（完整 `cli_reference()`） |
| D8 | 标记格式 | 托管区块：`<!-- AskHuman:begin DO NOT EDIT (managed by AskHuman) -->` … `<!-- AskHuman:end -->`；独占文件头：`<!-- AskHuman:managed-file DO NOT EDIT (managed by AskHuman) -->`。均为 Markdown 注释，不渲染 |
| D9 | 区块 upsert/remove | 安装：区块存在→替换其内部、不存在→追加到文件末尾（前置一个空行）；卸载：删整段（含两行标记）并清理多余空行；**两者都不动用户其它内容** |
| D10 | 「已安装」判定 | Cursor=`askhuman.mdc` 存在且含 `managed-file` 头标记；Claude=`CLAUDE.md` 含 `AskHuman:begin` 区块；Codex=`AGENTS.md` 含 `AskHuman:begin` 区块 |
| D11 | Cursor 作用范围提示 | UI 给小字：Cursor 全局规则仅当**项目位于 home 目录之下**时生效；其它位置请到 Cursor Settings 手动配置 |
| D12 | Cursor Hook 子项 | 保留安装/卸载 + 打开 hooks.json，负责 24h 超时；Unix 生成 `.sh`，Windows 生成 PowerShell 5 兼容 `.ps1`。 |
| D13 | Claude/Codex Hook 子项 | Claude timeout hook 已实现，产物同样按平台生成 `.sh` / `.ps1`；Codex 使用原生 hooks 与 AskHuman 协议，不需要独立 timeout 脚本。 |
| D14 | 布局 | 三组配置卡片**紧凑**，压缩留白 |
| D15 | 跨平台 | Cursor/Claude/Codex 的 **Rules 与 Hook 文件读写为跨平台**（Windows 用 `%USERPROFILE%`）；「定位」复用现有 reveal 思路（mac `open -R` / linux 打开所在目录 / win `explorer /select`），「打开」用系统默认程序。Windows Hook 命令使用原生绝对路径与 PowerShell 5 兼容脚本。 |

## 4. 约束与既有规则（不可破坏）

- **不破坏用户内容**：对共享文件（CLAUDE.md / AGENTS.md）只在自有 `begin/end` 区块内增删；对独占文件只在带本应用标记时才删除。
- **幂等**：重复安装不产生重复区块/重复文件内容；卸载后再装可还原。
- **stdout / 结果契约不变**：本需求仅涉及设置窗口与文件读写，不触碰 CLI 输出契约、退出码、弹窗/渠道逻辑。
- **沿用现有 i18n 双语**：所有新文案补齐中/英（Tab 名 `Agent` 除外，两语一致）。
- **沿用现有「marker + upsert + remove」幂等思路**（与 `integrations/cursor_hook.rs` 一致，纯函数可单测）。

## 5. 验收标准

1. 设置页 Tab 显示为 **Agent**（中英一致）；顶部「参考提示词」卡文案为「复制后添加到你的 Agent 的 Rules 中」/英文对应。
2. **Cursor Rules**：点「安装」后 `~/.cursor/rules/askhuman.mdc` 被创建，首行是 `alwaysApply:true` frontmatter、随后是 `managed-file` 头标记与完整提示词；状态变「已安装」并出现「卸载」「定位」。点「卸载」后文件被删除；手动放一个无标记的同名文件时「卸载」不应删它。
3. **Claude Code Rules**：点「安装」后 `~/.claude/CLAUDE.md`（不存在则创建）末尾出现 `AskHuman:begin…end` 托管区块且含完整提示词；文件中用户原有内容原样保留。重复「安装」不重复区块（替换内部）。「卸载」后仅该区块被移除，其它内容不变。
4. **Codex Rules**：同 3，作用于 `~/.codex/AGENTS.md`。
5. 三个 Rules 子项的「已安装/未安装」状态在重开设置页后判定正确。
6. Cursor 分组内仍有可用的 **Hook** 子项（行为同现状）；Claude/Codex 的 Hook 子项显示「即将支持」占位。
7. UI 给出 Cursor「项目需在 home 目录下」的小字提示；三组卡片布局紧凑、页面高度合理。
8. 纯函数（区块 upsert/remove、独占文件构建/识别）具备单测并通过；`./scripts/install.sh` 编译通过，安装后的 `AskHuman --settings` 行为符合上述。

## 6. 关键决策依据（备查）

- **为什么 Cursor 用 `.mdc` 而非 `RULE.md`**：CLI（`~/.local/share/cursor-agent/.../index.js`）与 IDE（`Cursor.app/.../workbench.desktop.main.js`）规则目录加载器均写死 `**/*.mdc`，全包 `RULE.md` 0 次命中。
- **为什么 Cursor 是「项目在 home 之下才生效」**：规则加载按父目录回溯到 `/`，`~/.cursor/rules` 只是回溯途中的一站。
- **为什么 Claude Code 不用 import**：在已有 `begin/end` 标记保护下，独立文件 + `@import` 的唯一实质收益只是「CLAUDE.md 足迹更小」；代价是 Claude 对「项目外 import」首次弹审批（拒绝则规则静默失效）、import 已知 quirks、双文件复杂度、且与 Codex 不对称。直接写区块更稳、更省、与 Codex 一致。
- **为什么 Codex 只能写共享 AGENTS.md**：`@include` / `~/.codex/instructions/` 仍为未实现提案；`AGENTS.override.md` 会整体顶替用户全局文件，不可用。

## 7. 已知限制 / 后续

- 推荐提示词内含「把 Shell 工具超时设为 24h」这条。**Cursor 由现有 Hook 自动保障**；**Claude Code / Codex 暂无超时保障**（其 Hook 为后续），在它们各自 Hook 落地前，长时间等待可能被各自默认工具超时截断。该限制可接受，后续随 Claude/Codex 的 Hook 子项补齐。
