# Todo 队列 + whats-next：任务间隙的下一步派发

> 状态：设计定案（2026-07-15，经六轮 AskHuman 评审），待实现。
> 实现分支：`feat/todo-whats-next`（worktree `../HumanInLoop-todo`，Dev Instance popup-only）。

## 1. 需求

agent 正在执行一个耗时任务时，用户脑中已有下一个想法。用户**不想打断当前任务**（插话
Interject 是「打断进行中」的纠偏语义，不适合），希望：

1. 随手把想法排进一个 **todo 列表**；
2. 列表**持久化**：agent 报错退出 / daemon 重启后，下次在这个项目仍能访问；
3. agent **完成当前任务后**主动拿到待办；或在它提问时，用户能把待办快速发给它；
4. 某条待办**开始执行后自动从列表清除**。

与插话（`docs/specs/agent-interject.md`）的分工：插话在**任务进行中**注入（PreToolUse
拦截、打断当前工具调用）；todo 在**任务完成后的间隙**派发（不打断任何进行中的工作）。
两者互不替代、共存。

## 2. 设计总览

```
输入入口（随时增删查）                 送达出口（何时进入 agent）
┌──────────────────────────┐          ┌────────────────────────────────┐
│ CLI  AskHuman todo …     │          │ whats-next 提问（主路径）：      │
│ Popup 折叠待办区（增删）  │──┐    ┌─▶│  agent 完成任务后必调，          │
│ IM   /todo、/todo-rm     │  │    │  │  待办渲染为选项 chip，           │
│ GUI  独立待办窗口 + 托盘  │  ▼    │  │  选中提交＝开始执行＋原子出队    │
└──────────────────────────┘ 项目级 │  ├────────────────────────────────┤
                             todo 队列─▶│ Popup 折叠待办区 chip（辅路径）：│
                             （持久化）  │  普通提问也可点选待办作答＋出队  │
                                        └────────────────────────────────┘
```

- 用户否决了 Stop hook 注入方案：Stop 时用户可能已明确要结束，此时 agent 又跑新任务
  「太奇怪」。取待办必须发生在**结束确认之前**。
- 定案核心：把现有 rules 的「结束前必须问、确认后才能结束」**标准化**为一次
  `whats-next` 提问——待办即选项，「结束本轮」也是选项；选待办＝继续干，选结束＝
  同意结束。一问两用，不重复啰嗦。

## 3. 设计定案

### D1 数据模型与持久化：项目级 FIFO 队列

- 归属：**项目级**（`project.rs` 的 git 根路径 key，回退 cwd）。同项目的新会话 / 新
  agent / 多 agent 共享一份队列；不随 session 结束清理，长期保留直到出队或删除。
- 条目：`{ id: uuid, text: String, created_at_ms, auto: bool, agent_kind?: String }`。当前已实现版本为
  纯文本；图片 / 文件附件的后续扩展已定案，见 `docs/specs/todo-attachments.md`，在该需求实现前
  本节以下仍描述现行无附件模型。
  `auto`＝**自动执行**标记（第 17 轮定案，见 D2；`false` 不落盘，旧文件兼容）；从历史
  恢复的条目恒为 `auto: false`。`agent_kind` 只在识别到 Agent 调用 CLI `todo add` 时记录
  家族（`claude / codex / cursor / grok`）；人工、GUI、IM 新增和旧数据均为空。
- 顺序：FIFO 追加；顺序可在 GUI 待办窗口拖拽调整（第 14 轮定案，见 D9），头部条目在
  选项类展示点优先出现。
