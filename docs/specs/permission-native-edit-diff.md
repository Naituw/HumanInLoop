# 权限弹窗原生编辑 Diff —— 产品与技术规格

> 状态：已实现并于 2026-07-14 通过 Claude Code `Edit` 与 Codex `apply_patch` 真实权限弹窗验收。
>
> 调研基线（2026-07-14）：Claude Code 2.1.205、Codex CLI 0.144.4、
> Cursor 3.7.36 / cursor-agent 2026.07.09-a3815c0、Grok CLI 0.2.93。
>
> 实施步骤：`docs/plans/permission-native-edit-diff.md`。

## 1. 背景与目标

当前 Claude Code / Codex 的 `PermissionRequest` 经 AskHuman 转成结构化确认后，权限弹窗只显示工具名、
路径和原始 JSON/patch。对原生编辑工具而言，用户无法一眼判断哪些行会增加、删除或替换。

本功能在**本地权限弹窗**中为 Agent 原生编辑操作展示单栏 unified diff，同时保持原审批闭环不变：

- 弹窗和批准/拒绝按钮先显示，不等待文件 I/O；
- 先根据 Agent 原生载荷展示拟议变更，再异步 best-effort 读取本地文件快照补齐上下文；
- 读取失败、超时或不安全时明确降级，不影响审批；
- 不用 `git diff`，不混入与本次请求无关的工作区改动；
- 新增 Agent 时只增加原生 Hook / edit adapter，不修改通用 Diff UI。

## 2. 支持边界

### 2.1 Agent 能力口径

“支持权限 Hook”必须同时满足：

1. 在 Agent 原生权限弹窗出现前触发；
2. 提供结构化工具名和工具输入；
3. Hook 同步阻塞原请求；
4. Hook 可以把 allow 和 deny 都返回给同一请求。

只支持预先 deny、但 allow 后仍进入 Agent 自己权限服务的 `PreToolUse` 不属于审批闭环。

| Agent | 当前闭环事件 | 原生编辑载荷 | 本功能结论 |
|---|---|---|---|
| Claude Code | `PermissionRequest` | `Edit`、`Write`、`NotebookEdit` | `Edit` / `Write` 显示 Diff；`NotebookEdit` 首期仅显示折叠原始参数 |
| Codex | `PermissionRequest` | 规范工具名 `apply_patch` | 显示单文件或多文件 Diff |
| Cursor | 无可代答原生权限弹窗的事件 | 有编辑事件，但非审批闭环 | 不接入 |
| Grok | 无 `PermissionRequest`；`PreToolUse` 只能显式 deny | `Edit` | 不接入 |

依据：

