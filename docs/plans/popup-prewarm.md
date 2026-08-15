# 计划：弹窗预热（方案6 进程池）

> Windows 注（2026-08）：本文保留首期 Unix 计划措辞；Windows 已使用 shared daemon/popup path，
> 以下“仅 Unix”是历史记录，当前平台边界以实现与 Windows parity plan 为准。

> 需求与决策见 `docs/specs/popup-prewarm.md`。本计划描述实现方案与全部技术/规则细节，按既有架构落地。
> 仅 Unix（daemon 路径）；非 Unix / 无显示自动不生效。**默认开、可关**。

## 0. 核心思路一句话

把现有「daemon 按请求 spawn 一个 `--popup` 进程 → 该进程连上→收 Show→建窗→show」改造成：daemon **提前**
spawn 一个 `--popup --warm` 进程，它**先建好隐藏窗 + 挂载前端**停在待命态、连上 daemon 入「热池」；来请求时
daemon 把 `Show` 发给池中热连接（而非 spawn 新进程），热进程注入内容、绘制完成才 show。**复用现有 `Show`
通路**，只新增「热池待命 + 领用」一步。冷路径完整保留作回退。

## 1. 配置

- `config.rs`：`general` 增 `popupPrewarm: bool`，**默认 `true`**（`MenuBarIconMode` 同文件风格；`load_without_secrets`
  即可读，非密钥）。语义：是否启用弹窗预热。设置页「通用」Tab 增一个普通开关（**非实验性**），文案如
  「弹窗预热（更快弹出，常驻少量内存）」。`config_watch` 已有热重载；变更经既有路径生效（见 §5）。
- 旧 config 无此字段 → serde default `true`（行为即开启）。

## 2. IPC 协议增量（`ipc/mod.rs`）

- `ClientMsg::GuiWarmReady`：热 helper 连上 daemon 的握手（替代 `GuiHello{token}`；热 helper 启动时**还没有**
  token）。daemon 据此把该连接登记进热池、不关联任何请求。
- **复用 `ServerMsg::Show`**：daemon 领用某热连接时，向它发送既有 `Show(ShowPayload)`（与冷路径同一消息），
  热进程据此注入内容。**无需新增下发消息**。
- 其余消息（`Cancel`/`ConfigChanged`/`UpdateState`/`AgentResolved`/`FocusPopup`/`Answer`）通路不变。
- 旧 daemon / 旧 helper 不识别 `GuiWarmReady` → 协议增量、PROTOCOL_VERSION 不变；热 helper 只会由**新** daemon
  以 `--warm` 拉起，故不存在新 helper 连旧 daemon 的情况（同二进制）。

## 3. daemon 侧（`daemon/mod.rs` + `daemon/request.rs` + `daemon/spawn.rs`/`spawn_gui_helper`）

### 3.1 热池数据结构
- `ServerState` 增「热池」：一个最多 1 项的容器，元素为一个「待命热连接句柄」——含其写端 `gui_tx`
  （`UnboundedSender<ServerMsg>`）+ 一个 `oneshot`/`Notify` 用于「把领用的 `Arc<RequestEntry>` 交给该连接的
  holder 任务」。用 `Mutex<Option<WarmSlot>>`（池大小恒 1）即可，避免过度设计。

