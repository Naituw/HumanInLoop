# Codex 权限弹窗“本会话 / 始终允许”——调研与产品技术规格

> 状态：已实现（2026-07-25，D1–D53 全部落地；上游同步纪律见 `docs/PROGRESS.md`）；本文只固化已确认结论与当前源码事实。
>
> 初始调研基线（2026-07-18）：HumanInLoop 当前工作区；相邻目录
> `/Users/wutian/Developer/Codex` 的 `main` 分支，commit `d2d00b6632`。
> Shell 与 App Server 相关结论已在同日按最新 commit
> `6bd3f5e3db8275c10c7e4bbcc1342c32a89b7eee` 复核。
> 调研同时包含源码静态阅读与 Codex CLI 0.144.4 隔离 TUI 实测；尚未修改产品功能。

## 1. 背景与目标

HumanInLoop 当前通过 Codex `PermissionRequest` Hook 接管原本即将出现的权限确认，并在本地 Popup 与
IM 中提供“批准一次 / 拒绝”。Codex TUI 对部分审批还会提供“本会话允许”或“始终允许”，因此同类请求经
AskHuman 处理时可能反复弹窗。

本需求的目标是：在**不要求修改或替换用户标准 Codex** 的前提下，尽可能让 AskHuman 权限确认具备 Codex
原生审批的记忆能力，并覆盖 Codex 原生支持的审批类型：

- 单次批准；
- 本会话允许；
- 永久允许；
- 拒绝 / 取消。

最终选项必须以 Codex 对当前请求真实支持的能力为边界；不能因为 UI 可以渲染任意 choice，就为当前请求
伪造一个 Codex 本身不支持或作用域不同的“始终允许”。

产品北极星（2026-07-18 用户确认）：核心场景是用户不在电脑边、在手机上回答权限询问；该机制导致的
**用户作答次数不得多于原生 Codex TUI**，目标是一致或更少。任何新增能力都以这个次数对比为验收视角。
据此排查，会“比原生多问”的缺口共三类：缺记忆选项（本规格主体）、guardian / auto-review 环境
（D36）、同会话重复的 network host / MCP 工具（D37）；`request_permissions` 不经过 Hook，只能回到
电脑上原生回答，属于标准接口硬边界而非多问（§5.4）。

## 2. 已确认的产品决策

