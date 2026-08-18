# 性能优化：弹窗启动延迟（从 agent 调用到用户看到内容）

> 状态：埋点 + harness + 基线已落地；**首轮（方案1/2/7）+ 次轮（方案3/4/5）已实现并量化**。
> 余下：方案8 延后 show/骨架屏（观感）、markdown-it 按需懒加载。
> **方案6（弹窗预热/进程池）已实现**（默认开，`docs/specs/popup-prewarm.md`）：daemon 常驻 1 个隐藏 `--popup --warm`
> helper，来请求领用直接上屏，藏掉每进程一次性的 WebView 初始化；§6.3 的「远期评估」结论已被它取代——落地形态是
> **独立预热 helper 进程**（非 gui-host 预建窗），并把「延后 show」（方案8）合入热路径（领用注入正文后才 show）。
> 目标：尽可能压缩「`AskHuman` 被调用 → 用户在弹窗里看到 Message/问题」的端到端时间。
> 涉及面（macOS/Linux/Windows shared daemon 路径）：`cli/mod.rs`、`client/mod.rs`、`daemon/mod.rs`、`daemon/request.rs`、
> `app/mod.rs`（`run_gui_helper` / `launch`）、`commands.rs`（`popup_init`）、前端 `src/main.ts`、
> `src/views/PopupView.vue`、`agents/detect.rs`。
> 不改：stdout 契约、退出码、结果区块格式、抢答语义。

## 1. 背景与度量对象

当前为「常驻 Daemon + 瘦客户端 CLI + 独立 GUI Helper」三进程架构。一次提问跨 **4 个执行环境**：
CLI 进程 → Daemon → GUI Helper 进程 → 前端 WebView（JS）。我们关心的「用户感知时延」是：

```
T_start  = CLI 进程启动（用户/agent 敲下命令的瞬间）
T_visible = 弹窗里第一帧真正画出 Message/问题（不是空窗，而是有内容）
延迟 = T_visible - T_start
```

需区分两种场景：

- **热路径**：Daemon 已在运行（绝大多数后续调用）——优化主战场。
- **冷启动**：Daemon 未运行（开机后首次）——额外付 Daemon 自启 + 该进程首个 WebView 的一次性代价。

## 2. 完整调用链（热路径）

> 标注 `[阻塞]` = 串行处于关键路径上的等待；`[固有]` = 框架/架构决定、基本不可去；`[可优化]` = 非必须却挡在路径上。

### 2.1 CLI 进程（`cli::dispatch` → `client::run_ask`）

1. 解析 argv（`parse_ask`）。
2. `detect_caller_agent()`：unix 下**同步**沿进程树多次 `fork+exec` 调 `ps`（`agents/detect.rs::process_chain` → `walk_agent_pid_from_self`），拿 agent 家族 / session / pid。**[可优化]**（见 §5）。
3. 建当前线程 tokio runtime → `ensure_running()`：`connect()` + 发 `Hello` + 读 `HelloAck` 一次往返。**[固有，小]**
4. 发 `Submit(TaskRequest)`，随后阻塞读流式回包（`Accepted`/`Final`）。

### 2.2 Daemon（`handle_submit`）

1. `draining` 闸门判断。
2. `AppConfig::load_without_secrets()`（读 `config.json`，无钥匙串）取 `auto_activation`。**[小]**
3. agent 注册表 `touch_activity*`。
4. `ensure_inbound_listeners().await`：无「工作中」agent 立即返回；否则 `AppConfig::load()`（**钥匙串**）+ 可能连 IM。**[可优化]**
5. `registry.create(task)`：建 Coordinator（`new_ipc`）+ token + 注册被动 popup adapter。**[廉价]**
6. `hooks::fire_ask_received(...)`：fire-and-forget。**[小]**
7. 写 `Accepted` 回 CLI。
8. **`attach_im_channels().await`**：**无条件** `AppConfig::load()`（**读钥匙串 5 项**）；对每个启用的 IM 做 `ensure_*_router().await`——若 Router 非存活则**网络连接**（首请求冷连，可能秒级）。**[阻塞][可优化]**
9. **`spawn_gui_helper(token)`**：到这一步才 spawn 弹窗进程（`Command::spawn`，非阻塞返回）。

