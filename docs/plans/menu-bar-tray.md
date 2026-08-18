# 开发计划：菜单栏状态图标 + 统一 GUI 宿主进程

> Windows 注（2026-08）：本文保留首期 macOS/Linux 实施步骤；Windows tray、GUI Host、named-pipe
> 单窗路由与 HKCU Run 已由平台对齐项目实现，以下 Windows 隐藏/单进程描述仅作历史记录。

> 关联需求：`docs/specs/menu-bar-tray.md`
> 计划描述方案与技术/规则细节；具体代码以实现为准。本计划自成一体，可脱离 spec 阅读。

## 0. 方案总览

```
角色一览（同一二进制按 argv 切换）
  AskHuman CLI                ── 提问/管理（不变）
  AskHuman daemon run         ── 无 GUI 常驻（既有；新增：按需拉起宿主 + 托盘状态推送）
  AskHuman --popup …          ── 短命弹窗 Helper（既有）
  AskHuman --gui-host         ── 【新增】单实例 GUI 宿主：托盘图标 + 设置/历史/Agent 窗口
  AskHuman --settings/--history/agents monitor
                              ── 【改造】不再各自建窗，改为「路由到宿主」打开对应窗口

GUI 宿主进程（单实例 flock，macOS=accessory）
  ├─ 自有 IPC（gui-host.sock）：收 OpenWindow{settings|history|agents} / Shutdown / Ping
  │     CLI/弹窗导航 → 连宿主 → 发 OpenWindow → 宿主聚焦/新建唯一窗口；宿主不在则先 spawn
  ├─ 窗口管理：create_settings/history/agents_window（已有「聚焦或新建」）→ 全局单窗
  ├─ 托盘：Tauri tray-icon + 原生菜单；图标三态（运行·空闲 / 运行·待答 / 停止）
  ├─ daemon 客户端：
  │     ├─ TraySubscribe（非保活）→ 收 TrayState → 刷新菜单文字 + 切图标
  │     └─ 窗口期保活连接（windows>0 时持有一条普通连接，计入 daemon active → 续命；=0 即关）
  ├─ 配置监听：menuBarIcon 模式 / 语言变化 → 重建菜单 + 装/卸登录项
  └─ 二进制换新：感知 pending（来自 TrayState）→ 无窗口时换新（re-exec / 交 launchd）

daemon（既有 + 增量）
  ├─ menuBarIcon!=off 时：启动/配置变更尝试 spawn --gui-host（单实例去重，兜底）
  ├─ TraySubscribe → handle_tray_sub（不计入空闲保活）
  └─ broadcast_tray_state（连上即一帧 + 变化即推）

生命周期要点（spec D4/D5）
  active：daemon 跑时图标在；无窗口 + daemon 空闲退出 → 图标消失、宿主退出。
  always：登录项常驻；图标恒在；无窗口时 daemon 仍空闲退出 → 图标转「停止」态、宿主不退、被动重连。
  续命：任意窗口打开 → 续命正在跑的 daemon；末窗关闭 → daemon 重新计时退出。图标本身不续命。
```

---

## 1. 配置 `src-tauri/src/config.rs`

- 新增枚举 `MenuBarIconMode { Off, Active, Always }`（serde rename_all="lowercase"，默认 `Off`）。
- `GeneralConfig` 增 `menu_bar_icon: MenuBarIconMode`（camelCase → `menuBarIcon`），默认 `Off`；同步 `Default` 与单测。
- 缺字段/未知值经现有容错走默认（旧配置零影响）。

## 2. 路径 / 单实例 `src-tauri/src/paths.rs` + 复用 `daemon/lifecycle.rs`

- `paths.rs` 增：`gui_host_sock()`（`~/.askhuman/gui-host.sock`）、`gui_host_lock()`（`~/.askhuman/gui-host.lock`）。
- 单实例：参照 `daemon/lifecycle.rs` 的 flock 封装做 `gui-host.lock`（宿主启动即加锁；失败=已有宿主 → 退出）。

## 3. 角色分发 `src-tauri/src/cli/mod.rs`

