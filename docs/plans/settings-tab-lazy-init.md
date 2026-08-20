# 计划：设置页按 Tab 异步加载，去掉首屏阻塞

> 状态：已实施（2026-08-20）
>
> 关联：`docs/plans/pi-agent-integration.md`、`docs/plans/agent-lifecycle-integration-binding.md`、
> `src/views/SettingsView.vue`、`src/views/settings/useIntegration.ts`、
> `src/views/settings/useAgentTasks.ts`、`src-tauri/src/commands.rs`、
> `src-tauri/src/integrations/agent_launch.rs`

## 1. 背景

设置窗口挂载时，无论当前打开哪个 Tab，都会在 `onMounted` 里按顺序 `await`：

1. `initIntegration()`：并行查询五家 `agent_mode_status`；
2. Pi 的 `agent_mode_status` 是**同步** Tauri command，内部却调用完整 `readiness(pi)`：
   login shell 定位 `pi`，再 `pi --version`，并用阻塞轮询等子进程；
3. `initGeneral()`：历史条数、提示音、Liquid Glass；
4. `refreshAgentTaskSettings(false)`：再跑一遍五家 `agent_task_readiness()`；
5. `initAbout()`：版本号 + 静默检查更新。

本机实测：`pi --version` 约 1.09s，五家 readiness 约 1.45s；Pi 探测会被跑两次，串行预算约 2.8s。
第一轮发生在同步 command 里，体感是窗口「卡住」。Git 归因：同步 `agent_mode_status` 早已存在，
但 `1b1e74d` 首次把 `readiness(pi)` 塞进去；lifecycle 状态读取不是秒级来源。

渠道健康已经按「打开渠道 Tab 再拉」工作，本轮对齐其它耗时路径。

## 2. 目标与非目标

### 2.1 目标

- 设置窗口在 `get_settings` 返回后即可交互，不再被 Agent CLI 探测或 readiness 挡住。
- 耗时工作异步执行，对应区域有 loading，禁止整页空白或把默认 `mode: "none"` 闪成「未集成」。
- 按 Tab 启动该 Tab 真正需要的重活；集成页签红点是唯一例外（见 §3）。
- `agent_mode_status` 只读本地集成产物，不再启动任何 Agent CLI。
- Pi 版本只由 readiness 探测一次；可执行路径 / 版本 60s TTL 缓存；集成卡与高级页复用同一结果。
- 高级页「刷新」强制失效缓存。

### 2.2 非目标

- 不改 IM `/new`、doctor、fork 的就绪语义，只让它们享受路径/版本缓存（与现有
  `fork_readiness` 60s 缓存同一量级）。
- 不预取未打开 Tab 的五家 CLI readiness。
- 不改集成产物的安装/更新/卸载契约，不改 Pi 最低版本 `0.82.0`。
- 不做设置窗口骨架屏、不做前端 bundle 再拆分。
- 不把 `get_settings` 本身改成懒加载（首屏仍需要 config）。

## 3. 已确认决策

1. **集成页签红点**：设置窗口打开后**后台**拉五家本地 `agent_mode_status`（不 `await`、不挡页面），
   红点可以在用户尚未点开「集成」时出现。
2. **打开「集成」时的后端范围**：只确保本地集成状态 + **Pi 一家**的版本探测。五家 CLI
   readiness 留给「高级」页。
3. **缓存**：可执行路径 / Pi 版本 60s TTL，与现有 `fork_readiness` 一致；高级页「刷新」
   （当前 `refreshAgentTaskSettings(true)`）强制失效。
4. **启动粒度是设置 Tab**，不是整个设置窗口。打开对应 Tab 再启动该 Tab 的重活。
5. **Loading 在对应区域**，不是整页遮罩。

## 4. 目标调度

| 时机 | 启动 | 不启动 |
| --- | --- | --- |
| 窗口 `get_settings` 完成 | 后台 `initIntegration()`（本地五家状态 + 提示词 / MCP 路径 / 协作风格默认文案） | 不 `await`；不跑五家 readiness；不跑 `pi --version` |
| 当前 Tab 是「通用」（含默认打开） | `initGeneral()`、`initAbout()` | — |
| 当前 Tab 是「集成」 | 若本地状态还在飞，卡片 loading；另外异步拉 **Pi** readiness，把版本叠到 Pi 卡 | 不拉其它四家 CLI |
| 当前 Tab 是「高级」 | `refreshAgentTaskSettings(false)`：工作目录索引 + 五家 readiness（走缓存） | 不冷扫工作目录（仍只在「管理工作目录」面板） |
| 当前 Tab 是「渠道」 | 保持现状：打开再拉 `channelHealth` 并 10s 轮询 | — |
| 搜索跳转 / `?tab=#锚点` / `settings-goto-tab` | 先切 Tab 并启动该 Tab 的 ensure，数据就绪后再滚动高亮 | 不在挂载时预跑全部 Tab |
| 集成卡上改模式 / lifecycle / hook | 刷新该 Agent 的本地状态；若高级页已加载过 readiness，用缓存重算文件侧字段。不因此触发五家 CLI 重探 | 除非用户点高级页「刷新」 |

