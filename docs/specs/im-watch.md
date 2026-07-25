# /watch 实时关注 agent 进度（IM 单卡静默刷新）

> 状态：四渠道已实现。无参选择、活跃槽联动与重新关注属于后续演进，分别见
> `docs/specs/im-select-card.md`、`docs/specs/im-auto-end-watch.md`、`docs/specs/watch-rewatch.md`。
>
> **当前行为补充（2026-07）**：`/watch`、`/status` 及多订阅时的 `/unwatch` 无参路径使用通用单选卡；
> `autoActivation` 开启时，执行 watch/status/msg 等上下文操作会激活当前渠道，切离渠道可按
> `autoEndWatch` 自动结束其订阅。下文早期“回纯文本列表”与“watch 不切活跃槽”的描述已被这些专题
> 规格取代。

> 在 IM 里 `/watch <编号>` 关注一个 agent：机器人发一张「实时状态卡」，此后该 agent 的所有进度
> **就地更新这张卡**（编辑消息，不发新消息、零打扰）；卡片自带**动态按钮**（取消关注 / 立即刷新，
> 随状态变化，agent 结束后禁用并定格终态）。构建于既有生命周期追踪（`agents/registry.rs`）与
> `/status` 活动解析（`agents/activity.rs`）之上。
>
> 渠道：飞书（P1 首发）+ Telegram + Slack + 钉钉（多渠道扩展，`docs/plans/im-watch-channels.md`；
> 四渠道全部上线）。引擎与语义（签名门控 / 跟底 / 终态 / 持久化）全渠道一致，差异仅在渲染与
> 传输（见「渠道差异」节）。附带定案：**提问投放并集**——按需发送开启时，提问卡投放
> 「最后活跃渠道 ∪ 正在 watch 提问 agent 的渠道」（见「更新引擎」节）。

## 决策记录（用户经 AskHuman 定案）

- **D1 整体形态＝方案 A「纯静默单卡」**：进度只编辑卡片，**不**在关键转折发新消息。
  用户附加要求：卡片不是纯文本，下方要有**按钮**，且按钮随 watch 状态自动更新
  （如 agent turn 结束后按钮呈现不同状态）。
- **D2 钉钉路径＝专用模板**（用户后台新建 watch 模板），留待 P2；P1 不做钉钉。
- **D3 分期**：P1 仅飞书（完整交互 + 全部细节），Telegram / Slack / 钉钉后续批次。
- **D4 P1 交付方式**：设计写入文档后**直接实现**，不中途审核；完成后交用户真机验收再调。

## 渠道可行性（已查证）

| 渠道 | 就地更新机制 | 频控 / 时限 | 按钮 | 状态 |
|---|---|---|---|---|
| 飞书 | PATCH `im/v1/messages/:id`（代码已有 `patch_card`） | 单消息 5 QPS；应用 1000/min；仅 14 天内可改；卡片 ≤30KB | 卡片 button + callback（FsRouter 按 message_id 路由） | **已上线（P1）** |
| Telegram | `editMessageText`（已有），48h 内可编辑 | ~1 次/秒/会话 | inline keyboard | **已上线** |
| Slack | `chat.update`（已有 `update_message`） | Tier 3 ≈50/min/频道 | Block Kit button | **已上线** |
| 钉钉 | `PUT /v1.0/card/instances` 更新公有变量（已有） | 实测 2s×60 连发零频控，p50 ≈60–95ms | 模板内按钮（Stream 回调） | **已上线**（专用模板，M4） |

数据粒度约束：数据源是 lifecycle hook（工具事件实时）+ transcript 尾部（Cursor 文字回合结束才落盘、
Codex 完成时整条写入、Claude 渐进写入），**没有 token 级流**。卡片是「变化驱动的最新一帧」，
不是逐字打字机。

## 命令面（`autochannel::classify` 扩展）

- `/watch <编号>`（别名 `/关注`）：关注该编号 agent（编号同 `/status`）。成功的回执**就是卡片本身**，
  不再回文本。重复 watch 同一 agent＝**换新卡**：旧卡定格为「已由新卡片接替」，发一张新卡
  （把卡拉到会话底部）。
- `/watch`（无参）：有可选 Agent 时发通用单选卡；无 Agent 时回文本空态。已关注项带「· 关注中」徽标，
  点选它沿用“换新卡”语义。详见 `docs/specs/im-select-card.md`。
  （验收反馈调整：原设计只列关注，改为先给可选对象、免用户再敲一次 /status。）
