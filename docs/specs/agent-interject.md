# Agent 插话（Interject）：工作中主动向 agent 发消息

> 状态：已实现（Claude Code / Codex / Cursor；Grok 不支持）。
> 实现计划见 `docs/plans/agent-interject.md`。

> **实现期补充（2026-07）**：GUI composer、状态窗口、托盘和 IM `/msg` 均已接入；发送入口收敛为
> **仅工作中、非 Grok**。`/msg <内容>` 可按唯一 watch 目标直发，否则复用通用单选卡选择工作中的 Agent。
> `/msg` 无参数与 `/msg <编号>` 的一次性消息输入卡已实现，见
> `docs/specs/im-msg-compose-card.md` 与 `docs/plans/im-msg-compose-card.md`。
> 排队消息真正被 PreToolUse hook 消费后，Daemon 向原 IM 来源回推“已阅读”回执；即时送达、撤回、覆盖或
> 会话结束时未消费的消息不回执。
>
> **附件补充（2026-08）**：桌面 composer 与 Agent 控制台支持文件/图片附件及仅附件插话；IM `/msg`
> 仍为纯文本。普通文件始终引用源文件绝对路径，不复制、不托管；没有源路径的剪贴板图片复用问答弹窗的
> `temp/askhuman` 临时目录，由既有 24 小时 GC 清理。

## 1. 需求

目前只有 agent 主动调用 AskHuman 提问时，人类才有机会发消息。当我们观察到 agent 方向跑偏时，
希望**不等它提问、主动把话插进去**。核心思路：利用各家 agent 的 **PreToolUse hook**（工具调用前
必经、可 block、可回传内容）作为插话的注入点。

用户体验目标：

- 在 **Agent 状态窗口**与**托盘菜单**对某个工作中的 agent 点「发送消息」→ 弹出输入窗口；
- 提交后，消息在该 agent **下一次工具调用**时送达（当前工具调用被 deny，模型带着消息重新规划）；
- **混合等待**（用户定案的优化）：如果用户已经点开了输入窗口（表达了「我要说话」的意图），
  且该家 hook 支持长超时，则 hook 应**暂停等待**用户提交或取消——提交＝deny+内容；取消＝放行。
  「点开弹窗＝准备好要输入」，让 agent 停下来等，比让消息晚一拍更好；
- 不支持长等待的家（Grok）不等（且首期整体排除，见 D1）。

## 2. 调研结论：四家 PreToolUse 能力（零实测，静态分析 + 官方文档）

> 证据来源：Claude Code 官方 hooks 参考（code.claude.com，2026-07 现行版）；Codex 本机源码快照
> （2026-07-03，`codex-rs/hooks` + `core/src/hook_runtime.rs`）；Cursor 官方 hooks 文档 + 本机
> cursor-agent bundle（2026.06.26）静态核对；Grok 本地官方文档（0.2.82 `10-hooks.md`）+ 既有逆向
> 调研 `docs/specs/grok-cli-integration-research.md`。全程未实际调用任何 agent（计费红线）。

| 能力 | Claude Code | Codex | Cursor | Grok |
|---|---|---|---|---|
| **block 工具调用** | ✅ `permissionDecision:"deny"`；另有 `ask`（原生确认弹窗）、`defer`（仅 `-p`）；exit 2 也可 | ✅ deny JSON 或 exit 2+stderr；`ask` 不支持（fail-open） | ✅ `permission:"deny"`；exit 2 也可；**`ask` schema 接受但不生效**（官方文档明示 + 社区 bug 多版本未修） | ✅ 仅 PreToolUse 可 deny（JSON `decision:"deny"`，无论退出码）；其余事件全被动 |
| **把消息带给模型** | ✅ 最强：deny 的 reason 给模型；`additionalContext`（v2.1.9+，system-reminder 注入，allow 时也可用）；`updatedInput` 改写入参 | ✅ 同强：deny reason 以 `blocked by PreToolUse hook: {reason}` 喂回模型；`additionalContext` 以 developer 消息入会话（源码 `record_additional_contexts`）；`updatedInput` | ⚠️ 一条可靠通道：**deny 时 `agent_message` 喂回模型**（官方文档「fed back to the agent」）。`additional_context` 官方只给 postToolUse/sessionStart；preToolUse 响应 protobuf 有该字段但未文档化、社区报「记录但不进模型」bug | ❌ 最弱：无 additionalContext / updatedInput（二进制字符串表无这些字段 + 实测 hook stdout 不进模型，三证）；deny 的 `reason` 官方只说进 UI scrollback，**是否喂回模型无任何正面证据** |
| **超时足够长** | ✅ 默认 600s，按 hook `timeout`（秒）配置，文档无上限；hook 运行期间该工具调用等待 | ✅ 默认 600s（源码 `unwrap_or(600).max(1)`），按 hook 配置，无上限；async hook 未支持 → 天然同步阻塞 | ✅ 默认 60s（bundle 常量），按 hook `timeout` 配置；>3600s 仅 console 警告、无硬上限；**超时默认 fail-open**（`failClosed:true` 可改，本功能不用） | ⚠️ 默认仅 **5s**，可配但超时 fail-open + 官方明示「长 hook 阻塞 UI、保持短小」→ 长等待不可靠 |