### 3.2 热 helper 连接处理（控制循环新增分支）
- `control_loop` 增 `ClientMsg::GuiWarmReady` → 返回 `Control::GuiWarm`。
- 新 `handle_gui_warm(reader, writer, state)`：
  1. 建该连接的 `gui_tx`（专用写任务，串行写出 `ServerMsg`，与 `handle_gui` 同款）。
  2. 把 `(gui_tx, assign_tx)` 放入热池槽（若槽已占则说明已有热实例 → 多余的这个直接关闭退出，保证池恒 ≤1）。
  3. 等待二选一：① `assign_rx` 收到被领用的 `Arc<RequestEntry>`；② 读连接得到 EOF/错误（热进程死亡）→
     清空热池槽 + 触发补热（§3.5）+ 返回。
  4. 被领用后：把 `entry.gui` 槽设为本 `gui_tx`；发送 `request::show_msg(&entry)`；若 `entry.resolved_agent`
     已就绪则补发 `AgentResolved`；带上当前自更新态（与 `handle_gui` 一致）；随后**进入与 `handle_gui` 相同的
     应答读取循环**（读 `Answer` 投协调器 / `cancel.notified()` / EOF→cancel）。收尾清空 `entry.gui` 槽。
  - 即：`handle_gui_warm` = 「待命领用」+ 复用 `handle_gui` 的「下发 show + 读应答」尾段（抽成共享函数
    `serve_gui(entry, reader, gui_tx, state)` 供冷/热两路共用，避免复制）。

### 3.3 领用（改 `handle_submit` 的 spawn 落点，承接方案3）
- 方案3 现在 `Accepted` 后调 `spawn_gui_helper(token, …)`。改为新函数 `dispatch_popup(entry, state, perf…)`：
  1. **尝试领用热池**：取出热池槽中的 `WarmSlot`（`Mutex` 内 `take`）。若有：通过其 `assign_tx` 把
     `entry.clone()` 交给该 holder 任务（holder 即按 §3.2-4 下发 Show 并服务）→ 标记 `popup_ok=true`、
     打点 `dmn.spawned`(沿用) 或新增 `dmn.assigned`；**随后立即触发补热**（§3.5）。
  2. **无热实例**（池空 / 方案6 关 / 无显示）：回退**现有冷路径** `spawn_gui_helper(token, …)`（不变）。
- token 仍在 `registry.create()` 即登记；冷路径不变。热路径下 token 其实用不到（连接已在池中），但保留 token
  字段不影响。

### 3.4 何时补热（top-up，恒维持 1）
- 触发点：① daemon 启动 ready 后；② 每次领用热实例后；③ 热实例死亡（EOF）后；④ 配置 `popupPrewarm`
  由关变开时；⑤ drain/换新结束、新 daemon 起来后（即新进程的启动补热）。
- 补热前置条件（任一不满足则不补，方案6 自动失效）：`popupPrewarm == true` **且** 有可用显示（§3.6）
  **且** 未处于 draining/pending（§3.7）**且** 池中尚无热实例 **且** 未在补热中（去重，避免并发多 spawn）。
- 补热动作：`spawn_warm_helper()`——与 `spawn_gui_helper` 同构，但 `--popup --warm`、**不带 token**；perf
  env 不带（预热不计入某次请求的 perf_id；热路径的 perf 由领用时的请求 perf_id 驱动，见 §6）。

### 3.5 显示可用性判定（§D-M3）
- `has_display()`：
  - macOS：恒 true（GUI 会话；daemon 在用户态）。（如需更严谨可后续用 CoreGraphics 活动显示数，本期从简。）
  - Linux：`DISPLAY` 或 `WAYLAND_DISPLAY` 任一非空则 true，否则 false（headless）。
  - 其它：false。
- 仅 `has_display()` 为真才补热。

### 3.6 生命周期（§D-M4，关键正确性）
- **不续命**：热池连接（`handle_gui_warm` 在「待命」阶段）**不计入 daemon `active`**、空闲判定**不引用**热池
  （与 `handle_tray_sub` 同策略：进入时 `active.fetch_sub(1)` 抵消、退出 `fetch_add(1)`，或直接不在该连接上
  累加 active）。即：只有「待命」的热实例存在时，daemon 仍可正常空闲退出。
- **空闲退出回收**：daemon 走空闲退出 / `cleanup` 时，关闭热池连接（drop `gui_tx` → 写任务结束 → helper 收
  EOF 自行退出）。daemon 进程退出后，其 spawn 的热 helper 因连接断开（§4.4）自杀，无悬挂进程。