- `/unwatch <编号>`（别名 `/取消关注`）：取消该关注，旧卡定格「已取消关注」+ 回一条确认文本。
- `/unwatch`（无参）：恰有 1 个关注→取消它；多个→发通用单选卡；0 个→提示。
- `/unwatch all`（`/unwatch 全部`）：全部取消（**仅本渠道的**关注，不动别的渠道）。
- **渠道门控**（`watch::channel_supported`）：四渠道（飞书 / Telegram / Slack / 钉钉）全支持；
  门控保留兜底未来新渠道。`/help` 引导文案仅在支持渠道列出 watch 命令。
- **Slack 用 `!` 前缀**（`!watch 3` / `!unwatch`，B 案）：Slack 客户端把一切 `/` 开头输入拦截为
  本地 slash command，未注册的名字发不出来。`!` 备用前缀四渠道通用（仅已知命令生效，未知 `!xxx`
  仍按普通文本处理）；Slack 的提示/引导文案展示 `!`，其余渠道仍展示 `/`（`autochannel::cmd_prefix`）。
- 关注上限：**每渠道 5 个**（超出回提示）。同一 agent 可在多个渠道各自关注，互相独立；
  同渠道重复 watch＝换新卡。

## 卡片（飞书卡片 JSON 2.0，`feishu/card.rs::build_watch_card`）

```
👁 实时关注 [3] · Cursor — HumanInLoop          ← 样式化头部行（icon + 蓝色小字）+ hr
🟢 工作中 · 累计工作 6 分钟                       ← 状态行（emoji 状态 + 累计有效工作时长，见下）
「重构 daemon 空闲退出逻辑」                      ← 会话标题（有则显示）
最近动态（14:32:05）：                            ← 绝对时刻（本地时区），不用相对时间
我已经完成 registry 的改动，现在开始跑单测验证……
                                                 ← 文字与足迹之间空一行（用户定案：不用分隔线）
… 已省略 2 步                                     ← 灰字：文字之后超出 3 步窗口的更早调用数
● **读取**: *registry.rs*                        ← 足迹时间线：**最后一段文字之后**最近 ≤3 步
● **编辑**: *mod.rs*                                （旧→新），彩色圆点状态（已完成灰 / 进行中绿 /
● **运行命令**: *Rebuild after… (cargo build)*      失败红）；类别词加粗、参数斜体（去类别 emoji）
▸ 📋 TODO 4/7 · 当前：跑单测                      ← TODO 折叠面板（标题即摘要；默认收起，点开
                                                    见全清单；agent 未用 todo 功能则不出现）
────
最后更新 14:32:07                                 ← 灰色小字
[ 取消关注 ]  [ 立即刷新 ]                        ← column_set 两列按钮
```

- **状态行四态**：`🟢 工作中` / `⚪ 空闲` / `🙋 正在等待你的回答`（该 agent 有在途 AskHuman 提问时
  覆盖显示，优先级最高）/ `⏹ 已结束`。
- **累计有效工作时长（2026-07 验收调整）**：四态都可附 `· 累计工作 X`。时长在同一 session 内
  跨 Turn 累加，只有真正进入 `Idle` 的区间被扣除：`turn-start` / 首个工具 Activity / 无 Hook 的
  working 兜底开启活动区间，真正 `turn-end` 冻结并累加；Stop Hook 选择 continuation 不产生
  `turn-end`，故不重置，AskHuman / Stop confirmation 等待也继续计入。Idle / Ended 终态卡展示
  冻结总值。registry 将 `activeElapsedSecs`（已完成区间）与 `activeSince`（当前区间起点）持久化，
  snapshot 的 `activeElapsedSecs` 为含当前区间的生效总值。旧 `agents.json` 无法准确重建历史活动，
  升级后从 0 开始累计，绝不回退 `startedAt` 墙钟近似。<1 分钟在 Watch 卡不显示（单选卡仍显示秒）；
  **累计时长不入签名**，只随其它变化或手动刷新更新，避免纯走钟反复编辑。