> 关键问题：**最耗时的弹窗进程启动（步骤 9）被排在了步骤 8 的钥匙串读取 + IM 网络连接之后**，而弹窗与 IM 本是「并行抢答」关系。
> （§2 描述的是**优化前**调用链；方案3 已把 spawn 提到 attach 之前、方案4 已省掉无 IM 时的钥匙串、方案5 已把 detect 移出 CLI，详见 §4/§5/§6 的「已实现」标注。）

### 2.3 GUI Helper 进程（`run_gui_helper` → `launch`）

1. `tauri::async_runtime::block_on`：`connect()` + 发 `GuiHello{token}` + 循环读到 `Show` 为止。**[固有，小；但见 §6]**
2. `AppConfig::load_without_secrets()`（config.json）。**[小]**
3. **`tauri::Builder::default()...build(generate_context!())`**：创建原生窗口 + WebView。**这是整条链最重的一步。** **[固有]**
4. setup：`WebviewWindowBuilder`（`visible(false)`）→ 设出现动画 → 挂 Liquid Glass → `win.show()` → 播放提示音。
5. `app.run(...)` 进事件循环。

### 2.4 前端（WebView 加载 `index.html` → `main.ts` → Vue）

1. `index.html` 内联底色（防白闪）。
2. `main.ts bootstrap()`：`applyLanguage("auto")` → **`await getSettings()`**（→ Rust `AppConfig::load()` **读钥匙串**）→ `applyLanguage(配置语言)` → **`createApp().mount()`**。**[阻塞][可优化]**——Vue 应用在 `getSettings` 返回前**完全不挂载**，弹窗一直是空窗。
3. `PopupView.onMounted`（**全部串行 await**）：
   `await getSettings()`（**又一次钥匙串**，只为拿 speechLanguage/speechShortcut）
   → 多个 `await listen(...)` 注册事件
   → `await speechAvailable()` → `await setupSpeechListeners()`
   → `await popupUpdateState()`
   → **最后**才 `await popupInit()` 拿到 `request` → 渲染内容（= T_visible）。**[阻塞][可优化]**

> 关键问题：返回内容的 `popupInit()`（纯内存态、最廉价的命令）被排在**所有**其它 await 之后，其中还有一次**读钥匙串**的 `getSettings()`。

## 3. 等待点是否必须

| 等待点 | 位置 | 性质 | 说明 |
|---|---|---|---|
| WebView/Tauri 初始化 | Helper §2.3-3 | **固有** | 整链主要耗时；GUI 框架决定。只能「提前开始」或「预热」，难「去除」（见 §6） |
| 独立进程 spawn | Daemon §2.2-9 | **固有** | 三进程架构代价；`spawn` 本身非阻塞 |
| 各 IPC 往返 | 多处 | **固有，小** | 本地 Unix socket / Windows named pipe，通常亚毫秒级，可忽略 |
| `await getSettings()` 挂载前 | 前端 §2.4-2 | **可优化** | 仅为 language 却读钥匙串 + 阻塞挂载；helper 已用无钥匙串配置加载过 |
| `popupInit()` 排在 onMounted 最后 | 前端 §2.4-3 | **可优化** | 内容被多个 await（含钥匙串）挡住 |
| `spawn_gui_helper` 在 IM attach 之后 | Daemon §2.2-8/9 | **可优化** | 弹窗启动被 IM 钥匙串+网络挡住 |
| `attach_im_channels` 无条件读钥匙串 | Daemon §2.2-8 | **可优化** | 无 IM 也读；与 `ensure_inbound_listeners` 重复 load |
| `detect_caller_agent` ps 游走 | CLI §2.1-2 | **可优化** | Submit 前同步跑；agent 信息只用于非关键 badge + 注册表 |

## 4. 优化方案（按收益/风险）

### 高收益·低风险

