# 需求：菜单栏状态图标 + 统一 GUI 宿主进程

> 状态：已实现（macOS/Linux/Windows 桌面；Linux 托盘 best-effort）。
> 关联计划：`docs/plans/menu-bar-tray.md`
> 影响面：新增单实例 **GUI 宿主进程**（隐藏角色 `--gui-host`，承载托盘图标 + 设置/历史/待办/Agent/Interject/任务窗口）；改造既有窗口入口（CLI `--settings`/`--history`/`agents monitor` 与弹窗导航按钮）为「路由到宿主」；daemon 生命周期（按需拉起宿主）与 IPC 协议（增量，PROTOCOL_VERSION 不变）；`config.rs` 新增三态 `general.menuBarIcon`；新增宿主自有 IPC（Unix `gui-host.sock` / Windows named pipe）；新增三平台登录项集成（仅「一直显示」）；新增托盘图标资源；设置页「通用」Tab。**不改**正常提问流程的 stdout 契约、退出码语义、既有 IPC 兼容性。

> **实现期补充（2026-07）**：GUI Host 后续增加每 session 唯一的 Interject 窗口；`TrayState` 增加待答
> 请求与 Agent 摘要，使菜单可聚焦 Popup、打开插话 composer 或聚焦终端。托盘菜单由 `app/tray_menu.rs`
> 按稳定 key 做最小 diff，daemon socket 用文件监听唤醒重连。`general.daemonLifecycle=keepalive` 是与
> `menuBarIcon` 正交的 daemon 常驻策略，GUI Host 与 daemon 使用各自登录项。
>
> **渠道故障可见化（R7，2026-07）**：daemon 进程内新增渠道健康登记表（`channels/health.rs`）——四家
> IM 客户端的统一请求出口失败即登记、该渠道下一次任何成功操作即清除（纯内存态，daemon 重启即清）。
> 快照经 `TrayState.channel_issues` / `StatusInfo.channel_issues` 下发（旧端缺字段 → 空，不显示）；
> 登记表内容变化即推一帧 TrayState。托盘状态区逐渠道显示可点击的「⚠ 渠道异常（时间）」行，点击打开
> 设置窗口并定位渠道 tab（新开窗经 URL `?tab=`、已开窗经 `settings-goto-tab` 事件）；设置页渠道卡
> 顶部显示错误横幅，渠道 tab 可见期间每 10s 轻量轮询刷新。
>
> **D11 补充（宿主换新时效，2026-07）**：除 15s 轮询外，**最后一个窗口关闭时立即**检查并换新
> （off 模式除外——宿主本就随之退出）；盘上二进制已换新但**有窗口挡住**自动换新时，托盘菜单出现
> 「重启菜单栏应用以完成更新」项（`binary_stale` 标记入菜单签名，换新完成后自动消失），点击即
> 释放单实例锁并重启宿主（always 交 launchd KeepAlive，其它自我 re-exec；窗口随之关闭，用户知情选择）。
>
> **D8/D10 补充（daemon 换新可解释性，2026-07）**：菜单内主动更新落盘后，立即刷新 daemon
> 更新快照并发送常规 Hello 指纹握手，不等待 15s 轮询；未运行 daemon 不为此额外拉起。状态区保留
> 真实的运行中 daemon 旧版本，并紧接一行说明目标版本与当前原因：有在途时显示等待数量及“不打断
> 作答”，排空完成时显示正在重启，daemon 已停时说明下次使用将启动新版。普通 stop/restart drain
> 仍显示通用「正在完成在途请求」。
>
> **D8/D11 补充（daemon 停止时更新，2026-07）**：GUI Host 不依赖 daemon 完成手动检查与安装。
> Host 启动时从 `update.json` 恢复有新版状态；手动检查即时改写菜单，并用菜单内只读状态行显示
> 「正在检查 / 已是最新版 / 检查或安装失败原因」，执行中禁用重复操作。daemon 恢复后的旧快照与本地
> 检查结果合并，不得把刚查到的更新入口覆盖掉。Host 换新使用更新前缓存的稳定可执行路径，避免 Linux
> 替换运行中 inode 后 `current_exe()` 指向 `(... deleted)`；新 Host 拉起失败时旧 Host 保持运行并显示错误。
>
> **D8/D10 补充（检查结果一致性，2026-07）**：设置页与菜单检查共享 `update.json`，写操作持
> `update.lock`，缓存版本只前进不后退。设置页检查成功须即时刷新 Host 菜单；若 daemon 正在运行，
> 通过增量 IPC 令其立刻重载并广播给弹窗（不运行则不拉起）。菜单检查成功后保留“已检查到新版本
> vX.Y.Z”或“已是最新版”结果行，不能因菜单点击后自动关闭而让用户看不到任何反馈。
>
> **Agent 集成更新提醒（2026-07）**：GUI Host 启动时检查当前四家 Agent 集成，覆盖版本换新后的
> 首次检查；daemon 每次从停止变为运行时复查。判定与设置 Agents tab 的 `agent_mode::needs_update`
> 完全同源。存在待更新项时，托盘状态区显示一条可点击灯泡提示：单项列 Agent 名，多项只显示数量，
> 点击直达 Agents tab；
> 没有待答时，template 图标右上显示带透明挖空的小实心圆；有待答则仍优先显示问号。设置页成功
> 更新集成后立即复查并清除提醒，无需新增 daemon IPC。