- **足迹时间线（验收反馈新增，替代单工具行）**：**最后一段助手文字之后**的最近 ≤3 次工具调用
  （旧→新）——最新事件是文字时不显示任何工具行（用户定案：文字之前的调用属于上一段叙述）；
  文字之后超出 3 步窗口的更早调用以灰字「… 已省略 N 步」标注在时间线上方（计数入签名）。
  每步 = 状态圆点 + **类别词加粗** + *参数斜体*（用户定案：不用类别 emoji）。状态判定——
  **只有末步可能「进行中」**：新调用/助手文字的出现即证明前面的步已结束（Cursor 等家族不一定
  写工具结果事件，「后面又发生了事」是唯一可靠的完成信号）；带 `is_error` 的工具结果 → 失败
  （红点）。**Cursor 家族例外**（验收反馈修正「答完问题 AskHuman 仍绿点」）：Cursor 的
  transcript 只在工具**跑完后**才落盘该次调用（实测 in-flight 探针不可见）且从不写 tool_result
  ——落盘即已结束，故 Cursor 的 transcript 步一律「已完成」，「进行中」只能由实时 hook 的
  `currentTool` 并入（Claude/Codex/Grok 在调用开始时即写盘，末步无结果 = 真在跑，维持原判定）。
  Codex Code Mode 的工具被包在 `custom_tool_call{name:"exec"}` 内：从规范包装中提取
  `tools.exec_command({...})` 的 JSON 参数，`rg/grep/sed/cat/head/tail/less/jq` 等常见只读命令显示为
  「读取文件」，其余命令保底显示为「运行命令」，其它内层工具保留工具名；对应
  `custom_tool_call_output` 闭合完成/失败状态。`apply_patch` 不按 JavaScript 正文展示，改读完成时的
  结构化 `event_msg.payload.type="patch_apply_end"`，按 `success` 标为完成/失败，并从 `changes` 只展示
  `首文件名 +N`（不读取或外发 diff 正文），避免同一次编辑重复出现。
  hook 实时 `currentTool` 并入为进行中末步（2026-07-25 起按**内容收敛**：驻留到被 transcript
  同款步接替 / turn-end 清除 / 10 分钟 TTL 兜底；旧「与文件 mtime 比时刻」会被无关写入秒级
  挤掉造成闪烁）。**Cursor IDE（Agent Window）会话**（2026-07-25）：其 jsonl 在长回合内冻结
  数小时，activity/标题/完整会话一律优先读 Cursor 全局 `state.vscdb` 实时源
  （`agents/cursor_vscdb.rs`，只读 + 失败回退 jsonl；CLI 会话不在库中天然走 jsonl，且其
  工具状态为真实值、不做「一律已完成」收敛）。飞书卡用
  `<font color='green|grey|red'>●</font>` 彩色圆点；`/status`（纯文本，全渠道）用 emoji 圆点
  🟢/⚪/🔴（无粗斜体）。AskHuman 提问 = 一次 Shell 调用：等待时绿点，答完变灰点 + 状态行
  🙋→🟢 + 步数 +1，三处可感知。Cursor/Claude 的 Shell 调用自带人话 `description` → 显示
  `描述 (命令)`。`/status <编号>` 同享同一份解析。
- **TODO 清单（用户定案 A+B：摘要常显 + 折叠面板全清单）**：解析 transcript 里 agent 自报的
  任务清单——Cursor/Claude 的 `TodoWrite`（Cursor 带 `merge=true` 增量更新，按 id 就地重放合并；
  Claude 恒整表替换）、Codex 的 `update_plan`（整表）；Grok 无此机制。这些调用**不入**足迹
  时间线（实时 hook 侧同样过滤）。cancelled 条目剔除；条目文字截断 60 字符。飞书卡上渲染为
  **折叠面板**（collapsible_panel，默认收起不占高度，上外边距 12px 与足迹时间线拉开——用户
  定案）：标题即摘要行 `📋 TODO 4/7 · 当前：xxx`（「TODO」不翻译，用户定案）
  （done/total 计数 + 首个进行中条目；无进行中省略「当前」段），展开见全清单（进行中绿点加粗 /
  已完成灰点删除线 / 待办空心圈 ○）。清单变化计入签名（触发编辑；PATCH 会重置收起态，可接受）。
  `/status <编号>`（纯文本，全渠道）仅附摘要行。限制：解析窗口为 transcript 尾部 256KB，
  太久未更新的清单可能超窗丢失（best-effort）。
- **绝对时间**：卡片只在内容变化时编辑，相对时间会走字失真，故一律绝对时刻（今日 `HH:MM:SS`，
  跨日 `MM-DD HH:MM`，本地时区经 `libc::localtime_r`）。