- 新增隐藏分支 `"--gui-host"`（仅 unix）：`crate::app::run_gui_host(AppConfig::load_without_secrets())`。宿主只需 general（模式/主题/语言），不读密钥。
- 改造 `--settings` / `--history`（unix）：不再直接 `run_settings/run_history`，改为 `host_open(WindowKind)`（见 §4）——宿主在则路由、不在则拉起。
  - 非 unix（Windows）：保持现状（直接 `run_settings/run_history`，单进程）。
- `agents monitor`（`cli/agents_cmd.rs`）：GUI 路径由「`run_agents` 直接建窗」改为 `host_open(Agents)`；`--json/--text` 文本快照路径不变。

## 4. 宿主自有 IPC `src-tauri/src/gui_host/`（或并入 `client`/`ipc`）

- 协议 `HostMsg`（NDJSON，复用 `ipc::codec`）：
  - `OpenWindow { kind: "settings"|"history"|"agents", all?: bool }`
  - `Shutdown`（mode→off 时由 daemon/设置触发宿主退出，可选）
  - `Ping`（探活）
- `host_open(kind)`（CLI/弹窗用）：
  1. 连 `gui_host_sock()`；连通 → 发 `OpenWindow` → 退出（窗口在宿主里打开/聚焦）。
  2. 连不上 → `spawn --gui-host`（detached），轮询连上后发 `OpenWindow`；超时则回退（极端情况下直接本进程建窗，保证不至于打不开）。
- 宿主侧监听 `gui-host.sock`：收 `OpenWindow` → 主线程 `create_*_window`（已「聚焦或新建」）→ 全局单窗。

## 5. 宿主进程 `src-tauri/src/app/`（复用 `launch()` 单一 Tauri 上下文）

`generate_context!` 每二进制仅展开一次（见 `app/mod.rs`），故宿主复用 `launch()`，新增 `#[cfg(unix)] View::GuiHost`：

- `#[cfg(unix)] pub fn run_gui_host(config)`：走 `launch(state, View::GuiHost, None)`。
- `launch()` setup 的 `View::GuiHost` 分支（不建初始窗口）：
  1. flock `gui-host.lock`；失败即退出。
  2. macOS：`app.set_activation_policy(ActivationPolicy::Accessory)`。
  3. 起宿主 IPC 监听（§4）。
  4. 若 `menuBarIcon != off` 且托盘可用：建 `TrayIconBuilder` + 原生菜单（§5.1）。
  5. 起 daemon 客户端任务（§5.2）。
  6. 起配置监听（§5.3：语言/模式 → 重建菜单 + 登录项）。
- `app.run(...)`：`View::GuiHost` 不属 `prevent_autoexit`；进程存活/退出由 §5.4 规则驱动（`app_handle.exit`）。Tauri 有 tray 或窗口时不会因「无窗口」自动退出；纯 off 且无窗口时由 §5.4 主动退出。

### 5.1 原生菜单与图标（Tauri tray API）

- 用 `tauri::tray::TrayIconBuilder` + `tauri::menu::{Menu, MenuItem, PredefinedMenuItem}`；菜单**原生**，文字来自 Rust i18n（§9），无 webview。
- 菜单结构（spec D7），按最新 `TrayState` 动态重建：
  - 状态区（disabled 只读项）：
    - daemon 运行：`AskHuman — 运行中 · v<ver> · 已运行 <uptime>`；daemon 停止（always）：`AskHuman — 未运行`
    - `<N> 个待答`（N>0）
    - `Agent：工作 <w> · 空闲 <i>`（有数据）
    - `IM：feishu, slack`（非空）
    - `● 有可用更新（v<latest>）`（available；点击 → 触发更新或打开设置，按 D8 处理）
  - 分隔符
  - 操作区（`on_menu_event` 按 id 分派）：
    - `open_settings` / `open_history` / `open_agents` → `create_*_window`（同进程，聚焦或新建）
    - `check_update`（始终）/ `apply_update`（available 时「更新到 vX.Y（答完后生效）」→ 见 §5.5）
    - daemon 运行：`restart_daemon` / `stop_daemon`；daemon 停止（always）：`start_daemon`
    - Agent 集成需更新时增加可点击灯泡提示：单项列 Agent 名，多项只显示数量；点击定位设置 Agents tab。