## 1. 背景与动机

守护进程（daemon）常驻后台收发渠道消息、协调抢答、追踪 agent，但用户**看不到它在做什么**。希望在 macOS 菜单栏（Linux 桌面托盘）提供一个默认常驻的小图标，展示运行状态（是否在跑、几个待答、IM 连接、agent 忙闲、有无更新），并能快速打开设置 / 历史 / Agent 状态、检查/应用更新、重启/停止 daemon。

讨论中进一步明确：既然要新增一个常驻 GUI 进程，索性让它**统一承载所有辅助 GUI 窗口（设置 / 历史 / Agent 状态）**，从而保证**每类窗口全局唯一**（重复点击只聚焦、不重复弹出），并让「菜单内更新」「菜单语言热切换」等都顺理成章。

### 关键架构约束

daemon 是**刻意「无 GUI」**的：不初始化 AppKit/GTK/Win32 WebView、不占主线程（见 `docs/overview.md`）。而菜单栏/托盘图标（macOS `NSStatusItem`、Linux StatusNotifierItem、Windows notification area）必须在主线程跑 GUI 事件循环。因此图标与窗口都必须放在**独立 GUI 进程**里，经用户私有的 Unix socket 或 Windows named pipe 与 daemon 通信。

## 2. 目标（一句话）

提供一个默认“一直显示”的**三态**开关（关 / 活动时显示 / 一直显示）；在菜单栏/托盘显示状态图标并提供状态与快捷操作；同时由一个**单实例 GUI 宿主进程**统一承载设置/历史/Agent/Interject 窗口，保证全局每类唯一。已有配置中的显式选择保持不变。

## 3. 已确认决策

