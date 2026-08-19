# Agent 生命周期追踪归入自动集成

> 状态：实现完成并通过本地验证（2026-08-19）
> 关联规格：`docs/specs/agent-lifecycle-tracking.md`、`docs/specs/agent-stop-confirmation.md`、
> `docs/specs/gui-agent-task-launch.md`、`docs/specs/im-agent-task-launch.md`
> 影响范围：五家 Agent 自动集成、生命周期 Hook / Pi Extension、Stop 共用 handler、设置页、
> CLI、Agent 任务 readiness、托盘更新提示与相关文案。

## 1. 背景与问题

当前产品把两类状态分开管理：

- `agent_mode` 管理自动集成模式及其 Rules / Skill、CLI runtime、MCP、上下文恢复、权限和 Stop；
- `agent_lifecycle` 在「设置 → 高级」中提供五个独立开关，磁盘安装状态本身就是开关真值。

因此用户完成 CLI/MCP 自动集成后，还要去另一个 Tab 单独开启生命周期追踪。未开启时，普通
AskHuman 调用仍可工作，但 Agent 状态、插话、Watch、任务启动 readiness 等能力处于半就绪状态。
生命周期已经是稳定的跨平台基础能力，不应继续以独立实验设置呈现。

本计划把 lifecycle 变成**自动集成拥有的可选 capability**：启用一个 Agent 的自动集成时默认
开启，在该 Agent 卡内允许关闭；关闭自动集成时必须移除 lifecycle 产物。生命周期不再能脱离自动
集成单独存在。

## 2. 已确认决策

| 编号 | 决策 | 结论 |
| --- | --- | --- |
| D1 | UI 归属 | lifecycle 从「高级」移入五张 Agent 自动集成卡，不再有独立总卡片。 |
| D2 | 新集成默认 | 首次选择 CLI/MCP 时 lifecycle 默认开启并立即安装。 |
| D3 | 用户控制 | lifecycle 仍有 per-Agent 开关；用户明确关闭后，更新、CLI↔MCP 切换和版本升级均尊重关闭。 |
| D4 | 未集成语义 | `mode=None` 时 lifecycle 实际产物必须不存在；切换到「未集成」立即卸载。UI 文案仍为「未集成」。 |
| D5 | 偏好保留 | 切到 None 时保留 lifecycle 偏好，仅卸载实际产物；以后重新集成时恢复用户之前的 on/off 选择。无历史偏好才使用默认 on。 |
| D6 | 旧集成补齐 | 已集成但缺 lifecycle 的旧用户不后台安装；显示“需更新”，用户点击更新后才安装。 |
| D7 | 旧孤立状态 | 旧版 `mode=None + lifecycle installed` 不静默删除；显示待清理更新，用户点击后卸载。 |
| D8 | 更新策略 | lifecycle 缺失、过期或应删除的漂移纳入现有 Hook 更新提示、单项更新和“全部更新”；取消 lifecycle 的 daemon 后台自动升级。 |
| D9 | 手动集成 | 手动提示词/MCP 示例继续保留，但不提供脱离自动集成的 AskHuman lifecycle；CLI 也不得创建 None + lifecycle。 |
| D10 | Stop | Stop 偏好仍独立；已集成时共享 Stop handler 按 lifecycle/Stop 两个偏好组合 `track` / `confirm`，None 时 handler 与 lifecycle 一并清理，Stop 偏好保留。 |
| D11 | readiness | `CLI / Lifecycle / Integration` 三项继续分别展示；Lifecycle × 改为跳转同一 Agent 集成卡。 |
| D12 | 模式识别 | `agent_mode::current()` 仍只由 Rules / CLI runtime / MCP 判断，绝不把遗留 lifecycle 当作自动集成模式信号。 |

## 3. 目标状态模型

### 3.1 两层状态

每家 Agent 同时具有：

1. 自动集成模式：`None | Cli | Mcp`（Grok 为 `None | Mcp`，Pi 为 `None | Cli`）；
2. lifecycle 偏好：`Option<bool>`，其中 `None` 表示尚未固化新语义，`Some(false)` 才表示用户明确关闭。

实际安装意图：