- 图标三态（spec D6 / 资产 §8）：`set_icon`：daemon 停止→停止变体；daemon 运行 + `active_requests>0`→待答变体；否则→空闲变体。macOS 图标均用 `icon_as_template(true)`；Agent 集成需更新且没有待答时，选右上小实心圆变体，待答问号优先。`set_tooltip` 概要。

### 5.2 daemon 客户端任务（状态 + 续命）

- **状态订阅（非保活）**：`transport::connect()`（**注意：不用 `ensure_running`**，避免图标把 daemon 拉起）→ 发 `ClientMsg::TraySubscribe` → 循环读 `ServerMsg::TrayState` → 缓存 + 主线程刷新菜单/图标。
  - 断连（daemon 空闲退出/停止/换新）：
    - **active**：若无打开窗口 → `app_handle.exit(0)`（图标消失、宿主退出）；（有窗口不可能，因为窗口续命了 daemon）。
    - **always**：图标转「停止」态；**被动重连**（定时 `transport::connect` 重试，不 `ensure_running`）；连上即恢复。
- **窗口期保活**：维护 `windows_open` 计数（窗口创建/销毁事件）。
  - `windows_open` 0→1 且 daemon 在跑：开一条**普通连接**到 daemon 持有（普通连接默认计入 daemon `active` → 续命）。1→0：关闭该连接（daemon 重新计时）。
  - 该连接不发特殊消息，纯占位；daemon 侧无需改动（默认计活）。
- **Agent 窗口**：打开时若需 daemon 数据 → `ensure_running()`（这是唯一允许由窗口启动 daemon 的入口）；其内部沿用既有 `AgentsSubscribe`（既有，本就计活）。

### 5.3 配置监听（语言 / 模式 / 登录项）

- 宿主用 `notify` 监听 `config.json`（参照 `daemon/config_watch.rs` 或 `watch_history_file`），去抖后重载 general：
  - **语言变化** → 重建托盘菜单（§5.1）为新语言（D9）。
  - **menuBarIcon 模式变化**：
    - →off：移除托盘图标 + 卸登录项；若无窗口则退出宿主。
    - →active：建图标（若 daemon 在）；卸登录项。
    - →always：建图标；装登录项（§7）。
- 也复用 daemon 的 `TrayState`/`ConfigChanged` 推送做实时切主题（窗口侧沿用既有 settings-updated 机制）。

### 5.4 宿主存活/退出规则

- 退出条件（任一周期性判定 / 事件触发时检查）：`windows_open==0` 且：
  - mode==off → 退出（宿主仅为窗口而生）。
  - mode==active → daemon 断连即退出（见 §5.2）。
  - mode==always → **不退出**（常驻，图标转停止态）。
- 例外：二进制换新（§6）在 `windows_open==0` 时主动退出/重启。

### 5.5 菜单内更新（D8）

- 宿主是 Tauri GUI 进程，直接调用 `update` 模块：`check_update` → `update::check()`；`apply_update` → `update::select_updater().apply()`（复用 self-update 现有逻辑：落盘新二进制；由 daemon graceful-drain 在在途答完后换新生效，不打断）。进度可选弹设置或托盘提示；最简：触发后由后续 `TrayState.pending` 反映「待生效」。

## 6. 宿主二进制换新 `（app/gui_host 内）`

- 信号来源：`TrayState` 增 `pending: bool`（daemon 既有「盘上二进制变化」探测，已在维护 `update.pending`）。宿主收到 `pending==true` 即知盘上有新二进制。
- 换新时机：仅当 `windows_open==0`（不打断在用窗口）。
  - **active**：`re-exec`（spawn 新 `--gui-host` + 释放锁 + 退出）或直接退出由 daemon 下次拉起。
  - **always**：交给登录项守护——macOS LaunchAgent `KeepAlive` 会在宿主退出后用**新二进制**重启；Linux 无 KeepAlive，宿主自我 `re-exec`（spawn 新自身 + 退出，新进程抢锁）。
  - always 下 daemon 因换新退出 → 宿主状态订阅断 → 图标转停止态 → 宿主**主动 `ensure_running`** 用新二进制拉起 daemon（仅 always 模式允许，为保持图标实时）→ 重连恢复。

## 7. 登录项集成 `src-tauri/src/integrations/login_item.rs`（仅 always）