| 编号 | 决策项 | 结论 |
|---|---|---|
| D1 | 平台范围 | **macOS + Linux + Windows 桌面**（Tauri 跨平台托盘）。Linux 为 **best-effort**：需图形会话（DISPLAY/Wayland）+ 托盘宿主（StatusNotifierItem/appindicator）；headless 服务器无图标、不报错、不影响 daemon。Windows 正式范围为单个交互式桌面会话；RDS 多会话 broker 不在本期。 |
| D2 | 统一 GUI 宿主进程 | 新增**单实例**宿主进程（隐藏角色 `AskHuman --gui-host`），全局承载**设置 / 历史 / Agent 状态 / Interject**窗口（每类或每 session 唯一）+ 菜单栏状态项。daemon 保持无 GUI、不承载任何窗口。用 **Tauri 2 tray-icon**（原生菜单）。macOS 设 **accessory**（不占 Dock/Cmd-Tab）。 |
| D3 | 彻底路由（全局单窗，Q2=彻底） | 所有打开窗口的入口都路由到宿主：CLI `AskHuman --settings`/`--history`/`agents monitor`、**弹窗导航栏「设置/历史」按钮**、托盘菜单项。宿主在则聚焦/新建对应窗口，不再各开进程。 |
| D4 | 三态开关 | `general.menuBarIcon` ∈ `off` \| `active` \| `always`（默认）。设置「通用」Tab，三平台均显示。配置缺字段时采用 `always`，已有显式值保持不变。<br>• **off**：无图标（宿主仍按需承载窗口，保证单窗）。<br>• **active（活动时显示）**：daemon 运行时显示图标；无窗口且 daemon 空闲退出后图标消失、宿主退出；不装登录项。<br>• **always（一直显示）**：图标常驻（宿主开机自启 + 常驻）。**图标本身不给 daemon 保活**，daemon 仍按空闲规则退出；退出后图标仍在但显示「停止」态，宿主被动重连，daemon 再次运行即恢复实时态。 |
| D5 | 窗口续命 daemon（统一规则） | **任意 GUI 窗口打开期间**给（**正在运行的**）daemon 续命；**最后一个窗口关闭后** daemon 重新计时、到点空闲退出。**图标本身从不续命**。打开窗口**不会主动启动** daemon（Agent 状态窗口除外——它需要 daemon 数据，会 `ensure_running` 拉起）。该规则与 tray 模式、与是否显示图标无关。 |
| D6 | 图标三态（视觉，Q5 扩展） | ① **运行·空闲**：单色模板图（macOS 随明暗自动着色）。② **运行·有待答**：带小圆点/高亮变体。③ **（仅 always）daemon 已停止**：可区分的「停止」态（暗淡/空心变体）。**待答数量在菜单内显示**（不在图标上叠数字）。 |
| D7 | 菜单内容 | **状态区（只读）**：daemon 运行中/未运行 + 版本 + 运行时长；N 个待答；Agent 工作/空闲数（有数据时）；已连接 IM（有时）；有可用更新（有时）。**操作区**：打开设置 / 打开历史 / 打开 Agent 状态；**检查更新**，有更新时「更新到 vX.Y（答完后生效）」点击即换新；daemon 运行时「重启 daemon」「停止 daemon」，daemon 停止时（always 态）显示「启动 daemon」。 |
| D8 | 菜单内更新（Q4） | 复用现有更新逻辑（宿主是 GUI 进程，可直接调用 `update::check/apply`）。点「更新」即落盘新二进制，并即时用 Hello 指纹握手触发已运行 daemon 换新；空闲时立即退出换新，有在途时由既有 **graceful-drain** 在答完后生效，**不打断作答**。15s 指纹轮询保留为兜底。 |
| D9 | 菜单语言热切换 | 宿主监听配置变更（界面语言）→ **即时重建菜单为新语言**。 |
| D10 | 状态新鲜度 | daemon 新增「**托盘状态订阅**」（**非保活**）：宿主连上即推一帧整合 `TrayState`，之后相关变化即推（提问受理/完结、IM 连接变化、agent 变化、更新态、进入排空）。图标据 `active_requests` 与「daemon 在否」切换三态；菜单文字用最近一帧。更新待生效时，运行中版本下方据 `pending + draining + active_requests` 显示具体等待原因。**该订阅不计入 daemon 保活**（见 D5）。 |
| D11 | 宿主二进制换新 | 宿主长寿（尤其 always），需随二进制更新换到新版。复用 daemon 的「盘上二进制变化 → pending」信号（经 `TrayState` 下发）。检测到新二进制后，**在『无打开窗口』时换新**（不打断在用窗口）：always 经 launchd `KeepAlive`/autostart 重启或自我 re-exec；active 自我 re-exec 或由 daemon 下次拉起。always 模式下 daemon 因换新退出后，宿主立即重新拉起新版 daemon 并重连。 |
| D12 | 开机自启（仅 always，Q2=含自启） | 切到 **always** 安装登录项、切走移除：macOS `~/Library/LaunchAgents/<id>.plist`（`RunAtLoad`+`KeepAlive`）；Linux `~/.config/autostart/<id>.desktop`；Windows `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 指向 AskHuman 受管 `wscript.exe //B //NoLogo` launcher，再以 hidden window style 拉起 GUI Host，避免 console-subsystem EXE 在登录时闪窗。切走/卸载会精确移除 Run value 与受管 launcher。使「重启系统后/daemon 没起时」图标也一直在。 |
| D13 | 单实例 + 宿主自有 IPC | 宿主用跨进程 `gui-host.lock` 单实例。宿主自带 IPC（Unix `gui-host.sock` / Windows named pipe，复用 NDJSON 编解码）接收「打开窗口/关闭/刷新」请求——**与 daemon 解耦**，使 daemon 未运行时也能打开设置/历史。Windows endpoint 继承与 daemon 相同的 SID/会话/配置目录隔离和受保护 DACL。宿主另作为 daemon 客户端：一条**非保活**状态订阅 + 「有窗口时」一条**计活**保活连接（实现 D5）。 |
| D14 | daemon 集成 | `menuBarIcon != off` 时，daemon 启动 / 配置变更尝试拉起宿主（单实例去重，作兜底；always 主要靠登录项）。新增 `ClientMsg::TraySubscribe` / `ServerMsg::TrayState`（非保活）。`PROTOCOL_VERSION` 保持 1（增量、旧端忽略未知变体）。 |
| D15 | Agent 集成更新提醒 | GUI Host 启动与 daemon 停→运行时按 Agents 设置页同一口径复查；待更新时显示可点击单行灯泡提示，单项列 Agent 名、多项只显示数量。无待答时 template 图标右上显示带挖空的小实心圆，有待答时问号优先。点击打开 Agents tab；设置内更新成功后即时清除。状态由 Host 本地缓存，不扩展 `TrayState`。 |