| 编号 | 决策 |
|---|---|
| D1 | 覆盖 Codex 原生所有审批类型，不只处理 shell 或文件写入 |
| D2 | 同时支持 Codex 原生的 session scope 与 permanent scope；另为可验证的原生文件编辑增加经明确确认的项目级 / 磁盘级 session scope，不泛化到其它工具 |
| D3 | 必须兼容用户当前安装的标准 Codex；不以维护 Codex fork、私有补丁或定制二进制为前提 |
| D4 | AskHuman 的 session scope 定义为**整个对话树**，根线程与所有协作子代理双向共享 |
| D5 | session scope 在 Resume 后继续有效；以 Codex Hook 提供的共享 `session_id` 分区，不以 `agent_id` 分区 |
| D6 | fork 后的新对话不继承原对话的 session scope |
| D7 | 永久规则应写入 Codex 自己使用的配置 / rules 文件，使未来未启用 AskHuman Hook 的 Codex 也能识别；具体写法必须遵循 Codex 源码语义 |
| D8 | 当前先形成规格与差距分析；尚未确认的匹配键、选项文案和降级策略继续逐项讨论，不在本文中擅自定案 |
| D9 | 原生文件编辑至少提供“本对话不再询问这些文件”，按本次请求涉及的精确文件路径逐个记录 |
| D10 | 若本次请求的所有文件都在项目内，额外提供“本对话允许所有本项目内的文件修改” |
| D11 | 若本次请求任一文件在项目外（包括内外混合请求），额外提供“本对话允许完全磁盘文件修改” |
| D12 | “本项目”沿用现有项目定义：从 Hook `cwd` 找最近 Git 根；非 Git 目录回退 `cwd` |
| D13 | 文件授权的 UI 文案不暴露 `apply_patch`；底层只匹配可验证的 Agent 原生结构化文件编辑，不覆盖 shell / MCP |
| D14 | `SingleSelectSubmit` 在所有渠道统一为“先选择、再提交”；Telegram 不再把选择按钮直接当终态 |
| D15 | session rule 在最后一次实际命中并自动允许后滚动保留 30 天；30 天无命中则清理，普通会话活动不续期 |
| D16 | 在设置 → 高级的最后提供授权管理入口；设置启动与进入高级 Tab 均不加载规则，只有用户打开管理面板时才按需读取 |
| D17 | 管理面板按 Codex 对话分组，只提供查看与“重置整个对话授权”，不提供逐条 / 逐路径撤销 |
| D18 | ~~普通 Shell 严格沿用 Codex 默认菜单范围：允许一次、有条件出现的永久 command-prefix、拒绝；不增加 AskHuman 自定义的 Shell session 选项~~（2026-07-18 被 D38 放宽：为达成“作答次数 ≤ 原生”北极星，增加受限的会话级选项） |
| D19 | `apply_patch` 的精确文件 / 项目 / 磁盘 session scope 完全由 AskHuman shadow rules 记录与匹配；不依赖或写入 Codex 当前 thread 的原生审批缓存 |
| D20 | 当前权限记忆需求排除 App Server 中转；AskHuman 不为此成为 Codex TUI 与执行引擎之间的全量通信代理 |
| D21 | 排除在 Hook 内调用 App Server 查询 Shell prefix；当前协议没有只读 exec-policy 评估 RPC，且原生审批事件要等 Hook 未作决定后才会产生 |
| D22 | Shell 下一步先评估按当前 Codex 版本完整复刻权限判断逻辑；在完成依赖、输入和版本同步分析前，不把复刻方式写成既定实现 |
| D23 | 当前 Codex 正常工具路径会先把 `TurnContextItem` 与对应 FunctionCall 持久化并 flush 到 rollout，再执行 PermissionRequest Hook；AskHuman 可以读取该来源，但 I/O 缺失、旧 schema 或无法唯一关联时不得猜测 |
| D24 | 权限记忆是现有“允许一次 / 拒绝”确认之上的可失败增强：记忆分析、规则读取或兼容性判断失败时仍保留现有 AskHuman 弹窗；只有基础确认链本身失败时才以空 stdout 交还其它 Hook / Codex 原生审批 |
| D25 | 用户选择记住但规则保存失败时，本次调用降级为“允许一次”，并在原作答渠道明确报告未保存成功；不得误报已记住，也不要求用户再确认同一次调用 |
| D26 | 记忆选择的保存与校验由 AskHuman daemon 统一负责；短命 PermissionRequest Hook 只负责把请求送入 daemon、等待最终裁决并向 Codex 输出结果。daemon 在保存完成前不得把选择定格为“已记住” |
| D27 | Shell 的复杂解析与 exec-policy 判断在有输入输出上限和硬超时的隔离 worker 中运行，不进入 `panic = "abort"` 的 daemon 进程；worker 只能返回经校验的记忆候选，不能直接产生 Hook allow / deny。worker 任意失败均回到现有弹窗 |
| D28 | Shell permanent 不另建 AskHuman 持久 shadow；Codex 原生 `default.rules` 是唯一授权来源。首版不做 Shell policy 缓存，隔离判断进程每次读取并解析当前原生规则；当前 Codex 再次调用 Hook 时，仅在能够证明这是普通 Shell 请求且最新原生 policy 对该请求为 allow 时自动返回 allow |
| D29 | Shell 的原生规则命中不能单独触发 Hook `allow`：AskHuman 还必须证明本次是普通首次 Shell 审批，并已覆盖当前 Codex 的完整 effective exec policy；strict auto-review、retry / escalation、managed policy 或其它上下文无法排除时保留现有弹窗。该即时判断是对原生规则的无状态重算，不是第二份授权 shadow |
| D30 | 当前 Codex 未 reload 时的即时生效使用 daemon 内存中的临时 policy provenance：按 thread / rollout、Codex 进程代次与 policy context 分区，只证明“已验证基线 + 本次原生追加”的关系，不缓存 policy 或 Allow 结果、不落盘也不直接授权。daemon 重启、摘要不匹配或上下文变化后 fail closed，恢复现有弹窗；不向 `default.rules` 写 AskHuman 私有证明注释 |
| D31 | Shell 判断复刻采用混合方案（回答 D22）：规则匹配语义交给用户已装 codex 二进制的只读 `codex execpolicy check`（经 Hook 调用进程树定位发起审批的那个 codex 可执行文件，保证版本精确一致）；AskHuman 只复刻输入重建、脚本拆分与候选派生，全程 fail-closed。该命令只做规则评估引擎，不当完整审批 oracle（层叠、分段、heuristics、请求阶段仍由外围证明，见 D29） |
| D32 | 复刻 `parse_shell_lc_plain_commands` 的 bash -lc 脚本拆分（与 Codex 同版本 tree-sitter-bash + 相同的节点白名单语义）；解析结果为 None 或与 Codex 语义不可证一致时，不产生任何记忆候选，也不自动放行 |
| D33 | 规则层叠发现复刻 user 层（`$CODEX_HOME/rules/`）、system 配置文件层与项目层（cwd 到项目根逐级 `.codex/`，含 projects 信任判定、路径规范化与 worktree 根解析）；检测到 MDM / 企业云配置 bundle / requirements `exec_policy` / `ignore_user_and_project_exec_policy_rules` 时整体禁用 Shell 记忆增强，回到基础弹窗 |
| D34 | 复刻 heuristics 判定（`is_known_safe_command`、`dangerous_command_match`、`render_decision_for_unmatched_command`、amendment 派生与禁选前缀表）；上游同步责任按 §6.4 的维护纪律执行 |
| D35 | 版本门控只设下限、不设上限（2026-07-18 修订，用户定案）：下限 0.122（PermissionRequest hook 引入版本，再往前 hook 不存在）；低于下限禁用 Shell 记忆（fail-closed）。高于已审计版本（`VERIFIED_CODEX_VERSION_CEILING`，当前 0.145-alpha）保持启用、仅记日志留痕。放宽依据：①拆分 fail-closed，解析不了自动降级；②auto-allow 只认 execpolicy 显式 allow，且 `execpolicy check` 调用用户装机 codex，语义随用户版本；③永久 prefix 落盘前用装机 codex verify（D45）；④对拍 git 历史证实 0.122 以来 Unix 侧安全逻辑变更均为我方已携带的收紧方向（0.126 git -C、0.129 heredoc/分页、0.145-alpha rm 强化），其余是 Windows/PowerShell 专属 |
| D36 | 能证明本次审批会路由给 guardian（rollout `TurnContextItem` 显示 `approvals_reviewer = auto_review` 且 `approval_policy ∈ {on-request, granular}`，即 `routes_approval_to_guardian_with_reviewer` 条件）时，Hook 不弹窗、不作裁决（exit 0 无 stdout），交回原生流程；原生此时由 guardian 自动审、用户不会被问，AskHuman 弹窗只会多问。无法证明时维持现有弹窗。turn 级 `strict_auto_review`（动态 `request_permissions` 授权触发）不在 TurnContextItem 中，按 D43 保守识别 |
| D37 | network / MCP 的会话级记忆属于达成北极星（作答次数 ≤ 原生）的必做范围，不是可选优化；识别与写入方式已按 D39/D40 定案 |
| D38 | 放宽 D18：普通 Shell 增加 AskHuman 自有的会话级选项，前缀优先（2026-07-18 细化）——前缀级与永久选项使用同一前缀来源（模型自带 `prefix_rule` 或原生派生逻辑的 fallback 推导，经相同的禁选前缀检查与 execpolicy 复验；能写入全局 default.rules 的前缀用于更弱的会话作用域不引入新风险），“本对话允许 `<prefix>` 开头的命令”；仅当推导不出前缀时回退提供精确命令级（脚本可完整拆分、所有命令不命中 dangerous 列表、版本在已验证窗口内，“本对话不再询问这些确切命令”，全 token 精确匹配入库）。匹配语义是 AskHuman 自有的 token 精确/前缀匹配，复用 §6.2 session rule 存储与管理面板；auto-allow 需每条命令被（精确命令集 ∪ 前缀集）覆盖、全部不命中 dangerous 列表并通过 guardian / retry 防护（D29/D36）。会话级与永久选项可同时展示：允许一次 / 本对话（前缀，或回退精确）/ 始终 / 拒绝；“始终允许”文案不带实现细节（写入通道仍是原生 default.rules，D28 不变） |
| D39 | network 域名放行记忆（回答 §7 原问题 3）：识别采用三重判真（description 严格匹配 `network-access {proto}://{host}:{port}` 机器格式、协议限四种合法值、与 rollout FunctionCall 中模型自带 justification 不同），判真失败按普通 Shell 请求渲染且弹窗始终展示触发命令原文；会话级为 AskHuman shadow 主机规则（键 = host 小写 + 协议 + 端口，比原生少 environment_id 一维）；永久级写原生 `default.rules` 的 `network_rule(...)` 行并同时记会话桥接规则兜住当前对话，不做规则回读（`codex execpolicy check` 不暴露 network_rule 评估）；永久 deny 不做（原生 TUI 默认菜单亦无）。选项：允许一次 / 本对话允许该主机 / 始终允许该主机 / 拒绝 |
| D40 | MCP 工具记忆（回答 §7 原问题 4）：识别靠 Codex 注册表生成的 `tool_name = mcp__<server>__<tool>`（模型不可伪造）；会话级 shadow 对**全部** MCP 工具提供（含插件 server 与 codex_apps 连接器），键 = 完整 hook tool_name 字符串（不拆分、规避 `__` 歧义；codex_apps 的 tool_name 自带连接器命名空间，天然达到原生 connector 隔离粒度），与原生同粒度（不含参数）；永久级当 server 定义在用户层 config.toml 或信任项目层 `.codex/config.toml` 时格式保留写入 `approval_mode="approve"` 并记会话桥接规则，插件 / codex_apps 走 D41 的跨会话 shadow 兜底；可读配置层显式设该工具为 prompt/writes 模式时两级记忆选项均不出现；不看 ToolCallMcpElicitation feature flag（其只门控原生 TUI 的 UI 选项） |
| D41 | 没有原生写入通道的 MCP server（插件定义 + codex_apps 连接器）的“始终允许”用 AskHuman 跨会话 shadow 兜底：与会话规则同一 rule store 的全局 namespace（不含 session_id），键 = 完整 hook tool_name 字符串；沿用 D15 语义（命中自动放行时刷新 `last_used_at`，连续 30 天无命中即清理）；auto-allow 仍受 guardian / retry 防护（D29/D36）；管理面板增设“跨会话授权”分组单独查看与重置。仅做兜底：能写原生配置的类型（用户层/项目层 MCP、Shell、network）一律写原生（D7 优先），不提供 shadow 永久。该“始终允许”只在 AskHuman Hook 生效期间起作用，弹窗文案须与写原生配置的“始终允许”可区分（§7 问题 5） |
| D42 | Shell retry 不做识别、改由 auto-allow 收紧保证安全（§6.1.2）：非 guardian 会话里原生每个 shell 调用至多问一次（首段审批后 `already_approved` 使沙箱失败重试自动豁免再审），Hook 收到的普通 Shell 请求不存在“同一调用的第二次询问”；永久 auto-allow 收紧为“每个拆分段都被显式规则 Allow 命中”（即 Codex 自身 bypass_sandbox 的判定条件，满足时 reload 过的原生会直接免沙箱放行、不会出现 retry 询问），heuristics 永不作为 auto-allow 依据、只用于选项派生与 dangerous 门槛；会话规则 auto-allow 与原生 ApprovedForSession 缓存行为天然镜像（缓存命中同样静默批准并豁免 retry），无需额外识别 |
| D43 | turn 级 `strict_auto_review` 的保守识别（补上 D36 缺口）：`request_permissions` 是模型的普通 function tool、其 FunctionCall 照常落 rollout；当前 turn 的 rollout 中出现任何 `request_permissions` FunctionCall 即视为该 turn 可能已开启 strict_auto_review，turn 剩余时间禁用 auto-allow 与记忆选项、回基础弹窗（fail closed，不解析授权结果，宁可多弹不可绕过 guardian） |
| D44 | owner 调用关联判据：turn_id + 命令原文精确匹配（上游 hook_command 即 `cmd`/`command` 原文，逐字节一致）；无 owner 调用即放弃派生与 auto-allow、保留基础弹窗。同 turn 多条相同命令的 owner 调用是常见重试形态（如 untrusted 下先带 require_escalated 被拒、再改普通重试），字段按保守规则逐项合成（2026-07-18 细化）：sandbox_permissions 任一条升权即整体按升权处理；prefix_rule 各条全一致才采用模型档位、否则弃用（fallback 前缀推导照跑）；justification 仅用于网络域防伪造，逐条检查——任一条与 description 相同即判伪造回基础弹窗，互不相同无害。owner 调用有两种落盘形态（2026-07-18 实测补充）：① 传统 `function_call`（`arguments` 为 JSON 字符串，命令键 `command` 或 `cmd`；数组形态的旧 shell 工具不匹配、按无 owner 处理）；② code_mode / unified exec 的 `custom_tool_call`（`name="exec"`，参数埋在 JS 源码 `tools.exec_command({...})` 里，用严格识别器逐调用抽取——单 cell 多次调用逐个解析，仅接受双引号 JSON 兼容字面量与浅层嵌套，解析失败的调用跳过（永不产生假 owner）；cell 源码中出现 `request_permissions` 子串按 D43 视为 strict_auto_review 风险）。实测确认 custom_tool_call 条目在 cell 开始执行时即落盘，弹窗时 owner 必然可见（D23 对 code_mode 同样成立） |
| D45 | worker 预算与 amendment 复验：隔离 worker 单次判断总预算 2 秒（`codex execpolicy check` 子调用 500ms 级），超时 / 崩溃返回 reason code 并回基础弹窗；永久规则写入前，把拟写入行放进 AskHuman 自己 runtime 目录（0600 权限）的临时规则文件，用装机二进制复验“目标命令确实 Allow 且规则可解析”，复验失败不写入并按 D25 报告 |
| D46 | 文件路径持久 identity（回答 §7 原问题 2）：与原生 apply_patch 审批键同构——词法归一的绝对路径（`~` 展开、`.`/`..` 词法折叠，同 Codex `AbsolutePathBuf` 语义：不 canonicalize、不解析 symlink、不存在的新文件同样成键）、字节精确比较（大小写敏感，即使在大小写不敏感的文件系统上，与原生 `PathUri` 相等性一致）；相对路径以 Hook `cwd` 为基准解析。精确文件 scope 纯词法匹配、不做文件系统检查（无 TOCTOU 窗口）；项目 / 磁盘聚合 scope 为 AskHuman 自有语义：规则创建时固化 project root，命中时词法前缀匹配，并对目标路径及其项目内祖先做 symlink 检查（发现 symlink 即 fail closed 回弹窗，落实 §4.2 的防逃逸要求）。remote environment：Hook 无 `environment_id`，键折叠该维度（同 D39 注记）；cwd 无法本地解析时 git 根发现失败，按 D12 回退 cwd 前缀，路径判断纯词法进行 |
| D47 | 选项文案基线（回答 §7 原问题 5，实现期可微调措辞、不得改变语义或作用域）：文件按 §4.3；Shell 按 D38（前缀优先 + “始终允许 `<prefix>` 开头的命令”，不带写入通道等实现细节字样）；network 按 D39；MCP 为“本对话允许此工具”/“始终允许此工具（写入 Codex 配置）”，D41 兜底型标注“始终允许（由 AskHuman 记住）”以区分；所有会话级选项统一带作用域副文本说明“本对话”含 Resume 与本对话的子代理；完全磁盘项维持危险样式（§4.3） |
| D48 | 管理与审计 v1 边界（回答 §7 原问题 6）：不做冲突提示（AskHuman 规则只有 allow 语义、无优先级冲突；原生永久规则不在面板显示，避免暗示可在面板管理）；面板详情按需展示规则键原文（路径 / 命令 / 主机 / 工具名，用户自己的数据，不脱敏）；daemon 以现有日志机制记录规则创建 / 命中 / 清理事件作为审计基线；保留周期即 D15/D41 的 30 天滚动，不另设配置项 |
| D49 | 内置 AskHuman 自身调用白名单（2026-07-18 定案）：Agent 调 AskHuman 本身不应弹权限窗。**Shell 严格版**——脚本必须是单条全字面量命令（专用解析器，仅允许字面量词 / 引号串 / 惰性 stdin heredoc，任何展开、替换、管道、多语句、文件重定向、赋值前缀都拒绝），argv[0] canonicalize 后必须等于当前 hook 二进制（裸名走绝对 PATH 项查找，相对项一律不认，防工作区伪造），且仅限询问类用法（自由 ask / `--whats-next` / `--agent-help` / `todo add`；裸单词首参视为潜在子命令拒绝，`daemon`/`config` 等永不放行）。**MCP 组合拳**——集成写入 `[mcp_servers.askhuman]` 时补 `approval_mode="approve"`（仅键缺失时写；用户改值或删键都尊重、不触发“需更新”），`ask`/`whats_next`/`todo_add` 工具补 `destructive_hint=false`/`open_world_hint=false` 注解（原生 auto 模式即免审批），Hook 内置 `mcp__askhuman__{ask,whats_next,todo_add}` auto-allow 兜底（要求所有可读配置层的 askhuman server `command` canonicalize 后均指向当前二进制，且无显式 prompt/writes 模式）。两类白名单均为内置常量逻辑：不进规则文件、不占管理面板，Codex 侧仍受 guardian / strict_auto_review fail-closed 门控（D36/D43）。**Claude Code 同样生效**：Shell 白名单直接复用同一解析器与二进制身份校验；MCP 兜底改为校验 Claude 配置层（`~/.claude.json` 顶层 `mcpServers` + 覆盖 cwd 的 `projects` 条目 + cwd→git root 沿途 `.mcp.json`，任一层 askhuman server command 不指向本二进制即拒绝）；两类都先检查 Claude settings 各层（managed / 用户 / 项目 `settings(.local).json`）的 `permissions.ask/deny` 是否有提及 AskHuman 的显式规则（粗粒度子串匹配，误伤只多弹一次窗），有则尊重用户配置不放行 |
| D50 | D43 放宽（2026-07-24 定案，跟随上游 #32232）：上游已明确 PermissionRequest Hook 在 guardian（含 turn 级 strict_auto_review）之前运行、Hook 的 allow 即最终裁决（官方集成测试背书），原「turn 内出现 `request_permissions` 即禁用 auto-allow 与记忆选项」的保守门槛取消；`request_permissions_risk` 字段与关联逻辑删除，reviewer=user 的 rollout gate 无条件 MemoryAllowed。D36 的 guardian-proven 交回原生路径不变。同批同步：`BANNED_PREFIX_SUGGESTIONS` 扩容至 0.145.0（#34271，新禁 `rm`/`git`/`sudo`/`env`/`npm run` 等），`VERIFIED_CODEX_VERSION_CEILING` → 0.146 |
| D51 | 前缀档位阶梯（2026-07-24 定案，2026-08-05 优化弹窗）：worker 在 base amendment 基础上按长度递减派生截断候选（`amendment_candidates`，每个候选须通过禁选前缀表 + 自身非危险 + 覆盖全部段的校验，段覆盖 = 前缀命中或该段独立评估为 allow，后者用 `segment_allows`）；推荐档取「智能 2-token」——base 第二词像子命令（不以 `-` 开头、不含 `/`、非数字）时取命令+子命令，否则 1-token，该长度候选被过滤时向更长升档、绝不降档。协议：`ConfirmChoice.variant`（`group`/`level`/`level_label`/`segment_label`/`recommended`），其中 `level_label` 保留整档位的兼容摘要标签，`segment_label` 是该档相对上一更短候选新增的完整 token 块；若安全校验跳过中间长度，跨过的 token 合并成一个不可拆分 segment。同 group 的 choice 是同一动作的不同档位；推荐档保留原 action id，其余 `<base>:<候选下标>`，每档独立 `MemorySave`（session 档 → `ShellPrefix` 规则，永久档 → `NativeWrite::PrefixRule`），daemon 校验与两阶段提交零改动。渲染：弹窗按 group 折叠为一行，全部 group 共享一条无间隙 token 轨道；已选档以前连续高亮、蓝点标识已提交边界，hover 只预览高亮，点击提交档位，`Reset` 返回推荐档，长 token 自身截断并可横向滚动；短内容不显示滚动条，溢出时使用 5px 低对比度圆角滚动条，轨道维持四边 7px 的对称内边距，滚动条不得遮挡 segment。IM 卡片只显示推荐档（`variant_visible` 过滤，回调仍携带 wire index；钉钉卡片 option id 是下发数组位置，提交端做位置→wire 翻译，`deny_index` 译为可见位置）。单候选退化为 `variant: None`，一切与旧版一致 |
| D52 | 宽松模式「只审危险」（2026-07-24 定案，V1 仅 shell）：opt-in——全局开关 `AppConfig.permissions.codex_relaxed_shell`（设置页「Codex 会话授权」卡顶部，默认关）+ 会话级弹窗选项「本对话开启宽松模式：只审危险命令」（`RuleKey::ShellRelaxed` 会话规则，30 天滚动 TTL，Destructive 危险配色；仅 eligible 弹窗出现，开启后同类请求直接放行故不会重复出现）。eligibility 由 hook 端计算并随 `MemoryQuery::ShellCommands.relaxed_eligible` 下发：脚本可完整拆分（Enhanced 路径隐含，含 rollout gate）&& `!policy_prompt_any`（原生显式 prompt 规则必弹）&& `!dangerous_any` && 无段命中扩展危险清单。扩展危险清单（`is_relaxed_dangerous_command`，独立于上游复刻、仅供宽松模式、包装深度超限 fail-closed）：任意 `rm`/`srm`/`shred`/`dd`/`diskutil`/`truncate`/`mkfs*`、`find -delete`、`xargs`（逐后缀递归）、git 破坏性（`reset --hard`/`clean`/`checkout --`/`checkout .`/`restore`/`push --force(-with-lease)/-f`/`branch -D`/`stash drop|clear`）、进程系统（`kill`/`pkill`/`killall`/`shutdown`/`reboot`/`halt`/`poweroff`/`launchctl bootout|unload|remove`）、递归权限（`chmod -R`/`chown -R`）；sudo/env/trap 与 `bash -c` 字面脚本递归识别。daemon 在规则 auto-allow miss 后判定（全局开 || 会话规则命中，命中刷新 TTL），以 `AUTO_ALLOW_ACTION_ID` 放行并逐次写审计日志（scope + 完整命令）。管理面板 kind `shellRelaxed`，重置会话即撤销 |
| D53 | YOLO 模式「本对话自动允许一切」（2026-07-25 定案）：会话级 opt-in，**无全局开关**（全局等效原生 `codex --yolo`，刻意不做）。开启入口＝**每个** Codex 权限弹窗（Basic / Enhanced / 危险命令弹窗一律有，Destructive 配色）末尾的「本对话开启 YOLO 模式：自动允许一切」选项：hook 在 `parse_permission` 统一注入 `remember_yolo` choice + `RuleKey::Yolo` 会话 save（Basic 弹窗因此也携带 `memory`，`query: None`）；描述文案写明关闭途径。作用域＝该会话**所有**权限请求（shell 含危险命令、文件、MCP、网络），daemon 在规则/宽松 miss 后判定：`agent_kind=codex && memory 存在 && 会话 Yolo 规则命中`（命中刷新 30 天滚动 TTL）→ `AUTO_ALLOW_ACTION_ID` 放行，逐次审计日志（有 query 记完整命令/路径/工具/主机，无 query 记 tool + summary）；hook 侧 `AUTO_ALLOW_ACTION_ID` 的接受条件放宽为 `memory` 存在（不再要求 query）。**关闭**：① 设置页管理面板——会话组摘要显示「YOLO 模式」徽标 + 专属「关闭 YOLO」按钮（`PermissionRulesOp::DisableYolo`，只删 Yolo 规则不动其它授权，kind `yolo`）；② IM `/yolo` 命令——无参发带「关闭」按钮的会话选择卡（`PickerKind::Yolo`，选项＝开着 YOLO 的会话、最近使用在前，展示与其余 agent 卡同口径：类型 · 项目名 / 标题 / 圆点 / 编号 + YOLO 徽标，不在册用注册表存档降级；点选即关并定格终态），`off <编号>` 按 Agent 编号直关，`off` 缺省恰一个开着直关、多个弹卡，编号解析不到落回选择卡；支持无前缀整句短语（`yolo`/`关闭yolo`/`yolo off` 等，一律指向列表卡，见 im-command-phrases）；渠道不支持卡片时文本列表兜底；③ 从 IM 卡片开启时终态文案附「发送 /yolo 可随时关闭」提示。Claude 不适用（V1 仅 Codex） |