- **方案1（前端挂载不再等钥匙串）**：`main.ts` 立即 `mount()`；语言改为挂载后异步应用，或经 `popup_init`/URL query 注入——利用 helper 已 `load_without_secrets()` 的配置，**零钥匙串、零阻塞挂载**。
- **方案2（onMounted 先要内容）**：第一步即 `await popupInit()` 并设 `request.value`（先渲染内容）；其余 `getSettings`/`listen`/speech/update 改为并行（`Promise.all`）或后台执行；speech 设置改走非钥匙串来源（同方案1 注入或新增轻量命令）。
- **方案3（daemon 提前 spawn）✅ 已实现（2026-06）**：把 `spawn_gui_helper` 提到写 `Accepted` 之后、`ensure_inbound_listeners` / `attach_im_channels` 之前。token 在 `create()` 即登记，不存在「helper 先连上、entry 未注册」竞态。弹窗进程的 WebView 初始化与 daemon 的 IM 连接**并行**。实测 COLD `daemon recv→spawned` 466→1ms、端到端 -49%（~464ms IM 建连不再挡弹窗）。

### 中收益·低风险

- **方案4（attach 省钥匙串）✅ 已实现（2026-06）**：`attach_im_channels` 与 `ensure_inbound_listeners` 都先用 `load_without_secrets()` 的 `enabled` 标志（`any_im_enabled()`）判定有无启用 IM，无则**完全跳过** `AppConfig::load()`（零钥匙串读取）；有 IM 时才 `load()`。
- **方案7（前端 bundle 代码分割）**：当前 `App.vue` **静态 import** 四个 view（Popup/Settings/History/Agents），打成单个 ~376KB chunk；弹窗只渲染 `PopupView` 却要下载/解析整包。把 Settings/History/Agents 改 `defineAsyncComponent(() => import(...))`，弹窗 chunk 只含 `PopupView` 及其依赖，减少解析/执行（落在 `show→fe.bootstrap` 与 `frontend boot` 段）。`PopupView` 保持静态以免延迟关键路径。markdown-it 等重依赖可进一步按需懒加载（仅 `isMarkdown` 时）。

### 观感优化（不减总时长，改善感知）

- **方案8（延后 `win.show()` 到内容就绪 + 骨架屏）**：现窗口在 `gui.win_show`（≈240ms）就可见、正文 `fe.painted`（≈570ms）才出现，中间 ≈300ms 停在「加载中」。可改为：前端 `fe.painted` 时 emit 事件→后端再 `win.show()`，消除「加载中」闪烁；或先渲染弹窗骨架（标题栏等）让加载态显得有意。注意这**不缩短**到内容的绝对时延（甚至窗口出现更晚），最好与方案6（预热）配合；单独做需权衡「白屏更久 vs 不见加载中」。

### 需谨慎评估

- **方案5（detect 移出关键路径）**：见 §5。
- **方案6（WebView 提前/预热）**：见 §6。

> 方案编号与「页面加载 90%」优化全景的对应（2026-06 讨论）：预热复用=方案6；bundle 代码分割=方案7；popupInit 提前=方案2；main.ts 不阻塞挂载=方案1；延后 show/骨架屏=方案8；detect 移 daemon=方案5。**首轮先做低风险组合：方案7 + 方案2 + 方案1**（见 `docs/plans/popup-launch-low-risk-optimization.md`）。

## 5. 详解：`detect_caller_agent` 移出 Submit 关键路径

### 5.1 现状与「为什么是关键路径」

`cli/mod.rs::dispatch` 在构造 `TaskRequest`、发出 `Submit` **之前**，先同步调用 `detect_caller_agent()`：

```
detect::detect_running_agent()        // 读本进程 env（快）
detect::session_id_from_env(kind)     // 读 env（快）
detect::walk_agent_pid_from_self(kind)// ← 沿进程树多次 spawn `ps`（fork+exec），数十 ms
```

`walk_agent_pid_from_self` 走 `process_chain`：从自身 pid 逐级向上，每级各跑一次 `ps -o ppid=,comm=` 和一次 `ps -o command=`（两次子进程）。链有几层就有几对 `ps`。这些子进程的 fork+exec 累加是**数十毫秒级**，且**完全串行挡在 `Submit` 之前**——daemon 在这段时间里根本还没收到请求，下游（accept → spawn helper → WebView）全部被推迟。