- **drain / 二进制换新**：进入 draining（检测到 pending 新二进制且有在途）时，**停止补热**并**回收**现有热
  实例（它是旧二进制）。换新完成、新 daemon 起来后由 §3.4① 用新二进制补热。`pending` 期间到来的请求走冷
  路径（现有 drain 语义）。

### 3.7 与既有方案3/4/5 的关系
- 方案3：`dispatch_popup` 仍在 `Accepted` 后、`attach_im_channels`/`ensure_inbound_listeners` 前调用，保持
  「弹窗与 IM 并行」。热路径下「弹窗」已是秒级领用，IM 仍并行。
- 方案5：热路径下 `AgentResolved` 后推照常（holder 在下发 Show 后会补发已就绪的 resolved_agent；未就绪则
  由 `spawn_agent_resolve` 走 `entry.gui` 推送——此时 `entry.gui` 已是热连接的 tx，通路一致）。

## 4. helper 侧（`app/mod.rs::run_gui_helper` + `launch` + `commands.rs`）

### 4.1 新增 `--warm` 角色入参
- `cli/mod.rs`：`--popup` 解析增 `--warm`（无值 flag）。`run_gui_helper(endpoint, token, warm: bool)`；warm 时
  `token` 为空。

### 4.2 warm 模式启动流程（与现有 cold 流程的差异）
- **cold（现状，保留）**：connect → `GuiHello{token}` → **阻塞等 Show** → 用 Show 建 `AppState` → `launch`
  建窗（含请求）→ show。
- **warm（新增）**：connect → `GuiWarmReady` → **立即 `launch` 建窗（无请求、隐藏、不 show）+ 挂载前端**
  → 前端进入待命 → 此后由连接 reader 循环处理**第一条 `Show`**（=领用，注入请求）与后续 `Cancel`/`ConfigChanged`/…
  - 即把现有「连接块里阻塞读 Show」从 warm 路径移除；warm 的 Show 改在**已建窗后**经 reader 循环处理。
  - `AppState.request` 改为**可后置**：warm 启动时无请求。实现上把 `AppState` 的 `request`（及 `source`/
    `project`/`agent_*`）改为内部可变（`Mutex`/`RwLock` 或单独的 `Arc<Mutex<Option<ShowPayload>>>` 警示槽），
    `popup_init` 在 warm 未领用时返回「无请求待命」标志。

### 4.3 注入与延后 show（§D-M2 / D-M2′，杜绝闪现 + 防卡死）—— 实现期已修正
- reader 循环收到**首条 `Show`**：`perf::set_runtime(perf_id, autodismiss)`（热 helper 无 env，靠 Show 透传开埋点）
  → 存入 `WarmPopup.show` 领用槽 + 回填 `GuiBridge.request_id` → `emit("popup-show")` 唤醒前端。
- 前端 `PopupView`：`popup-show` → `adopt()` 重新 `popup_init()` 取已领用请求 → `renderInit` 应用
  theme/language/语音/agent → 设 `request` 渲染 → `loadThumbs/DragIcons`。
- **关键修正（不能在隐藏窗上等 rAF）**：窗口 ordered-out 时 rAF 不回调（无 display link），故**不能**「先双 rAF 再 show」。
  改为：`renderInit` 里 `nextTick`（等 DOM 把正文更新好）→ **直接 `popup_show_window()` 上屏**（show 不依赖 rAF）；
  随后的双 `rAF`（窗口可见后才回调）只用于 `fe.painted` 打点 + harness autodismiss。冷路径不变（窗口 setup 已 show）。
- 后端 `popup_show_window` → `finalize_popup_show`：主线程对 `popup` 窗按**当前** `load_without_secrets()` 兜底重设
  size/always_on_top/主题（`apply_theme_to_windows`）→ 出现动画 + 玻璃 → `show()` → `gui.win_show` 打点 → 提示音 →
  `set_focus()` → Dock 角标。
