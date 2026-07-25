# 接管 Claude AskUserQuestion —— 开发计划

> spec: `docs/specs/claude-ask-user-question.md`（D1–D13）
> 分支 `feat/claude-ask-user-question`，worktree `../HumanInLoop-todo-history`，Dev Instance popup-only。

## 概述

一条新的 `PreToolUse` hook 把 Claude 的内置选择题接过来，用 AskHuman 现成的多问题卡收集答案，
再按官方约定的 `allow + updatedInput` 把答案交还 Claude：

```
Claude 调 AskUserQuestion
   │
   ├─ PermissionRequest hook  → 一律不输出，让路（spec D2，与开关无关）
   │
   └─ PreToolUse hook（matcher=AskUserQuestion，开关开启时才安装）
         ├─ 入参 questions → AskHuman 多问题卡（弹窗 / 四个 IM）
         ├─ 作答     → allow + updatedInput{questions, answers}
         ├─ 取消     → deny + 「必须重新询问」
         └─ 设施故障 → 不输出，回落 Claude 原生选择框
```

## 阶段 1：开关与 hook 安装

新增 `src-tauri/src/integrations/agent_ask_question.rs`，整体照搬 `agent_stop.rs` 的形状
（capability preference + 幂等安装/卸载/状态 + 与 mode 的关系）：

- preference 落 `~/.askhuman/ask-question-preferences.json`（`paths.rs` 加一个路径函数），
  仅 Claude 一家，**默认 true**（spec D3）；
- 安装写入 Claude settings 的 `PreToolUse` 数组，条目形如
  `{"matcher": "AskUserQuestion", "hooks": [{"type": "command", "command": "\"<exe>\" __ask-question-hook claude", "timeout": 86400}]}`；
- 现有的 JSONC 最小编辑（`hook_edit.rs`）按「事件 + 命令标记」增删自己的条目，需要扩展成能带
  `matcher` 写入并按 matcher 精确识别本应用条目，不得碰用户已有的 `PreToolUse` 条目
  （lifecycle 的无 matcher activity 条目必须原样保留）；
- 状态判定复用既有 outdated 语义：命令路径变化或 timeout 缺失即判为需更新，由 daemon 启动时的
  幂等迁移一并处理；
- 关闭开关 → 卸载该条目；`agents mode = None` 时同 stop/permission 一样卸载 active 能力但保留偏好。

## 阶段 2：hook 运行时

新增顶层模块 `src-tauri/src/ask_question.rs`（与 `permissions.rs` 平级，同为「隐藏 hook 适配器」），
CLI 增隐藏子命令 `__ask-question-hook <agent>`（`cli/mod.rs` 分发，仅 `claude` 有效）。

流程：

1. 读 stdin JSON，校验 `hook_event_name == "PreToolUse"` 且 `tool_name == "AskUserQuestion"`；
   缺字段、超长（沿用 `permissions.rs` 的 stdin / tool_input 上限）→ 不输出。
2. 解析 `tool_input.questions`（1–4 道）。每题取 `question`、`header`、`options[].label`、
   `multiSelect`。任一题缺 `question` 或选项非法 → 不输出（让路）。
3. 构造 `TaskRequest`：
   - `questions[i].message` = `header · question`（`header` 为空则只有 question），
     `multiSelect == false` 的题追加本地化「（只选一项）」；
   - 选项原样映射为 `OptionItem`（不标推荐、不加前缀）；
   - `single = false`、`select_only = false`、`is_markdown = true`、`output_format = Json`；
   - `source` = Claude Code 的来源名，`agent_kind = "claude"`，`agent_session_id` = 输入的
     `session_id`，`project` = 输入 `cwd`，`record_history = true`，`caller_pid` = 自身 pid。
4. `client::run_ask_capture(task, 24h)` 提交并取回 stdout（JSON 形态）。
5. 解析结果并映射（纯函数，便于单测）：
   - `action == "cancel"` → deny 分支；
   - `answers` 为空或每一题都既无选项又无文本 → 同样走 deny（spec D8 末句）；
   - 否则逐题构造 `answers` map：key 是**原始题面**（不含 header 前缀与「（只选一项）」标注），
     value 依 spec D6 拼接：选中标签 `, ` 连接（单选题只取第一个），有自由文本再 `, ` 追加；
     该题若带附件，把每个绝对路径换行追加在 value 末尾（spec D7）；未作答的题不写 key（D8）。
6. 输出：
   - 作答 → `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow",
     "updatedInput":{"questions":<原样数组>,"answers":{…}}}}`（`updatedInput` 整体替换，
     必须回传原 `questions`）；
   - 取消 → `permissionDecision: "deny"` + `permissionDecisionReason` = `status.cancel` 的本地化文案；
   - 其它一切失败 → 不输出，exit 0。

## 阶段 3：权限卡让路

`permissions.rs` 的 `parse_permission`：Claude 且 `tool_name == "AskUserQuestion"` 时直接返回
`None`（不接管、不输出），位置在构造 context / choices 之前。不走 `AutoAllow`——官方明确对这类
工具单独 allow 不满足交互要求，不输出才是干净的让路（spec D2）。

## 阶段 4：设置页与文案

- `IntegrationTab.vue` 的 Claude 区块加一行开关（与「权限审批」「结束确认」并列），仅 Claude 显示；
  `useIntegration.ts` 加对应状态与 toggle，走新的 Tauri command（照搬 stop 的 `*_status` /
  `*_set` 形状）。
- i18n 新增：开关标题与说明、题面的「（只选一项）」标注。中英各一份。

## 阶段 5：测试

`cargo test`：

- 题目映射：header 前缀拼接（有 / 无 header）、单选标注只加在 `multiSelect == false` 的题上、
  选项原样、题数 1 与 4 的边界。
- 答案回传：单选取首项、多选 `, ` 连接、只文本、选项+文本混合、附件路径换行追加、部分作答只写
  已答题、全空 → 取消。
- 决策输出：allow 带完整 `updatedInput`（`questions` 原样回传）、deny 带本地化理由、
  非 AskUserQuestion / 坏输入不输出。
- 权限让路：`AskUserQuestion` 的 PermissionRequest 输入不产生任何输出。
- 安装：带 matcher 的条目写入 / 卸载幂等，用户已有的 `PreToolUse` 条目与 lifecycle 条目不受影响。

真机验收（用户已批准跑一次真实 Claude Code，最小场景）：一道两选项的题 + 一次自由输入 + 一次取消，
按 spec §6 的 10 条逐条走。

## 阶段 6：文档

- `docs/overview.md`：Agent 集成一节补一句「Claude 的内置提问可接管为 AskHuman 提问卡」，
  并说明权限卡对它一律让路；
- `docs/overview-configuration.md`：新增 preference 文件的位置说明；
- issue #5 在实现合并后回复说明。

## 提交拆分

1. `feat(claude): answer Claude's own questions from AskHuman`（阶段 1–3）
2. `feat(settings): add the Claude question takeover switch`（阶段 4）
3. `test(claude): cover question mapping, answers and decisions`（阶段 5，量小可并入 1）
4. `docs: describe the Claude AskUserQuestion takeover`（阶段 6）
