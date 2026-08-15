# 需求：引入常驻 Daemon 架构（CLI 瘦客户端 + Daemon + GUI Helper）

> 状态：已实现（macOS/Linux/Windows）；三平台共用 daemon 业务核心，Windows 使用安全 named pipe 与原生进程/锁适配。
> 关联计划：`docs/plans/daemon-architecture.md`
> 影响面：**全局架构级**。这不是单一渠道需求，会重塑运行模型，影响后续所有需求。

> **当前实现补充（2026-07）**：本文保留最初迁移决策与协议设计。后续已加入 Slack Router、Popup
> 预热、graceful drain、主动 IM 入站和统一 GUI Host；当前代码地图见 `docs/overview.md`，专项边界见
> `docs/specs/popup-prewarm.md`、`docs/specs/daemon-graceful-drain.md`、`docs/specs/menu-bar-tray.md`。
> 本文 §6 的示例帧是设计期协议草图，当前消息真值以 `src-tauri/src/ipc/mod.rs` 为准。
>
> **Windows 对齐补充（2026-08）**：`daemon/unix_impl/` 目录名保留作历史兼容，但其中 server、Router、
> Coordinator、订阅与主动命令已经是共享 core。`ipc::transport` 在 Unix 使用私有 socket，在 Windows
> 使用按当前用户 SID、会话与配置目录隔离的 byte-mode named pipe；pipe DACL 仅允许当前用户和
> LocalSystem，并拒绝远程客户端。后台启动、HKCU Run 登录项、进程识别与文件锁由平台适配器承接。

## 1. 背景与动机

迁移前 `AskHuman` 是**单进程**模型：每次 CLI 调用都是一个独立进程，进程内按配置并行启动各 Channel（本地弹窗 popup / Telegram / 钉钉 / 飞书），首个终态结果生效（抢答），输出后退出。

这个模型有两类根本性问题：

1. **长连接无法独占共享**。钉钉官方限制「同一 client-id 同一时刻只允许一条 Stream」，飞书长连接同理。当前每个 `AskHuman` 进程各自开一条长连接，**连续快速 / 并发提问时**多条连接会相互抢消息，可能把用户回复投递到错误的进程。这是「每进程各自持连接」的结构性缺陷，给单个渠道打补丁（文件锁等）只能缓解，无法统一根治。

2. **无法在「没有提问」时接收渠道消息**。单进程模型只有在被调用、且仅在该次提问存续期间才监听渠道，因此做不到「用户在 IM 里主动发消息触发一个任务」这类能力（未来方向）。

引入一个**每用户一个、常驻的 Daemon**，由它独占持有所有 IM 长连接、承载渠道与抢答协调，可一次性解决问题 1（对所有现有及未来渠道统一生效），并为未来「渠道主动发起任务」打下基础。

## 2. 目标

- 把 IM 类 Channel（Telegram / 钉钉 / 飞书 / Slack）与抢答协调器迁入常驻 Daemon；每种长连接全局仅一条、常热复用。
- `AskHuman` CLI 退化为**瘦客户端**：解析入参 → 提交任务给 Daemon → 流式取回结果打到 stdout → 按终态映射退出码。
- 弹窗 GUI 拆为**独立的短命进程 GUI Helper**，使 Daemon 自身不必跑 GUI 事件循环。
- 维持**单一可执行文件**：同一二进制按子命令/参数切换角色。
- 保持所有既有对外契约不变（stdout 洁净、结果区块格式、退出码、配置容错、向后兼容等）。
- 解决「同 client-id/app 多开长连接互抢」问题（长连接单实例）。

## 3. 架构总览

核心提问链路的三类进程（均为同一个 `AskHuman` 二进制的不同角色），经本地 IPC 通信。后续增加的
GUI Host 负责设置、历史、Agent、Interject 窗口与托盘，不进入每次提问的数据面：

