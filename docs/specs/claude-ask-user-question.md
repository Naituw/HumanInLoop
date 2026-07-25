# 接管 Claude Code 的 AskUserQuestion

> 状态：设计定案（2026-07-25，四轮 AskHuman 评审），**未实现**。
> 实现分支：`feat/claude-ask-user-question`（worktree `../HumanInLoop-todo-history`，Dev Instance popup-only）。
> 触发来源：GitHub issue #5「option 形式的 ask，经常会变成只有『允许』和『拒绝』」。
> 依赖 / 复用：`docs/plans/agent-permission-approval.md`（PermissionRequest hook 与确认链路）、
> `docs/specs/multi-question.md`（多问题卡）、`docs/specs/strict-choice-and-structured-output.md`
> （JSON 输出契约）、`docs/specs/reply-history.md`。

## 1. 问题

Claude Code 有个内置工具 `AskUserQuestion`：它向人提 1–4 道选择题（每题带选项，可多选，也可以自己
输入文本）。这是 Claude 自己的提问机制，与 AskHuman 无关。

AskHuman 装了 `PermissionRequest` hook 之后，这个工具调用被当成一次普通的工具审批接管，渲染成
「Agent 请求调用以下工具 / 允许 / 拒绝」的确认卡——**原本的问题和选项全部丢失**，人在手机上只能
看到一个无从判断的授权请求，而终端里 Claude 的原生选择框仍然正常。issue #5 的两张截图正是这个对照。

## 2. 需求

1. **不要再用权限卡拦它**：`AskUserQuestion` 是「问你」，不是「动你的东西」，本来就不该走授权流程。
2. **把它接管成 AskHuman 的提问卡**：题面、选项、多选、自由输入原样呈现，人在弹窗或任意 IM 上作答，
   答案回传给 Claude，终端不再弹它自己的选择框。
3. **取消要用 AskHuman 的语义**：人明确取消时，必须让 Claude 重新询问，而不是悄悄退回原生提问框。

## 3. 上游能力（已核对官方文档）

**工具入参**（`tool_input`）：

```json
{
  "questions": [
    { "question": "Which framework?", "header": "Framework",
      "options": [{ "label": "React" }, { "label": "Vue" }], "multiSelect": false }
  ]
}
```

- `questions` 1–4 道；`header` 是短标题；`multiSelect` 是**每题各自**的。
- `answers` 是可选出参字段，形如 `{"Which framework?": "React"}`，多选把标签用逗号连接。
  Claude 自己从不填它，**由 hook 通过 `updatedInput` 提供，即可代替原生对话框作答**。

**PreToolUse 决策**（`hookSpecificOutput`）：

| 字段 | 语义 |
| --- | --- |
| `permissionDecision` | `allow` / `deny` / `ask` / `defer`；多个 hook 同时返回时 `deny` > `defer` > `ask` > `allow` |
| `permissionDecisionReason` | `deny` 时**给 Claude 看**；`allow`/`ask` 时只给用户看 |
| `updatedInput` | **整体替换** input 对象，未改字段也要一并回传 |
| `additionalContext` | 追加进 Claude 上下文的一段文本 |

官方对这类工具的明确说明：`AskUserQuestion` 与 `ExitPlanMode` 需要人交互，`allow` **必须配合
`updatedInput`** 才算满足交互要求——「hook 从 stdin 读入参，用你自己的 UI 收集答案，再放进
`updatedInput` 回传」。`defer` 只在 headless（`-p`）下有效，交互式会话忽略，故本设计不使用。

**原生对部分作答的处理**：问题默认一直挂着；设了 `askUserQuestionTimeout` 时超时自动关闭，
**提交已选中的选项**并告诉 Claude 人可能不在座位上、可稍后再问。即原生允许部分作答。

## 4. 设计定案

### D1 接管点：新增一条带 matcher 的 PreToolUse hook

新增 `PreToolUse` + `matcher: "AskUserQuestion"` → `AskHuman __ask-question-hook claude`，
`timeout` 86400（与权限 hook 同口径，等人期间不被判超时）。不改动现有的 PermissionRequest hook，
也不合并进 lifecycle 的无 matcher PreToolUse 条目。

### D2 权限卡对 AskUserQuestion 一律让路

`PermissionRequest` hook 遇到 `tool_name == "AskUserQuestion"` 时**直接不输出**（不是回 allow），
交回 Claude 原生流程。**与 D3 的开关无关**：开关关着时人应看到 Claude 自己的选择框，而不是我们的
授权卡。不用 allow 是因为官方明确「对这类工具单独 allow 不足以满足交互要求」，不输出才是干净的让路。

### D3 独立开关，默认开

新增一项 Claude 专属 capability preference（与 permission、stop 并列，独立保存），默认**开启**。
关闭时卸载 D1 的 hook 条目，Claude 回到原生提问框；D2 的让路始终生效。
入口在设置的 Agents 集成页 Claude 区块。仅 Claude Code 有此能力，其它三家不显示。