```text
lifecycle_desired = mode != None && lifecycle_preference.unwrap_or(true)
```

但只读状态查询不能因为 `None → true` 就写盘或自动安装。未固化偏好只用于计算默认 UI 和更新
提示；只有用户选择模式、切换开关或点击更新时才写入偏好并变更 Agent 配置。

建议新增 `~/.askhuman/lifecycle-preferences.json`，结构与 Stop/Permission 偏好文件一致：

```json
{
  "claude": true,
  "codex": false,
  "cursor": null,
  "grok": true,
  "pi": true
}
```

缺字段与显式 `null` 都按 `None` 处理。写入须复用 integrations mutation lock 与原子写；切换
失败时回滚偏好，不能出现 UI 偏好已变但磁盘 handler 未 reconcile 的状态。

### 3.2 真值与迁移矩阵

| mode | 偏好 | 实际 lifecycle | 状态 / 用户动作结果 |
| --- | --- | --- | --- |
| None | 任意 | 未安装 | 正常；偏好只为以后重新集成保留。 |
| None | 任意 | 已安装 | 旧孤立状态；显示待清理，不后台删除；点更新后卸载。 |
| Cli/Mcp | None | 未安装 | 旧集成尚未采用新默认；UI 默认开但显示未配置/需更新。 |
| Cli/Mcp | None | 已安装且最新 | 视为默认开启且正常；后续写操作固化为 `true`。 |
| Cli/Mcp | None | 已安装但过期 | 显示需更新；点更新后固化 `true` 并重装。 |
| Cli/Mcp | true | 未安装或过期 | 漂移；显示需更新，点更新后 reconcile。 |
| Cli/Mcp | true | 已安装且最新 | 正常。 |
| Cli/Mcp | false | 未安装 | 用户明确关闭，正常且不提示更新。 |
| Cli/Mcp | false | 已安装 | 漂移；显示需清理，点更新或再次关闭后卸载。 |

关键行为：

- 从 None 首次进入 Cli/Mcp：若偏好缺失，写 `true` 并安装；若已有显式偏好，尊重其值。
- Cli↔Mcp：保留偏好并 reconcile，不重新套用默认值。
- 从 Cli/Mcp 进入 None：立即卸载 lifecycle，保留偏好。
- `agent_mode::update()`：用户已主动点击更新，缺失偏好在 active mode 下固化为 `true`；显式
  `false` 只确保 lifecycle 不存在。
- 只读 status、doctor、托盘刷新不得写盘。

## 4. 后端改造

### 4.1 `agent_lifecycle` 增加偏好与 reconcile

在 `src-tauri/src/integrations/agent_lifecycle.rs` 中保留现有底层 Hook / Extension 安装器，并在其上
增加产品语义层：

- `preference(kind) -> Option<bool>`；
- `effective_enabled(kind, mode) -> bool`；
- `set_enabled(kind, bool)`：仅允许 active mode；保存偏好后安装/卸载，失败回滚；
- `reconcile_unlocked(kind, mode, adopt_default)`：由 mode set/update 调用；
- `status_for_mode(kind, mode)`：返回偏好、实际安装、过期和需更新；
- `target_for_kind` / `kind_for_target` 统一映射，避免五家 match 在多个模块漂移。

现有 `install_unlocked` / `uninstall_unlocked` 继续只做磁盘操作，不自行改变产品偏好。None 下的
`set_enabled(true)` 必须报错并引导先启用自动集成；`false` 可以幂等清理残留并记录显式关闭。

### 4.2 `agent_mode` 接管生命周期编排

`agent_mode::set_unlocked()` 在三条模式分支中统一 reconcile lifecycle：

- Cli/Mcp：完成目标模式核心产物后，根据 preference/default 安装或卸载 lifecycle，再 reconcile
  Permission、Stop 和 AskQuestion；
- None：卸载 lifecycle 后再以 None reconcile Stop，共享 Stop handler 必须最终不存在；
- Pi：同一 Extension 的 `cli/lifecycle/stop` 位在同一 mutation lock 内依次收敛，空配置时删除文件。

`artifact_updates()` 的 Hook 分类纳入 lifecycle：