- macOS：写/删 `~/Library/LaunchAgents/<bundle-id>.guihost.plist`（`ProgramArguments=[<exe>, --gui-host]`、`RunAtLoad=true`、`KeepAlive=true`）；`launchctl load/unload` 或 `bootstrap/bootout`（best-effort）。
- Linux：写/删 `~/.config/autostart/askhuman-guihost.desktop`（`Exec=<exe> --gui-host`、`X-GNOME-Autostart-enabled=true`）。
- 接口：`install()` / `uninstall()` / `is_installed()` / `needs_update()`（exe 路径变化时刷新）。由 §5.3 模式变化驱动；幂等纯函数 + 单测（路径计算/内容生成）。

## 8. 图标资产 `src-tauri/icons/tray/`

- macOS 模板（单色 + alpha，系统自动着色）：`idle`（空闲）、`active`（待答，带问号）、`stopped`（停止，带月亮），各含 `@2x`。Agent 集成提醒另有空闲 / 停止两张右上小实心圆变体；圆点周围留透明挖空，继续由系统随菜单栏自动着色。建议 18×18 / 36×36。
- Linux 彩色：`idle` / `active` / `stopped`，小尺寸（22~24px）。
- `include_bytes!` 内嵌（同 `icons/icon.png`），运行时按平台 + 状态选择。资产实现阶段产出（基于品牌图形派生）。

## 9. i18n `src-tauri/src/i18n`

- 新增菜单/状态文案键（en + zh）：标题行（运行中/未运行）、`N 个待答`、`Agent：工作/空闲`、`IM：`、`有可用更新`、`更新到 {v}（答完后生效）`、`检查更新`、`打开设置/历史/Agent 状态`、`启动/重启/停止 daemon` 等。
- 宿主语言 = `Lang::resolve(config.general.language)`；语言变更经 §5.3 重建菜单热切换。

## 10. IPC 增量 `src-tauri/src/ipc/mod.rs` + daemon `src-tauri/src/daemon/mod.rs`

`PROTOCOL_VERSION` 保持 1（增量、旧端忽略未知变体）。

- `ClientMsg::TraySubscribe`：宿主订阅状态（**非保活**）。
- `ServerMsg::TrayState { running, version, uptime_secs, active_requests, im_connections, draining, agents_working, agents_idle, update_available, update_latest, pending }`（变体名 camelCase、字段 snake_case，同二进制两端）。
- daemon：
  - `Control` 增 `TraySub`；`control_loop` 收 `TraySubscribe` → `return Control::TraySub`。
  - `handle_tray_sub`：注册 sender 到新 `tray_subs`、立即推一帧、读端探断开；断开清理。**不计入保活**：识别为 tray sub 后抵消其对 `active` 的占用（进入前 `fetch_sub(1)`、退出后 `fetch_add(1)`），且空闲判定（`active==0 && working_count==0 && !has_agent_subs`）**不引用 `tray_subs`**。
  - `broadcast_tray_state`：在既有中心点旁路调用（`handle_submit` 受理/完结、`attach_im_channels`/失效、`broadcast_agents_state` 同点、更新态广播同点、`begin_drain`）。无订阅者时廉价空操作。
  - **拉起宿主**：`serve()` 启动末尾与 `on_config_changed` 中，若 `general.menu_bar_icon != Off` 且 `tray_supported()` → `spawn_gui_host()`（detached，单实例锁去重）。`tray_supported()`：macOS 恒真；Linux 复用 `gui_available()` 保守门控。
  - 续命由「宿主窗口期普通连接」天然计入 `active` 实现，daemon 无需额外逻辑。

> 兼容性单测：参照 `update_state_roundtrip`，加 `TrayState` 往返 + 「旧端忽略未知变体」。

## 11. 弹窗导航路由改造 `src/views/PopupView.vue` + `commands.rs`

- 弹窗「设置 / 历史」按钮当前调 `open_settings`/`open_history`（在弹窗进程内 `create_*_window`）。改为**路由到宿主**：`open_settings`/`open_history` 命令内改为 `host_open(Settings/History)`（§4），实现全局单窗。
- 历史窗口的 `--all`、当前项目过滤等参数经 `OpenWindow` 字段传递。

## 12. 设置 UI `src/views/SettingsView.vue`