```
                 钉钉 Stream        飞书 WS        Telegram
                  (WSS)             (WS)           (HTTPS)
                    ^                 ^               ^
                    |  每种仅「一条」长连接（按 client_id/app 唯一，常热）
                    +--------+--------+-------+-------+
                             |                |
            ============================================================
            ||  AskHuman Daemon    askhuman daemon run                ||
            ||  每用户 1 个 · 常驻 · 无 GUI（不初始化 Tauri/AppKit）   ||
            ||                                                        ||
            ||  长连接 Router：按 out_track_id / user_id 把消息        ||
            ||  分发给「正确的那个请求」                               ||
            ||                                                        ||
            ||  每个活动请求 request_id 一套：                         ||
            ||    Coordinator + Preemption（首个终态生效，其余收尾）   ||
            ||      |- Telegram / DingTalk / Feishu Channel（进程内）  ||
            ||      +- Popup Channel(adapter) --spawn--> GUI Helper    ||
            ||                                                        ||
            ||  共享：Config(watch 实时生效) · Token 缓存 · daemon.log ||
            ||  IPC Server（Unix socket / Windows pipe，用户私有0600） ||
            ============^=======================================^=======
                        | CLI<->Daemon: 任务契约                | Daemon<->GUI: 渲染+收集
                        | 提交 AskRequest/流式结果/退出码       | show / answer / cancel / configChanged
              +---------+------+   +--------------+       +------+-----------------+
              | AskHuman CLI   |   | AskHuman CLI | ...   | GUI Helper(Tauri 窗口) |
              | (瘦客户端)     |   | 每调用一进程 |       | askhuman --popup       |
              | 解析入参->提交 |   | 短命         |       | 每个弹窗请求一进程     |
              | 打印 stdout    |   |              |       | 自己主线程跑GUI,答完退 |
              | 退出码 0/1/3   |   |              |       |                        |
              +----------------+   +--------------+       +------------------------+

   设置窗口（askhuman --settings）：独立 GUI 进程，不经 Daemon（它不需要渠道/协调器）。
```

一次提问的流转（Design A：Daemon 集中编排）：

1. 用户运行 `AskHuman ...`，CLI 解析入参为 `AskRequest`（`-f` 在 CLI 内解析为绝对路径，文件不存在即退 1）。
2. CLI 连 Daemon（不在则 detach 拉起 + 握手）；提交 `AskRequest`，Daemon 分配 `request_id`。
3. Daemon 为该请求建一套 Coordinator：IM Channel 作为进程内任务（共享那条长连接，经 Router 按 `out_track_id`/`user_id` 分流）；Popup Channel 则 spawn 一个 GUI Helper 进程并经 IPC 下发题目。
4. 任一渠道先到终态 → Coordinator 采纳它、cancel 其余（通知 GUI Helper 收尾、取消 IM 任务）。
5. Daemon 跑 `emit_result`（图片落盘到临时目录）→ 把渲染好的结果文本与退出码流式回 CLI；CLI 原样打印 stdout 并退出；GUI Helper 答完即退。
6. CLI 被 Ctrl-C / 杀掉 → socket 断开 → Daemon 取消该请求并清理 GUI + IM 任务。

## 4. 已确认决策