## 4. 约束与既有规则（不可破坏）

- **daemon 保持无 GUI**：图标与窗口全在宿主进程；daemon 不初始化 AppKit/GTK。
- **状态订阅不给 daemon 保活**（D5/D10 核心）：否则 active/always 的「daemon 仍空闲退出」语义被破坏。只有「打开的窗口」经独立计活连接续命 daemon。
- **stdout 洁净 / 退出码契约**不变；宿主/托盘日志走 stderr / 日志文件。
- **IPC 增量演进**：`PROTOCOL_VERSION` 保持 1，新增变体旧端忽略（参照 graceful-drain / self-update）。
- **默认 always、显式值兼容**：新配置缺字段时为 `always`；已有用户的显式 `off|active|always` 保持不变。off 模式不显示图标、不装登录项、不常驻（宿主仅在有窗口时短暂存在）。
- **彻底路由后**：弹窗导航「设置/历史」不再在弹窗进程内建窗，改为路由到宿主（行为上仍是打开对应窗口，只是全局唯一）。
- **Linux / headless 静默降级**：无图形会话/托盘宿主不可用时不显示图标、不报错。
- **不打断在途**：菜单内更新只落盘，换新交既有 drain；宿主二进制换新仅在「无打开窗口」时进行。

## 5. 验收标准