`ensure*` 必须幂等且合并 in-flight：同一窗口内同一种任务只跑一趟，第二次 await 同一 Promise。

## 5. 后端

### 5.1 `agent_mode_status` 恢复为本地快照

`commands.rs` 的 `agent_mode_status`：

- 删除 `agent_launch::readiness(stop_kind)`。
- `agent_version` / `minimum_version` 固定为 `None`。
- `version_supported` 固定为 `true`（表示「本 command 不再判断版本」；未探测完成时前端不得把
  它当成「版本合格」来隐藏 loading，见 §6.2）。
- `runtime_artifact_kind` 仍按 Agent 种类返回 `"extension"` / `"hook"`（纯本地）。
- 改为 `async` + `spawn_blocking`，避免五家并行文件 IO 占住 invoke 线程。

集成卡上的 Pi 版本文案改为消费 readiness 叠上去的字段，不再相信 `agent_mode_status` 的版本三元组。

### 5.2 路径 / 版本缓存

在 `agent_launch.rs` 抽出 `binary_probe(kind, force) -> { executable, pi_version }`：

- key = `AgentKind`；
- TTL = 60s；
- `force` 时跳过缓存并覆盖写入；
- 只缓存「login shell 定位 + Pi `--version`」，**不**缓存 lifecycle / 集成文件状态
  （那些必须跟设置开关即时一致）；
- 同 kind 并发 miss 做 single-flight，避免集成页 Pi 探测与高级页五家探测重叠时跑两次
  `pi --version`；
- `readiness(kind)` 走缓存；新增 `readiness_fresh(kind)` / `all_readiness_fresh()` 给强制刷新；
- `fork_readiness` 继续用自己的 60s 缓存，但其内部调用的 `readiness(kind)` 会命中本缓存。

`agent_task_readiness` 增加可选参数：

```text
agent_task_readiness({ kind?: AgentKind, force?: bool })
```

- 都不传：五家、走缓存（现有调用点保持兼容）；
- `kind: "pi"`：只探 Pi；
- `force: true`：走 `*_fresh`。

前端现有 `agentTaskReadiness()` 无参调用仍然合法。

### 5.3 其它调用方

`doctor`、IM `/new`、daemon inbound 继续调用 `all_readiness()` / `readiness(kind)`。
它们自动享受路径缓存，语义不变。本轮不为这些入口加 `force`。

## 6. 前端

### 6.1 `SettingsView` 挂载

`onMounted` 在写入 `config`、挂 listener 之后：

1. `void ensureIntegration()`（后台，为页签红点）；
2. `void ensureTabData(activeTab)`（当前 Tab 的重活）；
3. 若 URL 带 `#锚点`，`await` 该 Tab 的 ensure 再 `gotoSettingsTarget`。

禁止再 `await initIntegration(); await initGeneral(); await refreshAgentTaskSettings(); await initAbout()`。

Tab 点击、`settings-goto-tab`、搜索跳转都走同一 `ensureTabData`：

- `general` → `ensureGeneral()` + `ensureAbout()`
- `integration` → `ensureIntegration()` + `ensurePiVersion()`
- `advanced` → `ensureAgentTaskSettings(false)`
- `channel` / `experimental` → 无新增（渠道已有 watch）

切到「高级」时保留现有「每次可见都刷新 readiness」行为，但走缓存，所以再点开不应再卡 1s+。

### 6.2 集成 Tab

- `integrationLoading`：本地五家状态尚未回来时，自动集成区渲染一张 loading 卡
  （复用 `permission-state-spinner` + `common.loading`），**不**渲染默认 `emptyMode()` 卡片。
- `ensurePiVersion()`：
  - 若 `taskReadiness` 里已有 `pi`，直接 overlay；
  - 否则 `agentTaskReadiness({ kind: "pi" })`，把结果写入 `taskReadiness` 的 pi 槽
    （不补齐另外四家），再 overlay。