- 存储（第 9 轮定案，实现简化）：`~/.askhuman/state/todos.json` 即**唯一数据源**，
  所有进程直读直写；写操作（读-改-写）持跨平台文件锁串行化（Unix `flock` / Windows
  `LockFileEx` 语义由 `fs2` 统一，与 `history.jsonl` 跨进程写锁同模式）。**不做** daemon 内存运行态
  ——todo 无热路径（whats-next 每轮一次、增删查是人操作频率），双层结构无收益。
  文件形态：`{ "projects": { "<project_key>": [ {id,text,createdAtMs,agentKind?}, … ] },
  "history": { "<project_key>": [ {id,text,createdAtMs,agentKind?,doneAtMs}, … ] } }`，
  空项目键剪除；原子写（tmp + rename）。写入失败（权限 / 沙箱等）时 CLI `todo add` 与
  MCP `todo_add` **必须失败退出/报错**，不得打印「已添加」；编号为落盘后队列中的 1 基序号。
- **执行历史**（第 16 轮定案，GUI 勾选完成补充）：**「出队进历史」**路径（whats-next /
  Stop 卡 / 弹窗点选 → `take`，以及 GUI 待办窗口圆形复选框完成 → 同一 `take`）进入
  `history`（时间正序追加，展示倒序）；手动删除（✕）、清空**不记**。每项目按设置
  「待办历史保留条数」（`general.todoHistoryLimit`，默认 100）裁剪最旧；`0`＝停止新增、
  既有历史保留（与回复历史 `historyLimit` 同语义）。GUI 待办窗口的「历史」折叠区可查看
  （相对时间＋来源 agent）并用复选框**一键恢复**回待办队列末尾（hover 显示恢复图标）。
- 附带收益：CLI `todo` / `--whats-next` 的待办读写**不依赖 daemon 存活**；
  Unix / Windows 同一套代码，无平台特例分支。

### D2 whats-next：取代「结束前确认」的标准化提问

**CLI**：`AskHuman --whats-next [<Message>] [-o <建议任务> …] [-o! <推荐建议任务> …] [--stdin] [-f <file> …]`

- Message＝agent 的**完成报告**（可选，推荐 `--stdin` heredoc），`-f` 可附报告文件；
  **不接受** `-q`（问题固定）。`-o` / `-o!` 只传 agent 建议的具体下一任务，**不得传结束、
  停止或无事可做选项**（结束项由 AskHuman 内置）；`-o!` 沿用普通 Ask 的推荐样式，无建议时不传。
  **服务端兜底**：若 agent 仍传入结束类文案，规范化（去全部空白与标点、英文小写）后与
  中英内置「结束本轮 / End this turn」及固定同义词表整串相等则**静默丢弃**，不占任务位；
  内置结束项仍只在列表末位追加一次。非整句（如「结束本轮的文档」）不滤。
- 语义：**必然发出一个提问**，走完全现成的普通 Ask 链路（popup + 四 IM、抢答协调、
  活跃槽 ∪ watch 投放、24h 等待、排空）。项目 key 取 CLI 调用 cwd 的 git 根。
- 问题固定为「接下来做什么？」（按界面语言本地化）；选项顺序＝agent 本次传入的建议任务 +
  该项目当前各待办条目（每条一个 chip/按钮，内部选项值保留「执行待办：」前缀并携带条目 id；
  展示按渠道能力分流：本地 Popup 隐藏文字前缀，改为与“推荐”同视觉重量的半透明琥珀色
  `TODO` badge；飞书、钉钉也隐藏文字前缀，改为琥珀色富文本 `【TODO】`；Slack、Telegram
  因选项控件不支持局部文字配色，继续展示原文字前缀。普通建议不显示 TODO 标记，
  第 21 轮定案）+ **恒有**一个「结束本轮」选项（列表末位）。总选项最多 10 条：结束项
  固定占 1 个，建议任务优先占据其余 9 个容量，超出建议静默截取，待办使用剩余容量。
  无建议、无待办时只有「结束本轮」+ 自由输入框。
  发给 agent 的任务文本剥掉前缀、还原待办原文。whats-next 弹窗**不渲染** D7 待办区
  （待办已是选项本体）。
- 本地 Popup 打开期间监听 `todos.json`：建议任务与末尾结束项保持不变，中间待办选项按最新队列、
  排序和剩余容量实时重建，溢出数量同步更新；已选待办按 id 保留，若被删除或移出可见容量则清除。
  提交动态待办时以原文 + id 回传，保持派发文本和 best-effort 出队语义。已发送的 IM 卡保持请求快照，
  避免首答竞争期间各渠道选项漂移；下一轮 whats-next 自然读取最新队列。