- active mode：默认/偏好为 on 时，缺失或过期为 update；偏好 off 但仍安装也为 cleanup update；
- None：任何已安装 lifecycle 都是 cleanup update；
- 不改变 `current()`，避免旧 lifecycle 把 None 错判为 Cli/Mcp。

`update_artifact(Hook)` 必须 reconcile lifecycle，无论当前是 CLI 还是 MCP；`update()` /
“全部更新”经现有整包路径自然覆盖。单项 Rule/MCP 更新不应偷偷改变 lifecycle。

### 4.3 取消后台自动升级

删除 daemon 启动时对 `agent_lifecycle::migrate_outdated()` 的调用。保留状态检测，让托盘和设置页的
更新计数显示缺失/过期/待清理。Stop 与 AskQuestion 的既有迁移不在本需求顺带改变。

不得在 daemon 启动、status 查询、设置页加载时自动执行以下动作：

- 给旧集成补装 lifecycle；
- 升级已有 lifecycle Hook；
- 删除 None 模式下的旧孤立 lifecycle。

这些动作统一发生在用户选择模式、切换 lifecycle 或点击更新之后。

### 4.4 Stop 共用 handler

生命周期默认开启不改变 Stop 的独立偏好。目标组合为：

| mode | lifecycle 偏好 | Stop 偏好 | handler |
| --- | --- | --- | --- |
| None | 任意 | 任意 | 无；两项偏好均可保留。 |
| Active | off | off | 无。 |
| Active | on | off | `track`。 |
| Active | off | on | `confirm`。 |
| Active | on | on | `track + confirm`。 |

Claude/Codex/Cursor 继续共用一个 AskHuman Stop handler；Pi 继续由同一 Extension 配置两项能力；
Grok 不支持 Stop，只管理 lifecycle。

## 5. 前端改造

### 5.1 Agent 集成卡

`AgentModeStatus` 墕加 lifecycle 聚合状态，至少包含：

- `supported`；
- `enabled`（active mode 下的有效偏好，缺失时为默认 true）；
- `preferenceConfigured`；
- `installed`；
- `outdated`；
- `needsUpdate`。

在 `IntegrationTab.vue` 每张 active Agent 卡中增加“生命周期追踪”行：

- 实际产物徽标显示“已配置 / 未配置”；
- 开关绑定 `enabled`，不是直接绑定 `installed`；
- 缺失/过期时显示“更新”按钮；
- 提示说明它用于 Agent 状态、插话、Watch 和任务 readiness，自动集成首次启用时默认开启；
- Pi 仍显示为同一个 Extension capability，不虚构第二个文件。

None 模式不展示普通 lifecycle 开关。若检测到旧孤立产物，则展示专门的警告行“旧版生命周期追踪
待清理”与更新按钮，确保 D7 的提示可见，而不是只在顶部更新计数中出现一个无法定位的数字。

删除 `AdvancedTab.vue` 的生命周期整卡与 `useLifecycleSettings.ts`；把 toggle/refresh 逻辑并入
`useIntegration.ts`。`SettingsView` 初始化只需 `initIntegration()`，不再独立循环查询五家 lifecycle。

### 5.2 搜索、锚点与任务 readiness

- 设置搜索中的 lifecycle 结果从 `advanced` 改到 `integration`；
- 每行保留 `lifecycle-<kind>` 锚点并嵌在 `integration-<kind>` 卡内；
- `useAgentTasks.ts` 与 `NewTaskForm.vue` 的 Lifecycle × 都打开
  `integration#lifecycle-<kind>`；
- readiness 仍显示 `CLI / Lifecycle / Integration`，因为用户可在 active mode 中明确关闭
  lifecycle；
- `integration_ready` 继续表示传输集成本身，`ready` 仍为三项与，避免把诊断粒度合并丢失。

### 5.3 文案收口

同步中英文：