### D4 题目映射

| Claude | AskHuman 卡片 |
| --- | --- |
| `questions[i].question` | 题面 |
| `questions[i].header` | 题面前缀，`header · question`；`header` 为空则只用题面 |
| `questions[i].options[].label` | 预定义选项（不加前缀、不标推荐） |
| `questions[i].options[].description` | 附在选项文本后：`label · description`；**回传只用原始 label** |
| `questions[i].multiSelect` | 见 D5 |

### D5 单选 / 多选：整卡按多选渲染

AskHuman 的单选是**整张卡**的全局标志，而 `multiSelect` 是**每题**的，因此：

- 整张卡按多选渲染（`single = false`）；
- `multiSelect == false` 的题在题面末尾追加本地化标注「（只选一项）」；
- 回传时，若人给单选题选了多项，**取第一项**，其余丢弃。

不为此改造逐题单选（那要动弹窗与四个 IM 的渲染，代价与收益不成比例）。

### D6 自由输入保留

接管卡不启用严格选择：人可以只选选项、只写文本、或两者都给。回传规则：

- 只选了选项 → 选中标签，多选用 `, ` 连接；
- 只写了自由文本 → 那段文本原样；
- 两者都有 → `标签1, 标签2, 自由文本`（同一个逗号分隔，与多选的连接方式一致）。

依据：Claude 原生本就支持通过 `Other` 行或备注字段输入自己的文本。

### D7 附件：路径追加到答案文本

`answers` 的值只能是文本。人若在卡片上附了图片 / 文件，把各附件的**绝对路径**追加到该题答案文本
之后（换行分隔），Claude 可自行读取。回复图片沿用现有落盘路径。

### D8 部分作答：对齐原生

已作答的题写进 `answers`；未作答的题**不写这个 key**，不伪造「未回答」占位。至少一题有答案即
`allow`。**一题都没答**（提交但全空）视同取消，走 D9。

### D9 取消 → `deny` + 要求重新询问

人明确取消（弹窗取消 / IM 取消 / 全空提交）时：

```json
{ "hookSpecificOutput": { "hookEventName": "PreToolUse",
  "permissionDecision": "deny",
  "permissionDecisionReason": "<沿用 status.cancel 的本地化文案>" } }
```

即「用户取消了操作，你必须重新询问用户是否确定要取消，直到用户给出明确答复」。**不得**退回原生
提问框——那会把人赶回电脑前，也丢掉了「我不想现在答」的表态。

### D10 基础设施失败 → 让路给原生

弹窗拉不起来、所有 IM 不可用、daemon 连不上、hook 自身异常、CLI 非正常退出：**不输出任何内容**，
Claude 回落到原生选择框。判据是「人根本没机会表态」，与 D9 的「人明确表态」严格区分。

### D11 会话归属与历史

- PreToolUse 输入里的 `session_id` 作为 `agent_session_id`（家族 claude）上送，使控制台、watch、
  `--show-last` 与回复历史都能正确归到这个 Claude 会话；
- 来源名显示为 Claude Code；
- **写入回复历史**（D3 开启时的接管问答是一次真实问答）。

### D12 子代理一并接管

Claude 子代理调用 `AskUserQuestion` 时同样接管。它与「子代理不得使用 AskHuman」的交互协议是两回事：
后者约束的是我们自己的提问工具，而这里是 Claude 自身的机制，人一样需要能在手机上回答。

### D13 只做 AskUserQuestion

`ExitPlanMode`（计划审批）机制相同，但本次不做。

## 5. 不做的事

- 不使用 `defer`（交互式会话无效）。
- 不改造卡片支持逐题单选/多选（D5）。
- 不接管 `ExitPlanMode`（D13）。
- 不在 `answers` 里伪造占位答案（D8）。
- 不动 Codex / Cursor / Grok：它们没有同类内置提问工具。

## 6. 验收

1. Claude 提一道单选题：手机 / 弹窗上出现题面与选项，选中后 Claude 直接拿到答案继续，终端不再弹原生框。
2. 一道多选题：可多选，答案以 `, ` 连接。
3. 混合单选 + 多选的多题卡：单选题标注「（只选一项）」，多选了也只回传第一项。
4. 只写自由文本、不选选项：文本原样回传。
5. 选项 + 自由文本都给：按 `标签, 文本` 拼接回传。
6. 三题只答一题：Claude 收到一条答案，其余没有 key，会话继续。
7. 点取消：Claude 收到 deny 与「必须重新询问」的理由，随后重新发问；**不出现原生选择框**。
8. 关掉开关：Claude 弹自己的原生选择框，**不出现 AskHuman 的权限卡**（D2）。
9. 附件：答案文本后带绝对路径，Claude 能读到。
9b. 带 description 的选项：卡片上能看到描述，回传给 Claude 的仍是裸 label。
10. 回复历史里能查到这次问答，`--show-last` 可恢复。