### 2.1 “本会话”的最终作用域

这里有意采用比 Codex 当前内存缓存更符合产品预期的语义：

```text
AskHuman session approval scope
  = Codex session_id
  = 根线程 + Resume + 该根线程产生的所有协作子代理
```

Codex 已保证：

- `Session::session_id()` 来自整棵 agent tree 共享的 `AgentControl.session_id`；
- `PermissionRequest` Hook 的 `session_id` 使用上述共享值；
- 协作子代理另有自己的 `agent_id` / thread id，但不改变共享 `session_id`；
- Resume 会从 rollout 的 `SessionMeta.session_id` 恢复原值；
- 新建、clear 或 fork 的新对话生成新的根 thread/session identity。

因此 AskHuman 后续的 session rule 不应包含 `agent_id` 这一隔离维度。具体规则仍必须按文件、命令、host、
MCP 工具等精确资源键匹配；共享 session 并不等于把整个对话切换为无条件放行。

## 3. 当前 HumanInLoop 行为

当前实现位于 `src-tauri/src/permissions.rs`：

- Codex 请求只生成 `approve_once` 与 `deny`；
- Claude Code 若在本次请求中携带可识别的 `permission_suggestions`，才会额外显示其原生 rule suggestion；
- `approve_once` 对 Codex 输出 Hook `allow`；
- `deny` 输出 Hook `deny`；
- Codex 分支不会输出或伪造 `updatedPermissions`。