- 回答**写入回复历史**（它承载完成报告，是 agent 的正式提问；区别于 Stop 确认卡的
  hook 兜底卡不写历史）。
- **自动执行接管**（第 17 轮定案）：发问前先查队列，若存在 `auto` 待办则**不发卡**，
  直接把**最靠前**的一条自动待办出队（照常进执行历史）并输出其文本；本次建议任务不展示——多条自动待办
  按队列顺序天然形成逐条自动链，跑完后回到正常提问。agent 附带的完成报告照常落
  **回复历史**（自动路径没有卡片可看，历史窗口是唯一可查处）。仅 whats-next 有此行为；
  Stop 卡是兜底确认场景，**不自动**（见 D5）。

**MCP**：server 的 `whats_next`（与 `ask` 并列）入参
`{ message?, options?: [{ text, recommended? }], files? }`；工具描述将其定位为**任务完成后的交接**，
当前任务内的问题、决策或下一步仍走普通 `ask`；`options` 仅放具体下一任务，不含内置结束项，
薄壳将其翻译为 `-o` / `-o!`，再 spawn `AskHuman --whats-next …` 子进程（默认文本输出），
结果文本原样透传（与 `ask` 同为纯文本，见 spec mcp.md D5）。

**提交结果 → 语义映射**（纯函数，完整单测）：

| 用户提交 | 语义 | 出队 |
|---|---|---|
| 选中 agent 建议任务（可附自由文本补充） | 与普通 Ask 一致：建议在 `[selected_options]`，补充在 `[user_input]` | 不出队 |
| 选中某待办 chip（可附自由文本补充） | 执行该条（补充一并送达） | ✅ 按 id 原子出队 |
| 只填自由文本 | 执行该文本（全新指令） | 不出队 |
| 选「结束本轮」且无文本 | 同意结束：agent 输出结束 marker 后自然停止 | 不出队 |
| 选「结束本轮」但填了文本 | **视为继续**：文本是新指令（有话说＝还没完，与 Stop 卡「纯文字=继续」一致） | 不出队 |
| 取消 / 关窗 / 超时 | 沿用普通 Ask 取消语义（`[status]` 指示继续询问） | 不出队 |

- **出队时机＝「开始执行」**：Coordinator 首个终态回答的唯一汇聚点上，把选中的待办
  id 出队并 persist——即用户要求的「agent 开始执行后自动清掉条目」。
- 单选逐条循环：一次只派一条；agent 做完又回到 whats-next，天然形成逐条循环，
  每条之间都有人工确认点。

### D3 whats-next 结果输出：复用 Ask 标准区块（第 19 轮修订，替代「纯文本」旧案）

whats-next 是固定题目，输出直接复用普通 Ask 的区块词汇，agent 无需学习新结构：

- 建议任务 → 与普通 Ask 一致：选项原文放在 `[selected_options]`；若有自由文本补充，另放
  `[user_input]`。Agent 根据该建议是 whats-next 的下一任务继续执行。
- 派活（选待办 / 只填文本 / 结束+文本）→ `[user_input]` + **任务文本**：选中待办的
  原文（前缀剥掉）；有补充文本时按空行拼在其后；只填文本时即该文本。D2 的自动执行
  接管同构输出。
- 准许结束 → `[selected_options]` + 「结束本轮」选项原文（本地化文案）。第 7 轮的
  固定英文结束句已废除；agent 据 rules 语义（「returned that I approved ending the
  turn」）判断可结束，随后按既有 marker 行输出 `[user_confirmed_end_turn]`。
- 取消 / 超时 → 沿用现有 `[status]` 区块语义（指示继续询问）。
- 附件照旧按 `[files]` 附后；MCP `whats_next` 工具返回同一文本。
- `--agent-help` 只说用法与语义（用户会给出下一个任务或确认结束），**不描述输出结构**
  （第 19 轮定案：与 Ask 同语义故无需另行说明；`--stdin` 报告写法仍在 help，rules 里
  不重复）。