- [Claude Code Hooks / PermissionRequest](https://code.claude.com/docs/en/hooks#permissionrequest)
- [Codex Hooks / PermissionRequest](https://learn.chatgpt.com/docs/hooks#permissionrequest)
- [Grok Hooks](https://docs.x.ai/build/features/hooks)
- 四家本机 bundle / 源码与真实请求验证：`docs/plans/agent-permission-approval.md`

### 2.2 操作口径

首期仅处理 Agent 自带、载荷结构稳定的编辑工具：

- Claude `Edit`：`file_path`、`old_string`、`new_string`、`replace_all`；
- Claude `Write`：`file_path`、`content`；
- Codex `apply_patch`：Add / Update / Delete / Move，允许多文件；
- Claude `NotebookEdit`：识别为原生编辑，但首期不解析 `.ipynb`，只显示折叠原始参数和“不提供 Diff”状态。

以下不做语义 Diff：

- Bash、PowerShell 或其它 shell 内的 `sed`、重定向、脚本写文件；
- 任意 MCP 工具宣称的写操作；
- 未知、缺字段、超限或 schema 漂移的原生工具输入。

这些操作继续显示现有工具详情；不能从命令文本猜测最终文件系统效果。

### 2.3 平台与渠道

- 原生权限审批在 macOS / Linux / Windows 的 Claude Code + Codex 生效；
- Diff 只进入本地 Popup Helper；飞书、钉钉、Telegram、Slack 和历史记录保持现状；
- Windows 通过 `commandWindows`、安全 named pipe 与同一 Popup 模型复用相同 PermissionRequest 语义；PowerShell 复杂或含歧义命令 fail-closed 回基础审批。

## 3. 已确认产品决策

| 编号 | 决策 |
|---|---|
| D1 | 按“闭环原生权限 Hook”做能力型全覆盖，当前实际集合为 Claude Code + Codex |
| D2 | 首期只改本地权限弹窗，不改变 IM 卡片与历史 |
| D3 | 单栏 unified diff，不做左右并排 |
| D4 | 原始工具参数默认折叠，仍可查看 |
| D5 | Popup 首帧与审批按钮不等待快照 Worker；先显示载荷预览，再异步增强 |
| D6 | 普通路径可位于 workspace 外；相对路径以请求的 `cwd` 解析 |
| D7 | macOS 已知 TCC / 高风险路径预先跳过，对该路径不启动读取 Worker |
| D8 | Worker 硬超时 300ms；单文件最多 1MiB；单请求最多 64 文件、累计读取 4MiB |
| D9 | 快照状态始终可见，不能把 best-effort 结果表达成绝对事实 |
| D10 | 单文件最多展示 400 行、单请求最多 3000 行；只在完整 hunk 边界截断并显示精确省略计数 |
| D11 | `NotebookEdit` 首期只显示原始参数，不生成 cell 或 JSON Diff |
| D12 | 旧文件内容只存在于 Worker、Popup Helper 和本地 WebView 内存，不进入 daemon、IM 或历史持久化 |
| D13 | Diff 文件头对 `cwd` 内文件显示相对路径，`cwd` 外保留绝对路径；hover 保留原始完整路径 |

## 4. Diff 语义

### 4.1 两阶段预览

每个支持的请求都可能有两个阶段：

1. **载荷预览**：只使用 Hook 输入，不读文件；必须立即可渲染。
2. **快照预览**：Worker 成功读取当前文件后，补充真实 before、上下文与行号。

快照增强失败时保留第一阶段，审批按钮始终可用。Popup 不自动轮询或监听文件；状态显示快照读取时间，
最终工具执行仍由 Agent 自己校验 old string / patch context。

### 4.2 Claude `Edit`

- 载荷阶段对 `old_string` 与 `new_string` 生成拟议 hunk；
- 快照成功且旧串可定位时，补充文件内旧/新行号及上下文；
- `replace_all=true` 时展示所有可定位 occurrence，并在文件统计中汇总；
- 快照中找不到旧串、非 replace_all 但出现多个歧义匹配或文件已变化时，保留载荷 hunk并显示状态；
- 空 `new_string` 表达删除；空 `old_string` 不猜测插入位置，按 schema fallback 处理。

### 4.3 Claude `Write`

- 载荷阶段明确标为“拟议完整内容”，在 before 未知时不能声称全部为新增；
- 快照文件不存在时，以空 before 生成新文件 Diff；
- 快照文件存在时，以当前全文和 `content` 生成完整文件 Diff；
- 非普通文件、非 UTF-8、超限或读取失败时只显示拟议内容与状态。

### 4.4 Codex `apply_patch`

- 严格解析 `*** Begin Patch` / `*** End Patch` 包络以及 Add / Update / Delete / Move 文件段；
- 载荷中的 context、`+`、`-` 行直接形成拟议 Diff，多文件按 patch 顺序展示；
- 快照只用于补充/校验上下文、行号和 Delete 文件内容；增强失败不能丢掉原 patch；
- Move 显示旧路径 → 新路径，内容变更仍在同一文件区展示；
- 未知 header、嵌套非法段、路径缺失或解析不完整时整次回退到原始参数，不做“部分可信”解析。

### 4.5 `NotebookEdit` 与未知编辑

- 弹窗显示“此原生编辑首期不提供 Diff”；
- 原始参数折叠区仍完整遵守现有 12,000 字符展示上限；
- 不启动快照 Worker；批准/拒绝语义不变。

## 5. 展示模型

Popup 使用结构化模型，不把 Diff 拼成 Markdown/HTML：

```text
PermissionEditIntent
  agent + native_tool + workspace
  operation(s) + path(s) + proposed payload
  initial_diff? + worker_read_plan

PermissionDiffModel
  request_id
  snapshot_status + snapshot_at_ms?
  files[]
    old_path? + new_path + change_kind
    hunks[]
      old_start + new_start
      lines[]: context | add | delete | meta
    additions + deletions + omitted_hunks + omitted_lines
  total_files + additions + deletions
  omitted_files + truncated
```

稳定状态枚举至少包括：

- `payload_only`：仅根据 Agent 载荷预览；
- `snapshot_ready`：已结合本地快照；
- `new_file`：路径当前不存在，按新文件处理；
- `protected_path`：已知受保护 / 高风险路径，未读取；
- `timeout`、`too_large`、`too_many_files`、`non_utf8`、`not_regular_file`；
- `unreadable`、`source_mismatch`、`unsupported`。

状态使用 enum + 参数，由前端 i18n 生成文案；Rust 不传本地化句子。

## 6. 弹窗 UI

布局顺序：

1. 现有标题与权限原因；
2. 工具卡头部；
3. Diff 摘要：文件数、增加/删除行、始终可见的快照状态；
4. 文件区与 hunks；
5. 默认关闭的“原始工具参数”；
6. 现有批准/拒绝选项与可选拒绝原因。

渲染约束：

- 删除行红色、增加行绿色、context 中性、hunk/header 使用弱化强调色；
- 单栏显示 old/new 行号，长行横向滚动，不强制折行破坏代码结构；
- 文件路径可选择复制；Move 同时展示 old/new path；
- 每文件 400 行、全请求 3000 行，按完整 hunk 截断；摘要明确显示省略文件/hunk/行数；
- 使用 Vue 文本插值渲染代码，禁止把文件内容交给 `v-html`；
- 原始参数可以继续复用已消毒的 Markdown，但必须放进原生 `<details>`；
- 颜色之外同时保留 `+` / `-`、行号和可访问状态文本。

## 7. Worker 与文件安全

### 7.1 进程边界

- 真正的 metadata/open/read/diff 在短命隐藏角色 `AskHuman __permission-diff-worker` 中执行；
- Popup Helper 通过 stdin 发送有界 JSON，Worker 只向 stdout 返回有界 JSON；
- 不调用 shell，不执行 Agent 提供的命令，不写文件；
- 父进程设置 300ms 总 deadline；超时或 Popup 结束时 kill + wait；
- Worker 仍是同一应用权限主体，不宣称它是系统 sandbox。独立进程的目的在于时间、崩溃与资源隔离。

### 7.2 路径处理

- Claude 的绝对路径按原值处理；相对路径和 Codex patch 路径以请求 `cwd` 解析；
- 允许普通路径位于 `cwd` 外，不做“必须在 workspace 子树”限制；
- 路径只做词法规范化，拒绝 NUL 等无效输入；不得为了预检在 Popup/daemon 中 `canonicalize` 或读目录；
- macOS 按 path component 边界预跳过已知保护范围：Desktop、Documents、Downloads、iCloud / File Provider、
  网络盘和可移动卷；每个命中路径都不交给 Worker；
- Worker 对父级 symlink 做逐段 best-effort 检查，对最终文件使用 no-follow 打开并在打开后验证 regular file；
- 任何竞态、symlink 异常或路径检查不确定都降级为不读取。

### 7.3 资源限制

- Hook 原始 stdin 继续最多 1MiB，`tool_input` 继续最多 256KiB；
- 单文件最多读取 1MiB；每请求最多读取 64 个文件、累计 4MiB；
- Diff 每文件最多 400 行、总计 3000 行；单行和 Worker stdout另设防御性字节上限；
- 只接受严格 UTF-8 文本；二进制 / 非 UTF-8 不做 lossy Diff；
- Diff 算法自身设置小于 300ms 总 deadline 的内部 deadline，给序列化和进程退出留余量。

### 7.4 TCC 说明

macOS 没有适用于任意路径的可靠公开 TCC preflight API。Worker 超时只能避免 AskHuman 自身长期等待，
不能保证撤回已经出现的系统授权弹窗。因此已知保护路径必须在启动 Worker 前跳过；其它无法预知的系统策略
仍按 best-effort 降级。不得读取或推断 `TCC.db` 来伪造授权状态。

## 8. IPC 与数据生命周期

- `ConfirmTask` 新增向后兼容的 `popup_edit: Option<PermissionEditIntent>`；
- daemon 校验 agent/tool/大小后，仅把它复制到 `ShowPayload.popup_edit`；
- `ConfirmSpec`、`ConfirmRequest`、IM renderer、Confirm history/result 不增加 Diff 字段；
- `PopupInit` 把当前 cold/warm `ShowPayload.popup_edit` 暴露给本地前端；
- Popup 后端按 `request_id` 取当前 intent 并启动 Worker，返回结果也带相同 id；
- warm popup 领用新请求时清空上一请求的 Diff 状态，迟到结果按 id 丢弃；
- proposed 内容会随现有本地 IPC 临时传输；读取到的 before 内容绝不回 daemon，不写日志、不落盘。

## 9. 故障与竞态语义

| 场景 | 行为 |
|---|---|
| adapter 不识别 / schema 漂移 | 继续现有原始参数展示，不启动 Worker |
| 文件不存在 | Write/Add 视为新文件；Edit/Update/Delete 标为 source mismatch |
| 受保护路径 | 该文件不启动读取；显示 protected 状态，其它安全文件可继续增强 |
| Worker 超时/崩溃/畸形输出 | kill + wait；保留载荷预览并显示降级状态 |
| 用户先批准/拒绝或 IM 抢答 | 立即完成原审批；Popup 关闭，Worker 被终止/结果被丢弃 |
| 文件在快照后变化 | 不刷新；状态保留读取时刻，Agent 工具执行时自行验证 |
| daemon 排空或 Hook 基础设施失败 | 沿用现有语义：不裁决，回 Agent 原生权限弹窗 |

任何 Diff 故障都不能改变 allow/deny、Claude permission suggestion、dismiss、24h deadline 或首答胜出语义。

## 10. 非目标

- 不执行、模拟执行或 dry-run Agent 工具；
- 不修改文件，不做 stage/commit，不复用工作区 `/diff`；
- 不对 shell / MCP 做写入推断；
- 不做 side-by-side、语法高亮、word-level inline diff 或可编辑 patch；
- 不把 Diff 发到 IM、附件或历史；
- 不解析 Notebook cell；
- 不保证任意 macOS 路径都不会触发未知 TCC / 企业策略提示。

## 11. 验收标准

1. Claude Edit/Write 与 Codex apply_patch 权限弹窗能显示对应单栏 Diff；Codex 多文件/Move 可读；`cwd` 内路径相对显示。
2. 弹窗首帧、批准和拒绝按钮不等待任何本地文件 I/O。
3. 快照状态始终可见，所有降级路径保留审批能力与原始参数。
4. 已知 macOS TCC / 高风险路径不启动读取 Worker，不出现 AskHuman 触发的系统文件授权弹窗。
5. 普通 cwd 外 UTF-8 文件可在 300ms / 1MiB 等限制内增强；超限与超时明确降级。
6. 单文件 400 行、全局 3000 行均在 hunk 边界截断并报告省略计数。
7. before 内容不进入 daemon、IM、历史或日志；`ConfirmRequest` wire shape 不增加 Diff 数据。
8. warm/cold popup 都不串请求；Popup 或其它渠道先完成时 Worker 不残留。
9. Cursor/Grok、Bash/MCP、NotebookEdit 和未知 schema 保持明确的非 Diff / 原始参数行为。
10. 现有权限批准、拒绝、Claude suggestion、IM 抢答、超时和 fail-open 测试全部通过。