Popup 与 IM 的通用 Confirm 模型已经支持动态结构化选项，所以主要缺口不在展示控件，而在：

1. 如何判断当前 Codex 原生会显示哪些 scope；
2. 如何构造与 Codex 一致的匹配键；
3. 如何在本进程立即生效并在未来 Codex 会话永久生效。

## 4. Codex 原生审批能力矩阵

下表描述当前调研版本的 TUI / core 能力，不代表 AskHuman 已经能够从 Hook 输入完整重建这些选项。

| 审批类型 | 原生单次 | 原生 session | 原生 permanent | 主要作用域 / 存储 |
|---|---:|---:|---:|---|
| 普通 shell / unified exec | 有 | 协议支持，但普通默认菜单不一定提供 | 有条件提供 command prefix amendment | permanent 写 `~/.codex/rules/default.rules` |
| `apply_patch` | 有 | 有，“不再询问这些文件” | 无独立永久文件规则选项 | 内存键为 `environment_id + path` |
| 网络访问 | 有 | 有，按 host | 有，按 host 写网络 policy rule | session host cache + `default.rules` |
| MCP tool | 有 | 有 | 符合条件时有“始终允许” | session approval store + 用户/项目/plugin/app MCP 配置 |
| 动态 `request_permissions` | 按 turn grant | 有 session grant | 无 | 当前不经过 `PermissionRequest` Hook |
| execve / additional permissions 等特殊审批 | 通常有 | 依请求提供的 decisions 而定 | 依请求能力而定 | Hook 未收到完整 candidate metadata |

### 4.1 原生 session cache 并非全树共享

Codex 的 `SessionServices` 为每个 thread 初始化独立的
`tool_approvals: Mutex<ApprovalStore>`。`apply_patch`、shell / unified exec 与部分 MCP session 决策从当前
thread 的这份 store 查询。

普通 `spawn_agent` 会创建新的 Codex `Session`，只共享 `AgentControl`、环境与继承的 exec policy，不共享
`tool_approvals`。所以在当前原生 Codex 中：

```text
主线程对文件 A 选择“本会话不再询问”
  -> 只写主线程 ApprovalStore
子代理第一次修改同一个文件 A
  -> 子代理 ApprovalStore 为空
  -> 若该操作仍需要审批，会再次询问
```

`codex_delegate.rs` 中存在“由父 session 处理审批”的路径，但它服务于 guardian/review 等内部
sub-Codex，不是普通协作 `spawn_agent`，不能据此推断普通子代理共享批准缓存。

AskHuman 已确认采用 D4–D5 的对话树级语义，主动消除这个原生重复询问点。

### 4.2 AskHuman 文件编辑 session scopes

Codex 原生 `apply_patch` 没有永久允许：TUI 只有批准一次、对本次涉及文件执行 session allow、拒绝。
原生 session key 是 `environment_id + PathUri`，保存在当前 thread 的内存 `ApprovalStore`，不写 rules 或
config。一次 patch 涉及多个文件时，各文件分别入库；未来请求只有在所有涉及路径都已批准时才跳过询问。
Move 的源路径与目标路径都会入库。

AskHuman 保留这个精确文件语义，同时增加两个用户明确要求的聚合 session scope。选项由当前请求的完整路径集
动态决定：

```text
原生结构化文件编辑请求
  -> 所有旧/新路径均可可靠解析？
       -> 否：只显示批准一次 / 精确文件允许（若能安全表达）/ 拒绝
       -> 是：以 Hook cwd 检测最近 Git 根；无 Git 时用 cwd
            -> 所有路径都在 project root 内
                 -> 显示“本对话允许所有本项目内的文件修改”
            -> 任一路径在 project root 外（含内外混合）
                 -> 显示“本对话允许完全磁盘文件修改”
```

三个 session scope 的匹配关系为：

| Scope | 自动允许条件 | 不包含 |
|---|---|---|
| 精确文件 | 未来原生文件编辑请求的全部旧/新路径都在已批准路径集合中 | 未记录的新路径、shell、MCP |
| 本项目 | 未来原生文件编辑请求的全部旧/新路径都位于同一已批准 project root 内 | 项目外路径、其它项目、shell、MCP |
| 完全磁盘 | 同一 `session_id` 下未来所有可验证的原生结构化文件编辑 | shell、MCP、无法识别的编辑载荷 |

UI 使用“文件修改”而不是底层工具名。该文案成立的前提是 adapter 已把请求验证为 Agent 原生结构化文件编辑；
当前 Codex 实际对应规范 `apply_patch`。任意 shell 命令可能同时写文件、执行程序、联网或产生其它副作用，
Hook 无法只批准其中的写入部分，因此不得命中上述文件规则。

路径分类必须纳入新增/删除/移动的全部旧、新路径，解析相对路径时以 Hook `cwd` 为基准，并防止 `..`、
symlink 或不存在目标的父目录绕过 project root 边界。任何不能可靠分类的载荷均不得显示或命中聚合 scope。

### 4.3 选择与提交

文件授权沿用现有结构化 Confirm 的无默认选择表单，顺序为：

1. 允许一次；
2. 本对话不再询问这些文件；
3. 按当前完整路径集动态显示“本对话允许所有本项目内的文件修改”或“本对话允许完全磁盘文件修改”；
4. 拒绝。

本地 Popup、飞书、钉钉与 Slack 已经区分 selection draft 和 submit：选择只改变表单状态，提交才参与跨渠道
首答胜出。Telegram 当前把 `pc:do:<index>` 直接送入 coordinator 终态，是唯一例外；本需求将其统一为：

- 点击选项只更新卡片上的选中标记和 daemon 内 draft；
- 有选择后显示 / 启用“提交决定”；
- 只有提交 callback 才调用 `submit_wire`；
- 拒绝原因仍通过精确回复本卡形成 draft，随后显式提交；
- 其它渠道先完成时，Telegram 的未提交 draft 只定格，不影响首答。

这一修正适用于语义为 `SingleSelectSubmit` 的全部结构化 choice 表单，不为 permission 单独制造 Telegram
特例。完全磁盘选项不再增加第三次确认；“选择 + 提交”本身就是一致的显式确认流程。所有权限选项仍不预选、
不标推荐，完全磁盘项需要危险样式与明确的 Resume / 子代理作用域说明。

## 5. 标准 Codex Hook 的硬边界

### 5.1 Hook 输入没有原生候选项

当前 `PermissionRequest` command input 只有：

- `session_id`、`turn_id`、可选 `agent_id` / `agent_type`；
- `transcript_path`、`cwd`、`model`、折叠后的 `permission_mode`；
- `tool_name`、`tool_input`。

它没有向外暴露 core / TUI 已经算好的：

- `available_decisions`；
- `proposed_execpolicy_amendment`；
- `environment_id`；
- canonicalized command；
- `sandbox_permissions` / `additional_permissions`；
- network `host` / `protocol` / approval context；
- patch `grant_root`；
- approval attempt / retry reason。

Codex 还会把 `OnRequest`、`UnlessTrusted` 与 `Granular` 都折叠成 Hook
`permission_mode = "default"`，Hook 不能用该字段还原更细的原始 approval policy。

### 5.2 Hook 输出只有 allow / deny

当前 command Hook output 只接受：

- `behavior = "allow"`；
- `behavior = "deny"`。

若输出 `updatedPermissions`、`updatedInput` 或 `interrupt: true`，Codex 会把它视为不支持的输出并失败关闭。
Hook 的 `allow` 在 core 中只映射为 `ReviewDecision::Approved`，不能映射为：

- `ApprovedForSession`；
- `ApprovedExecpolicyAmendment`；
- `NetworkPolicyAmendment`；
- MCP `AcceptForSession` / `AcceptAndRemember`。

因此 AskHuman 若要兼容标准 Codex，只能：

1. 对当前请求返回普通 `allow`；
2. 在 AskHuman 自己的规则层记住 session / permanent 决定；
3. permanent 决定另行以 Codex 原生格式写入其配置文件。

### 5.3 Hook 与原生 cache 的调用顺序