- 「通用」Tab 新增三态控件「菜单栏图标」：`关 / 活动时显示 / 一直显示`（分段控件或下拉）。
- 仅 macOS/Linux 显示（前端平台门控，Windows 隐藏，与「实验性功能」同款）。
- 绑定 `general.menuBarIcon`，经既有 `get_settings`/`save_settings` 读写；保存后 daemon `config_watch` + 宿主配置监听即时生效（拉起/移除图标、装/卸登录项）。

## 13. Cargo 依赖与特性 `src-tauri/Cargo.toml`

- `tauri` 增特性：`tray-icon` + `image-png`（即 `["macos-private-api","tray-icon","image-png"]`）。
- Linux 运行时托盘依赖 `libayatana-appindicator3`（dlopen，best-effort；缺失即无图标）。
- macOS accessory 经 Tauri `set_activation_policy`，无需新增依赖。

## 14. 文档

- 实现完成更新 `docs/overview.md`：新增「GUI 宿主进程 + 菜单栏图标」小节（角色 `--gui-host`、三态、生命周期/续命规则、宿主自有 IPC、`TraySubscribe`/`TrayState`、非保活约定、登录项、二进制换新、资产、平台门控），并在目录树补 `gui-host.sock`/`gui-host.lock`/`icons/tray/`/`integrations/login_item.rs`。
- 清理 `docs/PROGRESS.md` 本任务条目。

## 15. 建议实施阶段（便于增量验证）

- **P1 宿主骨架 + 单窗路由**：`--gui-host` 进程 + flock + 宿主 IPC（OpenWindow）；改造 `--settings`/`--history`/`agents monitor` 与弹窗导航路由到宿主；验证全局单窗（此阶段可不带图标、不带 daemon 集成）。
- **P2 托盘图标 + 状态**：`TraySubscribe`/`TrayState` + 菜单 + 三态图标；daemon 拉起宿主（active）；状态订阅非保活 + 窗口期续命连接；菜单操作（打开窗口/重启/停止/检查更新/应用更新）。
- **P3 三态 + always**：三态设置 UI；always 常驻 + 停止态图标 + 被动重连；登录项安装/卸载。
- **P4 二进制换新 + 收尾**：宿主换新（re-exec / launchd KeepAlive）；i18n 全量；overview 文档；跨平台冒烟。

## 16. 验证

- `./scripts/install.sh` 编译安装；用新 `AskHuman` 自测：
  1. 三种入口重复开设置/历史/Agent → 始终单窗。
  2. active：提问→图标出现+圆点；静置至 daemon 空闲退出→图标消失（确认状态订阅不续命，可 `ASKHUMAN_DAEMON_IDLE_SECS` 调短）。
  3. 续命：开着设置窗时 daemon 不退；关窗后重新计时退出。
  4. always：图标常驻；daemon 退出→图标转停止态、菜单「未运行」+「启动 daemon」；重启系统后图标在（登录项）。
  5. 菜单更新（不打断在途）、语言热切换、宿主二进制换新（无窗口时切新版）。
  6. Linux 桌面冒烟；headless 不显示、不报错。
- `cargo test`：IPC 往返 + config 默认值 + 登录项纯函数单测。

## 17. 风险与缓解

- **状态订阅把 daemon 续命**（最关键回归）：tray sub 净占用 `active=0` 且空闲判定不引用 `tray_subs`（§10）；续命只由「窗口期普通连接」承担。专项验证（§16.2/16.3）。
- **菜单线程安全**：Tauri 菜单/图标更新需主线程；订阅任务经 `run_on_main_thread` 回主线程。
- **彻底路由的回退**：宿主拉起/连接失败时 `host_open` 兜底（极端情况下本进程直接建窗），保证「至少能打开窗口」。
- **always 资源**：常驻一个宿主进程（+ 偶发 daemon）；属用户显式选择，可接受。
- **Linux 托盘/自启不一致**：best-effort + 平台门控 + 文档说明；不阻塞 macOS 主路径。
- **二进制换新竞态**：单实例锁交接（旧宿主释放→新宿主抢锁，必要时短暂重试）；always 优先借 launchd KeepAlive 简化。
- **generate_context! 单次展开**：宿主复用 `launch()`，不另开 Builder。