### D4 rules 文案变更草稿（`prompts.rs` 单一来源，始终英文）

评审定案：在原措辞上做最小调整，不追加规则列表。Rules 只定义生命周期边界：whats-next 是
**当前任务完全完成后**领取独立下一任务的交接，不得用于当前任务内的问题、决策或下一步；参数
语义由 CLI `--agent-help` 与 MCP schema 分别说明。marker 行原样保留。

CLI 版 `cli_reference()` 原三行中，前两行调整为：

```text
- After fully completing the current task—and never for questions, decisions,
  or next steps within it—you MUST run `{program} --whats-next` for the
  end-of-task handoff before ending, to ask me for a separate next task.
- If it returns a task, start working on it immediately and repeat this protocol
  when done. Do NOT end the turn/conversation or mark the request as complete
  unless `{program} --whats-next` returned that I approved ending the turn and
  there are no more tasks.
```

第三行（「After the user explicitly approves ending the turn, you MUST append the
`{end_marker}` marker …」）**原样不动**。

MCP 版 `mcp_reference()` 对应前两行调整为：

```text
- After fully completing the current task—and never for questions, decisions,
  or next steps within it—you MUST call the AskHuman `whats_next` tool for the
  end-of-task handoff before ending, to ask me for a separate next task.
- If it returns a task, start working on it immediately and repeat this protocol
  when done. Do NOT end the turn/conversation or mark the request as complete
  unless the `whats_next` result says I approved ending the turn and there are
  no more tasks.
```

- 其余纪律（必须经 AskHuman 提问、附件经 `-f`、relentless interview、不擅自改方案、
  subagent 例外）**全部保留**；「anything I need to review must go through …」继续
  覆盖完成报告之外的中间产物。
- Grok skill 正文复用 `mcp_reference()`，自动跟随。
- rules 是托管产物：升级二进制后按现有 `agents update` / 过期徽标机制更新四家安装文案。
- （2026-07-24 补充）Codex Rules 不再为 task-suggestion generator 增加 scope exception，以便实际
  观察模型对 `whats_next` 等 AskHuman 工具的调用。可信 `thread_source ∈ {system,
  ambient_suggestions}` guard 会在弹窗、IM 和 todo 副作用前拒绝，并将拒绝原因写入 `daemon.log`；
  因此 Rules 行为不承担后台线程的安全边界。
- （2026-07-17 补充）Rules 在用户明确要求添加待办，或把一个**具体任务 / 已提出的建议明确延后**
  （如“稍后再做”）时要求 Agent 添加项目待办；不得把 Agent 自己的内部计划或尚未被用户接受的建议
  擅自入队。CLI 版调用 `<program> todo add "<concise task>"`，MCP 版与 Grok skill 调用
  `AskHuman todo add "<concise task>"`；Rules 只用 `concise` 轻量提示，不重复具体长度。

### D5 与 Stop 结束确认、插话的关系；Stop 卡待办派发（兜底送达点）

- whats-next 是**提示词层**协议；Stop 确认（`agent-stop-confirmation.md`）是 **hook
  层**兜底——agent 不守规矩直接停时仍有接管卡。两者独立开关、语义兼容：
  经 whats-next 选「结束」→ agent 按 rules 输出 `[user_confirmed_end_turn]` marker →
  Stop hook 现有的 marker 检测静默放行，**不会重复弹卡**。