- **动态按钮（D1 附加要求）**：
  - 活动态（工作中 / 空闲 / 等待回答）：`[取消关注]`（danger）+ `[立即刷新]`（default），
    behaviors callback `value={watch:"unwatch"|"refresh"}`。已在关注的 Agent 转为空闲后，卡片在 5 分钟
    宽限期内仍属活动态；只显示普通 `⚪ 空闲`，不展示或刷新倒计时。
  - 已结束：单个禁用按钮 `已结束 · 已自动取消关注`（结束即自动退订，卡片定格）。
  - 已取消 / 已换新卡 / 已跟底：单个禁用按钮 `已取消关注` / `已由新卡片接替` / `已移至最新卡片 ⬇`。
- 按钮回调经 FsRouter 卡片路由（watch 子系统注册一条 `RoutedFs`、`set_active(message_id)`），
  同步回 `callback_update_card(新卡)`——点击 Loading 直接变终态，无闪烁（复用 ask 卡机制）。
  - `unwatch`：ack 即回定格卡，同时移除订阅、持久化。
  - `refresh`：ack 即回按当前状态重算的新帧（并刷新签名）。

## 渠道差异（同一 `WatchFrame`，各渠道渲染器 + 传输）

帧（`watch::WatchFrame`）是**结构化、无标记语言**的（`Vec<ToolStep>` / `Vec<TodoItem>` / 文字 /
状态），签名对结构化内容计算、跨渠道一致。共享文案构件（头部 / 状态行 / 动态标题 / 省略标注 /
更新行 / 终态标签）在 `watch.rs`，各渠道只做标记语言与控件差异：

| 维度 | 飞书（`card_view`+`build_watch_card`） | Telegram（`telegram/watch.rs`） | Slack（`slack/watch.rs`） | 钉钉（`dingtalk/watch.rs`） |
|---|---|---|---|---|
| 载体 | 卡片 JSON 2.0 | HTML 消息（`parse_mode=HTML`） | Block Kit（context/section/actions） | 互动卡片高级版**专用模板** + 11 个变量（模板 `docs/assets/dingtalk-watch-card-template.json`，默认 ID 内置） |
| 状态圆点 | `<font color>` 彩色 ● | ○ 进行中 / ● 已完成 / ✕ 失败（用户定案：无彩色字体的渠道不用 emoji） | 同 Telegram | 彩色 ●（`<font colorTokenV2>`，与飞书同款配色；**相邻 font 标签间空格会被吞，NBSP 须放标签内部**） |
| 足迹步行 | `● **类别**: *参数*` | `○ <b>类别</b>: <i>参数</i>` | `○ *类别*: _参数_` | `● **类别**: *参数*`（整行包 h5 `sizeToken`，默认字号偏大） |
| 「已省略 N 步」 | 灰字 `<font color='grey'>` | 斜体 `<i>` | 斜体 `_…_` | footnote 字号小灰字（`sizeToken`+`colorTokenV2`） |
| TODO | 折叠面板（摘要标题 + 全清单） | **仅摘要行**（用户定案，无折叠组件） | **仅摘要行**（同左） | **CollapsePanel 折叠面板**（`todo_summary` 标题 / `todo_md` 内容 / `has_todos` 控显隐） |
| 活动态按钮 | 卡片 button + callback | inline keyboard（`watch:unwatch/refresh`） | actions block（`watch_unwatch/refresh`） | 模板 SingleButton×2（分栏并排；actionId `watch_unwatch/refresh`；`finalized=false` 时显示） |
| 终态 | 单个**禁用**按钮 + 终态文案 | 编辑为无按钮 + 末行加粗终态标签 | 编辑为无 actions + context 终态标签 | boolean 变量 `finalized` 条件显隐：按钮行隐藏、显禁用灰标签 `final_label` |
| 按钮回调 | FsRouter 卡路由 + oneshot **同步回卡** | TgRouter `set_card_route`（仅卡回调，不认领自由文字）→ 应答 + 就地编辑 | SlRouter `set_active(ts, "")` → 就地编辑（ack 在 ws 层） | Stream 卡回调按 outTrackId 路由（`parse_watch_action`；空回包 ack 端上无报错，实测） |
| 发送/编辑 | `send_card` / `patch_card` | `sendMessage` / `editMessageText`（活动态编辑须**重传 keyboard**，不传即移除按钮） | `chat.postMessage` / `chat.update`（DM 频道 `conversations.open` 解析） | `createAndDeliver` / `PUT card/instances`（`updateCardDataByKey` 按 key 更新公有变量） |
| 每卡最短编辑间隔 | 1s | 1s | 2s（Tier 3 ≈50/min） | 1s（实测 2s×60 连发零频控、p50 ≈60–95ms，四渠道最快） |
| 编辑时限 | 14 天 | 48 小时 | 无明确期限 | 无明确期限（PoC 未触及） |
| message_id 编码 | open_message_id | message_id 十进制串 | 消息 ts | outTrackId（自铸 uuid） |