补充事实：

- **PreToolUse 只在「要调工具时」触发**：模型纯思考/长文本输出阶段没有拦截点。编码类任务工具调用
  密集，插话粒度实际够细；且按本项目提示词协议，agent 结束回合前必调 AskHuman → 每回合至少有一次
  工具调用可拦（用户据此定案不需要 Stop hook 兜底，见 D6）。
- **hook 每次工具调用都会 spawn** → 绝不能无条件弹窗/等待；必须有「用户先表达意图」的信号。
- Codex 各输出格式与 Claude 兼容（`hookSpecificOutput` 同构）；纯 `allow`（无 updatedInput）被记
  unsupported（fail-open 无害）。Codex 用户级 hook 需信任哈希（`[hooks.state]`），本项目已复刻算法。
- Cursor 恒兼容加载 `~/.claude/settings.json` → 双触发，需运行时 env 去重（`report.rs` 已有）。
- Claude 多 hook 冲突时 deny > defer > ask > allow；Cursor 超时=放行（fail-open），等待被放行时消息
  仍留队列、下次工具调用送达，不丢。

## 3. 设计定案

### D1 范围：跟随生命周期追踪；Grok 排除

- 插话依赖会话身份与状态展示，**跟随「生命周期追踪」实验开关**（`agent_lifecycle`，按家开启），
  无独立开关、无新增安装产物：**扩展现有 PreToolUse activity hook**（`__agent-hook <agent> activity`）
  ——触发时在上报 activity 之余顺带向 daemon 查询/等待插话。
- **Grok 首期排除**：不改其 hook 行为，AgentsView / 托盘对 grok 会话不显示「发送消息」入口。
  理由：无可靠「传话」通道（见 §2），deny-only 只能拦停不能解释，体验差；等 Grok hook 能力演进。
- macOS、Linux 与 Windows 共用 lifecycle hook、daemon 队列和 GUI Host；Grok 仍排除。

### D2 消息模型：每 session 一份条目列表（queue of entries）

- daemon 内存中按 `session_id` 维护
  `{ entries: Vec<InterjectEntry { text, attachments }>, composer_open: bool }`。每个条目可以同时有文本和
  附件，也可以仅有附件；文本与附件均为空不入队。附件是 `FileAttachment` 元数据及绝对路径引用。
- **弹窗**：打开时把现有全部条目文本按空行拼接预填，并恢复待送达附件；用户在其上编辑，提交＝
  **整体覆盖**队列（连续第二次点「发送消息」看到上次未消费内容与附件、可直接编辑覆盖——用户定案）；
  取消＝不动队列。已消失的源文件显示为不可用引用，用户可以移除。
- **Agent 控制台**：文本与附件都采用**追加**语义；待送达区域同时显示条目数和附件数。
- **IM**：`/msg <编号> <内容>`＝**追加**条目（IM 看不到旧文本，覆盖会静默丢内容）；工作中目标的
  `/msg <编号>` 打开一次性输入卡，空闲目标仍回显当前待送达全文；无参 `/msg` 可先选目标再输入；
  `/msg-clear <编号>`（`/撤回`）＝清空。回执告知当前共几条。当前 IM 入口仍只接收文本，不接收附件。
- **送达**：hook 消费时全部非空文本按空行拼成一条消息，所有附件按条目顺序展平为路径列表，一次性
  带给模型，随后清空。socket 写失败时必须把原始条目边界、文本、附件与 IM 已阅读回执渠道完整回到
  队首，不能用一次覆盖提交折叠或覆盖期间新入队的内容。