- **Stop 卡加入待办派发**（第 8 轮定案）：Stop 确认卡选项变为「各待办 chip（内部值加
  「执行待办：」前缀；飞书、钉钉展示时改用琥珀色 `【TODO】`，Slack、Telegram 仍展示
  文字前缀；continuation 始终为待办原文）+
  继续对话 + 结束对话」——agent 跑偏直接停时，停下的那一刻仍能一键派下一条。
  与第 1 轮否决的「Stop hook **自动**注入」不同：这是确认卡上的**手动选择**，
  不点待办就不会执行。
  - 项目 key：Stop hook stdin 自带 `cwd` / `workspace_roots`，映射 git 根。
  - 提交映射：选待办（±文字）＝以该条（带补充）为 continuation 并按 id 出队；
    只填文字＝继续（文字为指令）不出队；选「继续对话」＝原有语义；
    选「结束」＝结束并**丢弃文字**（维持 Stop 卡现有规格，不与 whats-next 强行统一）。
  - continuation prompt 沿用现有各家原生语义分流（Claude 结构化包裹 / Codex、Cursor
    裸传）；Stop 确认按家开关、默认关、Grok 不支持、不写历史等既有约束全部不变。
  - 自动执行待办在 Stop 卡上**不自动派发**（第 17 轮定案）：照旧作为选项由人点选，
    选项文本也不加 ⚡ 标（自动待办通常轮不到人点）。
- 插话不变：任务进行中仍用 Interject / `/msg`；todo 不提供「打断进行中任务」的能力。

### D6 入口一：CLI `todo` 子命令（跨平台）

```bash
AskHuman todo add [--auto] <text>  # 追加一条（按 cwd 的 git 根归项目；--auto＝设为自动执行）
AskHuman todo list                 # 列出本项目待办（带编号；自动执行条带 ⚡auto 标）
AskHuman todo rm <编号>            # 删除一条
AskHuman todo clear                # 清空本项目（需交互确认，或 --yes 跳过）
```

- macOS、Linux 与 Windows 均经 shared daemon（连接或拉起）；`todos.json` 仍是唯一数据源，所有独立进程写入由跨平台文件锁串行化。
- 输出人类可读；后续需要时再加 `--output json`（首期不做）。
- `todo add` 复用提问调用方识别：先读 Agent 环境变量，无法识别时沿平台原生父进程树兜底；
  识别成功则把 Agent 家族写入待办来源。来源不改变 CLI 输出，人工调用保持无来源。
- `--agent-help` 用两行简述项目待办：它用于提醒用户操作，或记录用户要求稍后执行的任务，
  不是 Agent 的内部工作计划；用户要求添加或明确延后具体任务时用 `todo add` 添加。另给出软建议：
  写成一个可执行的句子，尽量不超过 **100 个字符**；不做硬截断，避免丢失必要上下文。

### D7 入口二：Popup 待办下拉区（第 11 轮改版）

- 弹窗（PopupView）在**最后一个问题之后**显示「待办」下拉区（项目 key 随 AskRequest
  已有传递），**仅在该提问项目有待办时**出现：
  - 收起时只显示数量（一行「▸ 待办 N」，样式与预选答案同构）；
  - 展开后每条待办是与预选答案同构的**可选行**：选中＝提交时其文本按
    「另外看一下这个待办任务：{text}」整行并入**最后一题**回答（手输文本在前、
    空行分隔，进 `[user_input]`），并按 id **best-effort 出队**；
  - 每条行内**删除**按钮；**不提供新增**（第 11 轮定案：新增走 CLI/GUI/IM，界面更干净）；
  - 普通提问的 Popup 宿主监听 `todos.json` 并投递 `todos-updated`；提问保持打开时，从独立待办窗口、CLI、
    MCP 或 IM 新增/删除/完成待办后，本项目列表与数量实时重载，已被移除条目的本地选中态同步清除；
  - sequential 多题模式只在最后一题的面板显示；严格选择（select-only，禁自由文本）
    下仅可删除、不可点选。
- **点选作答多题也启用**（第 11 轮定案，取代「仅单题」旧规）：选中文本恒并入最后一题。
- whats-next 弹窗**不渲染**本区（待办已是问题选项本体，见 D2）。
- IM 普通提问卡**不加**待办区（评审定案：任务中途的提问用待办作答易答非所问，
  且多题卡歧义；IM 侧待办送达统一走 whats-next 卡）。

### D8 入口三：IM `/todo`、`/todo-rm`（跨平台 daemon 入站命令层）