- overlay 只改 Pi 的 `agentVersion` / `minimumVersion`（恒为 `"0.82.0"`）/ `versionSupported`
  （`binary_ready`）。
- overlay 完成前，Pi 卡不展示「版本过低 / 未找到」错误，可在版本行显示一小段 loading。
- 协作风格、参考提示词、MCP 示例与本地状态并行拉取；提示词区短暂空白可接受。

页签红点仍绑定现有 `updateSummary`。本地状态后台返回后红点出现；在此之前没有红点。

### 6.3 高级 Tab

- `taskSettingsBusy && taskReadiness.length === 0` 时，就绪列表位置显示 loading，不渲染空列表。
- 「刷新」改为 `ensureAgentTaskSettings(true)`：工作目录冷扫描 + `force: true` 的五家 readiness。
- 工作目录条数在 workspaces 返回前可显示占位或沿用 0 + loading，避免「已保存 0 个」闪一下。

### 6.4 通用 Tab

`initAbout` 已有「检查中…」，且 `checkUpdate` 本身不阻塞 `initAbout` 的版本号。
本轮只是把它从「任何 Tab 挂载都启动」收成「通用 Tab 可见才启动」。
`?tab=channel` 打开设置时不打更新检查。

## 7. 测试

### 7.1 Rust

- `agent_mode_status`（或抽出的纯函数）对 Pi 的版本三元组为 `None` / `None` / `true`，
  且测试夹具证明它不调用 `binary_probe` / 不增加 probe 计数。
- probe 缓存：写入测试快照后，非 force 第二次 `binary_probe` 复用；`force` 后覆盖。
- single-flight：并发两次 `readiness(Pi)` 只触发一次真实探测（可用测试计数器或 mock）。
- 缓存失效：TTL 内 force 必重探；不要求真的 sleep 60s（测 `force` 与手动过期时间戳即可）。

### 7.2 前端

- `IntegrationTab`：`integrationLoading` 时没有 `#integration-*` 卡片；loading 结束后才有。
- Pi 版本：`versionSupported === false` 的既有文案回归保留；新增「版本探测中不展示错误」。
- 抽出 `ensureTabData` 的调用约定单测（或轻量 mount `SettingsView` 并 mock ipc）：
  - 挂载在默认 `general`：会调 `agent_mode_status`，**不会**调无参 `agent_task_readiness`；
  - 切到 `integration`：会调 `agent_task_readiness` 且带 `kind: "pi"`；
  - 切到 `advanced`：会调无 `kind` 的 `agent_task_readiness`；
  - 高级「刷新」带 `force: true`。

## 8. 文档

- 本文件为实施计划；不另开 spec（产品决策已记在 §3）。
- `docs/overview.md` 前端命令列表里补一句：`agent_mode_status` 只聚合本地集成产物；
  Agent CLI 是否在 PATH / Pi 版本由 `agent_task_readiness` 负责。
- 不改 `docs/PROGRESS.md`（本轮做完即删，无需跨会话挂账）。

## 9. 验证

1. 前端测试 + 相关 Rust 测试。
2. `./scripts/install.sh`。
3. 实机打开设置（默认通用 Tab）：窗口应马上可点 Tab / 改主题；集成页签红点稍后出现；
   点「集成」Pi 卡若需探测则有 loading，整页不卡；点「高级」就绪列表先 loading 再出结果。
4. 再点一次「高级」应明显快于第一次（缓存）。点「刷新」应重新探测。
5. `?tab=integration#lifecycle-pi`：等集成状态回来后再滚动高亮，不提前滚到空卡。

## 10. 实施顺序

1. 后端：拆 `binary_probe` 缓存 + `agent_mode_status` 去探测并改为 async + readiness 可选
   `kind`/`force`。
2. 前端：`ensure*` 调度、去掉挂载期整串 `await`、loading UI、Pi overlay。
3. 测试与 overview 一句。
4. `./scripts/install.sh` 后按 §9 点一遍。

## 11. 风险

- 红点比现在晚几百毫秒出现：可接受（本地文件状态，不再含 CLI）。
- 未打开「高级」时，集成卡改模式不会预热五家 CLI；下一次打开高级会拉（有缓存则 Pi 已有）。
- IM `/new` 在用户刚装上 Pi 的 60s 内可能仍看到「找不到 CLI」。与现有 `fork_readiness`
  缓存同类；高级页「刷新」或等 TTL。本轮不扩大为「安装后主动失效」。
- `agent_mode_status` 改为 async 只影响 invoke 线程模型，TS 侧本来就是 Promise。