对 shell、unified exec、`apply_patch` 等路径，`PermissionRequest` Hook 在 orchestrator 判断
`NeedsApproval` 后先运行，而原生 `with_cached_approval` 查询位于随后执行的 runtime approval 阶段。
因此即便 Codex 当前 thread 的原生 session cache 已经批准过，Hook 仍可能先收到请求。

AskHuman 从会话开始即接管审批时，自己的 shadow rule 可以避免重复展示；但这进一步说明不能把 Codex 私有
`ApprovalStore` 当成 Hook 的可查询真相。

### 5.4 不是所有原生审批都经过该 Hook

Codex 动态 `request_permissions` 有 turn / session grant 菜单，但当前没有进入
`PermissionRequest` Hook。这一类无法仅靠现有 Hook 接管，必须在最终覆盖矩阵中明确标为“标准 Codex
接口不可达”，不能假装已支持。

### 5.4.1 guardian 审批没有直接的“转交用户确认”出口

guardian（auto-review）在 Hook 之后运行，其评估结果只有 Allow / Deny 两种
（`GuardianAssessmentOutcome`），没有“需要用户确认”这个第三态：

- Allow → `ReviewDecision::Approved`，直接执行；
- Deny（含评审超时、内部错误的 fail-closed）→ 本次工具调用被拒绝，拒绝消息连同固定指示
  （`GUARDIAN_REJECTION_INSTRUCTIONS`：不得绕过；只有用户知情并明确批准后才可继续，否则停下请求用户输入）
  回给模型；
- 同一 turn 连续 3 次 guardian 拒绝或滑动窗口内 10 次拒绝会触发熔断，直接中断整个 turn 交还用户。

因此“用户确认”只会通过模型间接发生：模型收到拒绝后在对话里直接问用户，或发起
`request_permissions` 动态授权（原生 TUI 菜单，不经过 `PermissionRequest` Hook，§5.4）。这印证了 D36 的
安全性：guardian 路由的审批点上用户原生不会被问，AskHuman 跳过不会漏掉任何原生用户提问；后续用户参与
（模型停下来问 → 自然 Stop，可由既有 Stop 确认 / watch / 插话链路带到手机）与本 Hook 无关。同时 Hook
`allow` 先于 guardian 返回、会绕过 guardian 复审，这是 auto-allow 必须在 guardian 环境禁用（D29/D36）的
另一半理由。

### 5.5 App Server 不是当前 Hook 的判定服务

App Server 的 command approval server request 确实包含 Hook 缺失的
`availableDecisions` 与 `proposedExecpolicyAmendment`，但它们是 Codex core 处理真实工具调用后主动发给正式客户端的
审批事件，不是客户端可调用的只读判定 API。当前协议没有
`execPolicy/evaluate(command, thread_id)` 一类方法；`config/read` 只读取配置，`command/exec` 与
`thread/shellCommand` 会发起真实执行，不能用作 dry-run 权限计算。

调用顺序同样阻止 Hook 把当前 App Server 当作旁路 oracle：Codex 先等待 `PermissionRequest` Hook；只有 Hook
未返回裁决时，core 才继续生成原生审批事件。Hook 等待该事件会形成互等；先退出 Hook 又会失去通过当前 Hook
返回决定的机会。另启 App Server 则缺少当前 thread 的完整运行状态，还可能真实执行、再次触发 Hook 或得到不同
配置上下文的结果。

让 AskHuman 位于 TUI 与 App Server 之间虽可直接截获完整审批，但这要求转发全部 Codex 协议流量、接管连接与
daemon 生命周期，并扩大故障和数据边界，已超出当前提问工具的产品范围。因此 D20–D21 将两种 App Server
路线排除出本需求；若 Codex 未来向 Hook 暴露最终候选或新增只读评估 RPC，可重新评估。

### 5.6 Rollout 时序与非回归降级

当前 Codex 在 `handle_output_item_done` 识别到工具调用后，先等待
`record_completed_response_item`。该调用经 `record_conversation_items` 把当前 `turn_id` 补到
FunctionCall，并通过 `LiveThread::append_items`、local thread store 的 `durable_write` 与
`RolloutRecorder::flush` 完成 JSONL 写入屏障；之后才构造并调度 tool future。PermissionRequest Hook 位于
tool runtime 的 approval 阶段，因此正常路径中 Hook 开始前，对应 FunctionCall 已可从 `transcript_path` 读取。
当前 turn 的 `TurnContextItem` 还会在首次 model sampling 前持久化。

这项时序保证不等于请求身份总能唯一恢复：Hook 没有 `call_id`，同一 turn 的相同 command 并发调用仍可能歧义；
旧 Codex schema 也可能没有 FunctionCall 的 `turn_id` metadata。Rollout 写入持续失败时，Hook 取 path 前的再次
materialize 也可能无法补齐。因此任何缺失、截断、schema 不支持或多候选情况都只会使记忆增强不可用，不得产生
自动允许。

权限处理分成两个故障域：

1. 基础确认适配器继续只依赖现有 Hook 字段，负责始终可用的“允许一次 / 拒绝”；
2. rollout 关联、Codex 版本适配、原生判断复刻、shadow rule 与永久落盘只负责增加记忆选项或自动命中。

第二层任一步失败时回到第一层，而不是取消 AskHuman 确认。只有基础解析、daemon/IPC、Popup 与 IM 投递等现有
确认链也失败时，`__permission-hook` 才保持当前行为：exit 0 且不输出 stdout，使 AskHuman 不作裁决，由其它匹配
Hook 继续处理；若没有其它裁决，再进入 Codex 原生审批。内部异常不得合成为 `allow` 或 `deny`。

需要落盘的记忆选择不能在用户点击时先显示成功。最终实现必须等规则提交完成后再定格所有渠道：提交成功显示
“已允许并记住”；提交失败按 D25 返回当前调用的普通 `allow`，并显示“本次已允许，但未能保存授权”。具体两阶段
提交由 daemon 持有：某一渠道提交记忆选择后，daemon 先锁定本次请求，执行规则保存与校验，再同时定格所有渠道
并把最终裁决交还仍在等待的短命 Hook。

Shell 记忆能力的分析运行在短命隔离 worker 中，复用当前 permission diff worker 已采用的子进程边界、限长
stdin/stdout、`kill_on_drop` 与硬超时模式。daemon 必须先保有基础“允许一次 / 拒绝”确认能力，再把只读输入交给
worker；worker 崩溃、超时、输出非法、规则读取失败或版本不兼容时只丢弃新增候选，不影响基础弹窗。worker 输出
还需由 daemon 按当前请求绑定信息复核，且没有直接返回 Hook allow / deny 的权限。超时预算与 amendment
写入前复验按 D45 执行。

Shell permanent 的提交结果以 Codex 原生规则写入为准，不再写 AskHuman 持久 shadow。当前尚未 reload 的 Codex
再次进入 Hook 时，AskHuman 由短命隔离判断进程重新读取并解析最新 Codex 文件。
但 `default.rules` 不是完整 effective policy：Codex 会合并所有启用 config layer 的 `.rules`，再叠加
`requirements.toml`、MDM 或 cloud managed exec-policy。AskHuman 只有在能证明这些来源均已纳入判断时，才可把原生
规则的 Allow 作为自动允许依据；缺失或不可读取的 managed overlay 必须视为未知，而不是视为空。

同一个 Bash `PermissionRequest` 还可能来自 strict auto-review、sandbox / network retry 或 execve escalation；Hook
输入没有 `call_id`、`:retry` 后缀、retry reason、sandbox permissions 或 strict 状态。Codex 当前实现中 Hook
`allow` 会先于 Guardian / 用户审批直接返回，因此会绕过 strict auto-review。retry 阶段的安全性按 D42 的
收紧条件消解（全段显式规则 Allow 即与原生免沙箱放行等价），strict auto-review 按 D36/D43 识别后禁用；
network 请求按 D39 判真后另行处理。原生规则写入失败则按 D25 降级为允许一次。

## 6. 已确认的总体实现模型

在 D3“只依赖标准 Codex”的约束下，需要两层记忆：

```text
PermissionRequest arrives
  -> AskHuman 对明确支持的 session 类型查询 session shadow rules
       -> 命中：直接返回 allow，不展示重复弹窗
       -> 未命中或不适用：只展示本请求可证明支持的选项
            -> 批准一次：仅返回 allow
            -> 本对话允许：写精确资源 / 项目 / 磁盘 / 主机 / MCP 工具 session rule，再返回 allow
            -> Shell 始终允许：只写 Codex 原生 rules，再返回 allow
            -> network / MCP 始终允许：写各自原生配置 + 会话桥接规则（§6.5），再返回 allow
            -> 插件 / codex_apps MCP 始终允许：写跨会话 shadow（D41），再返回 allow
            -> 拒绝：返回 deny
```

### 6.1 Shell permanent 的唯一真相与首版读取策略

直接从 Hook 外修改 `~/.codex/rules/default.rules` 不会让当前已经运行的 Codex
`ExecPolicyManager` reload。AskHuman 不为 Shell 再保存一份持久 shadow，而是把 Codex 原生文件作为唯一真相：

- 首版不设置 daemon policy 缓存、常驻 worker 缓存或完整命令结果缓存；D30 的临时 provenance
  只保存旧 Codex policy 基线与本次原生追加之间的摘要关系，不保存解析结果或授权判断；
- 每次判断都由短命隔离进程读取并解析当前支持版本的完整 policy；
- 当前 Codex 自带的 `codex execpolicy check` 可复用同版本原生 rule parser，但它只评估传入的显式规则文件与
  command tokens，不负责还原 active config layers、managed overlay、shell 分段、heuristics 或请求阶段；这些缺口
  必须由外围证明或安全降级，不能把该命令当作完整审批 oracle；