- **窗口构建仍设 `background_throttling(BackgroundThrottlingPolicy::Disabled)`**（cold/warm 都设）缓解被遮挡时的节流；
  但**不**指望它为「从未 show 的隐藏窗」驱动 rAF——上屏靠上面的 nextTick+后端 show，与 rAF 无关。
- **锁屏/息屏实测**：弹窗照常上屏（`gui.win_show` 触发）；show 后的 `fe.painted`/autodismiss 因无刷新而暂停，仅影响 harness。

### 4.4 warm 进程的自我退出
- warm reader 循环：收 `Cancel` / 连接 EOF/错误 → `app.exit(0)`（与现状一致）。故 daemon 关闭热连接 →
  warm 进程自杀，无悬挂。
- 领用后行为与 cold helper 完全一致（答完即退）。

### 4.5 前端 `popup_init` / 待命态
- `popup_init` 返回结构增「是否待命（无请求）」标志；warm 未领用时前端渲染**空**（窗口本就隐藏，用户看不到）。
- 已领用 / cold：`popup_init` 照常返回完整请求（cold 路径不变；为兼容，cold 仍可走 `popup_init` 取请求，
  warm 走 `popup-show` 事件注入——两者最终都设 `request` 渲染，复用同一渲染代码）。

## 5. 配置热切换与外观一致性
- warm 窗建于预热时刻的 config；领用前 config 若变，靠两条兜底：① daemon `ConfigChanged` 广播包含热池连接
  （holder 待命阶段也转发 → 前端 `settings-updated` + 原生外观同步，复用现有 §A12 逻辑）；② `popup_show_window`
  在 show 前用**当前** `load_without_secrets()` 重设 theme/size/glass/always-on-top（最终兜底）。

## 6. 性能埋点与 harness（§D-M5）—— 已实现
- **perf 透传（解决热 helper 无 env）**：`ShowPayload` 新增 `perf_id`/`perf_autodismiss`（`request.rs::create` 从
  `TaskRequest` 填）。热 helper 领用收 `Show` 时 `perf::set_runtime(perf_id, autodismiss)` 写进程级运行时上下文；
  `perf::effective_id()`/`autodismiss()` 在 env 缺省时回退到它，使热进程的 `gui.show_recv`/`fe.painted`/`gui.win_show`
  与 CLI 的 `cli.start` **同 perf_id** 关联。`commands::perf_mark`/`popup_init.perf` 改用 `effective_id`。
- 埋点：热路径 `dmn.assigned`（领用时刻，替 `dmn.spawned`）、`gui.show_recv`、`fe.painted`、`gui.win_show`
  （`finalize_popup_show` 内）。warm 进程的 `gui.start`/`gui.build_*` 在**预热阶段**（无 perf_id，不入该请求组）——
  热路径 e2e 看 `cli.start→fe.painted`，GUI 段只剩「注入+绘制」（无 WebView 初始化）。