> 「移出关键路径」= **不要让 `Submit` 等这次进程树探测**。因为这份 agent 信息（家族/pid/session）只用于：(1) daemon 侧的生命周期活动刷新；(2) 弹窗顶栏的 **agent badge**。**两者都不是首帧渲染必需**——弹窗没有 agent 信息也能完整显示 Message/问题（badge 的「所在终端」探测本来就已经是弹窗渲染后再异步补的，见 `popup_agent_terminal`）。

### 5.2 可选做法（择一）

- **(a) 延迟下发**：CLI 先发 `Submit`（不带 agent 信息或只带能从 env 秒拿的部分），随后在后台跑 ps 游走，拿到后用一条**后续 IPC 消息**补发；daemon 收到后刷新注册表，并（可选）经 GUI 连接把 agent_kind/pid 推给弹窗，badge 像终端那样「后到先显纯文字、补全后升级」。
- **(b) daemon 侧探测**：CLI 只把**自己的 pid** 放进 `TaskRequest`，由 daemon 在 accept 之后**异步**从该 pid 向上 walk（CLI 在请求存续期保持连接，进程树仍在）。CLI 端零 ps 开销。注意 daemon 与 CLI 不在同一进程树，必须从 CLI pid 起 walk，不能从 daemon 自身。
- **(c) 并行化（最小改动）**：CLI 里把 ps 游走与 `ensure_running()`（connect+Hello 往返）**并发**跑，二者都完成后再 `Submit`。只能掩盖掉与握手往返重叠的那部分，收益有限，但改动最小、语义最稳。

> 取舍：(a)/(b) 收益最大但需动 IPC 协议与 badge 的「后补」逻辑；(c) 改动最小、收益有限。建议先量化 §7 基线里 detect 这一段实际占多少，再决定是否值得做 (a)/(b)。

> **已确认决策（2026-06）**：采用 **(b) daemon 侧探测**——CLI 只在 `TaskRequest` 带自己的 pid，
> daemon accept 后**异步**从该 pid 向上 walk 进程树拿 agent 家族/pid（CLI 请求期间保持连接，进程树仍在），
> 把 ps 开销完全移出 CLI 关键路径。

> **✅ 已实现（2026-06，方案5(b)）**：
> - CLI `detect_caller_agent()` 只读 env（家族 + 会话，~0ms），不再 `ps` walk；`TaskRequest` 新增 `caller_pid`（CLI 自身 pid），`agent_pid` 恒 `None`。
> - daemon `handle_submit` 在**已 spawn 弹窗后**起一个独立 task：`spawn_blocking` 从 `caller_pid` 向上 walk（`walk_agent_pid`；env 判不出家族的 MCP 情形用 `walk_any_agent` 兜底），拿到家族/pid 后补刷注册表活动，并经新 `ServerMsg::AgentResolved` 后推弹窗。
> - 弹窗 badge「后到补全」：helper 收 `AgentResolved` → 缓存（`popup_agent_resolved` 供挂载 pull 初值）+ emit `agent-resolved` 事件；`PopupView` 拿到家族先显纯文字、再据 pid 异步 `popup_agent_terminal(pid)` 升级成「可点 ↗」。`handle_gui` 握手时若已解析完则随 Show 补发（覆盖「解析早于连接」竞态）。
> - 实测 CLI `detect` COLD -97%（38.7→1ms）、WARM -96%（27→1ms）；daemon 异步 walk 不进端到端（off-path）。端到端 sanity：本仓 `AskHuman` 弹窗仍正确显示 `cursor` badge 且可点。

## 6. 分析：WebView 初始化能否与其它部分并行 / 更早开始？

「WebView 最重」指 §2.3-3 的 `tauri::Builder::build()`（创建原生窗口 + WKWebView/WebKitGTK + 加载嵌入式前端资源 + 起 JS 引擎）。能否并行，分三个层次：

### 6.1 让它「更早开始」——可行，即方案3 ✅ 已实现

WebView 初始化发生在 **GUI Helper 进程**里。它**何时开始**取决于 daemon **何时 spawn** 这个进程。原 spawn 排在 `attach_im_channels`（钥匙串 + IM 网络连接）之后。**方案3 把 spawn 提前**（到写 `Accepted` 之后、attach/inbound 之前），让 WebView 初始化与 daemon 的 IM 连接**并行**——已实现，实测 COLD 端到端 -49%（~464ms IM 建连不再挡弹窗，`daemon recv→spawned` 466→1ms）。