1. **默认 off**：全新/未开启时，行为与现状一致，无图标、无登录项、无常驻宿主。
2. **全局单窗（D3）**：无论从 CLI（`AskHuman --settings`）、弹窗导航「设置」按钮、还是托盘菜单，重复打开「设置」始终只有一个窗口（再次触发即聚焦）；历史 / Agent 状态同理。
3. **active 模式**：开启「活动时显示」→ 发起提问 daemon 运行 → 菜单栏出现图标（不占 Dock）；未答时图标带圆点、菜单「N 个待答」；答完圆点消失。无窗口静置至 daemon 空闲退出 → 图标消失、宿主退出。**确认状态订阅没有把 daemon 续命**（空闲退出仍按时发生）。
4. **窗口续命（D5）**：daemon 运行时打开设置窗并停留 → daemon 不空闲退出、图标稳定不闪；关闭最后一个窗口后 daemon 重新计时、到点退出。
5. **always 模式**：开启「一直显示」→ 装登录项；图标常驻。无窗口静置 → daemon 空闲退出，但**图标仍在并切换为「停止」态**（D6③），菜单显示「未运行」且「停止 daemon」变为「启动 daemon」；之后发起提问/打开 Agent 窗口 → daemon 运行 → 图标恢复实时态。重启系统后图标自动出现（登录项）。
6. **菜单操作**：打开设置/历史/Agent 状态各聚焦/打开唯一窗口；「检查更新」可用；有更新时「更新到 vX.Y」点击触发换新且不打断在途作答（落盘后 drain 生效）。daemon 控制项随运行态正确切换（重启/停止 ↔ 启动）。
7. **语言热切换（D9）**：界面语言切换后，菜单文字即时变为新语言。
8. **宿主二进制换新（D11）**：更新落盘后，宿主在「无打开窗口」时换到新版（always 下经登录项/KeepAlive 或自我 re-exec）；有窗口时不打断、待窗口关闭后换新；always 下 daemon 换新退出后图标短暂「停止」再恢复，最终 daemon 与宿主均为新版。
9. **Linux 桌面**：支持托盘的桌面环境功能同 macOS；headless 开启 → 无图标、无报错、daemon 正常。
10. **Windows**：三态设置、托盘菜单、窗口单例、named-pipe 路由与 HKCU Run 登录项行为与 D1–D15 一致；注销后后台角色不跨交互会话残留。Win11 自动重登实测中 Explorer 从 Session 2 重建为 Session 1，GUI Host、daemon 与 warm popup 均在新 session 恢复；登录前 6 秒的 60 帧桌面截图和逐帧可见窗口审计没有出现 conhost/PowerShell/cmd/AskHuman 控制台窗口。RDS 多会话不在本验收范围。
11. **回归**：提问/抢答/drain/自更新/IM 自动激活/历史/Agent 订阅等既有功能不受影响；daemon 空闲退出/指纹换新/排空语义不变。
12. **Agent 集成提醒（D15）**：制造任一当前模式产物过期后，换新 GUI Host 或启动 daemon，菜单出现灯泡提示（单项列名、多项计数）；无待答时图标显示右上实心圆，有待答时仍显示问号；点击定位 Agents tab；在设置中更新后提示与圆点立即消失。

## 6. 访谈反馈记录（按时间，供追溯）

> 以下为方案访谈中用户逐步给出的意见，已并入上方决策；保留原始记录便于回溯。

1. 初版（纯托盘）选择：生命周期严格跟随 daemon（C）、macOS+Linux、各窗口独立进程、菜单内容若干、图标带待答圆点。
2. 反馈：① 重复点击不能重复弹窗；② 菜单内也要加「更新」；③ 菜单要做语言切换；④ **提议把所有 GUI 窗口都放进新进程以保证全局单窗** → 采纳为「统一 GUI 宿主进程」，并彻底路由所有入口（D2/D3/D8/D9）。
3. 关于生命周期：用户提出「**GUI 窗口打开就给 daemon 续命，最后一个窗口关闭后 daemon 再计时退出**」，并指出「进程与图标必须解耦（不开图标时 GUI 仍要这个进程）」 → 采纳为 D5。
4. 用户进一步提出**三态开关**（off / 活动时显示 / 一直显示），并问宿主是否也需要 daemon 那样的二进制换新逻辑 → 采纳三态（D4）+ 宿主二进制换新（D11）；always 含开机自启（D12）。
5. 澄清「一直显示」：**无窗口时图标不给 daemon 保活，daemon 仍空闲退出，仅图标常驻（显示停止态）** → 确认并入 D4/D5；daemon 停止时菜单「停止 daemon」改为「启动 daemon」。
6. 追加：**always 模式下 daemon 退出时，图标要有可区分的视觉区别**（停止态）→ 并入 D6③。