| 编号 | 决策项 | 结论 |
|---|---|---|
| A1 | 总体架构 | **Design A**：Daemon 集中承载 IM Channel + Coordinator；GUI Helper 独立进程；CLI 瘦客户端。**所有 ask 都走 Daemon**（含纯弹窗场景）。 |
| A2 | 交付形态 | **单一可执行文件**，按子命令/参数切换角色（busybox 风格）。`daemon run/start/stop/restart/status/logs`；隐藏 `--popup` 为 GUI Helper；`--settings` 沿用现有 GUI。 |
| A3 | 进程角色 | CLI（多、短命）/ Daemon（每用户 1、常驻、无 GUI）/ GUI Helper（每弹窗 1、短命）/ Settings（独立 GUI，不经 Daemon）。 |
| A4 | 长连接归属 | 各 IM 长连接由 Daemon **全局独占持有**（钉钉按 client_id、飞书按 app 唯一），常热复用；进程内 Router 按 `out_track_id`/`user_id` 把消息分发给对应请求。← 根治 TODO 问题 1。 |
| A5 | IPC 传输 | NDJSON（一行一个 JSON 消息）over Unix domain socket（mac/Linux）/ Windows named pipe；权限用户私有（0600 / pipe ACL）。 |
| A6 | 任务契约复用（D-D） | CLI↔Daemon 与 Daemon↔GUI 复用同一套「任务契约」（`show` 是 `submit` 的子集），减少协议面。 |
| A7 | GUI Helper 鉴权（D-E） | Daemon spawn GUI Helper 时下发**一次性 token**（argv/env），GUI 连回出示，防本机其它进程冒充。 |
| A8 | stdout 洁净 / 警告路由（D-C） | Daemon 事件分型：`result/stdout` 类 → 该请求对应 CLI 的 **stdout** 原样打印；`warn/log` 类 → 该 CLI 的 **stderr** + `daemon.log`。daemon 自身永不污染任何 stdout。 |
| A9 | 附件传递（D-A） | 同机同用户同文件系统 → 传**路径**而非字节。Daemon 跑 `emit_result` 集中落盘（图片 base64 由各渠道/GUI Helper 回传 Daemon 统一写盘）；CLI 只打印。 |
| A10 | 临时文件清理 | Daemon 常驻，**启动时 + 定期**清理过期的 `temp/askhuman/<request_id>/` 目录（修历史「从不清理」的小泄漏）。 |
| A11 | 请求上下文（D-B） | `-f` 解析留在 CLI（输出绝对路径 → **不传 cwd**，并保留「文件不存在即退 1」）；**硬性上送 source name**（来自调用方环境变量 `ASKHUMAN_ENV_SOURCE_NAME`）；上送 CLI 解析好的 `lang`（使 `auto` 跟随调用方而非 Daemon）；`request_id` 由 **Daemon 分配**（权威，用于临时目录）。 |
| A12 | 配置实时生效（D-F） | Daemon **监听 `~/.askhuman/config.json`**（`notify`，去抖、处理原子写/rename）：变更即重载 + 比对差异重连/增删 IM 连接 + 刷新缓存；并经 Daemon↔GUI IPC 向活动 GUI Helper 下发 `configChanged` → 弹窗实时切主题/语言。独立设置窗口仍在自身进程内更新自己。 |
| P1 | 自启 + 单实例 | CLI 连不上 endpoint → 拉起 `daemon run`，轮询等可连再连。macOS 优先 bootstrap 到当前 `gui/<uid>` launchd domain；无 GUI domain 的 headless 环境回退 `setsid`。Linux detach；Windows 用 `CREATE_NO_WINDOW` / 新进程组后台启动。`daemon run` 先抢跨进程 `daemon.lock`：抢不到=已有活 Daemon → 退出（`start` 幂等）；Unix 抢锁后清理 stale socket，Windows named pipe 无残留文件。`keepalive` 分别使用 launchd、autostart desktop 或 HKCU Run。 |
| P2 | 版本治理 | 两层并存：`protocol_version`（管 IPC 不兼容）+ **二进制内容指纹**（内容哈希，按路径/mtime/size 缓存，路径或时间变化不等于内容变化）。指纹/协议不一致 → Daemon graceful drain 后退出，CLI 用新二进制重新拉起。开关 `ASKHUMAN_DAEMON_AUTORESTART=0` 可关闭自动换新。 |
| P3 | 空闲退出 | 默认 `general.daemonLifecycle=activity`：无在途请求、无工作中 Agent、无活跃 watch/需保活窗口连接时按空闲策略退出；人在环里的题不会被空闲计时杀掉。`keepalive` 模式跳过空闲退出并安装登录项。 |
| P4 | 崩溃中途 | Daemon 在请求中途意外死亡 → CLI 连接断开 → 该请求判失败，CLI **退出码 3 + 明确报错**（不静默重试交互题）。 |

## 5. 进程角色详述

### 5.1 AskHuman CLI（瘦客户端）

- 解析 argv（沿用现有 `cli::args` / `file_attachment` / 帮助/版本），构造 `AskRequest`；`-f` 在此解析为绝对路径并校验存在性（缺失即退 1，不进 Daemon）。
- 捕获请求上下文：source name（环境变量）、解析好的 `lang`、`is_markdown`。
- 连接 Daemon（不在则自启 + 握手；指纹/协议不一致则触发换新后重连）。
- 提交请求；接收 `warn`（→ stderr）与最终 `final{stdout, exitCode}`（stdout 原样打印），按 `exitCode` 退出。
- 被信号中断 → 关闭连接（Daemon 据此取消请求）。
- 纯信息命令（`--help`/`--version`）仍本地直接输出，不连 Daemon。

### 5.2 AskHuman Daemon（`daemon run`）