### 6.2 进程内：WebView build 与 `Show` 往返重叠——小幅可行

当前 helper 先 `block_on(connect + GuiHello + 读 Show)`，**之后**才 build WebView。但**窗口配置（尺寸/主题/玻璃）来自 `load_without_secrets()` 的 config，不依赖 `Show`**；`Show` 只提供题目内容，而题目是前端挂载后经 `popup_init` 才取的。所以理论上可以：**先 build WebView（窗口立即出现 + 显示「加载中」）**，把 `Show` 的接收放到后台任务，等内容到了再经 `popup_init` 注入。

- 收益：WebView native 初始化与 `Show` 往返重叠。但 `Show` 是**本地 socket、亚毫秒级**，正常情况下重叠收益很小；只有当 daemon 迟迟不发 `Show`（如 daemon 忙）时才显著。
- 代价：要重排 `run_gui_helper` 的「先拿 Show 再 launch」结构，`popup_init` 需要能「等 Show 到达」。属中等改动、收益有限，**优先级低**。

### 6.3 真正「隐藏」WebView 代价：预热 / 进程池——收益最大但是架构改动

WebView native 初始化是**每进程一次性**的硬成本，单次调用内无法和「它自己」并行（前端 JS 必须等 WebView 起来）。要进一步压，只能**把这次成本从关键路径里挪走——预热**：

- **思路**：维持一个**已初始化好、隐藏窗口**的 GUI Helper（或一个常驻、可复用的 GUI 宿主来承载弹窗）。请求到来时只需「填内容 + `show()`」，跳过冷 build。理论上可把感知时延降到接近「IPC + 渲染」量级。
- **约束/代价**：
  - 现架构**刻意**让弹窗跑在 daemon 之外的短命进程里——正是为了让 **daemon 不碰 AppKit/主线程**。预热进程不能是 daemon 本体。
  - unix 已有长命的**统一 GUI 宿主**（`--gui-host`，跑 AppKit）。一个方向是让宿主**预建隐藏弹窗窗口**、来请求时复用——但要解决：宿主可能未运行（仅在 `menuBarIcon != off` 时常驻）、多并发弹窗、主题/语言/玻璃热切换、窗口与请求生命周期解耦、预热窗口的内存常驻。
  - 复杂度与回归风险高，且与「弹窗独立进程、可被 drain 换新」等既有不变量交织。
- **结论（已落地，2026-06）**：未走「gui-host 预建窗」方向（宿主可能不在 + 多并发 + 生命周期耦合），改为 **daemon 直接预热
  一个独立 `--popup --warm` helper 进程**（已挂载、隐藏待命，入热池），来请求由 `dispatch_popup` 领用喂 `Show` 直接上屏，
  把每进程一次性的 WebView 初始化挪出关键路径。默认开、可关、非保活、无显示不预热、透明回退冷 spawn。详见
  `docs/specs/popup-prewarm.md` / `docs/plans/popup-prewarm.md`；harness 增 `hot` 档量化 `show→painted` 降幅。

> 一句话：WebView **能更早开始**（方案3），**能与本地 Show 往返小幅重叠**（6.2，收益小），但**单次内无法与自身并行**；要真正「藏掉」它只能**预热**（6.3，架构级、远期）。

### 6.4 待澄清的更大问题：WebView 初始化 vs「弹窗弹出前的全部准备工作」并行（待基线后定）

> 用户原意不止「与 IM 并行」（§6.1）。真正想问的是：**WebView 初始化能否与「弹窗真正弹出所需的、排在它前面的全部准备工作」并行？**

这些「准备工作」散布在多个环境，按发生顺序：

- CLI：argv 解析、`detect_caller_agent`（将移到 daemon，§5）、握手往返、构造 TaskRequest。
- daemon：config 读取、agent 注册、`ensure_inbound_listeners`、`registry.create`、hook、`attach_im_channels`（钥匙串 + IM 网络）。
- helper（进程内、build 之前）：connect + `GuiHello` + 读 `Show`、`load_without_secrets`。

