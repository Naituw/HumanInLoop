# IM 命令与主动交互概览

> 本文是 `docs/overview.md` 的专题补充，记录四种 IM 的共享命令模型和代码入口。卡片交互与功能边界以文中引用的 specs 为准。

## 全局模型

- 飞书、钉钉、Telegram、Slack 的长连接与 Router 由 Daemon 独占；每种渠道全局只保留一条连接，再按卡片或用户身份路由到请求。
- Daemon 存活期间持续接收 IM 入站消息，不要求当时已有提问。命令交给共享的 `autochannel` / daemon 处理，平台模块只提供收发、卡片渲染和回调解析。
- 同一提问可以投放到 Popup 和多个 IM，但仍由 Coordinator 保证首个终态回答生效并取消其它渠道。
- `/` 是通用命令前缀；Slack 客户端会拦截未注册 slash command，因此共享命令另支持 `!` 前缀，Slack 的提示文案默认展示 `!`。
- **无前缀整句短语**也可触发无参命令（如「新建会话」「状态」「插话」）：规范化（去空白与标点）后与词表整串匹配，与 slash 一样优先于作答。规格见 `docs/specs/im-command-phrases.md`。
- Agent 的数字编号来自 daemon 生命周期内稳定的 seq，供 `/status`、`/watch`、`/msg`、`/diff` 等命令共用。

## 活跃槽与按需发送

`channels.autoActivation` 默认关闭：关闭时，每次提问发送到所有已启用 IM；开启时，通常只投放到当前有效活跃槽。活跃槽也可以是 `popup`，并持久化于 `~/.askhuman/state/auto-channel.json`。Popup 不可用且「活跃槽 ∪ watch 渠道」选不出任何可用 IM 时，为避免零投递，该次提问会兜底发到所有可用 IM；人在其中某个渠道回复后，它会自然成为新活跃槽。

在某渠道发普通消息、回答问题或执行会改变交互上下文的命令，会把它设为活跃槽；切槽时把在途未答请求补推到新渠道。`channels.autoEndWatch` 开启后，切离某个真实 IM 时会结束该渠道的 watch。具体决策见 `docs/specs/im-auto-end-watch.md` 和相关实现计划。

正在 watch 某 Agent 的渠道会加入该 Agent 新提问的投放并集，因此按需发送不会让 watch 用户错过需要回答的问题。

## 命令地图

- `/new`：macOS 上依次选择最近 workspace、三重就绪的 Agent 和权限，并通过渠道原生输入控件提交任务；Slack 显示为 `!new`。所选 workspace 对应项目有 TODO 时，飞书/钉钉/Slack 可在最终输入卡直接选择，Telegram 先选手动输入或 TODO；选择 TODO 后可附加补充文字。Terminal.app 成功打开后才把 TODO 记为已执行，来源渠道默认自动 watch 新会话。
- `/help`、`/?`：按当前配置和是否有在途问题生成分组命令与作答提示；飞书用消息卡片、
  Slack 用 Block Kit、钉钉用 `sampleMarkdown`、Telegram 用 HTML，富文本失败回退分组纯文本。
  规格见 `docs/specs/im-help-rich-text.md`。
- `/here`：把当前 IM 设为活跃槽。
- `/status [编号]`：无编号查看 Agent 列表；带编号查看最近助手文字和当前/最近工具活动。
- `/watch [编号]`：创建或替换实时状态卡；无编号时可用单选卡选择 Agent。
- `/unwatch [编号|all]`：结束关注并定格旧卡；无编号且有多个订阅时可用单选卡选择。
- `/msg`：唯一关注目标可发送时直接打开一次性输入卡，否则先选工作中的非 Grok Agent（按钮为“选择”）；输入卡展示目标与待送达预览。飞书/Slack 同卡变身并定格，钉钉先终态化选择卡再进入输入，Telegram 使用 ForceReply，完成后删提示并回复短终态。
- `/msg <编号>`：工作中目标打开一次性输入卡，空闲目标查看待送达内容；`/msg-clear <编号>` 撤回待送达内容。
- `/msg <编号> <内容>`：直接给工作中的非 Grok Agent 追加插话；`/msg <内容>` 保留按关注关系直发或带正文选目标的快捷流。
- `/yolo [off [编号]]`：列出开启 YOLO 的 Codex 会话并提供关闭入口；`off` 可关闭唯一会话或指定 Agent 编号。
- `/diff [编号]`：导出目标 workspace 的 unstaged 与 untracked 变更摘要和附件。
- `/stage [编号]`：显示变更确认卡，确认后执行 `git add -A`；不会绕过 Confirm 直接暂存。
- `/transcript [编号]`：按渠道适配的附件格式导出完整会话。
- `/todo [内容]`：项目待办管理。无参先用单选卡选项目，再打开待办管理卡（飞书代码卡自带输入表单，钉钉复用提问卡模板的输入框，TG/Slack 为文本列表 + 命令提示）；带内容时先选项目再直接追加。旧 `/todo <Agent 编号> [内容]` 继续兼容，但不再主推。
- `/todo-rm`：先选项目，再用就地刷新的选择卡逐条删除；旧 `/todo-rm <Agent 编号>` 继续兼容。
- `/todo-auto [内容]`：先选项目，再切换待办的自动执行标记（⚡，whats-next 时直接派发不发卡）并提供新增入口；飞书在切换卡底部直接带输入框，钉钉有待办时发切换卡 + 独立新增卡、无待办时只发新增卡，TG/Slack 提示 `/todo-auto <内容>`。命令带内容时选中项目后直接新增自动待办。旧 `/todo-auto <Agent 编号> [内容]` 继续兼容。