- 只有实测证明规则读取或解析成为瓶颈后，才另行评估缓存设计；
- 当前 Codex 再次调用 Hook 时，普通 Shell 请求若按最新原生 policy 已为 allow，则 Hook 自动返回 `allow`；
- 若 Hook 可能代表 sandbox retry、strict auto-review 或其它无法还原的阶段，不因 prefix rule 自动放行。

这样未来新 Codex 进程直接使用原生全局规则，当前未 reload 的 Codex 也能由 Hook 按同一真相补足普通请求，同时没有
两份永久规则的同步问题。临时 provenance 按 thread / rollout identity、Codex 进程代次与 policy context 分区；
不同子代理、cwd 或配置上下文不得借用另一份基线。它只保存在 daemon 内存中，daemon 重启、摘要不匹配或其它
policy 文件变化后立即失效并恢复弹窗。规则读取或判断失败时同样按 D24 显示原有弹窗。

MCP、network 的 permanent 即时层已按 §6.5 定案为“原生写入 + 会话桥接”，与 Shell 的规则回读方案不同：
`codex execpolicy check` 只评估命令前缀规则，不暴露 network_rule / MCP 配置的评估，回读需要自行解析，
得不偿失；而桥接方案的效果恰好等于原生（原生的 in-memory 更新同样只惠及当前进程）。

### 6.1.1 Shell 判断复刻的混合架构（D31–D35，2026-07-18 定案）

复刻不等于全量移植。Codex 的 Shell 审批判断被拆成四块，各自采用不同策略：

| 组件 | 策略 | 依据 |
|---|---|---|
| 规则匹配语义（prefix/network/host_executable、Starlark 解析） | **不复刻**：调用已装 codex 二进制的 `codex execpolicy check --rules <file>... -- <tokens>`（2025-11-20 引入的只读评估 CLI，输出 matchedRules + decision JSON，无副作用） | 漂移风险最大的部分交给与运行中 Codex 完全同版本的二进制（D31） |
| bash -lc 脚本拆分 | **复刻** `parse_shell_lc_plain_commands`（tree-sitter-bash 版本对齐 Codex Cargo.lock + 相同节点白名单）；`bash.rs` 近 6 个月改过 7 次，属版本绑定面 | D32 |
| 规则层叠发现 | **复刻常用层**：user + system 文件层 + 项目层（信任判定）；MDM / 企业云 / requirements 只做存在性检测，检测到即禁用记忆增强 | D33，移植量约 300–400 行，信任判定可用 Codex 自身测试对拍 |
| heuristics 兜底与候选派生 | **复刻**（约 800 行判定逻辑 + 对拍测试），版本门控只设下限 0.122，超出已审计版本仅记日志 | D34–D35 |

关键输入的来源与失败处理：

- 命令 argv：Hook `tool_input.command` 为脚本原文，按 `[shell, -lc, script]` 包装后与 Codex 解析路径语义等价（bash/zsh/sh 同路径；无法识别的 shell 类型 fail-closed）。
- `approval_policy` / `permission_profile` / `approvals_reviewer`：读 rollout 当前 turn 的 `TurnContextItem`（每 turn 首次采样前已落盘）；缺失或旧 schema 即放弃记忆增强（D23）。
- 模型自带 `prefix_rule` / `sandbox_permissions` / `justification`：读 rollout 对应 FunctionCall arguments；Hook 无 `call_id`，必须以 `turn_id` + 命令原文唯一关联，存在歧义即放弃。
- codex 二进制定位：Hook 进程的父进程链中的 codex 可执行文件；`--version` 同时用于 D34/D35 的版本下限判定。
- auto-allow 防误放行沿用 D29/D36，retry 与 turn 级 strict_auto_review 的处理按 D42/D43（§6.1.2）。

### 6.1.2 retry 安全性与 turn 级 strict_auto_review（D42–D45，2026-07-18 定案）

**retry 不识别，靠收紧消解**（D42）。orchestrator 的事实依据：

- 非 guardian 会话里，一个 shell 调用至多产生一次询问。`NeedsApproval` 首段审批通过后
  `already_approved = true`，沙箱失败重试时 `should_bypass_approval` 直接豁免再审；只有首段被政策放行进
  沙箱（`Skip`，无询问）时，“沙箱失败，是否不带沙箱重试？”才是该调用的第一次也是唯一一次询问。
- retry 的 `{call_id}:retry` run id 只装饰 TUI 的 Hook 生命周期事件，不进 Hook stdin（
  `PermissionRequestCommandInput` 无该字段），计数识别既不可行也不需要。

因此 Hook 无法也无需区分“首段询问”与“retry 询问”，安全性由两条收紧保证：

1. 永久 auto-allow 仅在**每个拆分段都被显式规则 Allow 命中**时触发——与 Codex 自身 `bypass_sandbox`
   （免沙箱直接执行）的判定条件一致。满足时 reload 过的原生会直接免沙箱放行、根本不会出现 retry 询问，
   所以即使我们在 retry 时刻 auto-allow，结果也与原生等价。heuristics（known-safe 列表）**永不**作为
   auto-allow 依据，只用于选项派生与 dangerous 门槛（D34 复刻范围不变，用途收窄）。
2. 会话规则 auto-allow 与原生 `ApprovedForSession` 缓存天然镜像：原生缓存命中同样静默批准并使 retry
   豁免（`with_cached_approval` 返回 ApprovedForSession → `already_approved`），行为一致。

**turn 级 strict_auto_review 的保守识别**（D43）。config 级 `approvals_reviewer = auto_review` 由 D36 靠
`TurnContextItem` 识别；turn 级动态开启（`request_permissions` 授权响应带 `strict_auto_review = true`）
不落 TurnContextItem，但 `request_permissions` 本身是模型的普通 function tool，FunctionCall 照常落
rollout。识别规则：当前 turn 的 rollout 中出现任何 `request_permissions` FunctionCall，即视为该 turn
可能已开启 strict_auto_review，turn 剩余时间禁用 auto-allow 与记忆选项、回基础弹窗。不解析授权结果
（授权可能失败或不含 strict 标志），宁可多弹、不可让 Hook allow 绕过 guardian 复审。

**FunctionCall 关联判据**（D44）与**worker 预算 / amendment 复验**（D45）见决策表。

### 6.2 session rule 的持久化要求

D5 要求 Resume 后继续有效，因此仅保存在 daemon 内存不足以满足语义。最终存储至少需要：

- 以 `session_id` 为顶层 namespace；
- daemon / GUI 重启后仍可读取；
- 不以 `agent_id` 分裂根线程与子代理；
- 对未知或无法稳定 canonicalize 的请求 fail closed：不命中、不自动允许；
- 每条规则记录 `last_used_at`；只在规则实际匹配并自动允许时刷新；
- 规则连续 30 天没有命中后清理，普通聊天、Resume 或无关工具调用不续期；
- 定义硬容量上限与用户撤销机制，防止活跃会话在 30 天内生成无限精确文件键。

存储基线（实现期可调数值、不改语义）：rule store 落在 AskHuman daemon 现有数据目录下的独立文件，权限
0600；硬容量上限按 session 500 条、全局 10000 条，超限时不写入新规则并按 D25 在原渠道报告“本次已允许，
但未能保存授权”，不静默淘汰已有规则。永久 Codex rules 不受 30 天 session 清理影响。

### 6.3 查看与重置

设置页“高级”Tab 的最后一张卡保留全局“宽松模式”开关，并提供静态“管理 Codex 会话授权”入口；会话列表
不在卡片内展开，而是在与“管理工作目录”同口径的 macOS sheet 中管理。该功能使用频率低，必须渐进加载：

1. 设置窗口启动不读取 rule store；
2. 切换到高级 Tab 也不查询 rule store，卡片不显示需要后端统计的动态 count；
3. 用户点击管理按钮后才打开独立面板、连接 daemon 并加载对话摘要；
4. 展开某个对话时再加载其完整 scope / 路径详情；
5. 设置搜索只索引静态标题与说明，不触发规则加载。

列表按 Codex `session_id` 分组；优先使用 daemon 已有 agent registry 中的对话标题与项目名。Registry 不保留的
较旧会话，在用户打开面板时按精确 `session_id` 从既有 Codex rollout 解析首条真实用户消息作为标题，复用
`agents::title` 的既有过滤与截断逻辑；解析失败才回退项目名 / “未命名对话”，缩短的 session id 只作为辅助元数据，
不再充当主标题。收起态以紧凑 badge 展示已有 scope（精确文件 / project root / 完全磁盘 / shell / network /
MCP / YOLO），点击整行原位展开完整规则和预计清理时间。

每行右侧只保留统一尺寸的省略号菜单，避免多个文字按钮挤压标题；菜单承载“关闭 YOLO”（存在时）与
“重置此对话授权”。重置前显示独立确认对话框，一次删除该 `session_id` 下全部 AskHuman session rules，
不做逐条或逐路径编辑。daemon 必须串行完成原子落盘和内存 matcher 失效后再报告成功；此后下一次相关请求
重新弹窗。设置页不直接编辑存储文件。永久 Codex rules 不属于“重置对话授权”，其查看 / 撤销需要随对应
永久类型另行设计，不能在这里暗中修改 Codex 原生配置。

D41 的跨会话 shadow 授权（插件 / codex_apps MCP“始终允许”）不属于任何单个对话，在面板中单列
“跨会话授权”分组，提供单独查看与重置；“重置此对话授权”不影响它们。

### 6.4 复刻逻辑的维护纪律（随 D34 生效）

复刻组件与 Codex 上游存在持续同步义务。失同步的主要后果是覆盖率损失（少给选项）；auto-allow
路径不依赖复刻列表（只认装机 codex 的 execpolicy 显式 allow，见 D35 修订），因此不再用版本上限
挡新版本，改为日志留痕 + 定期对拍。