- **无 GUI**：永不调用 `tauri::Builder`/初始化 AppKit；是个普通后台进程。
- IPC Server：接受 CLI 连接（提交任务）与 GUI Helper 连接（出示 token）。
- 请求管理：每个活动 `request_id` 一套 `Coordinator` + `Preemption` + 一组 Channel 任务（复用现有 `run_conversation` / `MessagingChannel` / `Channel` 抽象）。
- 长连接持有 + Router：每种 IM 一条长连接，按 `out_track_id`（卡片回调）/`user_id`（聊天图片/文件）路由到对应请求；钉钉卡片回调的 3 秒 ACK 由 Daemon 处理（空响应即时 ACK，卡片灰显走 OpenAPI updateCard）。
- `emit_result`：集中落盘 + 渲染结果文本，回传发起该请求的 CLI。
- Config watch：见 A12。
- 生命周期：见 P1–P4；日志写 `~/.askhuman/daemon.log`。

### 5.3 GUI Helper（`askhuman --popup`）

- 由 Daemon spawn（带一次性 token + IPC endpoint）；在自己的主线程跑 Tauri 弹窗（沿用现有 `PopupView` 与窗口创建/毛玻璃/主题逻辑）。
- 收 `show`（题目 + 请求上下文）→ 展示并收集答案（选项/文字/图片 base64/文件路径）→ 回 `answer`；被抢答收 `cancel{winner}` → 展示「已在 X 回答」并关窗；收 `configChanged` → 实时切主题/语言。
- 窗口外观配置（主题/置顶/尺寸/动效）由 GUI Helper **自行读取 config**（同二进制可读），Daemon 只发题目 + token + 必要上下文。
- 答完 / 被取消 / 连接断开即退出。

### 5.4 设置窗口（`askhuman --settings`）

- 维持独立 GUI 进程，不经 Daemon。写盘 config 后，Daemon 经 config watch 感知并生效（A12）。

## 6. IPC 协议（NDJSON，推荐字段；细化见计划）

### 6.1 CLI ↔ Daemon

- `hello`（CLI→D）：`{type:"hello", protocolVersion, clientVersion, binaryPath, binaryFingerprint:{mtime,size}, pid}`
- `helloAck`（D→CLI）：`{type:"helloAck", protocolVersion, daemonVersion, status:"ok"|"restarting", reason?}`；`restarting` 时 CLI 等待并重连新 Daemon。
- `submit`（CLI→D）：`{type:"submit", request:{questions[], message:{text, files[]}, isMarkdown, source, lang}}`
- `accepted`（D→CLI）：`{type:"accepted", requestId}`
- `warn`（D→CLI，流式）：`{type:"warn", text}` → CLI stderr。
- `final`（D→CLI）：`{type:"final", stdout, exitCode:0|1|3}` → CLI 原样打印 stdout 后退出。
- 取消：CLI 断开连接即视为取消（无需显式帧）。

### 6.2 Daemon ↔ GUI Helper（复用任务契约）

- 启动：`askhuman --popup --endpoint <socket> --token <one-time>`。
- `hello`（GUI→D）：`{type:"hello", role:"gui", token}`
- `show`（D→GUI）：`{type:"show", requestId, request:{questions[], message, isMarkdown, source, lang}}`
- `answer`（GUI→D）：`{type:"answer", requestId, answers:[QuestionAnswer]}`（图片 base64、文件路径，结构同现有 `QuestionAnswer`）
- `cancel`（D→GUI）：`{type:"cancel", requestId, winner}` → 弹窗收尾关窗。
- `configChanged`（D→GUI）：`{type:"configChanged", general}` → 实时切主题/语言。
- 收尾：`answer` 后或被 `cancel` 后 GUI 退出（连接断开等价于放弃）。

## 7. 落盘位置（`~/.askhuman/`）

- `config.json`：用户配置（新位置；缺失回退旧 `~/.humaninloop/config.json`）。
- `daemon.sock`：IPC socket（Windows 用 `\\.\pipe\askhuman-<user>`），权限仅本人。
- `daemon.lock`：`flock` 排他锁，保证单实例。
- `daemon.json`：运行元信息 `{pid, version, protocolVersion, startedAt, socket, binaryFingerprint}`。
- `daemon.log`：运行日志。
- 临时产物仍在系统 temp 的 `askhuman/<request_id>/`（由 Daemon 写与清理）。

## 8. 约束与既有契约（不可破坏）