传输抽象：daemon 侧 `WatchClient` 枚举（`for_channel` 构造 + `send`/`edit`/`min_edit_interval_ms`），
引擎与命令处理只面向它。编辑时限到期（TG 48h / 飞书 14 天）→ 编辑失败计入 fails，≥5 自动退订
（既有机制兜底）。钉钉 PoC 探针保留为回归工具：`AskHuman debug dd-watch-poc`（`cli/debug_cmd.rs`，
隐藏子命令；`--count 0` 只发一张样式预览卡）。

## 更新引擎（daemon 内）

- **触发**：① `AgentEvent`（turn/tool/会话结束）即时 `Notify` 唤醒；② 周期 tick——有「工作中」
  订阅时 2s、只有处于空闲宽限期的订阅时 10s、无订阅时纯等 Notify；③ 提问创建 / 答复完成也
  Notify（等待回答状态即时进卡）。
- **防刷**：每帧算**签名** =（状态、标题、最近文字、工具行、等待标志）——**不含活动时刻和累计工作时长**：
  `at` 来自 transcript mtime / 工具心跳，会在内容不变时走动（如 Claude 渐进写盘），计入会造成
  「内容没变、卡片却被编辑（只有底部时间在跳）」的无谓更新（验收反馈修正）。签名不变不编辑；
  每卡最短编辑间隔按渠道（飞书/TG/钉钉 1s、Slack 2s；tick 本身 ≥2s）。编辑失败记日志、下一拍全量重渲染
  重试（帧是全量的，丢帧无损）；**连续 5 次失败**（如超编辑时限 / 卡被删）自动移除订阅。
- **机制说明（轮询 vs filewatch）**：引擎是「事件驱动为主 + 有界轮询兜底」的混合——hook 事件
  （turn/工具/提问/答复）即时 Notify，不等 tick；tick 只对**被关注的 ≤5 个** agent 读 transcript
  尾部（256KiB tail read），2s/10s 自适应，成本可忽略。transcript filewatch 暂不引入：四家
  transcript 路径形态各异且会轮转/新建（需盯目录 + 重挂），而 mtime 变化本就不等于展示内容变化
  （仍要读尾部算签名），watcher 只省掉「无变化时的 tail read」，收益小于复杂度。
- **跟底重发（验收反馈新增）**：watch 卡被会话里的**非 watch 消息**（用户消息 / 机器人文本回执 /
  提问卡）顶上去后，用户会找不到卡。机制：daemon **按渠道**记淹没水位线（`WatchState::disturb`，
  渠道 → 上述消息发生时刻 Unix 毫秒；watch 卡自身的发送 / 编辑**不**计入——watch 卡之间互不影响、
  无级联），每张卡记 `sent_at_ms`。下一次**内容变化**时若该渠道水位 `> sent_at_ms`（已淹没）→
  不就地编辑旧卡，改为**发一张新卡到会话底部**接续，旧卡定格「已移至最新卡片 ⬇」；
  订阅换绑新 message_id（持久化 + 回调路由重建）。约束：
  - **节流 30s**（用户定案）：同一订阅两次跟底至少间隔 30s，窗口内的变化仍就地编辑（不丢内容）。
  - **提问期间抑制**：该渠道有在途 AskHuman 提问时不跟底（不打断问答会话），只就地编辑。
  - **答复完结豁免**：某渠道参与的提问完结后清零**该渠道**订阅的节流（`last_move_ms=0`）——
    下一次内容变化**立即**跟底，用户答完马上能在底部看到回答引发的更新（用户定案）。**注意**：
    完结本身不标记扰动——作答/取消只是就地编辑提问卡、不产生新消息（TG/Slack 用户以文字作答，
    该文字本身已作为入站消息记过扰动）；淹没判定完全依据提问卡发出时刻（attach 处标记）。
    否则提问期间新 /watch 的卡（已在底部）会被误判淹没而重发出连续两张卡（验收反馈修正）。
  - 内容不变仍不动卡（跟底也以内容变化为前提，纯静默原则不破）。