- `/todo`（无参）→ 复用现有跨渠道**单选卡**选一个项目；`/todo <text>` → 把文本
  暂存在 picker payload，选中项目后直接新增。旧 `/todo <n> [text]`（n＝`/status`
  稳定编号）继续兼容，但不再作为主入口。
- 项目候选＝工作中 Agent 项目 ∪ 空闲在线 Agent 项目 ∪ 未隐藏的最近 workspace ∪
  已有待办的项目（去重）。排序＝工作中 → 空闲在线 → 置顶 workspace → 其他最近
  workspace → 仅存在于待办存储的项目；活动组内仍按置顶/最近排序。与 `/new` 一样，
  首屏列 5 个，超出时提供「显示更多」。
- 确定项目后发**管理卡**：列出该项目全部待办 + 新增入口：
  - 飞书 / 钉钉卡片支持表单输入 → 卡上直接带输入框提交新增（飞书为代码组装卡；
    钉钉**复用提问卡模板**的 `allow_input` 输入框，无需注册新模板，提交后经
    OpenAPI 复位表单以便连续新增）；
  - Telegram / Slack 无可靠卡内输入 → 卡上提示 `/todo <text>`，发送后重新选择本项目。
- `/todo-rm` → 先选项目，再复用现有单选卡逐条选择删除（复用既有卡片模型，无新协议）；
  旧 `/todo-rm <n>` 继续兼容。
- `/todo-auto`（第 17 轮定案）→ 无参先选项目并打开「切换自动执行」控制面——每条待办
  一个「切换」按钮（已自动的带 ⚡ 徽标），点击翻转开/关并**就地刷新**（可连续切多条）；
  同时提供新增自动待办入口：飞书在同一张切换卡底部放输入框，提交后新增为 ⚡ 并刷新列表；
  钉钉因已发布 Select 模板无输入组件，有待办时发切换卡 + 复用提问模板的独立新增卡，
  无待办时只发新增卡；Telegram / Slack 提示 `/todo-auto <text>`。空项目仍可直接创建第一条。
  `/todo-auto <text>` 选中项目后直接新增一条**自动执行**待办；旧
  `/todo-auto <n> [text]` 继续兼容。管理卡（`/todo`）的新增入口不做自动标记，自动待办经
  `/todo-auto` 或 GUI 设置。
- 没有任何项目候选时回提示；不要求存在存活 Agent。与 `/status` 同门控（daemon 存活即可用）。

### D9 入口四：GUI 独立待办窗口 + 托盘（macOS/Linux/Windows GUI Host 承载）

- 新窗口类型（`WindowKind::Todo`，全局唯一），入口：托盘菜单「待办…」+ 所有提问/确认弹窗
  顶栏「项目待办」+ Agent 状态窗口各 agent 卡片 + 托盘各 agent 子菜单「添加待办…」
  （弹窗与后两者预选对应项目；
  带预选项目打开时窗口自动聚焦新增输入框，第 12 轮定案）。
- 带**项目选择器**：候选＝有待办的项目 ∪ 活跃 agent 的项目 ∪ 最近 workspace 索引
  （与 IM `/todo` 同源：工作中 → 空闲 → 置顶 workspace → 其他最近）。GUI 用两个
  `<optgroup>` 分段：「有待办」在前（含条数），「最近项目」在后（已在上一组的 key 不重复）。