- **stdout 洁净契约不变**：结果仍只输出 `[选择的选项]`/`[用户输入]`/`[图片]`/`[文件]`/`[状态]` 区块；所有日志/警告走 stderr 或 `daemon.log`。
- **结果格式不变**：单题 = 现状；多题 = `# Qn` + `---`（复用 `output::aggregate_output`）。
- **退出码语义不变**：0（发送/取消正常）/ 1（参数或落盘错误）/ 3（异常，含 Daemon 中途失联）。
- **抢答语义不变**：首个终态生效，其余 `cancel_by_other` 收尾、不重复投递。
- **配置容错不变**：缺字段走默认、未知字段忽略；新位置缺失回退旧 `~/.humaninloop`。
- **向后兼容**：旧环境变量 `HUMANINLOOP_*` 仍兼容；Cursor Hook 新旧 MARKER 仍识别。
- **release 构建**：生产构建仍 `--features custom-protocol`。
- **TLS**：沿用 rustls，不引入 OpenSSL。新增依赖允许：`notify`（config watch）、平台 IPC 相关（如 `tokio` 的 `UnixListener` / `named_pipe`，已在 tokio 内）。

## 9. 验收标准

1. 单次提问（仅弹窗）：CLI → Daemon → GUI Helper 完整跑通，结果区块与退出码与现状一致；杀掉 CLI 能取消并清理弹窗。
2. 启用 IM 渠道并行抢答：弹窗与 Telegram/钉钉/飞书并行，首个终态生效、其余收尾；输出契约不变。
3. **并发/连续提问**：同时发起多个 `AskHuman`，各自的用户回复被正确投递到对应请求，不再串扰（TODO 问题 1 解决）。
4. 自启 + 单实例：无 Daemon 时首个 ask 自动拉起；并发 ask 不会拉起第二个 Daemon。
5. 版本/指纹换新：`./scripts/install.sh` 重编后（即使 version 未变），下一次 ask 自动用上新逻辑（无需手动 restart）。
6. `askhuman daemon status/stop/restart` 行为正确；空闲 5 分钟自动退出；人在环里的长等待不被误杀。
7. 设置实时生效：改设置后，Daemon 重载并重连相应渠道；打开着的弹窗实时切主题/语言。
8. 临时目录被 Daemon 定期清理；Daemon 永不污染 stdout。
9. `--help`/`--version`/`--settings` 行为不变；Telegram 行为、钉钉卡片/文本回退、飞书卡片等渠道功能不回归。

## 10. 非目标 / 后续

- **渠道主动发起任务**（IM 收消息即启动任务，无 CLI 触发）：本期不做，但架构为其预留（Daemon 已常驻持连接 + 已是编排中枢）。届时 Daemon 转常驻模式。
- **GUI 并入 Daemon（菜单栏常驻应用）**：本期明确**不做**——把 GUI 留在独立短命进程，正是为绕开「常驻进程跑 AppKit/主线程/Dock/抢焦点」的复杂度。若未来要做常驻应用再单列一期。
- **文件锁串行化**（TODO 问题 1 的轻量备选）：被本架构取代，不再单独实现。

## 11. 风险与坑（实现期重点关注）

- 钉钉卡片回调 **3 秒 ACK**：Daemon 收到即空响应 `{"response":{}}` ACK，卡片灰显改走 OpenAPI updateCard；需实测不触发「提交失败」。
- 路由表清理 + 幂等：题目结束/被抢答即删路由；钉钉可能重推，注销后到达的重复回调要「ACK 后丢弃」。
- **同一用户并发两题的自由附件归属**仍有歧义（卡片回调带 `out_track_id` 可精确路由，但聊天里的自由图片/文件只能按 `user_id` 归属）—— 这是固有限制，Daemon 也不能完全消除，需文档说明。
- 单实例启动竞争（TOCTOU）：靠 `flock` 原子化；stale socket 在抢锁后清理。
- 跨平台：Windows 后台角色使用 `CREATE_NO_WINDOW` / 新进程组；named pipe 必须维持用户 SID、会话、配置目录隔离与远程拒绝，Unix socket 维持用户私有文件权限。
- config watch：去抖、原子写触发 rename 事件、跨平台由 `notify` 兜。
- 副作用记录：`ASKHUMAN_FEISHU_DEBUG` 等环境调试开关，daemon 化后看 **Daemon 进程**的环境（飞书连接住在 Daemon 里）。