三个 todo 命令共用项目候选：工作中 Agent 项目 ∪ 空闲在线 Agent 项目 ∪ 未隐藏的最近 workspace ∪ 已有待办的项目。排序依次为工作中、空闲在线、置顶 workspace、其他最近 workspace、仅存在于待办存储的项目；组内保留置顶/最近顺序。选择卡与 `/new` 一样首屏列 5 个，超出时提供“显示更多”。
- `/detect`：渠道配置识别流程使用的临时命令；成功后只回执已填字段，不回显完整 ID。

命令支持中文别名与无前缀短语的部分由 `autochannel` 统一分类；平台特有前缀、附件格式和卡片能力在传输层适配。

## Watch

Watch 规格见 `docs/specs/im-watch.md`。订阅持久化在 `~/.askhuman/state/watch.json`，daemon 重启后尽量继续编辑原卡。状态卡展示 Agent 工作中、空闲、等待回答或已结束，以及跨 Turn 累计、扣除真正 idle 的工作时长和最近活动；状态变化、提问和回答会唤醒引擎。已在关注的 Agent 进入空闲后保留 5 分钟宽限，期间开始下一 Turn 可由原卡继续。

引擎按结构化帧签名避免无变化编辑，并按平台限制节流。Agent 结束、用户取消、自动切槽或连续发送失败都会让订阅进入终态并释放。共享状态在 `watch.rs`，平台渲染和回调在各自 `<channel>/watch.rs` 与 Router 中。

## 单选卡、Confirm 与导出

无参命令需要选择 Agent 时复用 `select.rs` 的传输无关模型；选项以 session id 为稳定身份，展示 daemon seq。飞书、Telegram、Slack 可就地刷新或把选择卡变成 watch 卡；钉钉受模板能力限制，选择后可能另发目标卡。详细协议见 `docs/specs/im-select-card.md`。

`/stage` 复用 `confirm/` 的跨渠道双动作展示与传输，但 Git 指纹、slot 到业务动作的映射和 `git add -A` 仍由 daemon 台账负责。diff/transcript 内容分别由 `gitutil.rs`、`agents/transcript_full.rs` 生成，再由 `export/` 转成渠道支持的附件格式。详细协议见 `docs/specs/im-diff-stage-transcript.md`。

`/msg` 的目标选择复用同一单选卡模型，一次性输入由 `PickerKind::MsgCompose` 管理；30 分钟 TTL 与
`~/.askhuman/state/msg-compose.json` 的无正文最小恢复账本保证重复提交、过期和 daemon 重启不会误发。
共享视图/校验在 `msg_card.rs`，实际队列仍由 `agents/interject.rs` 管理；交互规格见
`docs/specs/im-msg-compose-card.md`，插话能力见 `docs/specs/agent-interject.md`。

`/todo`、`/todo-rm`、`/todo-auto` 的项目选择、逐条删除与自动执行切换同样复用单选卡台账（`PickerKind::Todo/TodoRm/TodoRmEntry/TodoAuto/TodoAutoEntry/TodoManage`）；项目路径直接作为稳定选项 ID，待新增文本暂存在 picker payload。待办存储直读 `todos.json`，命令层实现在共享 daemon core 的 `daemon/unix_impl/todo.rs`（目录名为历史遗留），macOS/Linux/Windows 语义一致；能力边界见 `docs/specs/todo-whats-next.md`。

`/new` 的 workspace / Agent / 权限步骤也复用单选卡；Telegram 有项目 TODO 时再复用一张任务来源
选择卡。最终任务输入复用结构化 Confirm 的渠道原生输入能力，但只投放到命令来源渠道；TODO 按所选
workspace 的 git root 读取，任务卡保存文本快照，Terminal 成功打开后才 best-effort 出队。启动参数不经过
shell：IM 数据进入一次性 `0600` LaunchRecord，Terminal shell 只接收 AskHuman 绝对路径和 UUID
token。详细边界见 `docs/specs/im-agent-task-launch.md`。

## 主要代码入口

- `src-tauri/src/autochannel.rs`：命令分类、帮助文案、活跃槽与共享回复文案。
- `src-tauri/src/daemon/mod.rs`：入站监听、命令分派、补推、watch/select/confirm 台账。
- `src-tauri/src/watch.rs`、`select.rs`、`msg_card.rs`：传输无关状态、选择与 `/msg` 输入视图模型。
- `src-tauri/src/agents/workspaces.rs`、`integrations/agent_launch.rs`：workspace 索引、readiness 与安全终端启动。
- `src-tauri/src/gitutil.rs`、`confirm/`、`export/`：Git 操作、确认和附件导出。
- `src-tauri/src/{telegram,dingtalk,feishu,slack}/`：平台传输、渲染、回调与路由。