- harness（`scripts/perf-popup.mjs`）增 **`hot`** 档：daemon 保活 + `popupPrewarm=true`，每次测前 `waitWarmReady`
  （pgrep 热进程在 + settle）确保领用而非冷回退，**hot-only 聚合**（仅 `dmn.assigned` 在的样本）；新增
  `daemon (recv→assigned/hot)` 行；cold/warm 档显式 `popupPrewarm=false` 保持与旧基线可比。基线含 cold/warm/**hot** 三组。
- **屏幕守卫保留**：实测 `background_throttling(Disabled)` **不能**让「从未 show 的隐藏窗 / 息屏锁屏」回调 rAF（无显示刷新），
  故 `fe.painted` 仍只在解锁可见时触发 → harness 仍须解锁运行，**保留锁屏拦截 + caffeinate**（不简化）。

## 7. 回退矩阵（§D-Q4，正确性核心）
| 场景 | 行为 |
|---|---|
| `popupPrewarm=false` | 不补热；所有请求走冷 spawn（等价方案6 之前） |
| 无显示（headless） | 不补热；走冷 spawn |
| 池空（首个请求 / 并发第 2+） | 该请求走冷 spawn；之后补热 |
| 热进程崩溃/超时未就绪 | 池视为空 → 冷 spawn + 重新补热 |
| draining/pending | 停止补热、回收热实例；请求走冷 spawn（现有 drain 语义） |
| 非 Unix | 单进程回退，方案6 不存在 |

## 8. 影响文件（预估）
- `src-tauri/src/config.rs`（`general.popupPrewarm`）、前端设置页开关（`SettingsView.vue` + `types.ts` + `ipc.ts`）。
- `src-tauri/src/ipc/mod.rs`（`ClientMsg::GuiWarmReady`）。
- `src-tauri/src/cli/mod.rs`（`--warm` 解析、`run_gui_helper` 签名）。
- `src-tauri/src/daemon/mod.rs`（热池、`handle_gui_warm`、`serve_gui` 抽取、`dispatch_popup`、补热 top-up、
  `has_display`、生命周期回收、drain 集成）+ `daemon/request.rs`（热池槽类型，若放 ServerState）。
- `src-tauri/src/app/mod.rs`（warm 启动流程、`launch` 支持无请求建窗 + `background_throttling`、把「show 那串」
  移入 `popup_show_window`）+ `commands.rs`（`popup_init` 待命标志、`popup_show_window` 命令、AppState 请求槽可变）。
- `src/views/PopupView.vue`（`popup-show` 事件注入 + 待命态 + 调 `popup_show_window`）+ `lib/ipc.ts`/`types.ts`。
- `scripts/perf-popup.mjs`（`hot` 档）+ `docs/perf/baseline.json`（三组）。
- 文档：本 spec/plan + `docs/specs/popup-launch-performance.md` §4/§6 标注 + `docs/overview.md` + PROGRESS。

## 9. 风险与缓解
- **默认开 → 资源**：常驻 1 个隐藏 WebView 进程（内存）。缓解：池恒 1、不续命、无显示不开、可关。
- **闪现/卡死**：靠单次使用 + 隐藏到绘制完成才 show + `background_throttling(Disabled)`（三重保证）。
- **生命周期回归**：热连接不续命 + drain/idle 回收 + 二进制换新重补；逐项在回退矩阵与验收里覆盖。
- **AppState 请求可变**：warm 需后置请求，改为内部可变，注意并发（领用恰一次、单线程主线程注入）。
- **并发**：池恒 1，第 2+ 并发走冷 spawn，不引入多窗复杂度。

## 10. 实施顺序与进度
1. ✅ 配置开关 + 设置页（默认开）。
2. ✅ `GuiWarmReady` + helper `--warm` 启动 + 前端待命/注入 + `background_throttling`；**上屏改 nextTick+后端 show**（见 §4.3 修正）。
3. ✅ daemon 热池 + `handle_gui_warm`/`serve_gui` + `dispatch_popup` 领用 + 冷回退。
4. ✅ 补热 top-up + `has_display` + 生命周期（不续命/idle 回收/drain 回收/重补）；GUI-free 实测开/关/重补/停均符合预期。
5. ✅ harness `hot` 档 + perf 透传埋点（`ShowPayload`+`set_runtime`）；**屏幕守卫保留**（背景节流不能驱动息屏 rAF）。
   ⏳ 待解锁采三档基线（`--update-baseline`）。
6. ⏳ 真机 sanity（连弹多次、并发第 2 冷回退、改主题、drain 换新、headless）+ 视觉确认无闪现。
7. ⏳ 文档收尾（本 plan/spec + overview + perf spec §4/§6）+ 提交。
> 每步 `./scripts/install.sh` 后用新 `AskHuman` 验证；任何方案调整先经 AskHuman 确认、不擅自改。