- 实现中维护显式的 `VERIFIED_CODEX_VERSION_FLOOR`（0.122，硬下限）与 `VERIFIED_CODEX_VERSION_CEILING`
  （最近一次逐行对拍的版本，仅用于“超出即记日志”），与复刻来源的 Codex commit 一并记录；
- 每次抬升已审计版本前，重新对拍以下上游文件的变更：`shell-command/src/bash.rs`、
  `command_safety/is_safe_command.rs`、`command_safety/is_dangerous_command.rs`、
  `core/src/exec_policy.rs`（fallback / amendment 派生 / 禁选前缀表）、`config/src/loader/`（层叠与信任）、
  `execpolicy check` 的 CLI 契约；
- 功能开发完成后，在 `docs/PROGRESS.md` 登记“定期同步 Codex Shell 判定复刻”事项，写明检查清单与
  当前已验证版本，防止跨会话遗忘。

### 6.5 network 与 MCP 的记忆模型（D39–D41，2026-07-18 定案）

#### 6.5.1 network 域名放行

原生链路（`core/src/tools/network_approval.rs`）：命令执行中被代理拦截的域名触发一次 PermissionRequest
Hook，payload 为 `tool_name = Bash`、`command = 触发它的那条 shell 命令`（无归属命令时为
`network-access {target}`）、`description = "network-access {protocol}://{host}:{port}"`。Hook 回 allow 只是
一次性放行（`AllowOnce`），**不写任何原生缓存**——这是当前 AskHuman 下同一 host 每次拦截都弹窗、确定劣于
原生 TUI 的场景。原生菜单的“本会话允许该主机”键为 host（小写归一）+ 协议 + 端口 + environment_id；
“永久允许”通过 `NetworkPolicyAmendment` 往 `default.rules` 追加 `network_rule(...)` 行（与 shell prefix_rule
同一写入通道），并更新自己进程的内存策略。

AskHuman 方案：

- **判真三条件**（全部满足才按 network 渲染，否则按普通 Shell 请求处理，只给允许一次 / 拒绝）：
  1. description 严格匹配 `network-access {proto}://{host}:{port}` 机器格式；
  2. 协议 ∈ {http, https, socks5-tcp, socks5-udp}；
  3. rollout 中该命令 FunctionCall 的模型自带 `justification` ≠ 该 description（普通 Shell 请求的 description
     字段是模型写的 justification，可被模仿成 network 样式；真正的 network 请求中 description 由 Codex 生成）。
  弹窗无论判真与否都展示触发命令原文，进一步压缩伪造欺骗空间。
- **会话级**：shadow 主机规则，键 = host + 协议 + 端口，对话树共享（§2.1）。与原生的唯一差异是缺
  environment_id 一维（Hook 拿不到）：同一对话跨本地/云端环境访问同一主机会共享放行，风险极低，接受。
- **永久级**：追加机器格式的 `network_rule(host=..., protocol=..., decision="allow")` 行到 `default.rules`
  （复用 shell 的写入通道与文件锁语义），**同时**记一条本对话的会话桥接规则。已在跑的 Codex 不 reload
  外部写入、后续拦截仍进 Hook，由桥接规则放行；新会话由原生规则直接放行、不再拦截。效果与原生等价
  （原生 in-memory 更新同样只惠及当前进程）。永久 deny 不做：原生 TUI 默认菜单也不提供。
- auto-allow 前同样必须通过判真三条件；判真失败的请求永不因主机规则自动放行。

#### 6.5.2 MCP 工具

原生链路（`core/src/mcp_tool_call.rs`）：Hook 收到 `tool_name = "mcp__<server>__<tool>"`（Codex 按注册表
生成，模型无法伪造）、`tool_input` = 工具实参。原生“本会话记住”键 = server + connector_id + tool_name
（**不含参数**），仅当该工具 approval_mode 为 Auto 时提供；“始终允许”额外受 ToolCallMcpElicitation
feature flag 门控，落盘为向**定义该 server 的配置层**（用户 config.toml / 项目 `.codex` / 插件配置 /
codex_apps 的 `apps.<connector>.tools`）写 `approval_mode="approve"`，随后 reload 用户层。

AskHuman 方案：

- **会话级：全部 MCP 工具都提供**（含插件 server 与 codex_apps 连接器）。键 = 完整 hook tool_name
  字符串，不拆 server/tool（规避 `__` 分隔歧义）；codex_apps 的 tool_name 自带连接器命名空间（如
  `mcp__codex_apps__calendar__create_event`），全字符串键天然达到原生按 connector 隔离的粒度。粒度与
  原生一致：同一工具换参数不再问。语义依据：shadow 是 AskHuman 自己的记忆、不落 Codex 配置，以用户在
  弹窗上的明确选择为准，不依赖对方配置层的可读性。
- **永久级（有原生通道）**：server 定义在用户层 `config.toml` 或信任项目层 `.codex/config.toml`（信任判定
  复用 D33）时，用格式保留编辑写 `mcp_servers.<server>.tools.<tool>.approval_mode="approve"`（复用现有
  Codex config.toml 编辑基础设施），同时记会话桥接规则兜住当前对话。
- **永久级（无原生通道，D41 兜底）**：插件定义的 server 与 codex_apps 连接器（原生落盘需要 Hook 拿不到的
  connector_id / 插件配置发现）改用 AskHuman 跨会话 shadow：全局 namespace（不含 session_id）、键 =
  完整 hook tool_name、D15 的 30 天滚动清理。这里没有双重真相问题——该类型不存在我们能写的原生真相源，
  shadow 即唯一真相；且该授权只在 AskHuman Hook 生效期间起作用，卸载/停用后回到原生 TUI 逐次询问。
- **尊重显式配置**：能读到的配置层（用户层 + 信任项目层）里该工具被显式设为 prompt/writes 模式
  （= 用户要求每次都问）时，两级记忆选项都不出现，与原生一致；读不到的层不因此禁用会话选项。
- 不看 ToolCallMcpElicitation feature flag：它只门控原生 TUI 的 UI 选项，`approval_mode="approve"` 本身是
  文档化的原生配置（同 D38 的精神：AskHuman 自有菜单语义）。

## 7. 问题定案索引（2026-07-18 全部关闭）

原“尚未定案的问题”已逐项与产品确认完毕，留作索引：

1. **shell 复刻的收尾细节** → D42–D45/§6.1.2：retry 不识别、靠 auto-allow 收紧消解；turn 级
   strict_auto_review 以当前 turn 出现 `request_permissions` FunctionCall 为保守信号 fail closed；
   FunctionCall 关联判据、worker 预算与 amendment 复验边界见 D44/D45。
2. **文件路径的持久 identity** → D46：与原生审批键同构的词法归一绝对路径（不 canonicalize、不解析
   symlink、大小写敏感、新文件可成键）；聚合 scope 命中时 symlink fail-closed 检查。
3. **网络请求识别** → D39/§6.5.1：判真三条件 + 主机 shadow + network_rule 写入与会话桥接。
4. **MCP always allow** → D40/D41/§6.5.2：会话级全覆盖；永久级用户层/信任项目层写原生配置，插件与
   codex_apps 用跨会话 shadow 兜底。
5. **非文件选项文案** → D47：各类型文案基线与两种“始终允许”的区分标注。
6. **永久与 session rule 管理** → D48：无冲突提示、详情不脱敏、日志审计基线、30 天滚动保留。
7. **不可达审批** → §5.4 + D43：`request_permissions` 明确保留原生 TUI，不暗示已接管；其出现同时作为
   turn 级 strict_auto_review 的 fail-closed 信号。Codex 未来扩展 Hook 协议时再议。
8. **Hook 信息不足时的降级** → D24 总则 + 各类型条款（Shell D31–D35/D42、文件 §4.2/D46、network D39
   判伪降级、MCP D40、guardian D36/D43）：始终保留基础“允许一次 / 拒绝”弹窗，只有基础确认链失败才以
   空 stdout 交还 Codex 原生审批。

## 8. 源码依据

HumanInLoop：

- `src-tauri/src/permissions.rs`：当前 Codex 只提供 approve once / deny；Claude suggestion 回放；Hook stdout。
- `src-tauri/src/integrations/agent_permission.rs`：PermissionRequest Hook 安装、信任与状态管理。

Codex：

- `codex-rs/core/src/hook_runtime.rs`：Hook 输入构造、共享 `session_id`、subagent context、permission mode 折叠。
- `codex-rs/hooks/src/schema.rs`、`events/permission_request.rs`、`engine/output_parser.rs`：command Hook 输入/输出契约及
  unsupported fields。
- `codex-rs/core/src/state/service.rs`、`session/session.rs`：每个 Session 的独立 `ApprovalStore` 与 Resume
  `session_id` 恢复。
- `codex-rs/core/src/agent/control.rs`、`agent/control/spawn.rs`：整棵 agent tree 共享 `AgentControl.session_id`，
  协作子代理另建 Session。
- `codex-rs/core/src/tools/sandboxing.rs`：`with_cached_approval` 的 session cache 行为、
  `should_bypass_approval`（已批准过即豁免 retry 再审），D42 依据。
- `codex-rs/core/src/tools/orchestrator.rs`：approval → sandbox → retry 的完整时序、`already_approved`
  与 `bypass_retry_approval`、retry 的 `{call_id}:retry` run id，D42 依据。
- `codex-rs/core/src/tools/approvals.rs`、`hooks/src/events/permission_request.rs`：`run_id_suffix` 只装饰
  Hook 生命周期事件、不进 Hook stdin；hook 决策优先于 guardian / 用户审批。
- `codex-rs/core/src/tools/handlers/request_permissions.rs`：`request_permissions` 是普通 function tool，
  FunctionCall 落 rollout，D43 识别依据。