- 功能：列表、新增、删除（两段式确认，第 13 轮定案；确认态点击别处即取消）、**勾选完成**
  （圆形复选框：乐观 UI 划线变灰，1 秒内可取消勾选撤回；超时走 `take` 进历史，与执行出队
  同列表；✕ 删除仍为永久删除、不进历史）、清空（第 18 轮定案：待办列表与执行历史各有清空
  入口，位于各自区块尾部/头部右侧；点击弹 **模态确认框** 提示不可恢复——危险操作需光标
  移动后二次确认，不用就地展开）、**拖拽排序**（第 14 轮定案：行首 handle 拖动重排，头部
  条目在各选项类展示点优先出现；仅 GUI 窗口提供调序，新增仍追加到底部；手柄列始终占位以
  与历史行复选框对齐）、**自动执行开关**（第 17 轮定案：新增行带「设为自动」开关；已有
  条目 ⚡ 跟在 meta 行文字右侧——已开启时常显琥珀色，未开启仅行 hover 出现；新增行开关
  使用零延迟自绘提示，固定说明“开启后，Agent 在完成前序任务后，将自动执行该待办。”，点击
  切换后驻留 0.5 秒，不使用系统 tooltip）；
  行布局：正文行（复选框 + 文本 + ✕）+ meta 行（相对时间；有 agent 则 `· {Agent}`；⚡）；
  相对时间 hover 用零延迟自绘 tooltip 显示绝对时间（不用系统 title）；执行历史同样布局，
  默认已勾选略透明，hover 复选框变恢复图标、点击立即 restore；
  实时同步＝宿主进程监听 `todos.json`
  文件变化（复用 `config_watch.rs` 的 notify 模式，第 9 轮定案）——daemon 未运行时
  窗口照样可用。

### D10 平台矩阵

| 能力 | macOS / Linux | Windows |
|---|---|---|
| `--whats-next` / MCP `whats_next` | ✅（提问经 daemon；待办直读文件） | ✅（提问经 named-pipe daemon；待办直读文件） |
| CLI `todo` 子命令 | ✅ 直接文件 + 跨进程锁 | ✅ 直接文件 + 跨进程锁 |
| Popup 折叠待办区 | ✅ 直读文件 | ✅ 直读文件 |
| IM `/todo`、`/todo-rm`、`/todo-auto` | ✅ | ✅ |
| GUI 待办窗口 / 托盘入口 | ✅ | ✅ |

### D11 竞态与边界

- **提交以卡上文本为准，出队 best-effort**：同项目两个 agent 同时 whats-next 时同一
  待办出现在两张卡，先提交者出队；后提交者选同一条 → 文本照常送达（你点的就是你要
  的），出队发现条目已不在则跳过，不报错、不要求重选（IM 卡片无法强制刷新到眼前）。
  GUI/CLI 删除赶在点卡之前同理。
- 同一请求内**首答胜出**等既有约定全部沿用（whats-next 就是普通 Ask）。
- 待办文本超长时 IM 按钮/选项按各渠道既有截断规则展示，送达内容始终为原文全文。
- **选项条数上限**（第 14 轮定案；2026-07-17 扩展）：whats-next **总选项最多 10 条**
  （建议任务优先 + 待办 + 末位结束项）；Stop 卡、`/todo-rm` 删除卡仍只列队列
  **前 10 条**（`todos::MAX_OPTION_TODOS`，渠道选项数硬限制的安全值），超出时正文尾部附
  「……还有 x 条待办未列出」提示；顺序靠 GUI 窗口拖拽调整（D9）。
- daemon 重启 / agent 崩溃：队列由 `todos.json` 恢复，无会话绑定故无需清理逻辑。
- Dev Instance：`ASKHUMAN_HOME` 隔离下各实例有各自的 `todos.json`（自然成立）。

## 4. 单元测试要求（骨架，实现计划再展开）

- 队列存储：add/rm/clear/出队幂等、persist 往返、空项目剪除、Unix/Windows 文件锁。
- whats-next 参数解析：与 `-q` 互斥，接受 `-o`/`-o!` 建议任务，覆盖 Message/`--stdin`/`-f` 组合。
- 提交映射五分支（D2 表）纯函数全覆盖；出队 best-effort（条目已删）分支。
- 输出契约：任务文本（含补充拼接）/ 固定结束句 / 取消 `[status]` 三种渲染；
  MCP 返回同一文本。
- rules：新文案包含 whats-next 指令、不再含旧「请求反馈」两行、marker 行原样保留；
  CLI/MCP/Grok skill 三处一致性。