- 删除“在高级页单独开启”“实验性 lifecycle”等描述；
- IM 按需发送提示改为“Agent 集成默认开启 lifecycle；关闭后工作/空闲识别会受影响”；
- Agent 状态空态改为“只有已集成且开启生命周期追踪的 Agent 启动后才会显示”；
- IM `/status` 空态引导到“设置 → Agents → 对应 Agent 卡”；
- 手动集成区明确：手动提示词/MCP 示例不包含 AskHuman 管理的 lifecycle；需要状态追踪时使用自动集成；
- Pi Extension 说明继续强调同一文件承载 CLI、lifecycle、恢复、插话与 Stop，不把 capability 行误写成
  独立 Extension。

## 6. CLI、doctor 与清理

### 6.1 `agents lifecycle`

保留命令，作为 active 自动集成内 capability 的 headless 开关：

```text
AskHuman agents lifecycle <agent> [on|off]
```

- active mode：on/off 写偏好并 reconcile；
- None + on：非零退出，提示先运行 `agents mode <agent> <cli|mcp>`；
- None + off：幂等记录 off 并清理旧残留；
- 查询输出同时区分 preference 与实际配置/需更新，不再称“实验性 Hook”。

`agents show` 把 lifecycle 列入当前自动集成 capability；`agents mode ... none` 与
`agents cleanup` 不再额外调用独立 uninstall，统一由 `agent_mode` 完成。legacy 命令迁移提示删除
“independent lifecycle”措辞。

### 6.2 doctor

doctor 的生命周期字段保留，因为它仍是重要诊断面，但应补充：

- `enabled` / `preferenceConfigured`；
- `installed` / `needsUpdate`；
- None 下遗留产物明确标记 cleanup，而不是看起来像正常独立功能。

JSON 字段只增量增加，不删除现有 `lifecycle.installed/needsUpdate/supported`，避免破坏脚本。

## 7. 文档更新边界

实现时同步以下现行规格：

- `docs/specs/agent-lifecycle-tracking.md`：重写 D15/D16、设置入口、默认/关闭/迁移语义；
- `docs/specs/agent-stop-confirmation.md`：删除 None + track-only 的现行产品组合，更新共享 handler 矩阵；
- `docs/specs/gui-agent-task-launch.md` 与 `docs/specs/im-agent-task-launch.md`：更新锚点与 readiness 修复入口；
- `docs/plans/pi-agent-integration.md`：补充 lifecycle preference 与 Extension reconcile；
- `docs/overview.md`：设置命令与前端结构不再描述独立 lifecycle UI。

历史计划中记录当时决策的段落不做全量改写；只更新仍被作为现行契约引用的文档，必要处加“已由本计划
替代”的注记。

## 8. 测试计划

### 8.1 Rust 单元 / 集成测试

- lifecycle preference：缺字段、true、false、损坏文件、原子写和失败回滚；
- §3.2 全真值矩阵；
- None→Cli/Mcp 默认 on，显式 off 后的 mode 切换与 update 不复开；
- active 旧集成缺 lifecycle 只报 update，不在 status/daemon 启动时写盘；
- None 旧孤立 lifecycle 只报 cleanup，点 update 后卸载；
- `current()` 不受 lifecycle 孤立产物影响；
- Hook artifact update 在 CLI/MCP/None 三态按目标 reconcile；
- Stop `mode × lifecycle × stop` handler 矩阵，保留外部 handler 和 Codex trusted hash；
- Pi Extension 的 `cli/lifecycle/stop` 组合、偏好保留和空配置删除；
- CLI None + lifecycle on 失败、off 清理、active on/off 成功；
- daemon 启动不再自动迁移 lifecycle。

### 8.2 前端测试

- active Agent 卡显示 lifecycle 行，None 正常状态不显示；
- 新集成默认开，显式关闭后 refresh 保持关闭；
- 缺失/过期显示更新，None 遗留显示清理警告；
- “全部更新”统计包含 lifecycle；
- Lifecycle readiness 链接进入 Agents Tab 对应锚点；
- 设置搜索 lifecycle 命中 Agents Tab；
- 中英文不回显翻译 key。

### 8.3 端到端验证

按项目规范运行 `./scripts/install.sh` 后，用新安装二进制验证五家：

