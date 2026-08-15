# Agent 权限请求经本地弹窗 / IM 审批

> Windows 注（2026-08）：本文保留首期 Unix 实施步骤；Windows 已由
> `docs/plans/windows-platform-parity.md` 接入相同 PermissionRequest/daemon 语义，以下 Windows
> unsupported 段落仅作历史记录，当前边界以对应 spec 与 Windows parity plan 为准。

> 状态：已实现（2026-07）
> 范围：macOS / Linux；Claude Code + Codex 原生 `PermissionRequest` 闭环  
> 实现前提：只代答原本即将出现的权限弹窗；除原样应用 Claude 提供的 allow suggestion 外，
> AskHuman 不自行构造或改写 Agent 权限规则
> 前置任务：`docs/plans/codex-mcp-blocking.md` 已独立完成；本计划不包含通用 MCP 阻塞行为

## 1. 目标

当 Claude Code 或 Codex 准备弹出权限确认时，由用户级 `PermissionRequest` Hook 把请求交给
AskHuman。AskHuman 同时投放到本地弹窗和当前应接收提问的 IM 渠道，沿用现有首答胜出、落败端收尾、
IM 按需发送与在途补推机制。权限卡默认使用“单选结果 → 提交”的表单交互；Telegram 因平台交互能力
改为 choice 按钮一键决定：

- **批准一次**：Hook 返回 `allow`，只批准当前权限请求；
- **按 Claude 原生建议更新权限并批准**：仅当 Claude 输入携带可识别的
  `permission_suggestions` 时显示，Hook 原样回放所选 `addRules + allow` suggestion；
- **拒绝**：Hook 返回 `deny`；可附加最长 1000 字的可选原因。

Codex 当前不支持 `updatedPermissions`，因此只有“批准一次 / 拒绝”。本地第一次关闭、Cmd/Ctrl+W 或
Esc 只显示“关闭将拒绝本次权限请求”的安全确认，用户再次确认才等同拒绝；直接选择拒绝并提交不再追加
确认。Daemon、弹窗、网络或渠道等基础设施故障，以及等待 24 小时仍无人处理，均不替用户作决定：Hook
不输出裁决，让 Agent 回到自己的原生权限弹窗。

若 Claude 同时配置其它 PermissionRequest 决策 Hook，2.1.205 实测没有 deny-wins 保证；本节的“批准/拒绝”
只表示 AskHuman 向 Claude 提交的决定，最终行为可能被另一个 Hook 覆盖。用户明确接受该共存风险，设置页必须
显示高风险警告和独立关闭入口。

本需求还要沉淀一套可复用的 Confirm 决策交互，而不是在权限功能里复制 `/stage` 或普通 Ask 的业务逻辑。
展示层抽出 Ask/Confirm 共用的选择表单；请求协议、稳定 action id、首答终态与历史语义仍由 Confirm 独立
维护。首期只接权限审批，未来脚本确认、危险操作确认等可复用同一请求模型和抢答通道。

## 2. 已确认的产品决策

| 决策 | 定案 |
|---|---|
| Agent 范围 | 只做有原生闭环的 Claude Code、Codex；Cursor、Grok 不做替代权限系统 |
| 动作 | 始终有“批准一次 / 拒绝”；Claude 有受支持的原生 allow suggestion 时动态增加精确作用域的权限更新选项；Codex 不伪造该能力 |
| 表单交互 | 本地、飞书、钉钉、Slack 先单选再提交且首次不预选；Telegram choice 按钮一键决定 |
| 选项顺序 | “批准一次” → Claude 原生规则（若有）→ “拒绝”；不标推荐，避免安全决定被 UI 引导 |
| Suggestion 上限 | 最多展示前 8 条通过白名单的 Claude allow suggestion；超出时明确显示“另有 N 条未展示”，固定批准/拒绝仍可用 |
| 拒绝原因 | 仅选择 deny 时有效，可空、最多 1000 字；批准分支服务端强制丢弃 comment |
| 原因输入 | 本地/飞书/钉钉选 deny 后原位显示输入框；Slack 始终显示可选多行输入；Telegram 精确回复本卡形成 reason draft，再点 deny 提交 |
| 抢答范围 | 本地弹窗 + 现有 Ask 会投放的 IM；首个有效动作胜出，其余端定格收尾 |
| IM 按需发送 | 完全沿用 `auto_activation`、当前活跃槽、watch 关联渠道和在途补推 |
| IM 操作者校验 | 飞书/钉钉/Slack 同时校验卡片与配置目标用户；Telegram 以配置 chat_id 为授权边界，同一 chat 中任何成员提交都算有效决定 |
| 关闭语义 | 第一次关闭/Cmd-W/Esc 只弹“关闭将拒绝”确认；确认关闭才 deny，返回则继续审批 |
| 故障语义 | 基础设施故障 = 不裁决，回 Agent 原生权限弹窗 |
| 等待上限 | 24 小时；到期定格为“已过期”，随后回原生权限弹窗 |
| Hook 超时 | 内部到期仍为 24 小时；Claude/Codex command Hook timeout 设为 25 小时，保证 Daemon 先完成过期收尾 |
| Daemon 排空 | 与普通 Ask 一致：已接收请求继续等待并阻塞 graceful drain；排空期间拒绝的新请求回原生弹窗，`--force` 仍可立即终止 |
| 历史 | 不写 AskHuman 回复历史，避免命令、路径、MCP 参数长期落入 `history.jsonl` |
| 终态文案 | allow 显示“已通过 X 允许 / Allowed via X”，deny 显示“已通过 X 提交拒绝决定”；只表示 AskHuman Hook 的返回，不声称工具已经执行 |
| 详情与持久选项 | 优先提取可读摘要；内容超长时明确标记截断，仍允许批准一次或选择 Claude 原生持久规则；规则与作用域单独醒目展示，截断也必须明示 |
| 设置入口 | Claude/Codex 各自在现有 Agent 卡内增加“权限审批”开关，默认开；不新增第四种集成模式 |
| 生效关系 | CLI/MCP 模式与权限审批开关共同决定是否安装；关闭只卸 AskHuman PermissionRequest 条目，保留 Rule/MCP/CLI timeout；None 卸载条目但保留偏好 |
| 旧安装升级 | 已处于 CLI/MCP 的旧用户显示现有“需更新 / 更新”按钮；用户点更新或主动重设当前 mode 后按默认开启的权限开关补齐 PermissionRequest Hook，Daemon 不静默迁移 |
| CLI 集成接口 | 与设置页统一为 `mode none|cli|mcp` 整包；`update [agent]` 显式更新单家/全部；permission、lifecycle 各用独立 on/off 命令；移除细粒度 install/uninstall flags 并返回迁移提示 |
| Hook 能力状态 | 内部拆分 timeout_hook / permission_hook，UI 可聚合成 Hook 包；共同的 permission hook 不参与 CLI/MCP 模式推断；只称“已配置”，不声称当前 workspace 一定已生效 |
| Hook 被策略禁用 | 可读范围内检测到时显示 known-blocked warning，不修改用户策略，也不算 `needsUpdate`；未检测到不等于证明没有 managed/project policy |
| 其它 PermissionRequest Hook | 保留并共存；检测状态明确是部分视图。Codex 说明其它 Hook 可能延迟/拒绝且 deny 胜出；Claude 常驻说明其它来源可能影响结果，检测到可见 handler 时升级为 allow/deny 可互相覆盖的高风险警告 |
| Claude Hook 顺序 | 新装 AskHuman group 追加在现有 PermissionRequest groups 后；更新保持原位置，不自动重排或声称可修复冲突 |
| 平台 | 首期 macOS/Linux；Windows 等 named-pipe Daemon 完成后再接入相同语义 |

## 3. 四家能力结论

调研只判断“Hook 能否在原生权限弹窗出现前阻塞，并把人的决定同步返回 Agent”，不把
`PreToolUse deny`、自动运行或修改 Agent 权限策略当作同等能力。