- `codex-rs/core/src/tools/runtimes/apply_patch.rs`、`shell.rs`、`unified_exec.rs`：各 runtime 的 approval key 与
  cache 调用位置。
- `codex-rs/core/src/tools/network_approval.rs`：网络审批全链路——Hook payload 构造（`network-access {target}`
  description）、`HostApprovalKey`（host+协议+端口+environment_id）、Hook allow 仅 `AllowOnce` 不写缓存、
  `NetworkPolicyAmendment` 落盘，D39 的识别与差异依据。
- `codex-rs/core/src/mcp_tool_call.rs`：MCP 审批全链路——`McpToolApprovalKey`（server+connector+tool，不含
  参数）、session key 仅 Auto 模式提供、`maybe_persist_mcp_tool_approval` 的分层落盘（用户/项目/插件/
  codex_apps）、ToolCallMcpElicitation 门控，D40 依据。
- `codex-rs/core/src/tools/handlers/mcp.rs`：MCP hook tool_name 的 `mcp__<server>__<tool>` 构造（含
  codex_apps 连接器命名空间），D40 会话键依据。
- `codex-rs/core/src/codex_delegate.rs`：内部 delegated sub-Codex 审批路径及其与普通 `spawn_agent` 的区别。
- `codex-rs/execpolicy/src/execpolicycheck.rs`、`cli/src/main.rs`：`codex execpolicy check` 只读评估 CLI
  （2025-11-20 引入，0.144.4 实测可用），D31 的规则评估引擎。
- `codex-rs/execpolicy/src/amend.rs`：`blocking_append_allow_prefix_rule` 的 default.rules 追加格式
  （文件锁 + 逐行查重），Shell permanent 写入按此格式。
- `codex-rs/shell-command/src/bash.rs`、`command_safety/`：D32/D34 复刻对象与变更频率依据。
- `codex-rs/config/src/loader/mod.rs`、`state.rs`、`config_layer_source.rs`：层叠来源、项目信任判定与
  `ignore_user_and_project_exec_policy_rules`，D33 复刻与存在性检测范围。
- `codex-rs/core/src/guardian/review.rs`：`routes_approval_to_guardian_with_reviewer` 条件，D36 的识别依据。
- `codex-rs/protocol/src/protocol.rs` `TurnContextItem`：含 `approval_policy`、`permission_profile`、
  `approvals_reviewer`，是 Hook 缺失输入的 rollout 补齐来源。
- `codex-rs/utils/absolute-path/src/lib.rs`：`AbsolutePathBuf`“绝对且词法归一、不保证 canonicalize”的
  语义与 `canonicalize_preserving_symlinks` 的逻辑路径偏好，D46 的同构依据。

## 9. 实现者参考（具体契约与挂点）

本节汇总实现所需的具体输入输出形状与现有代码挂点，均已在调研基线版本核实；实现时若发现与装机
Codex 版本不符，按 D24/D35 fail closed。

### 9.1 各类型 Hook 输入的具体形状

Hook stdin 是 `PermissionRequestCommandInput` 的 JSON（字段见 §5.1）。按 `tool_name` 分派：

| 类型 | `tool_name` | `tool_input` | 备注 |
|---|---|---|---|
| Shell / unified exec | `"Bash"` | `{"command": "<脚本原文>", "description": "<模型 justification，可缺省>"}` | matcher alias 无 |
| 文件编辑 | `"apply_patch"` | `{"command": "<patch 原文>"}` | matcher aliases `Write`/`Edit`（只影响 hooks.json matcher，stdin 中 tool_name 恒为 `apply_patch`）；文件路径需自行解析 patch 文本（`*** Add/Update/Delete File:` 行 + `move to`），相对路径按 Hook `cwd` 词法绝对化（D46） |
| network 放行 | `"Bash"` | `{"command": "<归属 shell 命令或 network-access {target}>", "description": "network-access {proto}://{host}:{port}"}` | 判真三条件见 D39；`{target}` = `format_network_target`，host 已小写 |
| MCP 工具 | `"mcp__<server>__<tool>"` | 工具实参对象（无实参时为 `{}`） | codex_apps 连接器的 namespace 已含连接器名；会话键取整个 tool_name 字符串（D40） |
| spawn_agent 等其它 | `"spawn_agent"` 等 | 各自形状 | 不在记忆范围，走基础弹窗 |

### 9.2 `codex execpolicy check` CLI 契约（D31 评估引擎）

```text
codex execpolicy check --rules <path> [--rules <path> ...] [--resolve-host-executables] -- <token> [token ...]
```

- 输出单行 JSON：`{"matchedRules": [RuleMatch...], "decision": "allow"|"prompt"|"forbidden"?}`；
  `decision` 是所有命中规则的最严值（max），无命中时字段缺省——**缺省即未命中，不是 allow**。
- 评估时 `heuristics_fallback = None`，正合我们需要（heuristics 由 AskHuman 侧按 D34/D42 单独处理）。
- 任一规则文件读取/解析失败即非零退出 → fail closed。
- 每个拆分段单独一次调用（或逐段传入多次调用）；D42 的 auto-allow 要求**每段**的 matchedRules 中存在
  policy 来源、`decision == allow` 的命中（对应 Codex `bypass_sandbox` 判定），仅 `decision` 字段为
  allow 但依赖 heuristics 的组合不成立（该 CLI 不含 heuristics，故天然满足）。
- 二进制定位：Hook 进程父链中的 codex 可执行文件（D31）；`codex --version` 用于 D34/D35 版本下限判定。

### 9.3 `default.rules` 写入契约（Shell / network 永久）

- 路径：`$CODEX_HOME/rules/default.rules`（默认 `~/.codex/rules/default.rules`）。
- 机器格式（必须逐字符一致，含空格；这也是 D39 network 解析白名单的依据）：
  - `prefix_rule(pattern=["curl"], decision="allow")`（token 为 JSON 字符串序列化，`, ` 分隔）
  - `network_rule(host="api.github.com", protocol="https", decision="allow", justification="...")`
    （justification 可省；host 经 `normalize_network_rule_host` 小写化、拒绝通配符）
- 写入语义与原生 `append_locked_line` 相同：先 `create_dir` rules 目录，advisory 文件锁（flock）内
  逐行查重后追加；AskHuman 写入端必须同样持锁，避免与并发的 Codex 原生写入互踩。
- network 协议取值：`http` / `https` / `socks5-tcp` / `socks5-udp`（与 D39 判真的合法值一致）。

### 9.4 rollout（transcript）读取要点

- 路径来自 Hook stdin 的 `transcript_path`（可为 null → 放弃记忆增强）。JSONL，每行一个 item。
- `TurnContextItem`：取**当前 turn**（按 Hook stdin `turn_id` 匹配）最近一条；需要字段
  `approval_policy`、`permission_profile`、`approvals_reviewer`（D36）。缺失/旧 schema → 放弃增强（D23）。
- owner 调用：按 `turn_id` + 参数中的命令原文关联（D44）。两种形态：`function_call`
  （`arguments` 是 JSON 字符串，内含 `command`/`cmd`、`justification`、`prefix_rule`、
  `sandbox_permissions`）；code_mode 的 `custom_tool_call`（`name="exec"`，从 `input` JS 源码
  严格逐调用抽取 `tools.exec_command({...})` 的参数对象，见 D44）。多条 owner 字段不一致时
  逐字段保守合成（D44 细化），不再全盘放弃。
- D43 扫描：当前 turn 内 `name == "request_permissions"` 的 FunctionCall 存在即 fail closed；
  code_mode cell 源码含 `request_permissions` 子串同样计为风险。
- 读取只做只读顺序扫描 + 尾部窗口即可（当前 turn 的条目在文件尾部）；持续 I/O 失败按 §5.6 回基础弹窗。

### 9.5 HumanInLoop 现有挂点

- `src-tauri/src/permissions.rs`：基础确认适配器（第一故障域），记忆增强作为其上的可失败层（§5.6）。
- `src-tauri/src/permission_diff/worker.rs`：隔离 worker 既有模式（子进程、限长 stdin/stdout、
  `kill_on_drop`、硬超时），D27/D45 的 Shell 判断 worker 按此复制。
- `src-tauri/src/integrations/mcp_config.rs`：Codex `config.toml` 保格式编辑通道，D40 的
  `approval_mode="approve"` 写入复用。
- `src-tauri/src/integrations/agent_permission.rs`：PermissionRequest Hook 的安装与状态，新增能力不改变
  hooks.json 契约（仍是同一个 `__permission-hook` 入口）。
- rule store 新增于 daemon 数据目录（§6.2 存储基线）；设置页管理面板挂在“高级”Tab 最后一张卡（§6.3）。

### 9.6 建议实现顺序（依赖关系）

1. **rule store + 会话级文件编辑**（D9–D13/D46）：纯 shadow，无 Codex 复刻依赖，先打通存储、匹配、
   30 天清理、两阶段提交（§5.6）与管理面板（§6.3）。
2. **MCP**（D40/D41）：会话键最简单（tool_name 字符串），永久级复用 mcp_config 编辑；带出 D41 的
   跨会话 namespace 与面板“跨会话授权”分组。
3. **network**（D39）：判真三条件 + 主机规则 + `network_rule` 写入（写入通道与 Shell 共用，可先行）。
4. **Shell**（D28–D35、D38、D42–D45）：最重，依赖 worker、execpolicy check 集成、脚本拆分/层叠/
   heuristics 复刻与版本门控；其 auto-allow 防护（D36/D43 的 rollout 读取）在此阶段全量启用，但
   rollout 读取组件可在阶段 1–3 先以“读不到就不给选项”的形态接入。
5. 全类型接入后按 §6.4 在 `docs/PROGRESS.md` 登记上游同步纪律。