- **提问投放并集（M4 用户定案）**：按需发送（autoActivation）开启时，提问卡投放渠道 =
  最后活跃渠道 ∪ **正在 watch 提问 agent 的渠道**（`attach_im_channels` 按 `agent_session_id`
  匹配订阅）。动机：用户在 A 渠道 watch、在 B 渠道（如弹窗）作答时，A 的 watch 卡显示
  「正在等待你的回答」但 A 收不到提问卡，非常迷惑。多渠道并发时收尾走既有抢答逻辑，不动
  抢答机制；开关关闭时维持旧「全发」行为。Popup 不可用且上述并集选不出可用 IM 时，为避免零投递，该次同样全发所有可用 IM。
- **结束语义**：注册表里该 session 变 `ended`（或记录彻底消失）→ 立即把最后一帧定格（⏹ +
  禁用按钮）→ 自动退订、持久化。已在关注的 session 变 `idle` 时先就地更新为活动态空闲卡并保留
  **固定 5 分钟宽限**；同一 session 在期限内重新 Working / Waiting 则原订阅无缝继续，每次再次进入
  Idle 都重新获得完整宽限。持续空闲满 5 分钟才定格 `已空闲 · 已自动取消关注` 并退订（10s 空闲
  tick 内完成）。期限取 registry 已持久化的 `lastActivity`，daemon 重启不重置；若恢复时已过期则
  首拍收尾。对本来就 Idle 的 Agent 新发 `/watch <n>` 仍只发一次性终态回顾卡，不建立订阅。已淹没
  时结束帧沿用跟底：新卡直接是终态卡，旧卡定格「已移至最新卡片」。
- **订阅持久化**：`~/.askhuman/state/watch.json`（`[{channel, sessionId, messageId, createdAt}]`，
  原子写；channel = feishu/telegram/slack/dingding）。daemon 重启 / 换新后**恢复并继续编辑同一张卡**；
  恢复时按 session_id 重解析显示编号（`seq` 不跨重启保留）。agent 的 hook 事件会 `ensure_running`
  拉起 daemon，故重启后 watch 自动续命。
- **闲退守卫**：有活跃 watch 订阅（含空闲宽限期）时 daemon 不因空闲退出；Ended 或空闲宽限到期后
  订阅消亡，故保活有界。
- **路由韧性**：各渠道 watch 回调路由任务随该渠道 Router 生命周期重建（`ensure_inbound_listeners`
  末尾 + 新订阅时 + tick 发现路由缺失时幂等重挂；`WatchState::routes` 渠道 → 任务句柄）。

## 复用与新增触点

- 复用：`agents/activity.rs`（尾部解析 + 工具归一化）、`autochannel` 的实时工具融合（抽出
  `activity_parts`）、`agents/registry.rs::snapshot`（seq / 状态 / currentTool）、
  `feishu::client::patch_card/send_card`、`FsRouter/TgRouter/SlRouter::register`、
  `telegram::TelegramClient::send_message/edit_message_text`、
  `slack::client::post_message/update_message/open_dm`、
  `RequestRegistry::in_flight_agent_{session_ids,pids}`（等待回答判定）。
- 新增：`src-tauri/src/watch.rs`（纯逻辑：结构化帧 / 签名 / 共享文案构件 / 持久化 / 本地时间
  格式化 / 渠道门控）、`feishu/card.rs::build_watch_card/parse_watch_action`、
  `telegram/watch.rs`（HTML 渲染 + inline keyboard）、`slack/watch.rs`（Block Kit +
  `parse_watch_action`）、`dingtalk/watch.rs`（模板变量渲染 + `parse_watch_action` + 内置
  默认模板 ID；模板 `docs/assets/dingtalk-watch-card-template.json`）、
  `telegram/router.rs::set_card_route`（仅卡回调、不认领自由文字）、`dingtalk/router.rs`
  Reader 放行 watch actionId 转发（原只转发提交）、`daemon/mod.rs` watch 引擎
  （`WatchClient` 传输枚举 + 按渠道路由/扰动 + 提问投放并集）+ 命令分派、
  `paths::watch_file`、i18n `watch.*`。

## 后续

- 可选：答完提问自动关注该 agent；`/watch` 关键转折附加通知的开关（用户当前明确选纯静默）。
- 可选：钉钉 watch 模板 ID 设置项（现仅内置默认；提问卡 `cardTemplateId` 已有先例）。