| Agent | 原生事件 | 单次批准 / 拒绝 | 权限更新 | 首期 |
|---|---:|---:|---:|---:|
| Claude Code | `PermissionRequest` | allow / deny 均支持 | 输入可带 `permission_suggestions`；allow 可回 `updatedPermissions` | 支持 |
| Codex | `PermissionRequest` | allow / deny 均支持 | 当前明确不接受 `updatedPermissions` | 支持单次决定 |
| Cursor | 无可代答原生权限弹窗的事件 | allow 后仍会进入 Cursor 权限服务；只能预先 deny | 无闭环 | 不支持 |
| Grok | 无用户级 `PermissionRequest` | PreToolUse 只能 deny，超时/失败 fail-open | 无闭环 | 不支持 |

证据口径：

- Claude Code：官方 [Hooks reference](https://code.claude.com/docs/en/hooks#permissionrequest)；
  `PermissionRequest` 在交互会话的权限对话框即将显示时触发，可返回 allow/deny；输入可带原生
  `permission_suggestions`，allow 时原样回放 suggestion 到 `updatedPermissions` 等价于选择原生权限更新项。
  command Hook 默认 600 秒、可配置超时；非交互 `-p` 模式不触发该事件，因此保持 Claude 原生行为。
- Codex：官方 [Hooks / PermissionRequest](https://learn.chatgpt.com/docs/hooks#permissionrequest) 与本机
  Codex Rust `0.144.1` 源码
  `codex-rs/hooks/src/events/permission_request.rs`、`schema.rs`；输入含 `tool_name`、`tool_input`、
  `cwd`，输出支持 allow/deny，当前不接受 `updatedInput`、`updatedPermissions` 或 `interrupt`。
- Cursor：本机 Cursor `3.7.36` 的 `cursor-agent-exec` bundle；Claude 兼容映射中
  `PermissionRequest` 为 `null`，`preToolUse` 的 `ask` 未实现，allow 仍进入本地权限服务。
- Grok：本机 Grok `0.2.93` 的 `10-hooks.md`、`22-permissions-and-safety.md`；用户 Hook 列表没有
  `PermissionRequest`，PreToolUse 只能 deny，Hook 超时/失败 fail-open。

Claude 多 Hook 实测（2026-07-11，Claude Code `2.1.205`）：两个独立 `PermissionRequest` matcher group
分别返回 `allow + updatedPermissions` 与 `deny`，无害临时 `touch` 请求最终显示
“Allowed by PermissionRequest hook”，allow rule 落入临时 project settings 且命令执行。官方只说明所有匹配
handler 并行，未为 PermissionRequest 定义冲突优先级；因此不能把 Claude 共存描述成 deny-wins。用户在看过
结果后明确选择“继续共存，仅显示高风险警告”。测试产生的临时规则、文件、项目和会话状态已清理。

## 4. 非目标

- 不为 Cursor/Grok 打开 Auto-run、Always Allow 或 `failClosed`，再用 AskHuman 自建替代权限系统；
- 不自行构造、合并或直接写权限配置；只原样回放当前 Claude 请求给出的、经白名单验证的
  `addRules + behavior=allow` suggestion；
- 不消费 `setMode`、`addDirectories`、`remove*` 等其它 suggestion，不修改工具输入，也不向 Codex 输出
  当前不支持的 `updatedInput` / `updatedPermissions`；
- 不绕过 Agent 的 deny、ask、managed policy 或 sandbox 规则；其它规则仍可在 Hook allow 后拒绝/再询问；
- 不把权限详情写入回复历史，不在首期新增持久化审计日志；
- 不把权限审批开关扩成第四种 agent mode；它只控制 AskHuman PermissionRequest Hook，CLI/MCP 三态不变；
- 不把通用确认开放为新的公开 CLI 参数；先稳定内部请求契约，公开脚本接口另立需求；
- 不在首期提供 Windows 行为不一致的“全 IM 群发”简化版。

## 5. 总体设计

### 5.1 Confirm 是独立语义，选择表单是共享展示

不要把权限结果伪装成普通 Ask 的本地化选项文本后再反解。Confirm 保留稳定、机器可读的请求/结果模型，
同时从现有 Ask renderer 抽出内部 `ChoiceFormView`，让两类 interaction 共用“选项 + 条件输入 + 提交”的
展示构件：

```rust
ConfirmRequest {
    id,             // daemon-owned
    title,
    context: Vec<ConfirmField { id, label, value, kind }>,
    detail: ConfirmDetail { summary, body_md },
    choices: Vec<ConfirmChoice { id, label, description, role }>,
    presentation: ConfirmPresentation::SingleSelectSubmit {
        input: Option<ConfirmInput {
            id,
            visible_when_action_id,
            label,
            placeholder,
            max_chars,
        }>,
        submit_label,
        default_action_id: None,
    },
    dismiss_action_id,
    created_at_ms,  // daemon-owned wall-clock display value
    expires_at_ms,  // daemon-owned wall-clock display value
}

ConfirmResult {
    action_id,
    comment: Option<String>,
    source_channel_id,
}
```

共享层边界：

- `ChoiceFormView` 只描述视觉和控件状态；普通 Ask 适配为现有单/多选与“输入始终显示/始终隐藏”，
  Confirm 适配为稳定 action id、单选、条件输入和提交；
- 公开 `AskRequest` / `ChannelResult`、CLI stdout、history 格式完全不变；Confirm 仍走独立 IPC、终态和
  固定不写 history，不提供可由调用方打开的 history flag，也不把权限决定混成普通答案；
- 渠道 callback 只传服务端生成的 wire index/slot；daemon 按在途台账映射到 action id，不能信任客户端
  回传的任意字符串；
- M-1 已抽出的 `ConfirmView` 继续服务 `/stage` 的直接双按钮。权限呈现使用新的选择表单，两者复用 action
  role、终态样式与 transport 基础设施，不强迫 `/stage` 改成“选择后提交”。

这是“结构化外壳 + Markdown 详情”，不是把所有工具输入字段化：

- `context` 承载必须稳定显示、独立本地化和独立限长的元信息；`kind` 至少支持 text / path /
  timestamp，平台可据此把路径 home 前缀显示为 `~`、把时间按本地时区格式化；
- `detail.summary` 是始终保留的短摘要；`detail.body_md` 继续允许代码块、JSON 和工具专用 Markdown，
  未知工具无需扩 IPC schema；
- 各渠道先渲染 context、summary 与权限 suggestion 的准确 rule/scope，再给 `body_md` 分配剩余预算；
  任何截断都显式标记，不能让 Agent/workspace/权限模式或规则作用域静默消失；
- `ConfirmView` 是按渠道预算生成的展示态，可带 `detail_truncated` / `choice_detail_truncated`；原始
  `ConfirmRequest` 不接受调用方伪造“已截断”状态。

权限请求映射：

- `context` 固定包含稳定 id：`agent`、`project`、`workspace`、`tool`、`permission_mode`、
  `created_at`；权限 adapter 必须验证齐全后才能提交；
- 固定 choice：`approve_once`（primary）与 `deny`（destructive）；Claude 输入中每个通过白名单的
  `addRules + allow` suggestion 追加一个稳定索引 choice，并在 Hook 进程私有台账中绑定原始 suggestion；
- suggestion 白名单要求 `rules` 非空、数量/字段长度在硬上限内，`destination` 仅允许官方的 `session`、
  `localSettings`、`projectSettings`、`userSettings`；缺少 `ruleContent` 代表整项工具权限，必须以更醒目的
  “允许整个工具”文案展示，不能伪装成窄规则；
- 按 Claude 原始顺序最多取前 8 条合法 suggestion，与固定批准/拒绝合计不超过 10 个 choice；其余合法项
  不生成 action id，只显示稳定遗漏计数，避免客户端伪造“第 9 条”；
- `ConfirmInput.visible_when_action_id = "deny"`，可空、最多 1000 字；daemon 只允许 deny 携带 comment，
  其它 action 即使收到 comment 也强制丢弃；
- 顺序固定为 `approve_once` → 通过白名单的 Claude suggestions → `deny`；不设置 recommended 标记，首次
  无默认选项，未选择时 Submit 禁用/被渠道端拒绝；
- `dismiss_action_id = "deny"`，但本地必须先走关闭警告确认；
- Daemon 接收并验证 `ConfirmTask` 后才分配 id、`created_at_ms` 和 `expires_at_ms`；IPC 调用方不能指定或延长
  TTL。Daemon 同时从接收时刻建立固定 24 小时单调 deadline，两个 wall-clock 字段只供展示。

原始 Claude suggestion 不从 IM callback 回传，也不从 choice id 反序列化生成。Hook 进程在收到终态 action id
后，只能从提交前保留的私有映射取回对应原始对象；未知、重复或越界 id 一律无裁决 fallback。

作用域文案固定映射，不能笼统都写“永久允许”；括号内配置层名称保持 Claude 原文，不翻译：

| destination | UI 作用域 | 实际位置 |
|---|---|---|
| `session` | 本会话允许：`<规则>` | 内存，结束即失效 |
| `localSettings` | 始终允许（Local）：`<规则>` | `.claude/settings.local.json` |
| `projectSettings` | 始终允许（Project）：`<规则>` | `.claude/settings.json` |
| `userSettings` | 始终允许（User）：`<规则>` | `~/.claude/settings.json` |

Daemon 内部把两类请求包装为 `InteractionRequest::{Ask, Confirm}`、结果包装为
`InteractionResult::{Ask, Confirm}`，共享请求登记、首答协调、弹窗关联、渠道挂接、取消和收尾；只有请求验证、
展示适配与终态回传分支不同。

### 5.2 IPC 使用结构化确认终态

新增专用 IPC 负载，而不是让权限 Hook 解析普通 AskHuman stdout：

```text
ClientMsg::SubmitConfirm(ConfirmTask)
ServerMsg::ConfirmAccepted { request_id }
ServerMsg::ConfirmFinal { action_id, comment, source_channel_id }
ServerMsg::ConfirmFallback { reason }
```

- `ConfirmFinal` 只代表人明确提交的选择，或在本地关闭警告中再次确认的 deny；选项切换和 reason draft
  都是非终态，不参与首答竞争；
- 首个有效决定被原子终态闸门采纳后立即向 Hook 客户端发送 `ConfirmFinal`，不等待 popup/IM 定格；Daemon
  常驻任务异步完成其它端收尾，卡片更新失败不能拖住 Agent 继续执行；
- `comment` 仅 deny 可非空，IPC 两端都限 1000 字并再次校验；
- `ConfirmFallback` 代表到期、无可用渠道、GUI 异常退出且无其它可用端等无法取得人的决定；
- Hook 客户端遇到连接失败、协议错误或 `ConfirmFallback` 都必须安静退出 0 且 stdout 为空；
- Hook 被 Agent 杀死或 stdin 非法时同样不输出裁决；
- Hook 客户端连接在终态前断开，说明调用方已不再等待：daemon 通过同一终态闸门取消请求，关闭 popup、
  把已投放 IM 卡定格为“请求已取消”，不得继续留到 24 小时；断连与人提交竞态时仍只允许一个终态；
- 与普通 Ask 一致，已接收确认在 graceful drain 中继续等待；排空期间被拒绝的新提交直接无裁决退出，
  `--force` 或 Daemon 真实失联则由 Hook 客户端走基础设施失败路径；
- IPC 增量保持 serde 向后兼容；若实际改动使同一连接上的旧新端无法安全互通，再提升
  `PROTOCOL_VERSION`，不能依靠“通常同版本”掩盖不兼容。

### 5.3 协调器区分“人拒绝”和“系统失败”

现有 GUI Helper EOF 会被当作 popup cancel。确认请求不能照搬：

- 本地第一次关窗、Cmd/Ctrl+W 或 Esc 只进入警告态；只有点击“关闭并拒绝”才显式发送 `deny`，
  “返回”恢复原选择和原因草稿；警告态再次按 Esc 等同“返回”，绝不能确认拒绝；若已有被隐藏/保留的
  reason，警告必须说明确认关闭会一并发送该原因；
- 选择“拒绝”本身不终结；用户点 Submit 才发送 deny，且不再追加关闭警告；
- Helper 崩溃、连接断开或窗口未成功建立属于通道失败，不得合成 `deny`；
- 如果仍有 IM 渠道，继续等其它端；如果所有渠道均失败，返回 `ConfirmFallback`；
- 请求级渠道状态显式区分 `Starting / Ready / Failed / Terminal`：popup 只有在 Helper 连回并完成确认视图
  首屏展示后才算 Ready，IM 只有首张确认卡发送成功并取得消息 id 后才算 Ready；Router 已连接或 adapter
  已启动不等于卡片已送达；
- popup 从 dispatch 起 10 秒仍未完成首屏 Ready 则 Failed；每个 IM 的首次互动卡投递从启动起最多 60 秒，
  超时则 Failed。权限 Confirm 不沿用普通 Ask 的纯文本编号降级，因为无法可靠承载选择态、Submit 和安全
  详情；纯文本发送成功也不能算 Ready；
- 首投递超时与发送完成存在竞态时仍过同一状态闸门：Failed 后迟到成功取得的卡不得重新变 Ready，adapter
  必须立即 best-effort 定格为“请求已失效”并移除控件；
- 仍有 Starting 候选端时不得抢先 fallback；所有候选端均 Failed，或一度 Ready 的端后续全部失效且无
  Starting 端时，才以 `no_available_channel` 进入同一个系统终态闸门；
- 24 小时定时器与用户动作进入同一原子终态闸门，谁先提交谁生效；等待使用单调时钟 duration，
  `expires_at_ms` 只用于跨进程展示，服务端不拿它代替单调 deadline，避免系统时钟跳变导致提前批准或无限
  延后；过期后的迟到点击只做
  幂等收尾。
- 人决定、fallback、调用方取消或过期后，活动请求立即释放私有 suggestion/raw detail 等业务状态；另保留到
  原始 24 小时截止点的有界内存 tombstone，只含 channel/message 路由、终态类型/来源和已预算的终态渲染
  payload。重复/迟到 callback 只回“请求已结束”并 best-effort 重试移除控件，永不再次投递结果；
- tombstone 不落盘。Daemon 重启后收到旧卡孤儿 callback 时仅做平台 ACK/失效提示，绝不据 callback 重建请求、
  action 或 Claude suggestion。

普通 Ask 的“用户关窗 = cancel”语义不变。

### 5.4 复用 Ask 的投放与按需补推

`RequestRegistry` 的在途条目改为可携带 Ask 或 Confirm，以下机制按交互类型无差别工作：

1. 提交时立即分配 request id，优先领用预热本地弹窗；
2. `attach_im_channels` 根据现有配置投放：
   - `auto_activation=false`：所有已启用且可用 IM；
   - `auto_activation=true`：当前有效活跃槽，加上正在 watch 本次 agent session 的渠道；Popup 不可用且候选为空时，全发所有可用 IM 作可达性兜底；
3. 用户在另一 IM 执行 `/here` 或其它激活动作时，`backfill_inflight` 也补推尚未答复的确认卡；
4. 首答胜出后更新活跃槽，与普通 Ask 一致；
5. 其它弹窗/卡片定格为“已通过 X 提交批准决定”或“已通过 X 提交拒绝决定”，不能继续点击；终态只确认
   AskHuman 已提交人的选择，不声称 Agent 最终采用该决定或工具已经执行。

确认卡出现同样算非 watch 扰动；收尾后沿用现有 watch 跟底恢复逻辑。

### 5.5 通用确认渲染

把普通 Ask renderer 中的选项、输入、Submit 与终态静态回显抽成内部 `ChoiceFormView` 构件；Confirm
提供稳定 choice/action 映射和条件输入配置。`/stage` 仍负责 git 指纹校验和执行，只继续使用 M-1 的
直接双按钮 `ConfirmView` 与 transport，不迁入权限表单状态。

- 本地弹窗：`PopupView` 按 `InteractionRequest` 分流，复用 Ask 的 radio/textarea/Submit 视觉构件，
  隐藏题目导航、附件和语音；选择 deny 后才显示原因输入。第一次关闭进入原窗内警告态，确认后才 deny；
  裸 Enter 不提交，Cmd/Ctrl+Enter 只提交当前明确选择；无选择时任何入口都不能提交。
- 飞书：复用 Ask 卡的单选 toggle + form submit；toggle 到 deny 时回卡显示输入框，切走时隐藏；提交回调
  按 request/message id 路由并携带当前 form reason。
- 钉钉：新建独立 `permission-confirm` 模板，复刻 Ask 模板的 radio/Input/Submit，再增加“选 deny 才显示
  Input”；保留普通 Ask 模板和 `/stage` confirm 模板/ID 不动。
- Telegram：短 choice 直接显示完整按钮文案；选项超过三项、单项或总文案过长时复用 Ask 的数字键帽降级；
  所有 choice 按钮一键形成终态，不显示 Submit。卡片提示“如需说明拒绝原因，请先回复本消息，再点拒绝”，
  只接受带精确 `reply_to_message_id` 的文字作为该卡 reason draft；精确回复会自动选中 deny，但本身不终结。
- Slack：复用 Ask 的原生 `radio_buttons` + Submit，完整显示 choice label，并始终显示多行
  “拒绝原因（可选，仅拒绝时发送）”输入；精确 `thread_ts` 回复仍可作为 reason draft，自动选中 deny 并回填输入。

Slack 的 choice、输入和 reason draft 更新都只是卡片局部状态，只有 Submit callback 才抢答；Telegram 的
精确回复同样只更新 deny 草稿，但随后任一 choice 按钮都直接抢答。连续精确回复按换行追加；若追加后超过
1000 字，拒绝整条新回复、保留旧草稿并提示限长，不做静默截断。按用户定案不增加“清除原因”控件。
popup/飞书/钉钉/Slack 首次均无默认选择，空选择提交必须就地提示且继续等待；Telegram 无 Submit，choice
按钮即决定。已输入的 reason 在请求结束前保留；最终提交任何批准 action 时由服务端丢弃 reason。

协议 context 继续携带 Agent、workspace、工具名、权限模式和创建时间用于验证/路由，但经人工视觉验收，
popup 与 IM 不再重复展示已可从头部/会话判断的元数据；主信息统一为“理由 → 工具名 → 调用正文”。Claude
permission suggestion 另显示准确规则和目标作用域；控件 label 可用短标题规避平台选项文字限制，规则正文
独立换行展示。固定 choice 使用“允许 / Allow”和“拒绝 / Deny”；suggestion 主 label 直接采用上述作用域
与规则文案，多规则时主行显示规则数、其余完整规则放 description。终态卡回显最终
选择与拒绝原因是否已发送，文案统一使用“已提交决定”，不保留可点击控件，也不回显其它落败端的 reason
draft。

提交者校验必须发生在修改选择态、reason draft 或争抢终态之前：飞书校验 `open_id`，钉钉校验 `user_id`，
Slack 校验配置的 `user_id`，并同时校验本卡 message id。Telegram 按用户定案采用 chat 级授权，只校验配置
`chat_id`、本卡 `message_id` 和精确 reply 关系，不额外校验成员 `from.id`；同一 chat 中任何成员都可选择、
填写原因或提交批准/拒绝。

### 5.6 工具详情归一化与截断

Hook 输入不直接原样拼成卡片。先把 `tool_name + tool_input` 归一化为结构化详情：

- Bash / shell：完整命令优先，其次 description；
- Edit / Write / apply_patch：目标路径、操作类型，以及渠道容量允许的内容摘要；
- MCP：server、tool、参数 JSON；
- 其它已知工具：提取路径、URL、查询或目标等稳定字段；
- 未知工具：pretty JSON 兜底。

安全与容量规则：

- stdin 和单字段都设硬上限，先拒绝异常大载荷，避免 Hook/Daemon 内存放大；异常时回原生弹窗；
- 本地弹窗在总上限内展示完整归一化详情；
- context 字段各有独立硬上限且不能整项丢弃；路径超出平台单字段预算时保留项目名和路径首尾、在中间
  明确省略。IM 对 `detail.body_md` 按各平台卡片剩余预算截断，保留开头和结尾并明确显示“内容已截断”；
- permission suggestion 的 rule/scope 使用独立预算和明确的首尾截断标记，不把任意长 rule 塞进有短文本
  限制的 radio label。按用户定案，即使详情或 rule 展示被明确截断，仍保留批准一次和对应持久权限 choice；
- JSON/Markdown/卡片字段必须按平台转义，Hook 输入永远不能注入 callback id、action id 或 Hook 输出结构；
- 不读取 `transcript_path` 内容，不上传 transcript；该字段只用于诊断，不进入卡片。

### 5.7 Hook 适配器

新增隐藏入口：

```text
AskHuman __permission-hook claude
AskHuman __permission-hook codex
```

入口从 stdin 读取一次原生 Hook JSON，验证 `hook_event_name == "PermissionRequest"`，构造通用确认任务，
阻塞等结构化终态，再输出对应 Agent 的 JSON。stdout 只能出现最终裁决 JSON；连接失败、fallback、内部到期
等预期降级在 stdout/stderr 都保持安静，详细原因只记不含原始 `tool_input` 的 daemon 诊断。输入 schema 错误等
开发/配置错误也只能写通用摘要到 stderr，绝不回显敏感 stdin。

批准一次：

```json
{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}
```

Claude 原生权限 suggestion（仅示意；实际对象必须来自本次输入的私有台账，不能由 action id 构造）：

```json
{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow","updatedPermissions":[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"git status"}],"behavior":"allow","destination":"localSettings"}]}}}
```

拒绝：

```json
{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"The user denied this permission request via AskHuman. Reason: <optional user text>"}}}
```

拒绝原因按纯文本处理、trim 后限 1000 字；空原因使用固定默认文案。两家单次 allow/deny 格式当前同构，但代码
仍按 adapter 分开；只有 Claude adapter 接受并回放白名单 suggestion，Codex 永不输出该字段。两家都没有可靠的
allow-side Agent note，因此 comment 只对 deny 有效。

Claude 的 allow/updatedPermissions 不能覆盖 deny/ask/managed **permission rule**，但 Claude 2.1.205 实测
多个 PermissionRequest Hook 的 allow/deny 决策没有 deny 优先保证；AskHuman 的 allow 可能覆盖另一 Hook deny，
另一 Hook allow 也可能覆盖 AskHuman deny。Codex 当前源码则明确多 Hook 任一 deny 优先。

AskHuman 不声称自己的 allow/deny 一定成为 Agent 最终结果，只表示“用户已通过 AskHuman 提交该决定”。所有
匹配 PermissionRequest Hook 会并行启动并等待全部完成，另一 Hook 先决定也不会提前取消 AskHuman。检测到同
事件其它 handler 时，Claude 设置页/doctor 显示不可忽略的高风险警告，Codex 显示等待与 deny-wins 说明；
按用户定案两者都保留共存，不自动改序、禁用或覆盖。

本地权限弹窗对 Claude `Edit` / `Write` 和 Codex `apply_patch` 的专用 Diff 渲染、异步文件快照及安全边界，
见 `docs/specs/permission-native-edit-diff.md`；该数据保持 popup-only，不进入通用 Confirm、IM 或历史模型。

### 5.8 Hook 安装与共存

权限确认使用独立命令标记（例如 `__permission-hook`）和独立 capability；它仍显示在现有 Agent 卡的
Hook 产物区，不新增第四种 mode，也只触碰自己的配置条目：

- Claude：`~/.claude/settings.json` 的 `hooks.PermissionRequest`，nested shape，timeout 90000，并设置明确的
  `statusMessage` 告知正在等待 AskHuman 权限审批；
- Codex：`~/.codex/hooks.json` 的 `hooks.PermissionRequest`，nested shape，timeout 90000，同样设置等待
  AskHuman 的 `statusMessage`，并写
  `~/.codex/config.toml [hooks.state]` 信任哈希；哈希 identity 完整复刻 Codex 的规范化 handler，纳入
  有效 `matcher`、`statusMessage`、归一化 timeout 与 `async=false`，可选字段缺失时不写入 identity；
- Cursor/Grok：不写配置；status 返回 `supported=false` 和稳定原因码；
- Windows：四家 status 均对本功能返回暂不支持，避免生成行为不一致的 Hook。

模式与 Hook 产物包的关系：

| Agent / mode | Hook 产物包 |
|---|---|
| Claude CLI | 既有 CLI 超时 Hook；权限审批开关开时另装 PermissionRequest Hook |
| Claude MCP | 不安装 CLI 超时 Hook；权限审批开关开时装 PermissionRequest Hook |
| Codex CLI / MCP | 权限审批开关开时装 PermissionRequest Hook |
| Cursor CLI | 既有 CLI 超时 Hook；无 PermissionRequest 能力 |
| Cursor MCP / Grok MCP | 无 Hook 产物 |
| 任一家 None | 卸载该自动集成模式拥有的 Hook；保留权限审批偏好，生命周期 Hook 仍按自己的开关管理 |

内部状态必须分别保留 `timeout_hook` 与 `permission_hook` capability；UI/CLI 可再聚合显示当前模式的 Hook
产物包。`artifact_updates()` 在 Claude/Codex 处于 CLI/MCP 且权限审批偏好开启时检查 PermissionRequest
条目；因此升级前已集成用户会得到现有“需更新”，经 mode 重设、单项或“全部更新”才补齐，不能 daemon
启动时静默安装。None 下若发现 AskHuman 自有 PermissionRequest 残留，也把 Hook 标为“需更新”，显式更新或
重设 None 时清理残留但保留 enabled preference。
`Mode::None` 仍返回未集成，不能因为发现两种 mode 共有的 permission hook 而误判为 CLI；
`agent_mode::current()` 只参考 MCP 配置、Rule 和 CLI 专属 timeout hook。

权限条目必须与 lifecycle 的 `__agent-hook`、CLI timeout hook 和用户自有 Hook 共存。为此先从
`agent_lifecycle.rs` 抽出共享的 JSONC/TOML Hook 编辑器，并修正 Codex 当前“卸载 lifecycle 时按整个
hooks.json 路径删除全部信任项”的粒度：

1. 复用项目已有 flock 做一个 integrations 写锁；AskHuman 的 lifecycle、permission 与 mode 安装/卸载在读取
   hooks.json/settings/config.toml 之前获取，并持有到 hooks + trust 事务结束，避免多个 AskHuman 进程互相覆盖；
2. 变更前从旧 hooks.json 找出 AskHuman 自有 marker 对应的 trust keys；
3. 原子写入新 hooks.json；
4. 在写 config.toml 前重读最新内容，只移除旧 AskHuman keys，再写入新文件中仍存在的 AskHuman handlers 的
   hashes；保留用户/其它产品的 trust entries；
5. config.toml 更新失败或落盘后校验不一致时回滚 hooks.json，并对外报错，不能留下“Hook 新、信任旧”的
   半安装态；外部 Codex 进程不遵守 AskHuman flock，因此仍用文件指纹 + 有界重读重试缩小并发覆盖窗口；
6. lifecycle 或 agent_mode Hook 产物包任一安装、卸载、迁移后都走同一 reconcile；
7. 状态检查同时验证命令路径、事件、timeout 和 Codex trust hash；AskHuman 产物缺失/过期才显示更新。
   trust 计算覆盖 Codex 全部十种事件标签，避免 permission reconcile 丢失其它产品在 compact/subagent 事件上的
   已信任条目；回归测试包含 Codex 0.144.1 实际 PermissionRequest `currentHash` 已知向量。
8. 本计划不启用此前暂缓的 Daemon 周期 trust 自愈；权限条目也遵守“只提示需更新、用户显式 reconcile”的
   原则，避免把事务修正扩大成后台自动改配置。
9. Claude `disableAllHooks` / `allowManagedHooksOnly`、Codex `features.hooks=false` /
   `allow_managed_hooks_only=true` 等在 AskHuman 可读配置中发现的用户或组织策略只产生稳定
   `known_blocked_reason` warning，不修改策略、不伪装成 `needsUpdate`；未发现不能表述成“确认未阻断”。Codex
   自有 trust 缺失/过期仍属于可修复的产物漂移。
10. `other_handlers_detected` 只代表 AskHuman 可读配置中检测到同事件其它 handler，不是全局否定证明：Claude
   还会从项目/local、managed、plugin、skill/agent 加载，Codex 也可从用户/项目 inline config 加载。保留原配置
   并返回 coexist warning；Claude warning 必须
   明说“其它 allow 可能覆盖 AskHuman deny，AskHuman allow 也可能覆盖其它 deny”，Codex 说明 deny-wins；
   按用户定案不得拒绝安装、改序或覆盖用户 handler。
11. Claude 首次安装把 AskHuman matcher group 追加到现有 PermissionRequest groups 之后，尽量保留既有 Hook
    的当前行为；后续 update 只就地修复 AskHuman handler，不移动其相对位置。顺序不是安全保证，UI 仍显示
    高风险 warning。

这部分是权限功能安全共存的前置工作，不顺带改变 lifecycle 的产品行为。

### 5.9 设置页、独立权限开关与更新机制

Claude/Codex 各自在现有 Agent 卡的 Hook 区增加“权限审批”开关；这是 permission capability 的偏好，
不改变现有 CLI/MCP/None 三态：

- 默认开启；新切到 CLI/MCP 时随模式安装。旧版已经处于 CLI/MCP 的用户即使缺省偏好解析为开，也不静默
  安装，只沿用现有橙色“需更新 / 更新”流程；用户主动点击更新或重设当前 mode 才按开关 reconcile；
- 关闭时立即只卸载 AskHuman 的 PermissionRequest 条目，保留 Rule、MCP 配置、CLI timeout、lifecycle
  与用户其它 Hook；关闭后缺少 permission 条目不计 `needsUpdate`。关闭/切 None 只阻止后续 Hook，请求已被
  Daemon 接收并投卡后仍可提交或等到 24 小时；设置/CLI 结果要明确“已发出的审批仍有效”；
- 再开启时，若当前为 CLI/MCP 则安装/修复本 capability；当前为 None 时只保存偏好；切 None 会卸载条目
  但不重置偏好，之后重新集成可恢复默认行为；
- 开关需要窄职责的 Tauri/CLI command 与持久化 config 字段；不能借 `agent_mode::set` 重写整个 mode，
  也不能让 lifecycle 开关成为依赖；
- Claude CLI 的 Hook 区分别说明 CLI timeout 与权限审批；Claude MCP、Codex CLI/MCP 显示权限审批；
  Cursor/Grok 显示稳定“不支持原生 PermissionRequest”说明，不伪造开关；
- Windows 上 Claude/Codex 的 permission capability 判 unsupported，并说明“等待 Windows Daemon 支持”；
- 已集成且偏好开、但缺 PermissionRequest 的旧安装继续复用单项“更新”、总览 `Hook ×N`、Agents Tab 提示点
  与“全部更新”；偏好关则不计数；
- blocked policy 和 coexist warning 显示在 capability 下方：前者说明 Hook 不会运行；Claude coexist 是
  决策可互相覆盖的高风险状态，Codex coexist 是“全部等待、deny 胜出”。两者都不被“更新”按钮伪装成可
  自动修复，Claude warning 旁直接提供权限审批开关作为退出路径；
- capability 只显示“已配置”，不能显示“运行中/已生效”。Claude 始终附简短说明“项目、managed、插件等
  其它来源的 Hook 仍可能影响最终决定，可在当前 Claude 会话用 `/hooks` 查看实际来源”；只有
  `other_handlers_detected=true` 时升级成上述高风险详警。Codex 始终说明其它来源可能增加等待或拒绝，检测到时
  再显示当前 deny-wins 详警。

Rust/TS 状态不要把扩大后的概念压成单个 `hookInstalled`：至少分别返回 timeout / permission 的
`supported`、`enabled`、`configured`、`needsUpdate`、`knownBlockedReason`，以及 permission 的
`otherHandlersDetected`。后两者都是 AskHuman 可读范围内的正向发现，`None/false` 不得翻译成“已证明无策略 /
无其它 Hook”。Hook 包总状态只在展示层聚合；旧字段仅是同版本 Tauri 进程调用契约，可一起迁移。

CLI 写接口与设置页使用同一套整包语义，不再允许用户手工拼出 Rule/Hook/MCP 的半安装组合：

```text
agents mode <agent> [none|cli|mcp]
agents update [<agent>]
agents permission <claude|codex> [on|off]
agents lifecycle <agent> [on|off]
agents show [<agent>]
agents monitor [--json|--text]
```

- `mode` 省略目标值时查询；切到另一 mode 时卸旧装新，并按 permission preference 管理权限条目；切到
  `none` 卸载自动集成整包和权限条目，但保留 permission preference 与独立 lifecycle 状态；
- 设置 mode 无论是否真正切换，都 reconcile 该 mode 的完整托管产物，语义等同“设为目标 mode 并更新”；
  permission 不设缺失/过期/曾安装等特殊分支，只读取独立 preference：开则安装/修复，关则确保不安装，且
  mode 操作本身绝不改写该 preference。用户主动重设当前 mode 属于显式更新，不是 Daemon 静默迁移；
- `update <agent>` 更新该家当前 mode 的整包；clean None 为 no-op，None 有 AskHuman 残留时执行清理。裸
  `update` 逐家更新所有当前非 None 或有残留待清理的整包、逐家报告，任一失败则最终非零退出。permission
  preference 关闭时不安装权限条目，开启时会补齐旧安装缺失的条目；
- `permission` 省略 on/off 时查询；`on` 即使偏好已经开启也会在当前 CLI/MCP mode 安装/修复 capability，
  当前为 None 时只保存偏好；`off` 立即卸载本 capability。`lifecycle` 同样独立查询/切换，不归 mode 包管理；
- 保留只读 `show` 与 `monitor`。移除 `install/uninstall` 以及 `--rules/--hook/--mcp/--lifecycle` 写接口；旧调用
  不做猜测性兼容或部分执行，统一非零退出并给迁移提示：CLI/MCP 安装改用 `mode`，卸载改用 `mode ... none`，
  更新改用无 flags 的 `update`，lifecycle 改用独立命令。这是发布时必须写明的 CLI breaking change。

## 6. 实施里程碑

### M-1：先抽离通用双动作确认卡展示 / 传输层

本里程碑是纯重构，必须先完成且保持 `/stage` 行为不变，之后才开始 PermissionRequest 业务：

1. `confirm.rs` 抽出通用 `ConfirmAction { id, label, role }`、有序双动作 `ConfirmView`、
   `ConfirmFinalView` 和 wire slot；builder 不含 git/stage 语义；
2. 飞书/Slack 等平台按 action role 渲染按钮样式；Telegram 无按钮 style，只保持稳定顺序与文案。
   callback 一律只解析为第一/第二槽位；真实 action id 由 daemon 台账映射，不能把
   `confirm_ok/cancel` 当业务语义；
3. 钉钉保留既有已发布模板和 template ID：动态变量继续使用 title/markdown/两按钮文案/finalized/final_label；
   固定红/蓝槽位与 `confirm_ok/confirm_cancel` 只作为 wire slot，不修改或重发模板；
4. 从 daemon 的 `/stage` 区域抽出通用 `confirm::transport::{send, finalize}`，统一四渠道发送/定格；
5. `/stage` 台账继续独立保留 git_root、文件指纹与 `git add -A`，只保存通用 view 并把槽位映射回
   `stage_confirm/stage_cancel`；不得迁入权限状态或通用 transport；
6. builder、slot parser、固定钉钉模板变量、send/finalize 纯构件和 `/stage` 回归测试通过后单独提交。

### M0：通用确认模型与协议

1. 从现有 Ask renderer 抽内部 `ChoiceFormView` / choice option / 条件输入显隐；普通 Ask 只做适配，公开
   `AskRequest` / `ChannelResult` 不变；同时修 Slack `--single` 超过 10 项时多个 radio group 可各选一项、
   option text 超过 75 字会被拒的问题：全局单选状态由服务端统一，控件用短编号，完整长文本独立展示；
2. `models.rs`：增加 `ConfirmRequest`、`ConfirmField` / field kind、`ConfirmDetail`、`ConfirmChoice`、
   `ConfirmPresentation::SingleSelectSubmit`、`ConfirmInput`、带可选 comment 的 `ConfirmResult`、action role、
   终态/过期原因；
3. `ipc/mod.rs`：增加 Confirm task/client/server 消息和 GUI show/answer 负载；wire 只传 choice index/slot，
   由服务端台账映射 action id；
4. `daemon/request.rs`：登记项支持 Ask/Confirm 两类 interaction；pending 摘要、agent session 关联保持通用；
   Confirm 条目另持有候选渠道的 `Starting / Ready / Failed / Terminal` 状态，不能复用普通 Ask 的
   `attached: bool` 作为“可作答”依据；Daemon 在接收时生成权威 id、wall-clock 展示时间与 24 小时单调
   deadline，wire task 不接受自定义 TTL；
5. `app/coordinator.rs`：抽出共享首答/收尾核心；Ask 分支保持现有 stdout/历史，Confirm 分支返回结构化结果且
   永不写 history；Confirm 首答立即回 IPC，卡片定格异步执行，不复用 Ask 的“最多等 5 秒再返回”顺序；
6. 单测：首答唯一性、空选择不能提交、wire index/action id 与权限必填 context 校验、comment 仅 deny
   可用且限长、context 不随详情截断丢失、普通 Ask 输出/历史回归、Confirm 不落历史。

### M1：本地确认弹窗

1. `ShowPayload` / `popup_init` 支持 interaction enum，预热 helper 可领用任一类型；
2. `PopupView.vue` 增确认视图：元信息、详情、动态单选、deny 条件输入、Submit 和持久规则作用域；
3. 新增显式 `submit_confirm_action`；首次关窗/Cmd-W/Esc 显示原窗内警告，“关闭并拒绝”才提交 deny，
   helper EOF 只走通道失败；
4. 首次不预选、裸 Enter 不提交；Cmd/Ctrl+Enter 只能提交当前显式选择，无选择时与 Submit 一样被阻止，
   不能绕过关闭警告；
5. Helper 在确认视图完成首屏展示后显式上报 Ready；落败/过期/调用方取消时关闭窗口；不改变普通 Ask
   的取消确认与关闭语义；
6. 前端测试或可测试纯函数覆盖类型分流、条件输入、关闭警告、choice 映射、转义和截断标记。

### M2：Daemon 抢答、IM 卡与按需发送

1. `attach_im_channels`、`backfill_inflight`、watch 扰动/恢复改为接受通用 interaction；
2. 飞书/Slack 的 Ask builder 与本地控件抽共享 `ChoiceFormView` 适配；choice label 与长 rule 详情分开渲染；
   Slack 普通 Ask/Confirm 都以服务端选择态回卡，保证跨 10 项分组仍全局单选；
3. 飞书实现单选 toggle、deny 条件输入与 Submit；钉钉新增
   `docs/assets/dingtalk-permission-confirm-card-template.json`、独立默认 ID 与可选
   `permission_confirm_card_template_id` override，保留普通 Ask 与 `/stage` 模板/配置不动；
4. Slack 实现“单选 toggle + Submit”；Telegram 实现短文案直显/长文案数字降级的一键 choice。精确
   `reply_to_message_id` / `thread_ts` 回复自动选中 deny 并保存 reason draft；多条按换行追加，超 1000 字
   拒绝新回复且保留旧稿；批准分支服务端丢弃，不提供清除控件，另覆盖收尾与并发路由；
5. 四个 channel adapter 实现 Confirm 的发送、Ready/Failed 上报、回调、首答投递、落败定格和过期定格；
6. popup 10 秒 / 各 IM 60 秒首次投递 deadline 驱动 Starting→Failed；纯文本降级不算 Ready，迟到成功卡
   立即定格失效；24 小时 timer 与 Submit/确认关闭争抢同一终态；全通道失败返回 fallback；
7. 成功动作后沿用 winner 更新 active channel；新激活 IM 能补推在途确认卡；终态后释放原始权限状态并保留
   到原始截止点的轻量 message tombstone，迟到/重复 callback 仅重试静态收尾；Daemon 重启后的孤儿只提示失效；
8. `/stage` 继续使用自己的 M-1 双按钮 view/业务台账，确保既有行为不回归；
9. 测试 popup/四 IM 的空选择、choice 切换、reason draft、目标用户/Telegram chat 级校验、竞态、首卡发送
   失败/超时/迟到成功、纯文本降级不得 Ready、全部候选端失败、
   迟到点击、重复 callback、active/watch
   投放和 `/here` 补推；另回归 Slack 普通 Ask 的 11+ 单选全局互斥与 76+ 字选项完整展示。

### M3：权限输入归一化与 Hook 运行器

1. 新建权限模块解析 Claude/Codex stdin，限制总大小，提取 session/cwd/tool/tool_input；Claude 另解析
   `permission_suggestions`，只白名单 `addRules + behavior=allow` 并保留 action id → 原始对象私有映射；
2. 实现 Bash、文件工具、MCP、unknown JSON 的摘要器，以及 context/body/rule 各自预算和显式截断；
3. `client/` 增 `run_confirm`，基础设施失败/timeout/fallback 均返回“无裁决”；
4. `cli/mod.rs` 注册隐藏 `__permission-hook`，严格保持 stdout 洁净；
5. 两家 adapter 输出 allow/deny JSON；Claude 可原样回放本次输入中的白名单 suggestion，Codex 永不输出
   `updatedPermissions`；deny 可带可选 reason，allow 强制丢弃 comment；
6. 恶意 action id、suggestion、引号、换行、超长 Unicode 必须被白名单/serde/长度校验拦住，日志不得含
   原始 `tool_input`；
7. fake daemon 集成测试：approve once、Claude permission update、Codex 拒绝伪造 update、deny reason、
   user-close、helper-crash、daemon-loss、24h 虚拟时钟 timeout，
   并验证 25h Hook 外层超时不会抢先终止内部到期收尾。

### M4：agent_mode Hook 产物包与 Codex trust 共存

1. 从 `agent_lifecycle.rs` 抽共享 JSONC Hook 编辑、marker 定位、atomic write 与跨进程 integrations flock；
2. 实现内部 `agent_permission` status/install/uninstall/update 和按 Agent 持久化的 enabled preference；
   它是现有 mode 下的独立 capability，不是第四种 mode；
3. 扩展 `agent_mode::{set, update, update_artifact, artifact_updates, uninstall_all}`：
   - Claude/Codex 的 CLI 与 MCP 在 enabled 时管理 PermissionRequest；disabled 时不把缺失算 update；
   - Claude MCP 只卸 CLI timeout 条目，按 enabled 保留/安装 permission 条目；
   - None 卸载 permission 但保留 enabled preference；
   - `set` 对真实切换和重复设置都完整 reconcile 目标 mode；permission 始终只按 enabled preference 决定
     安装/卸载，不因“当前缺失”设置例外，也不由 mode 操作改写 preference；
   - Cursor/Grok 保持现有能力；
   - None 发现 AskHuman 自有 permission 残留时也产生 Hook needs-update；`set(None)` / `update(None)` 清理
     残留但不改 enabled preference，clean None 仍为 no-op；
4. 模式检测只读取 MCP/Rule/CLI 专属 timeout capability，不读取两种模式共有的 permission hook；
5. 重构 Codex trust reconcile，做到 feature 级增删、失败回滚、保留用户 Hook；
6. status 检测可读范围内的 policy blocked 与其它 PermissionRequest handler，字段使用
   `knownBlockedReason` / `otherHandlersDetected`，不得把否定值当完整证明；blocked policy 与 Claude/Codex
   分级 coexist warning 分开表达；按用户定案保留共存，不改用户策略或 Hook 顺序；
7. 组合测试至少覆盖：
   - CLI ↔ MCP ↔ None 的权限条目安装/保留/卸载；
   - enabled on/off、None 下保存偏好、重新集成恢复；
   - None 残留 Hook 的 needs-update、mode none 重设/显式 update 清理与 preference 保留；
   - off/切 None 只影响后续请求，在途 Confirm 仍可提交且 UI/CLI 明确提示；
   - 相同 mode 重设、真实 mode 切换、显式 update 都完整 reconcile，且 on/off 两种 permission preference
     均保持不被 mode 操作改写；
   - lifecycle → agent_mode update → 卸 lifecycle；
   - agent_mode update → lifecycle → 切 None；
   - lifecycle 自动迁移与 agent_mode 手动更新交错；
   - 用户同事件多 group、多 handler、JSONC 注释与自定义 timeout；
   - Claude 项目/local/plugin/managed 与 Codex project/inline 来源无法全局枚举时，UI/doctor 不误报“已生效 /
     无其它 Hook”；
   - config.toml 已有其它 `[hooks.state]`；
   - lifecycle/permission 两个 AskHuman 进程并发 install/update/uninstall 不丢 marker/trust，外部文件指纹变化触发
     有界重读重试；
   - trust 写失败时 hooks.json 回滚；
8. 明确禁止 daemon 自动补装 PermissionRequest：旧版已集成用户必须先看到现有“需更新”，再通过 mode
   重设、单项更新或“全部更新”安装；lifecycle 自己的既有自动迁移行为不变。

### M5：设置 UI 与现有更新可观测性

1. Rust/TS 分开表达 timeout / permission capability，并增加窄职责的权限审批开关 command；展示层再聚合
   Hook 包，不让 permission 状态参与 mode 推断；
2. Claude/Codex Agent 卡按 §5.9 显示默认开启的“权限审批”开关、安装/阻断/共存状态；Cursor/Grok/Windows
   显示不支持原因；
3. 验证旧 Claude/Codex CLI/MCP 且 enabled 的安装会产生现有 `hookNeedsUpdate`，单项“更新”、总览 `Hook ×N`、
   Agents Tab 提示点和“全部更新”全链路复用；
4. CLI 收敛为 `mode` 整包、`update [agent]`、`permission [on|off]`、`lifecycle [on|off]`；删除
   `install/uninstall` 与所有细粒度 flags，旧调用返回可执行的迁移提示；`doctor` 与 CLI query 复用 capability
   口径并只输出“已配置”、known blocked reason / 部分检测到的其它 Hook warning；Claude 附 `/hooks` 核实指引；
5. 测试裸 `update` 逐家执行/聚合退出码、同 mode 重设完整 reconcile 但不改 permission preference、旧命令
   不部分执行，以及所有迁移提示；
6. i18n 中英文齐全，Cursor/Grok/Windows 的“不支持”原因不能只靠灰色状态表达。

### M6：验证、文档与发布收尾

1. Rust 单元/集成测试、`cargo fmt --check`、`cargo test`；
2. `npm` 前端 typecheck/build；
3. 按项目规则运行 `./scripts/install.sh`，后续人工确认全部使用新安装的 AskHuman；
4. 无真实危险操作的端到端验收：Claude/Codex 各触发 harmless permission request，覆盖本地/IM 的空选择、
   批准一次、拒绝原因、关闭警告、Daemon/网络故障回原生、超时回原生；Claude 另用临时安全规则验证
   suggestion 原样回放、准确作用域和清理；
5. 四 IM 真机验收决定交互（Telegram 一键 choice，其它渠道选择+Submit）、条件输入/精确回复 reason draft、
   动态持久 choice、并发收尾、卡片不可重复提交、按需投放与 `/here` 补推；
6. 更新 `docs/overview.md`、`docs/specs/cli-config.md`、其它引用旧 agents flags 的规格/计划、用户 wiki、
   `docs/PROGRESS.md`；发布说明明确 CLI breaking change；功能提交使用清晰的 Conventional Commit，
   例如 `feat(hooks): route agent permission requests to AskHuman`。

## 7. 测试矩阵与验收标准

| 场景 | 预期 |
|---|---|
| 自动集成为“未集成” | Agent 原生行为完全不变，不 spawn AskHuman 权限 Hook |
| Claude 非交互 `-p` | `PermissionRequest` 不触发，保持 Claude 原生行为 |
| 新切换 Claude/Codex 到 CLI 或 MCP | 随现有集成产物一并安装 PermissionRequest Hook |
| 升级前已是 Claude/Codex CLI/MCP | 现有 Hook 产物显示“需更新”；主动重设 mode 或点击单项/全部更新后才补装 |
| 重复设置当前 mode，权限开关开 | 完整更新 Rule/timeout/MCP，并安装/修复 PermissionRequest；不改开关值 |
| 重复设置当前 mode，权限开关关 | 完整更新其它 mode 产物并确保 PermissionRequest 不安装；不把开关改回开 |
| 显式 `agents update [agent]` | 更新当前整包；偏好开启时补齐 permission，关闭时保持关闭；None 有残留则清理，裸命令逐家执行 |
| 旧细粒度 agents 写命令 | 不执行任何部分变更，非零退出并给出 mode/update/lifecycle 迁移提示 |
| None 但残留 AskHuman PermissionRequest | Hook 显示需更新；重设 None 或显式 update 清理残留并保留权限偏好 |
| Claude/Codex 请求本来无需权限 | `PermissionRequest` 不触发，AskHuman 不出现 |
| 表单首次显示 / 空选择 Submit | 无默认选择；不能终结，请求继续等待 |
| 本地“批准一次”先答 | Hook 输出 allow；所有 IM 定格“已提交批准决定”，不写回复历史 |
| IM“批准一次”先答 | Hook 输出 allow；本地窗关闭，其它 IM 定格赢家 |
| Claude 带合法 allow suggestion | 五端动态显示精确 rule/scope；选中并提交后只原样回放该 suggestion |
| Claude 无 suggestion / Codex | 不显示持久权限 choice；只能批准一次或拒绝 |
| 未知/非 allow suggestion | 不显示、不回放；不能靠伪造 action id 写权限 |
| 合法 Claude suggestion 超过 8 条 | 仅前 8 条有 choice/action id，清楚显示遗漏数；批准一次/拒绝照常可提交 |
| 任一端选择“拒绝”并提交 | Hook 输出 deny；可选原因只进 deny message；其它端全部定格“已提交拒绝决定” |
| Slack/Telegram 精确回复卡片 | 只更新该卡 reason draft，不抢答；Slack 选 deny 后 Submit，Telegram 再点拒绝 |
| Slack/Telegram 回复时尚未选 deny | 自动选中 deny 并保存草稿；Slack 仍需 Submit，Telegram 再点任一决定即终结 |
| Slack/Telegram 连续回复 | 换行追加；新回复会使草稿超过 1000 字时整条拒绝并提示，旧草稿不变 |
| 从 deny 切到批准再切回 | 原因草稿仍在；请求结束前不因临时切换丢失 |
| 已有 reason 却提交批准 | reason 被服务端丢弃，不进入 allow 输出或 history |
| 用户第一次关闭本地确认窗 | 只显示“关闭将拒绝”警告，不终结 |
| 用户确认关闭并拒绝 | 明确 deny，不回原生弹窗；返回则继续审批 |
| GUI Helper 崩溃但 IM 可用 | 不 deny；继续等 IM |
| Router 已连但首张确认卡发送失败 | 该 IM 记 Failed，不得把“已 attach”当作可作答渠道 |
| popup 10s / IM 60s 内未 Ready | 对应候选端记 Failed；迟到送达的卡立即定格失效，不得复活请求 |
| 互动卡失败但纯文本发送成功 | 仍记 Failed；权限请求不能在无可靠选择+Submit 的文本模式作答 |
| 所有渠道不可用 / Daemon 丢失 | stdout 为空；Agent 显示原生权限弹窗 |
| 24 小时无人处理 | 卡片定格过期；stdout 为空；回原生弹窗 |
| 已接收请求期间 graceful drain | 与普通 Ask 一致继续等待；请求完成后 Daemon 才退出 |
| 排空期间的新权限请求 | 不进入 AskHuman 等待；stdout 为空，回 Agent 原生权限弹窗 |
| IM 详情/rule 被截断 | 清楚显示首尾截断和准确作用域；按用户定案仍保留批准一次与对应持久 choice |
| `/here` 切换到另一 IM | 未答权限确认补推一次；旧端按活跃槽规则收尾/保持可追踪状态 |
| lifecycle 与 agent_mode 交错安装/卸载 | 只增删各自 marker；Codex 两类 trust 均保持正确 |
| Codex 其它 PermissionRequest Hook 返回 deny | 当前 deny-wins；AskHuman allow 不会放行，但 AskHuman 仍等到用户提交/超时 |
| Claude 检测到其它 PermissionRequest Hook | 原配置保留；设置/doctor 显示 allow/deny 可能互相覆盖的高风险警告与独立关闭入口 |
| 未检测到其它 Hook / blocked policy | 只代表 AskHuman 可读范围内未发现；不得显示“确认无冲突”或“已生效” |
| Hook 被用户/组织策略禁用 | 显示 blocked warning，不改策略、不算 needsUpdate |
| 权限审批开关关闭 | 只卸 AskHuman PermissionRequest 条目；CLI/MCP/Rule/timeout/lifecycle 不变 |
| 关闭开关/切 None 时已有在途卡 | 继续有效直到提交、调用方取消或 24h 到期；操作结果明确提示只影响后续请求 |
| Telegram 私聊 | callback/reply 必须属于配置 chat 与本卡；chat_id 即私聊会话 |
| Telegram 群聊 | 以配置 chat 为授权边界，任何群成员提交都算有效决定，不额外校验 `from.id` |
| Cursor/Grok | 设置页说明不支持；磁盘不写权限 Hook |
| Windows | 设置页说明待 Daemon 支持；无简化版行为 |

完成标准：上述自动化测试通过，Claude/Codex 与四 IM 的人工验收均有结果；普通 Ask、`/stage`、生命周期
追踪、插话、watch 和自动集成三态无回归。

## 8. 风险与控制

1. **远程放大高风险操作**：权限卡必须醒目标明 Agent、workspace、工具、准确规则作用域和截断。Claude
   原生 suggestion 会扩大未来权限；只能原样回放白名单 `addRules + allow`，不得自行猜规则。用户已明确接受
   在详情或规则展示被明确截断时仍可选择持久批准，UI 不得把它伪装成“批准一次”。
2. **Hook 卡住 Agent**：24 小时是产品定案；Hook/Daemon 必须有可取消连接和过期 timer，内部 24 小时先
   收尾，外层 command Hook 以 25 小时兜底。基础设施失败回原生权限弹窗，不能 fail-allow 工具本身。
3. **误把崩溃当拒绝**：确认协议必须要求显式 action；EOF 只是 channel failure。
4. **Codex trust 相互破坏**：权限功能上线前先完成 feature 级 reconcile 和交错组合测试。
5. **卡片并发串答**：路由键至少包含渠道消息 id 与 request id，action id 只能从服务端台账取，不能信任客户端
   回传的任意字符串。
6. **敏感参数外发**：新安装或用户点击现有更新后，权限确认随 Claude/Codex 的 AskHuman 集成生效；
   UI/更新说明需明确权限详情会发送到当前启用的 IM。未集成时关闭、不落历史，但 IM 平台自身的消息留存
   不由 AskHuman 控制。
7. **旧新 Daemon 混用**：利用现有二进制指纹/drain；协议无法兼容时提升版本并验证在途请求收尾。
8. **Claude 多 Hook 决策互相覆盖（已知且接受）**：2.1.205 实测 allow+权限更新可在另一个匹配 Hook 返回
   deny 时胜出、写规则并执行命令；反向也不能保证 AskHuman deny 胜出。用户明确选择继续共存。产品必须把它
   作为高风险状态醒目标示、提供独立关闭入口，并在版本升级验收中重复该探针；不得写成普通“仍需等待”提示
   或声称 deny-wins。Codex 当前仍按源码和测试维持 deny-wins 口径。

## 9. 建议提交拆分

1. `refactor(confirm): add reusable confirmation interaction core`
2. `feat(confirm): deliver confirmations through popup and IM channels`
3. `refactor(hooks): share hook config and reconcile codex trust`
4. `feat(hooks): route agent permission requests to AskHuman`
5. `feat(settings): show permission hooks in agent integration status`
6. `feat(cli)!: align agent integration commands with mode bundles`
7. `docs(hooks): document remote agent permission approval`

每个提交都必须保持可编译；对外可见的 `feat` subject 会进入 release notes，最终可在合并前按实际用户价值
调整拆分，避免把内部重构写成用户可见功能。CLI 提交必须带 `BREAKING CHANGE:` footer，明确旧
`install/uninstall` 与细粒度 flags 的迁移命令。