- **生命周期**：留队直到被消费；AgentsView 显示「待送达」徽标并可撤回；会话结束（ended）自动清空。

附件文件的生命周期：

- 从文件选择器或拖拽得到的文件只记录 canonical 绝对路径；AskHuman 不复制、不接管、不删除源文件。
- 剪贴板图片没有可引用的源路径，提交时写入现有 `temp/askhuman/interject-<uuid>/q1/` 请求临时目录，
  与问答弹窗一样由 24 小时 GC 清理，不引入长期托管目录。
- 队列保存超过源文件寿命或临时图片 24 小时寿命时，路径可能失效；插话通常会很快消费，接受这一权衡。

### D3 hook 三态协议（PreToolUse 触发时）

1. 队列有已提交消息 → 立即 **deny + 消息**（打断当前工具调用，模型带话重新规划——纠偏语义，用户定案默认）；
2. 该 session 的 composer 打开中 → hook **阻塞等待**提交或取消：提交 → deny+内容；取消/关窗 → allow 放行；
3. 都没有 → 立即 allow（毫秒级，零感知）。

各家 deny 输出格式：

- Claude / Codex：`{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"<包装后的消息>"}}`；
- Cursor：`{"permission":"deny","user_message":"<包装后的消息>","agent_message":"<包装后的消息>"}`
  （**双字段同文**，2026-07-06 live 实测修正，见 D3 末尾勘误）。

**包装文案**（英文协议文本，`prompts.rs` 单一来源，与既有 prompts 风格一致）。要点（用户定案）：
带明确前缀标明「这是用户发来的消息」；讲清 deny 语义——**拦下只是为了送信，不是不允许用这个工具**，
读完消息后如仍合适可原样重发同一调用：

```text
[USER INTERJECTION] The user sent you the message below while you were working.
This tool call was blocked only to deliver it — the tool is not forbidden; re-issue
the same call if still appropriate.

<user_message>
{message}
</user_message>

<attachments>
- "/absolute/path/to/file"
</attachments>
These are local file paths. Open and use them as inputs to the user's interjection.

Adjust your plan if needed. If anything is unclear, ask the user as instructed.
```

没有附件时省略整个 `<attachments>` 块；仅附件插话的 `<user_message>` 内容为空。路径用 JSON 字符串
转义，避免空格、引号等字符破坏协议。

（用户三轮定形：精简版正文；消息块用 XML tag；末句不点名具体提问工具——提问入口可能经脚本
封装、名字不一定叫 AskHuman，用最短的 "as instructed"。）

**Cursor 字段勘误（2026-07-06 live 实测 + bundle 静态核对）**：原设计据官方文档把协议文本放
`agent_message`（"fed back to the agent"）、`user_message` 放本地化短提示——实测模型只收到了
`user_message`（短提示），插话正文丢失。核对本机 Cursor IDE（`cursor-agent-exec/dist/main.js`）与
cursor-agent CLI（`hooks-exec`）的 deny 分支，喂回模型的拒绝理由均为
``l.user_message || `${toolName} blocked by preToolUse hook` ``（再拼 "Agent note"）；
`agent_message` 仅透传 protobuf、未见进模型的消费点。旁证：Cursor 对 Claude 格式 hook 输出的兼容
转换把 `permissionDecisionReason`（喂模型语义）映射到的正是 `user_message`。定案（用户确认）：
**`user_message` 与 `agent_message` 都放完整协议文本**——当前版本走 `user_message` 生效，未来
Cursor 若按其文档语义改用 `agent_message` 也不断；代价是 Cursor UI 的拦截提示显示整段协议文本
（内含用户原话），可接受。上方能力表中 Cursor「把消息带给模型」的通道应更正为 `user_message`。

并发工具调用（如 Claude 并行 tool use）：多个 hook 同时 poll 时，消息**只交付一个**（daemon 原子出队），
其余 allow；等待中的多个 waiter 在提交时同样只有一个拿到消息。

### D4 IPC 与轮询协议（性能优先，用户明确要求不影响所有 tool call）

- **热路径零文件 IO**：队列在 daemon 内存（HashMap，O(1) 查询）；`interject.json` 只在**变更时**
  （提交/追加/撤回/消费/会话结束）原子落盘、daemon 启动时读一次（D8）。**hook 不读任何文件**。