1. None→推荐模式：lifecycle 默认安装；
2. 关闭 lifecycle：普通 AskHuman 集成仍工作，Agent 不再登记，readiness 显示 Lifecycle ×；
3. CLI↔MCP / update：显式 off 不复开；
4. 重新开启：Agent 状态、插话、Watch 恢复；
5. 切 None：lifecycle 产物删除，Stop/Permission 等按各自既有语义收敛；
6. 构造旧 active 缺失与 None 孤立状态，确认只提示、点击更新后才写盘；
7. Pi 实测同一 Extension 的三项 flag，无重复 Extension 或残留空文件；
8. macOS、Linux、Windows CI 覆盖 JSON/TOML/PowerShell 路径与构建。

## 9. 实施顺序

1. 新增 preference 数据层与纯状态矩阵测试；
2. 把 lifecycle reconcile 接入 `agent_mode`、artifact update 与 Stop/Pi 共享产物；
3. 取消 daemon 自动 lifecycle migration，更新 CLI/doctor/cleanup；
4. 扩展 `AgentModeStatus`，把 UI 开关移入 Agent 卡并删除高级页独立状态域；
5. 更新 readiness 导航、搜索与全部相关提示；
6. 更新现行 spec/overview，运行完整测试、安装与五家针对性 E2E。

每一步都必须保持外部 Hook/配置格式保留编辑、stdout 洁净契约与现有 CLI/MCP AskHuman 调用不变。

## 10. 完成标准

- 新启用任一家自动集成时 lifecycle 默认安装；
- 用户可在该 Agent 卡中关闭，任何更新/模式切换都不擅自复开；
- None 模式最终不存在 AskHuman lifecycle 产物；旧孤立状态只提示、用户确认更新后才清理；
- 旧 active 缺失 lifecycle 只提示，点击更新后补装；
- 高级页不再出现独立 lifecycle 卡，所有链接和提示都指向 Agents Tab；
- Stop 共用 handler、Pi Extension、任务 readiness、托盘更新计数和 CLI/doctor 状态一致；
- 自动化测试、`./scripts/install.sh` 与三平台 CI 通过，现行文档无旧入口或独立开关描述。

## 11. 实施记录（2026-08-19）

已按本计划完成：

- 新增 `~/.askhuman/lifecycle-preferences.json` 的五家 `Option<bool>` 偏好；只读状态不写盘，active
  mode 缺偏好按默认 on 计算，显式更新/模式切换时固化；
- `agent_mode` 在 CLI/MCP/None、Hook 单项更新与整包更新中统一 reconcile lifecycle；None 清理实际
  产物并保留偏好，`current()` 仍只读取核心集成产物；
- lifecycle 漂移纳入 Hook 更新计数并取消其独立 daemon 启动迁移；Stop 与 AskQuestion 原有自动迁移
  职责保持不变；
- 高级页独立卡和 `useLifecycleSettings.ts` 已删除，五张 active Agent 卡内显示 capability；None 旧
  孤立状态显示显式清理提示；readiness、搜索、设置锚点、CLI/doctor 与中英文提示已同步；
- Pi 继续由单个 Extension 原子承载 `cli/lifecycle/stop` 当前状态，持久 lifecycle 偏好移到统一偏好文件。

验证结果：

- `cargo test --manifest-path src-tauri/Cargo.toml`：1155 passed、2 ignored；首次沙箱运行的 6 个回环
  HTTP mock 因 `Operation not permitted` 失败，沙箱外复跑全部通过；
- `cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings` 通过；
- `npm run build` 通过；`npm test` 为 27 个 Vitest 文件 / 172 tests + 5 Node tests 全通过；
- `./scripts/install.sh` 成功安装并签名 `/Users/wutian/.local/bin/AskHuman`；新二进制现场查询显示五家
  integration/lifecycle/readiness 全部一致，Pi 为 CLI、lifecycle on/configured、版本 0.84.2；
- 本机额外尝试 `x86_64-pc-windows-msvc` 交叉 `cargo check`；Rust 依赖已进入编译，但当前 macOS
  环境未安装 Windows C SDK，`ring` 编译因缺少 `assert.h` 停止，因此不能把该次尝试记为 Windows
  构建证据；
- 本轮按约定仅本地提交，不推送，因此未触发新的三平台 CI；既有 Pi 集成跨平台 CI 证据仍见
  `docs/plans/pi-agent-integration.md`。