其中**真正在 WebView build 之前、又同进程串行**的只有 helper 那段（§6.2，收益小）；CLI/daemon 的准备工作发生在**另外的进程**，靠「**提前 spawn helper**」（方案3）即可与之重叠——本质上 WebView 初始化就是 helper 进程的主体，让 helper 早点起来 = 让 WebView 早点和前面所有跨进程准备并行。

**结论（本轮先记录、不动手）**：是否还要进一步把 helper 进程内「build 之前的准备」与 build 重叠（§6.2），以及是否走预热（§6.3），**等 §7 基线数据出来、看清各段实际占比后再决定**。先把埋点/基线和方案1/2/3 做掉，用数据驱动后续取舍。

## 7. 性能度量方法论（埋点 → 基线 → 对比）

优化前必须先能**测量**，否则无法判断每个方案的真实收益、也无法防回归。

### 7.1 里程碑埋点（跨 4 个环境，带 request_id 关联）

在关键节点打**带时间戳 + request_id**的日志。同机跨进程用 **epoch 毫秒**（`SystemTime`）即可对齐（同一台机器时钟一致），前端用 `Date.now()`。建议里程碑：

| 标记 | 位置 | 含义 |
|---|---|---|
| `spawn` | harness 注入（`ASKHUMAN_PERF_SPAWN_TS`），CLI 写入 | harness spawn 子进程前一刻；含进程创建 + 二进制加载（main 之前不可见的开销） |
| `cli.start` | CLI 进程最早处（`main` 首行 `perf::record_start`） | T_start（进程内最早点） |
| `cli.detect_done` | `detect_caller_agent` 返回后 | 量化 §5 的 ps 开销 |
| `cli.submit` | 发出 `Submit` | |
| `dmn.submit_recv` | `handle_submit` 入口 | |
| `dmn.created` | `registry.create` 返回 | |
| `dmn.accepted` | 写 `Accepted` 后 | |
| `dmn.im_done` | `attach_im_channels` 返回 | 量化 IM attach 开销 |
| `dmn.spawned` | `spawn_gui_helper` 返回 | |
| `gui.start` | helper 进程最早处 | 进程 spawn→运行的间隔 |
| `gui.show_recv` | 收到 `Show` | |
| `gui.build_start` / `gui.build_done` | Tauri build 前后 | 量化 WebView native 成本 |
| `gui.win_show` | `win.show()` 后 | |
| `fe.bootstrap` | `main.ts` 进入 | WebView 起→JS 跑的间隔 |
| `fe.mounted` | Vue mount 后 | |
| `fe.popup_init_done` | `popupInit()` resolve | |
| `fe.painted` | 内容设入后**双 `rAF`** 回调（第二帧已真正合成上屏） | **T_visible**（贴近用户真正看到内容） |

前端时间戳回传：新增一个 `perf_mark(stage, ts)` Tauri 命令，把 `fe.*` 标记**追加写入** `~/.askhuman/perf.log`（与后端各环境同一文件，便于按 request_id 串起来）。注意 GUI 阶段 stderr 被重定向到 /dev/null，故走**专用文件**而非 stderr/stdout（也不污染洁净 stdout 契约）。

### 7.2 开关与传播（不影响正常使用）—— 已落地

- 用环境变量 `ASKHUMAN_PERF=1`（沿用项目「非空非 0 即真」惯例）门控；**默认关**，零额外开销/日志。
- **关联 id 而非 bool**：CLI 仅在 `ASKHUMAN_PERF` 开启时铸一个 `perf_id`（`"<pid>-<epoch_ms>"`），经 `TaskRequest.perf_id`（serde default，旧 daemon 忽略）透传。
- **统一以 `perf_id` 非空为写入开关**（不在写入处读 env）：daemon 是常驻进程、启动时并无 `ASKHUMAN_PERF`，故各环境的 `mark` 一律「`perf_id` 非空才写」——CLI 用自己铸的 id，daemon 用 `task.perf_id`，helper/前端用 daemon 透传来的 id。关闭即恒空、零开销。
- **传播给 helper**：daemon 在 `spawn_gui_helper` 时若 `perf_id` 非空，给子进程设 `ASKHUMAN_PERF=1`、`ASKHUMAN_PERF_ID=<perf_id>`（及测试用的 `ASKHUMAN_PERF_AUTODISMISS`）。helper 经 `perf::mark_env` 打点，前端经 `popup_init` 回传的 `perf` 标志决定是否上报 `perf_mark`。
- 实现见 `src-tauri/src/perf.rs`（含 `record_start` 在 `main` 最早处取进程出生时间，使 `cli.start` 准确）。