- **复用既有连接**：activity hook 本就 spawn `AskHuman __agent-hook` 并连 daemon 发 `AgentEvent`；
  插话只在同一连接上**多一次请求-响应往返**（本地 Unix socket / Windows named pipe，通常为微秒～毫秒级）。无新进程、无新连接。
- 协议：`ClientMsg::AgentEvent` 增 `interject_poll: bool`（serde default，旧 daemon 忽略）。
  daemon 收到 `interject_poll=true` 立即回一帧三选一：
  - `None` → hook 直接 allow 退出；
  - `Message(text, attachments)` → hook 输出含用户文本与附件路径块的 deny JSON 后退出；
  - `Hold` →（composer 打开中）hook 继续阻塞读第二帧 `Message(text, attachments)` / `Release`（取消）。
- **旧 daemon 兼容 / daemon 不可达**：hook 对首帧回复设短超时（~300ms），超时/断连一律 allow
  （fail-open，插话绝不拖慢正常工具调用）。
- 只有 **PreToolUse** 且通过既有去重（`running == intended`）的那次上报才 poll；PostToolUse 不 poll。
- 该 poll 连接**不计入 daemon 空闲保活**（等待可长达数小时，不能借此续命；类比 TraySubscribe 抵消法）。

### D5 hook 安装产物变更（等待需要长超时）

- Claude `~/.claude/settings.json` PreToolUse 条目、Cursor `~/.cursor/hooks.json` preToolUse 条目、
  Codex `~/.codex/hooks.json` PreToolUse 条目：显式加 `"timeout": 86400`（仅 PreToolUse；其余事件维持默认）。
- Codex 信任哈希包含 timeout 字段 → `codex_trusted_hash` 按条目实际 timeout 计算（PreToolUse=86400，
  其余 600），随安装写入 `[hooks.state]`。
- Cursor 超时 fail-open 属可接受降级（放行后消息留队列不丢）。

**已开启用户的 hook 更新流程**（用户要求明确设计；全部复用既有机制，无新流程）：

1. **过期判定扩展**：`agent_lifecycle::status()` 的 `outdated` 口径，除「命令逐字一致」外，
   增加「PreToolUse 条目 `timeout == 86400`」（Claude/Cursor/Codex 三家；Grok 不动）。
   Codex 另有信任哈希校验：期望哈希按 hooks.json 实际 timeout 计算，旧安装（无 timeout、
   哈希按 600 算）自动判不匹配 → `outdated: true`。
2. **自动迁移**：daemon 启动即调 `migrate_outdated()`——对「已安装且 outdated」的家族幂等重装
   （重写条目补 timeout、重算并写 Codex 信任哈希）。用户升级二进制后 daemon 重启
   （自更新 graceful drain / 登录自启 / 按需拉起）即自动完成，**无需任何手动操作**。
3. **手动兜底**：设置页实验区该家族显示「需更新」徽标 + 更新按钮；CLI `agents update <agent>
   --lifecycle` 与 `doctor`（`needsUpdate: true`）同口径。
4. 重装只触碰本功能标记（`__agent-hook`）条目，用户其它 hook 与文件格式保留（既有 CST/toml_edit 编辑）。

### D6 不做 Stop hook 兜底（用户定案）

排队消息若未被消费（回合内不再调工具就结束），留队等下一回合。按本项目提示词协议，agent 结束回合前
必调 AskHuman（即必有工具调用），实际不会落空。Claude/Codex 的 Stop 可 block+reason 属可行的后续增强，
首期不做。

### D7 入口与 composer 窗口

- **AgentsView**：每个工作中、非 grok 的 agent 卡片加「发送消息」按钮；有待送达时显示徽标 + 撤回。
- **托盘**（2026-07 随 gui-agent-console 改版调整为独立分组）：分隔线 +
  「打开 Agent 状态窗口」直达项 + **忙闲概览子菜单**（标签即「工作中 w · 空闲 i」）+
  「新建 Agent 任务」末项（生命周期未开启时该项留在窗口区兜底）——
  逐 agent 子菜单（标签＝类型+项目名，工作中在前，ended 不列）；每个 agent 下挂：
  **在控制台查看**、**发送消息**（仅工作中、非 grok）、**添加待办**、**聚焦终端**
  （沿用现有 pid+受支持终端条件）。「置为空闲」需二次确认，不进托盘、仍留状态窗口。
  `TrayState` 扩展 agent 摘要列表；菜单 diff 机制（`tray_menu.rs`）沿用。
