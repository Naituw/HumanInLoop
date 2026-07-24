# 主 Agent 专属 AskHuman 协议与 Sub Agent Guard

> 状态：已实现
> 关联计划：`docs/plans/main-agent-only-interaction-protocol.md`
> 调研方式：仅静态阅读 HumanInLoop、Codex 源码与本机已安装 Agent 产物；未启动 Agent / Sub Agent，未进行计费实测。

## 背景

AskHuman 从同一模板生成 Cursor、Claude Code、Codex 的全局 rules，并通过 Grok 的全局 skill 承载
等价协议；生成器允许在共享正文前部加入 Agent 专属的 scope rule。部分 Agent 会把这些全局指令继续
提供给 Sub Agent，导致主 Agent 与 Sub Agent 都可能直接调用 AskHuman 提问、要求结束确认，产生多个
并发问题和混乱的交互归属。

本需求不尝试阻止全局规则被 Sub Agent 读取，而是在共享协议里增加清晰的角色边界，并在支持可靠
启动上下文注入的 Agent 上增加一层 Hook 兜底。

## 目标

1. AskHuman 的强制提问、反馈确认和结束标记协议只约束主 Agent。
2. Sub Agent 即使读到共享协议，也明确知道该协议不适用于自己，并且不得调用 AskHuman 提问。
3. 主 Agent 每次启动 Sub Agent 时，必须在委派任务中明确告知其 Sub Agent 身份以及不得使用 AskHuman。
4. Claude Code 与 Codex 在 Sub Agent 启动时额外注入同义提醒。
5. 已经启用 AskHuman 集成模式的用户看到“需更新”，由现有更新动作补齐新提示词和 Hook。

## 非目标

- 不让 Sub Agent 完全停止读取全局 rules / skill；各 Agent 的继承机制由上游控制。
- 不禁止 Sub Agent 在返回给父 Agent 的普通结果中表达疑问、假设或阻塞。
- 不在 AskHuman MCP server 端猜测调用者是不是 Sub Agent；单次 MCP 调用没有可靠的父子身份字段。
- 不为 Cursor 实现任务标记校验、拒绝后重试或其它复杂补偿流程。
- 不为 Grok 安装无法向模型注入上下文的空 Hook。
- 本阶段不做真实 Agent / Sub Agent 实测。

## 静态事实

| Agent | 全局协议可能进入 Sub Agent | SubagentStart Hook 注入能力 | 首期策略 |
| --- | --- | --- | --- |
| Codex | 是；非 root 会继承父会话的 user instructions | 有；`SubagentStart` 接受 `hookSpecificOutput.additionalContext`，并转成子会话 developer context | 共享提示词 + Hook 双保险 |
| Claude Code | 普通自定义 Sub Agent 会读取用户 / 项目 memory；内置 Explore / Plan 例外 | 有；`SubagentStart` 支持 `hookSpecificOutput.additionalContext` | 共享提示词 + Hook |
| Cursor 3.7.36 | 可能 | 响应结构虽解析 `additional_context`，但桌面与 CLI 两条启动路径都只消费 `permission` / `user_message` | 仅共享提示词；暂不装 Hook |
| Grok 0.2.93 | interaction-protocol skill 可能被主、子会话读取 | `SubagentStart` 为 passive Hook，stdout 不进入模型 | 仅共享 skill 提示词 |

## 决策

### D1：在原 protocol 内增加 Sub Agent 例外

`cli_reference()` 与 `mcp_reference()` 的 `<mandatory_interaction_protocol>` 最前面增加两条加粗规则：

```text
**This protocol does not apply to subagents. If you are a subagent, do not use AskHuman.**
**When starting a subagent, tell it that it is a subagent and must not use AskHuman.**
```

它们必须位于现有 “must apply under all circumstances” 文字之前。其余 protocol 原文不改。

不要求主 Agent 使用固定 sentinel 或逐字模板；自然语言表达清楚上述两点即可。

2026-07-24 补充：Codex 桌面版会在用户创建任务前运行 Suggested prompts 的 task-suggestion
generator。为让后台线程实际尝试 AskHuman、并能直接验证运行时 guard，Rules 不再为该角色增加文案
例外；Codex 与 Claude、Cursor、Grok 一样只保留上面的 subagent 例外。安全边界由运行时提供：
Codex MCP 从可信 turn metadata 精确拦截 `thread_source ∈ {system, ambient_suggestions}`
（见 `mcp.md`），Stop Hook 对这两种来源 / ephemeral thread 静默放行并留下审计日志
（见 `agent-stop-confirmation.md`）。

### D2：Grok skill 的常驻 description 同步角色边界

Grok skill 正文继续复用 `mcp_reference()`。frontmatter 的常驻 description 只替换原句：

```diff
- It ALWAYS applies, with no exceptions.
+ It ALWAYS applies, with one exception: if you are a subagent, it does not apply to you.
```