### 7.3 采集 harness（`scripts/perf-popup.mjs` + `scripts/perf-mock-im.mjs`）—— 已落地

**无脑单命令**：`node scripts/perf-popup.mjs`（无参）跑一条**固定 canonical 场景**，对比固定基线 `docs/perf/baseline.json`（有则比、无则建、劣化退非零）；只保留 `--update-baseline`（刷新基线）与 `--keep-home`（调试保留临时目录）。**不提供任何改变「测什么」的参数**（runs/warmup/cold/threshold 全内置固定），确保读数跨时间/机器可比。设计与计划见 `docs/plans/perf-harness-deterministic-mock-im.md`。

canonical 场景每次：

- **隔离 daemon + 零钥匙串副作用（关键）**：在**临时 `HOME`** 下跑**独立 daemon**——socket/lock/perf.log/agents/config 全在 `$HOME/.askhuman`，重定向 `HOME` 即整链隔离，**绝不触碰用户真实 daemon / 在途提问**。另设 `ASKHUMAN_NO_KEYCHAIN=1`（`secrets.rs` 据此走「钥匙串不可用→明文回退」路径），因 OS 钥匙串**不随 `HOME` 隔离**，否则 `AppConfig::load()` 会把 config 里的占位密钥写进/覆盖用户真实钥匙串。
- **本地 mock IM 覆盖全部 4 渠道**：`perf-mock-im.mjs` 单进程同端口服务四家的 HTTP OpenAPI + WebSocket。canonical config 启用钉钉/飞书/Telegram/Slack 并指向 mock（Telegram/飞书用 config 的 base url；钉钉/Slack 硬编码端点用 `ASKHUMAN_{DINGTALK,SLACK}_API_BASE` env 覆盖，仅测试用、未设则不变）。mock 让各 `Router::connect()` 成功并保活、且接受发卡片（驱动渠道自身发送代码真跑），并对**建连与发送两处各注入 ~150ms 延迟**：建连延迟当下就落在弹窗关键路径（`attach_im_channels` 在 spawn 前、且三家 WS 串行连 → 冷启动 `im_attach` ≈450ms+），发送延迟当下因 `ch.start` 异步不进端到端、是「未来发送被改成阻塞」的探针。
- **冷 + 热都跑（同一次执行）**：**cold** = 每轮前 `daemon stop --force`（daemon 冷启动 + IM 每轮重连，IM-on-path 延迟落在这里）；**warm** = daemon 保活、IM router 复用（稳态弹窗时延）。各跑固定轮次、丢固定 warmup，按 `perf_id` 聚合隔离 `perf.log`，分别出 cold/warm 两张表（中位/p90/min/max）。
- 每次以 `ASKHUMAN_PERF=1 ASKHUMAN_PERF_AUTODISMISS=1` + 注入 `ASKHUMAN_PERF_SPAWN_TS` 拉起提问；弹窗画完首帧（双 `rAF` 的 `fe.painted`）即 `cancelPopup()` 自动退出。
- **屏幕可见守卫**：`fe.painted` 在真实 WebView 里靠 `requestAnimationFrame`，而 macOS 在**锁屏 / 息屏 / 窗口被遮挡**时会暂停 rAF → 弹窗永不上屏、autodismiss 不触发。故 harness：启动前与每轮前用 `ioreg -n Root -d1 -r` 读 `CGSSessionScreenIsLocked`，**锁屏即报错不跑**；运行期开 `caffeinate -d` 防息屏；任一轮弹窗未上屏（超时）即判定数据无效、报错中止（不产坏数据）。→ 跑测时请保持屏幕解锁、唤醒、勿遮挡弹窗。

### 7.4 对比与防回归 —— 已落地