- **composer 窗口**：GUI 宿主新窗口类型（`WindowKind::Interject`，URL 带 session 参数），
  **每 session 全局唯一**（聚焦或新建），观感按弹窗风格做；托盘/AgentsView 都经宿主路由
  （`host_open`），宿主不在则拉起，全程失败回退本进程建窗。
- **composer 状态与 daemon 同步**：窗口打开即经自己的 daemon 连接登记 `composer_open`（连接断开＝
  自动视为关闭，杜绝宿主崩溃后的僵尸「打开中」状态挂起 hook）；提交/取消发对应消息。
  该连接同样不计入空闲保活。
- **附件交互**：composer 与 Agent 控制台均支持选择/拖入任意文件和粘贴图片，也支持仅附件发送；
  composer 重开时恢复队列中的附件引用，默认随编辑后的整体提交保留。附件缩略图读取只发生在桌面 UI，
  不进入 hook 热路径。

### D8 持久化

`~/.askhuman/state/interject.json`（与 `watch.json` 同模式：原子写、best-effort）。只存 entries
（composer_open 是连接态不持久化）；daemon 换新（graceful drain 升级为常态）/重启后恢复，
会话结束清理对应条目。为兼容既有文件，`sessions` 继续保存文本字符串数组，新增可选的 `attachments`
map 按 session 与条目下标对齐保存附件数组；旧文件缺少该字段时按无附件加载，附件-only 条目以空文本
占位保存。

### D9 IM `/msg`（已实现）

命令语义见 D2。当前四个 IM 渠道保持纯文本，不提供文件选择/上传入口。与 `/status` 同门控
（daemon 存活即可用、不依赖 autoActivation 开关）；编号复用
`/status` 的稳定 seq；grok 会话回「该 agent 不支持插话」。空命令输入卡是一次性的，展示待送达
首尾预览，服务端限制 3000 个 Unicode 字符并在提交时重验目标；有效提交先原子消费卡台账，再复用
`deliver_msg`，成功只在原卡显示即时送达/排队结果（卡内不显示队列总条数）。30 分钟 TTL 与不含正文的最小恢复账本负责重启、
过期和重复 callback。四渠道适配及验收边界见 `docs/specs/im-msg-compose-card.md`。help 文案按渠道
前缀规则（Slack 用 `!`）。

## 4. 性能分析（回应用户关切：不能影响所有 tool call）

每次工具调用的增量成本（生命周期追踪开启时）：

| 成本项 | 现状（activity hook） | 加插话后 |
|---|---|---|
| 进程 spawn | 已有（`__agent-hook`） | **不变**（复用同一进程） |
| daemon 连接 | 已有（发 AgentEvent） | **不变**（复用同一连接） |
| 消息往返 | 0（即发即走） | **+1 次本地 IPC 请求-响应**（daemon 侧 O(1) 内存查表） |
| 文件 IO | 0 | **0**（hook 热路径不读附件；持久化只在插话变更时写、启动读一次） |

- 无插话时增量 ≈ 一次本地 socket 往返（微秒~毫秒级），相对 hook 进程 spawn 本身（几十 ms 量级）可忽略。
- daemon 不可达/旧版本：300ms 上限后放行，不阻塞。
- 生命周期追踪未开启的家族：hook 不存在，零成本。

## 5. 反馈意见记录

- （2026-07-06）用户确认混合方案：composer 打开＝等待信号；deny+消息为默认注入语义；Grok 首期排除；
  hook 并入 lifecycle activity hook；不做 Stop 兜底（提示词协议保证回合内必有 AskHuman 调用）；
  消息留队直到消费/会话结束；弹窗预填上次内容、提交覆盖；IM 采用追加、弹窗采用整体覆盖的统一队列模型；
  composer 用 GUI 宿主窗口；托盘 Agent 父项改子菜单（打开状态窗口 + 逐 agent 的发送消息/聚焦终端）；
  持久化 interject.json 但热路径不得有文件 IO（性能分析见 §4）。
- （2026-07-06）计划评审：用户确认已开启用户的 hook 更新流程（过期口径扩展 + `migrate_outdated()`
  自动迁移 + 设置页/CLI 手动兜底）；deny 包装文案三轮定形——`[USER INTERJECTION]` 前缀 + 说明拦截
  只为送信非禁用工具；末句不点名 AskHuman（选最短版 "as instructed"）；正文取精简版；
  消息块用 XML tag（`<user_message>`）。