- Popup：单题启用 chip、多题只管理；chip 文本并入 user_input 的拼接规则。
- IM：todo 项目候选并集、活动/置顶/最近排序、首屏 5 项与展开；`/todo` 选项目 / 带文本新增 /
  旧 Agent 编号直达；管理卡两种新增形态；`/todo-rm` 项目选择；`/todo-auto` 项目选择、
  飞书同卡新增、钉钉双卡/空项目单卡、TG/Slack 命令提示及提交后刷新。
- Stop 卡：待办 chip 前置、四分支提交映射（含「结束丢弃文字」维持现状）、
  选待办出队、marker 抑制下不弹卡不出队；既有 Stop 确认测试不回归。
- 竞态：双卡同条目先后提交、提交前删除。
- 来源：Agent CLI 新增写入家族、人工新增不写字段、旧 JSON 无字段兼容、执行历史与恢复保留来源。

## 5. 反馈意见记录

- （2026-07-15 第 1 轮）项目级归属定案；Stop hook 注入被否——「用户已确认结束后 agent
  又跑任务太奇怪」，取待办须在结束确认之前；需要全渠道可用的工作流，不只弹窗。
- （第 2 轮）用户提出 `--whats-next` 方案：agent 结束前必调、必发一问，有待办列待办、
  无待办给输入框，提交待办即删除。IM 侧定 `/todo <n>` 选 agent 后发管理卡（列待办 +
  输入新增）。Popup 点选填入的「编辑后不知道提交哪条」问题被用户指出。
- （第 3 轮）whats-next **取代**结束前确认（选结束＝同意结束、输出 marker）；单选逐条
  循环；「待办渲染为 chip 按 id 出队 + 自由文本补充」解法获认可；`/todo-rm` 选 agent
  后用现有单选卡选条目；Popup 折叠区含管理功能；IM 普通卡交互待设计（后被收回）。
- （第 4 轮）IM 普通提问卡**不做**待办区；Popup 折叠区 chip 模型认可；MCP 首期一起做；
  「结束+文字＝继续」定案；FIFO 不做排序。
- （第 5 轮）whats-next 卡片形态（固定问题 + 恒有结束选项 + 报告作 Message + 写历史）
  认可；IM 仅存活 agent 时可用；竞态「照常送达 + best-effort 出队」认可；平台矩阵认可；
  CLI 子命令加 `clear`（需确认）。
- （第 6 轮）worktree `../HumanInLoop-todo` + 分支 `feat/todo-whats-next`，popup-only
  Dev Instance；一次做完再统一验收；先审 spec 再动代码。
- （第 7 轮 spec 评审）rules 文案要**最小改动、不啰嗦**：在原措辞上把「必须提问请求
  反馈」换成「必须调 whats-next」即可；`--stdin` 等用法细节归 `--agent-help`，rules 不写；
  whats-next 结果**不要特殊结构**——就是一段文本（任务内容，或「用户同意结束」的固定句）。
- （第 8 轮）用户提出并定案：**Stop 确认卡也加入待办派发**（选项＝待办 chip + 继续 +
  结束），作为 rules 不被遵守时的兜底送达点；提交映射与 whats-next 对齐，唯
  「结束+文字＝结束丢弃文字」维持 Stop 卡现状。
- （第 9 轮实现前确认）存储简化定案：`todos.json` 即唯一数据源、全进程直读直写 +
  flock 串行化，不做 daemon 内存态与 CRUD IPC；GUI 待办窗口实时同步改为宿主监听文件
  变化。CLI 待办操作因此不依赖 daemon 存活。
- （2026-07-17 建议任务）`whats_next` 支持 CLI `-o`/`-o!` 与 MCP `options` 传入 agent
  建议的下一任务，仅确有建议时才传。卡片顺序为建议、待办、结束，总计最多 10 项；建议
  超限静默截取。选中建议保持普通 Ask 的 `[selected_options]`（补充文字仍为 `[user_input]`）；
  自动待办继续优先并直接接管。
- （2026-07-17 来源标记）Agent 经 CLI 添加待办时复用提问的调用方识别，只记录 Agent 家族；
  GUI 待办行正文下方显示「来自 Codex」式来源，执行历史也保留并显示，恢复后继续保留。