其余 description 原文不改。

### D3：Hook 只覆盖 Claude Code 与 Codex

新增独立托管的 `SubagentStart` Guard Hook：

- Claude Code：写入 `~/.claude/settings.json` 的 nested `SubagentStart` command hook。
- Codex：写入 `~/.codex/hooks.json` 的 nested `SubagentStart` command hook，并同步
  `~/.codex/config.toml` 的 hook trust hash。
- Cursor：首期不安装。当前 3.7.36 虽定义响应字段但不消费，安装会制造虚假的安全感。
- Grok：首期不安装。其 passive Hook stdout 不向模型注入。

Hook 调用新的隐藏子命令，例如：

```text
AskHuman __subagent-hook <claude|codex>
```

子命令不连接 daemon、不提问、不读取或保存会话数据，只向 stdout 输出对应事件的 JSON：

```json
{
  "hookSpecificOutput": {
    "hookEventName": "SubagentStart",
    "additionalContext": "You are a subagent. Do not use AskHuman."
  }
}
```

具体英文由 `prompts.rs` 中的单一函数生成，Hook 与共享 protocol 的测试共同锁定核心语义，避免文案漂移。
Hook 基础设施失败时 fail-open，不阻止 Sub Agent 创建。

### D4：Guard 跟随集成模式，不跟随 lifecycle / permission 开关

Guard 是共享指令包的一部分：

- Claude / Codex 处于 `Cli` 或 `Mcp` 模式时应存在。
- 模式切到 `None` 时移除本功能 marker 拥有的 Hook。
- 与实验性 lifecycle tracking、Stop confirmation、PermissionRequest preference 相互独立。
- 不新增用户开关，不自动为 `None` 模式安装。

Guard 状态纳入现有 Rule / Skill 更新口径，而不是新增设置行：

- 当前模式非 `None` 且 Guard 缺失、重复、命令路径过期、timeout 不匹配或 Codex trust 不完整时，
  `ruleNeedsUpdate = true`。
- 更新 Rule 单项时同时刷新规则正文并 reconcile Guard。
- 整包 `agents update` / 设置页整包更新继续通过 `agent_mode::set(current)` 补齐全部产物。

因此旧用户会因为提示词正文变化和 / 或 Guard 缺失看到现有“需更新”提示，点击现有 Rules 更新即可一次补齐，
无需新增前端概念。

### D5：配置编辑与所有权

- Guard 使用独立命令 marker `__subagent-hook`，只替换 / 删除含该 marker 的 handler。
- 保留同一 `SubagentStart` 事件中的用户 Hook、其它 AskHuman Hook、JSONC 注释与整体格式。
- 重复安装收敛为恰好一个期望 handler；卸载后若事件数组为空才删除事件键。
- Codex 写 hooks.json 与 trust 失败时沿用 Permission Hook 的回滚方式，恢复 hooks.json 与 config.toml。
- Hook 命令使用当前可执行文件绝对路径；二进制路径变化会判定过期。

## 兼容与升级

- 提示词正文变化会让已安装 rules / Grok skill 按现有精确内容比较自然标记为过期。
- Claude / Codex Guard 缺失也会使当前集成模式标记为需更新，即使 rules 正文已被手工提前替换。
- 不在应用启动时静默修改尚未选择更新的旧集成；用户通过现有更新动作完成迁移。
- Cursor / Grok 不显示 Guard 缺失，也不因此永久处于需更新状态。
- Rule/skill 正文或 Guard 需更新不阻止 IM `/new` 选择 Agent；集成 Tab 仍显示更新提示。
- 低层 `agent_rule_*` 状态仍描述 rules / skill 文件本身；面向用户的三态集成状态由
  `agent_mode` 聚合 rules 与 Guard。

## 验收标准

1. CLI、MCP、Grok 三份最终协议都先分流主 / 子角色，且 Sub Agent 分支没有反馈确认和结束标记要求。
2. 主 Agent 分支保留现有全部强制交互约束。
3. 协议明确要求主 Agent 在每次委派中告诉 Sub Agent 不得使用 AskHuman。
4. Claude / Codex 的 `SubagentStart` 配置可幂等安装、更新、卸载，并与用户 Hook 共存。
5. Hook 隐藏子命令只输出合法的 `SubagentStart additionalContext` JSON。
6. Codex Guard handler 的 trust hash 正确写入，失败可回滚。
7. 已启用 Claude / Codex 集成但缺 Guard 时显示需更新；更新 Rule 或整包更新后恢复最新状态。
8. Cursor / Grok 不安装 Guard，且不会因 Guard 不存在显示需更新。
9. 自动化验证不启动真实 Agent / Sub Agent；任何计费实测必须另行获得用户明确许可。
10. 四家 CLI / MCP Rules 均只含 subagent scope 例外，不含 task-suggestion generator 例外。