- **端到端 p90 为准的回归闸**：cold、warm 各自的 `e2e+spawn (spawn->painted)` p90 若超过 `baseline × (1 + 20%)`，脚本打印 `REGRESSION` 并 **退出码 1**（可接入 CI / 手动门禁）；各段也逐行给出 `delta%`，超阈用 `!` 标注，供归因。基线 JSON 含 `cold` / `warm` 两组。
- **逐方案**实施 + 重测：每落地一个方案，重跑对比，确认收益**归因到对应段**（如方案3 应让 cold 的 `im_attach`（~450ms）从端到端消失 → cold e2e 大幅下降）。
- 保持 `perf.log` 字段格式（`<epoch_ms>\t<perf_id>\t<stage>\t<pid>`）稳定，确保前后可比。
- 注意「WebView/页面加载」段是**每弹窗一进程的固有冷成本**（无热 WebView 可复用），是当前 e2e 的大头。

### 7.5 里程碑解读补注

- `gui.build_done` 标记的是 Tauri `build()` 返回（仅 builder 配置，~60ms）；**原生窗口创建 + 首帧页面加载发生在其后的 `run()`/`setup` 内**。故「窗口可见 / 页面起跑」要从 `gui.build_start` / `gui.show_recv` 量，harness 报表已据此命名（`window visible`、`page boot`、`GUI total`）。
- **起点两种**：`cli.start` 是进程 `main` 首行（进程内最早点，不含 main 之前开销）；`spawn` 由 harness 注入，含进程创建 + 二进制加载。两者之差（`proc spawn`）本机 ~10ms。`spawn → fe.painted` 是「含一切」的真·端到端，回归闸默认以它的 p90 为准（无 spawn 时回退 `cli.start → fe.painted`）。
- **终点 `fe.painted` 用双 `rAF`**：第一帧让正文进入 DOM、即将绘制，第二帧回调时该帧已真正合成上屏——比单 `rAF` 晚约 1 帧，但更贴近用户真正看到内容。注意它标的是「正文文本/选项可见」；附件缩略图 / 图片是其后异步加载，不计入。
- **用户会先看到「加载中」**：窗口在 `gui.win_show`（≈ build 起点后 240ms）就可见，但正文要到 `fe.painted`（≈570ms）才出现——这中间 ≈300ms 窗口停在 `popup.loading` 占位（`PopupView.vue` `v-if="!request"`）。`window visible → fe.painted` 这段就是这段「加载中」，也是当前最大优化空间。

## 附：基线数据（优化前，本机 macOS，**隔离 daemon** 18 次有效，单位 ms）

> 正式基线见 `docs/perf/baseline.json`（现含 `cold` / `warm` 两组，canonical 场景 + 全 4 渠道 mock IM，`--update-baseline` 刷新）。下表是**首版历史快照（无 IM，单组）**，仅留作页面加载占比的参照；`im_attach` 在新 cold 组里因 mock 注入 ~150ms 串行会显著非零。

| 段 | 中位 | p90 |
|---|---|---|
| proc spawn（spawn→cli.start，进程创建+加载） | 10 | 10 |
| cli（start→submit，含 detect） | 28 | 30 |
| ipc（submit→dmn.recv） | 0 | 0 |
| daemon（recv→spawned；隔离无 IM≈0） | 0 | 1 |
| spawn→gui 进程起 | 12 | 13 |
| gui 连接（start→show） | 0 | 1 |
| GUI 总（show→painted） | 412 | 418 |
| ↳ tauri build()（builder 配置） | 37 | 43 |
| ↳ window visible（build_start→win_show） | 188 | 197 |
| ↳ page boot（show→fe boot） | 348 | 357 |
| ↳ frontend（boot→painted，双 rAF） | 62 | 66 |
| ↳↳ popup_init | 14 | 16 |
| 端到端 cli.start→fe.painted | 453 | 464 |
| **端到端含 spawn（spawn→painted）** | **463** | **474** |

要点：端到端 ~0.47s 里 **GUI 段占 ~88%**，其中「页面加载到 JS 起跑」（show→fe.bootstrap，~0.35s）是绝对大头——印证 §6：真正的大头在 WebView/页面加载，CLI/daemon/IM/进程创建合计仅几十毫秒。低风险方案（7/2/1）主要压 `frontend boot→painted`（~62ms，含 popup_init 排序）与 `page boot`（bundle 解析）这两段。
