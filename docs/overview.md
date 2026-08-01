# AskHuman 项目概览（供 agent 参考）

> 跨平台「Human-in-the-loop」工具：命令行 `AskHuman` 在需要人类确认/补充时弹出窗口收集回应，并把结果按固定区块格式写到 stdout 供 AI 读取。

## 文档边界

- 主 overview 是每个任务必读的架构、模块地图、跨模块不变量和能力边界。
- 只对局部任务有用的当前实现地图放 `docs/overview-<topic>.md`；需求决策放 `docs/specs/`，实现步骤放 `docs/plans/`。
- 任务进度只放 `docs/PROGRESS.md`，不在 overview 记录 Phase 或验收历史；代码变化只有影响全局心智模型时才更新主 overview。

## 技术栈与形态

- **Tauri 2**：Rust 后端 + WebView 前端，单一可执行文件 `AskHuman`，跨 macOS / Windows / Linux。
- **前端**：Vue 3 + Vite + TypeScript，纯手写 macOS 风 CSS（无组件库）。
- **运行模型**：Unix 当前走「常驻 Daemon + 瘦客户端 CLI + 独立 GUI Helper / GUI Host」；非 Unix 仍走单进程回退。
- **输出契约**：CLI 的 stdout 只输出结果区块，所有日志走 stderr。

## 运行架构

> 详见 `docs/specs/daemon-architecture.md`。新需求设计时应按当前架构考量：渠道、长连接和抢答归 Daemon，弹窗归独立 Helper，CLI 只做入参与结果转发。

**进程职责（同一二进制按角色切换，本地 IPC 通信）**：

- **AskHuman CLI**（多、短命）：解析 argv（`-f` 在此解析为绝对路径、缺失即退 1）→ 提交 `AskRequest` 给 Daemon → 流式取回结果打到 stdout → 按终态映射退出码 0/1/3。
- **AskHuman Daemon**（Unix 每用户 1 个、常驻、**无 GUI**）：独占四种 IM 的 Router/长连接，承载每请求的 Coordinator/Preemption，集中落盘，监听配置变更，并管理空闲退出、二进制换新和排空。
- **Popup Helper**（每弹窗 1 个）：由 Daemon 启动，主线程运行 Tauri 弹窗，收题目、回传答案后退出。预热实例及其边界见 `docs/specs/popup-prewarm.md`。
- **GUI Host**（Unix 每用户至多 1 个、长命）：承载菜单栏/托盘，以及全局唯一的设置、历史、Agent、Interject 窗口；各打开入口通过自有 IPC 路由到宿主。

**关键约定**：每种 IM 渠道全局只保留一条连接；每个请求仅首个终态回答生效；IPC 和运行状态均为用户私有；既有 stdout、结果区块、退出码和配置兼容契约保持不变。Daemon 排空换新见 `docs/specs/daemon-graceful-drain.md`。

**重复提问收敛**（`docs/specs/duplicate-ask-coalescing.md`）：agent 那一轮被中断后常原样重发同一个提问，而被中断的 CLI 进程不会被杀。Daemon 因此在建请求前先按「会话键 + 提问指纹」判重——同一提问仍在等人回答就合流到那张卡（多等待者共享一个终态，全部离场才取消），5 分钟内已答过则直接重放上次答案并在 `status` 说明。历史、待办出队与 `ask-received` 仍只发生一次。

**Dev Instance（并行 / WorkTree 开发）**：每个已 `dev enable` 的 git 工作树可有独立 `ASKHUMAN_HOME`（`.askhuman-dev/home`）+ 实例 bin + 默认 popup-only 渠道，与主环境 daemon/生产 bot 隔离；入口按 cwd 标记 re-exec。详见 `docs/specs/dev-instance-parallel.md`、`docs/agent-worktree-setup.md`。

## 目录结构

```
AskHuman/
  vite.config.ts  package.json  tsconfig.json    Vite/前端构建配置
  scripts/                   安装、发布、版本与性能脚本
  docs/wiki/                 用户文档（中英双语）；specs/plans 为开发文档
  .github/workflows/         三平台构建与发布

  src/                       Vue 前端（Vite 根目录）
    index.html               前端入口、首帧关键样式与平台探测
    main.ts                  挂载 App、引入全局样式
    App.vue                  按 URL 路由 popup/settings/history/agents/interject/todos
    views/PopupView.vue      提问与回答弹窗（编排层；状态与区块组件在 views/popup/）
    views/AgentsView.vue     Agent 控制台（双栏：边栏+详情；spec gui-agent-console）
    views/console/           控制台子组件（边栏/指示器/Watch 帧/完整会话/diff 条/交互区/内嵌任务）
    views/InterjectView.vue  Agent 插话编辑器
    views/TodosView.vue      项目待办窗口（项目选择 + 增删清空）
    views/NewTaskView.vue    新建 Agent 任务窗口壳（表单主体在 views/newtask/NewTaskForm.vue，与控制台内嵌版共用）
    views/SettingsView.vue   设置页（编排层；各 tab 组件与域逻辑在 views/settings/）
    views/HistoryView.vue    回复历史列表、搜索与筛选
    components/HistoryDetail.vue  单条历史的只读详情
    lib/history.ts           历史记录展示相关纯函数
    lib/ipc.ts               Tauri invoke 封装
    lib/types.ts             与 Rust 模型对齐的 TS 类型
    lib/markdown.ts          Markdown 渲染与代码复制
    lib/theme.ts             主题应用与文件 Data URL
    styles/{tokens,base,controls}.css   设计 token、基础与控件样式

  src-tauri/                 Rust 后端
    Cargo.toml               Rust 依赖与 feature
    tauri.conf.json          Tauri 构建和窗口配置
    capabilities/default.json 窗口能力权限
    src/
      main.rs                入口，调用 cli::dispatch()
      dev_instance.rs · dev_presets.rs  WorkTree Dev Instance 与渠道预设
      macos_quicklook.rs     macOS Quick Look 与文件图标
      macos_menu.rs          macOS 附件原生右键菜单

      cli/
        mod.rs               argv 总分发与各运行角色入口
        args.rs              提问参数解析
        cfgio.rs             CLI 配置读写、密钥输入与本地化工具
        config_cmd.rs        config 子命令
        channel_cmd.rs       channel 配置、测试与识别子命令
        agents_cmd.rs        agents 模式、状态与 capability 子命令
        doctor.rs            daemon、渠道和 Agent 集成体检
        debug_cmd.rs         不进入 help 的调试子命令
        todo_cmd.rs          todo add/list/attach/detach/rm/clear 子命令
        file_attachment.rs   -f 路径解析与校验
        output.rs            文本/JSON 结果格式化
        image_writer.rs      回复图片落盘
        help.rs              CLI 帮助与版本文案

      models.rs              请求、选项、结果与 Confirm 共用模型
      config.rs              AppConfig 读写、默认值与旧路径兼容
      paths.rs               home/config/temp/history/IPC 等路径
      secrets.rs             系统钥匙串与密钥回退
      project.rs             从 cwd 识别项目根
      history.rs             JSONL 回复历史存储
      show_last.rs           按 Agent session/MCP instance 恢复最近完成问答
      context_binding.rs     MCP 会话的一次性 token 与 Grok 安全旁路绑定
      todos.rs               项目级待办队列（todos.json 直读直写 + 文件锁）
      todo_attachments.rs    Todo 附件托管、缩略图、生命周期与执行交付副本
      autochannel.rs         IM 命令分类、活跃槽与共享文案
      perf.rs                跨进程性能埋点
      prompts.rs             CLI/MCP Agent 参考提示词
      mcp/
        mod.rs               STDIO MCP server 运行时
        ask.rs               ask / whats_next / show_last / todo_add / todo_list / todo_update 工具
      hooks.rs               用户级 hooks
      watch.rs               四渠道 /watch 共用状态与文案
      select.rs              跨渠道单选卡模型
      gitutil.rs             IM /diff 与 /stage 的 Git 操作
      confirm/               跨渠道双动作确认模型与传输
      export/                diff/transcript 附件渲染
      agents/transcript_full.rs  四家 Agent 完整会话解析
      agents/workspaces.rs       IM 新任务的最近工作目录索引与四家冷扫描
      dingtalk/confirm.rs    钉钉双动作确认卡
      sound.rs               跨平台弹窗提示音
      commands.rs            前端调用的 Tauri command 集合

      app/
        mod.rs               Tauri 运行时与各类窗口创建
        gui_host.rs          GUI Host、托盘和 daemon 状态订阅
        tray_menu.rs         托盘菜单模型与最小 diff
        coordinator.rs       首答胜出与其它渠道取消

      channels/
        mod.rs               Channel、ResultSink 与 Preemption 抽象
        conversation.rs      会话型渠道公共编排
        popup.rs             本地弹窗 Channel
        telegram.rs          Telegram Channel 适配
        dingding.rs          钉钉 Channel 适配
        feishu.rs            飞书 Channel 适配
        slack.rs             Slack Channel 适配
        confirm.rs           Confirm 的四种 IM adapter

      telegram/
        mod.rs               Telegram Bot API 客户端
        markdown.rs          Markdown 到 Telegram HTML
        router.rs            Telegram 单长轮询与会话路由

      dingtalk/
        mod.rs / token.rs / client.rs / stream.rs / card.rs / textfile.rs / docx.rs
                             钉钉认证、Stream、卡片与附件
        watch.rs             钉钉 /watch 卡渲染与回调
        router.rs            钉钉单 Stream 连接与会话路由

      feishu/
        mod.rs               模块与错误类型
        token.rs             tenant_access_token 缓存
        client.rs            飞书 OpenAPI 客户端
        ws.rs                飞书 WebSocket 长连接
        card.rs              飞书卡片组装与回调解析
        router.rs            飞书单长连接与会话路由

      slack/
        mod.rs               模块与错误类型
        client.rs            Slack Web API 客户端
        ws.rs                Slack Socket Mode 长连接
        blockkit.rs          Block Kit 组装与回调解析
        markdown.rs          Markdown 到 Slack mrkdwn
        router.rs            Slack 单长连接与会话路由

      integrations/
        hook_edit.rs         JSONC Hook 的最小编辑
        mutation_lock.rs     集成配置写入的跨进程锁
        agent_permission.rs  Claude/Codex 权限审批集成
        cursor_hook.rs       Cursor 超时 Hook 管理
        claude_hook.rs       Claude Code 超时 Hook 管理
        agent_lifecycle.rs   四家 Agent 生命周期 Hook 管理
        agent_launch.rs      Agent readiness、一次性启动记录与 Terminal.app helper
        agent_rules.rs       Agent 全局 Rules 管理
        agent_subagent_guard.rs  Claude/Codex SubagentStart 提示 Hook
        agent_context_recovery.rs  集成 mode 托管的压缩恢复/会话绑定 Hook
        grok_skill.rs        Grok interaction-protocol skill 管理
        mcp_config.rs        四家 MCP server 配置管理
        agent_mode.rs        None/CLI/MCP 模式编排
        agent_stop.rs        Agent Stop 结束确认配置
        login_item.rs        macOS/Linux GUI Host 与 daemon 登录项

      ipc/
        mod.rs               CLI/Daemon/Popup 消息类型
        codec.rs             NDJSON 编解码
        transport.rs         Unix socket 传输
      gui_host/
        mod.rs               GUI Host 自有 IPC 与窗口路由
      client/
        mod.rs               CLI 到 Daemon 的连接与管理操作
        composer.rs          Interject 窗口的 daemon 连接
      daemon/
        mod.rs               daemon 子命令入口（Unix 转 unix_impl）
        unix_impl/
          mod.rs             状态与类型、serve 主循环、连接分发与请求提交
          watch.rs           watch 订阅持久化、tick 刷新与卡片回调
          select.rs          跨渠道单选卡发送、路由与回调分发
          inbound.rs         IM 入站命令层与共享命令处理
          todo.rs            IM /todo·/todo-rm·/todo-auto 命令与待办管理卡
          subs.rs            GUI/托盘/Agent 订阅广播与 Interject 连接
          detect.rs          渠道自动识别流程
        lifecycle.rs         单实例、指纹与空闲生命周期
        spawn.rs             Daemon 脱离启动
        request.rs           请求登记、Coordinator 与 GUI token
        config_watch.rs      config.json 监听与重载
      update/
        mod.rs               更新检测与 updater 选择
        direct.rs            GitHub Release 直接更新
        npm.rs               npm 全局更新
        notes.rs             release notes 获取与聚合
        state.rs             update.json 状态
      agents/
        mod.rs               Agent 类型、事件与模块入口
        detect.rs            Agent 家族、会话和进程探测
        report.rs            生命周期 Hook 上报与去重
        context_recovery.rs  压缩后提示、MCP token 注入与 Grok pending 上报
        title.rs             四家会话标题解析
        activity.rs          transcript 尾部活动解析
        cursor_vscdb.rs      Cursor IDE 会话实时源（state.vscdb；活动/标题/完整会话）
        registry.rs          Agent 状态推导、持久化与快照
        interject.rs         插话队列、等待与持久化
        stop.rs              Stop Hook 捕获与原生 continuation

  cliff.toml                 git-cliff 配置
  docs/release-notes/        每版本可选的 release notes 覆盖文件
```

## 运行流程

1. `main.rs` → `cli::dispatch()`：在创建任何窗口前按 argv 分流纯信息、管理、GUI 和提问命令。
2. Unix 上，CLI 把参数规范化为请求，连接或拉起 Daemon 后提交；非 Unix 直接走单进程回退。
3. Daemon 登记请求，按配置启动 Popup Helper，并把请求交给已启用的 IM Router；各渠道并行等待回答。
4. Coordinator 只接受首个终态结果，随即取消其它渠道；历史、回复图片和文件在这一汇聚点统一处理。
5. Daemon 把最终结果回传 CLI；CLI 只负责写 stdout 并按终态返回退出码。

## 前端 ↔ 后端命令（`commands.rs` ↔ `lib/ipc.ts`）

- 弹窗：`popup_init`、`submit_popup`、`cancel_popup`
- 附件：`open_path`、`preview_attachments`、`close_preview`、`read_image_data_url`、`file_icon_data_url`、`show_attachment_menu`
- 设置：`get_settings`、`save_settings`、`get_prompt`、`set_theme`、`update_theme`、`open_settings`、`popup_sound_support`、`play_popup_sound`
- 历史：`open_history`、`history_init`、`get_history`、`get_history_projects`、`history_count`、`trim_history`、`resolve_history_session_titles`、`delete_history_entries`、`clear_all_history`
- Cursor / Claude 超时 Hook：`cursor_hook_status` / `install` / `update` / `uninstall` / `reveal`；Claude 同名前缀命令
- Agent Rules：`agent_rule_status` / `install` / `update` / `uninstall` / `reveal` / `open`
- Agent 模式与配置文件：`agent_mode_status` / `set` / `update`、`mcp_config_reveal` / `open`、`agent_hook_reveal` / `open`、`mcp_command_path`
- 渠道测试与识别：`telegram_test`；钉钉、飞书、Slack 各自的 `*_test` / `*_detect_prepare` / `*_detect_wait`；共用 `detect_cancel`
- 版本自更新：`get_app_version`、`update_check`、`update_get_notes`、`update_apply`、`update_dismiss`、`popup_update_state`、`restart_settings`
- Agent 生命周期：`agents_init`、`agent_lifecycle_status` / `install` / `uninstall`、`agent_force_idle`
- Agent 控制台（spec gui-agent-console）：`agents_focus`（焦点会话，daemon 推 `agent-detail` 帧）、`focus_request`（去回答）、`interject_append` / `interject_peek`（输入框追加与待送达气泡）、`console_transcript`（完整会话分页）、`console_diff_stat` / `console_diff_file` / `console_stage`（项目 diff 状态条与暂存）
- 新建 Agent 任务（GUI）：`open_new_task`、`new_task_init`、`new_task_projects`(+`_refreshed`)、`project_key_of`、`new_task_launch`

Popup 的窗口、附件、来源标题与交互实现地图见 `docs/overview-popup-ui.md`：

- 弹窗支持 Markdown、附件拖入/拖出、原生预览与右键菜单。
- 来源名优先级为自定义环境变量 > 发起 Agent > 默认「the Loop」；头部同时展示 Agent、workspace 和提问时间。
- 多问题纵向模式由实验开关控制，设计见 `docs/specs/multi-question-vertical.md`。
- 推荐选项不自动预选，提交值始终为原文，规格见 `docs/specs/recommended-option.md`。

## UI / 主题

- 主题三态：`system`(prefers-color-scheme)/`light`/`dark`；前端切根类 + 后端设原生窗口主题。
- macOS 窗口材质三态：Solid 为完整不透明主题底且不使用 Visual Effects；Blur 使用 Tauri `underWindowBackground`；Glass 仅在 macOS 26+ 使用 `NSGlassEffectView`，旧系统的 `glass` 配置有效值为 Blur。三态共用 `TitleBarStyle::Overlay` + 隐藏标题；Windows/Linux 维持纯色不透明底。
- Markdown 配色见 `styles/controls.css`（链接/代码块/表头/引用/hr 等）。代码块 hover 右上角有拷贝按钮（`.code-copy`，点击复制 `<code>` 文本，弹窗/历史详情/设置发布说明共用 `renderMarkdown` 故都生效）。

## 配置

`~/.askhuman/config.json` 是主配置文件，模型与默认值在 `src-tauri/src/config.rs`；新位置缺失时自动回退旧 `~/.humaninloop/config.json`。
顶层分为 `general`、`channels`、`agentTasks` 和 `experimental`；缺字段走默认、未知字段忽略。完整字段地图见 `docs/overview-configuration.md`，用户向说明见 `docs/wiki/`。

### IM 命令与主动交互

四种 IM 共用 Daemon 内的入站命令层，平台模块只负责传输、渲染和回调；Daemon 存活时即持续监听消息，不要求已有提问。详细能力与代码入口见 `docs/overview-im-commands.md`。

- `channels.autoActivation` 关闭时向所有启用 IM 投放，开启时以当前活跃槽为主；切槽会补推在途请求，watch 渠道仍会加入对应 Agent 新提问的投放并集。
- 共享命令包括 `/new`、`/help`、`/here`、`/status`、`/watch`、`/unwatch`、`/msg`、`/msg-clear`、`/yolo`、`/diff`、`/stage`、`/transcript`、`/todo`、`/todo-rm` 和 `/todo-auto`；Slack 使用 `!` 作为可输入的备用前缀。
- macOS 开启 `agentTasks` 后，`/new` 依次选择 workspace、已就绪 Agent 与权限，在新的 Terminal.app 窗口启动真实交互会话；Daemon 只负责启动前流程，之后复用 lifecycle/watch。
- macOS 本地 GUI 亦可创建任务（不要求开启 `agentTasks`）：待办窗口行内按钮与托盘「新建 Agent 任务」打开统一的新建任务窗口，就绪判定与启动链路与 `/new` 相同；见 `docs/specs/gui-agent-task-launch.md`。
- Agent 数字编号在 daemon 生命周期内稳定，供状态、关注、插话和 Git/会话导出共用；无参目标选择复用跨渠道单选卡模型。
- Watch 订阅持久化并就地更新原卡；`/stage` 必须经过跨渠道 Confirm，不能直接执行暂存。

### 回复历史

- `general.historyLimit` 控制 `~/.askhuman/history.jsonl` 的全局保留数；0 停止新增，裁剪/立即清理时清空已有记录。
- 历史在 Coordinator 的唯一汇聚点记录“发送”和“用户主动取消”，系统取消不记。
- 记录按 git 根（回退 cwd）归项目；图片和文件只保存路径，读取对旧记录和缺失附件保持兼容。
- 完成问答额外记录可用的 Agent session ID 和 MCP instance ID；`AskHuman --show-last`
  或 MCP `show_last` 可在上下文压缩后读取当前会话最近一条提问语境与实际答案（省略空字段、
  未选候选项和 recommended 标记）。取得真实 session 后只做精确查询，不向弱键级联；
  只有完全非 Agent 的 CLI 才按项目回退。
- 历史窗口用单一两级菜单按“项目 → 单个 session → 多关键词 AND 搜索”组合筛选；真实 Agent session 使用
  `agentKind + agentSessionId`，缺失时才以 `project + mcpInstanceId` 作为明确标注的近似分组。
  从弹窗进入会优先定位本次 ask 的真实历史绑定，已打开的全局单窗也会重新定位。
- 清理菜单按上下文显示“删除当前搜索结果 / 清空所选会话 / 清空所选项目”；范围清理只提交确认时
  冻结的 entry ID，全量清空是独立动作，两者都只改
  `history.jsonl`，不触碰 daemon 重放缓存或待答卡片。完整约束见 `docs/specs/reply-history.md`。

### 密钥安全

- 钉钉、飞书、Telegram、Slack 的五项密钥优先存系统钥匙串，`config.json` 留空；钥匙串不可用时才回退明文，Unix 配置文件/目录权限收紧。
- macOS 钥匙串读取依赖 Aqua 安全会话；非 Aqua 来源拉起 daemon 时，经用户 GUI launchd 域启动，避免后台 daemon 读不到密钥。
- 非密钥路径使用 `AppConfig::load_without_secrets()`，只有 Daemon、IM 和设置密钥状态等确需凭据的路径使用 `load()`。读取边界见 `docs/overview-configuration.md`，完整设计见 `docs/specs/secret-storage-keychain.md`。

## 版本自更新（self-update）

> 需求/方案见 `docs/specs/self-update.md`、`docs/plans/self-update.md`。

- 支持 Direct（GitHub Release）与 npm 两种安装来源，`src-tauri/src/update/` 按 adapter 实现。
- apply 只把新二进制落盘，不主动 restart；Daemon 通过二进制指纹和 graceful drain 在所有在途请求结束后换新。
- 更新状态持久化到 `~/.askhuman/update.json`，Daemon 后台检查并推送给 Popup/GUI Host。
- release notes 默认由 Conventional Commits + git-cliff 生成，可用 `docs/release-notes/v<version>.md` 覆盖。

## 高级功能：Agent 生命周期追踪 + 状态窗口（Unix）

> 需求 `docs/specs/agent-lifecycle-tracking.md`，计划 `docs/plans/agent-lifecycle-tracking.md`。

- Unix 通过用户级 hooks 跟踪 Claude Code、Codex、Cursor、Grok；它与 Agent 集成 mode、IM autoActivation 相互独立。
- Daemon 的 `AgentRegistry` 以 session id 为主身份，推导工作中、空闲、已结束；pid/liveness 与超时只做兜底。
- 生命周期状态被 `/status`、watch、插话、托盘/状态窗口和 Daemon 空闲退出共同使用；修改事件或状态模型时必须检查这些消费者。
- Daemon 启动时幂等迁移已开启但过期的 hooks。只有工作中 Agent 或状态窗口连接阻止闲退；graceful drain 不受 Agent 存活影响。
- 入口为设置「高级」Tab、`AskHuman agents monitor` 和 `agents/registry.rs`；状态窗口由 GUI Host 承载并订阅 Daemon 快照。
- 状态窗口已改版为双栏「Agent 控制台」（spec `docs/specs/gui-agent-console.md`）：边栏项目分组会话
  + 详情区 Watch 帧（agents 订阅上的焦点会话子订阅复用 IM Watch 引擎推帧）+ 完整会话分页 +
  项目 diff 状态条 + 插话输入 + 内嵌新建任务；快照对 GUI 额外注入 `waitingRequestId`/`waitingPreview`。

## Agent 插话（Interject，Unix）

> 需求 `docs/specs/agent-interject.md`，计划 `docs/plans/agent-interject.md`。

- 插话依赖生命周期追踪，只对工作中的 Claude Code、Codex、Cursor 开放；Grok 不支持。
- Daemon 按 session 维护并持久化待送达队列；GUI composer 整体覆盖，IM `/msg` 追加。
- Agent 下一次 PreToolUse 读取并原子消费消息；composer 已打开时 hook 可等待提交或取消，热路径不做文件 IO。
- 排队消息被实际读取后向来源 IM 发阅读回执；即时送达、撤回、覆盖或未消费的消息不回执。
- 入口为 Agent 状态窗口、托盘菜单和 IM `/msg`；队列实现位于 `agents/interject.rs`。

## 项目待办 + whats-next

> 规格 `docs/specs/todo-whats-next.md`、`docs/specs/todo-attachments.md`。

- 待办按项目（git 根）归属，`~/.askhuman/state/todos.json` 是唯一数据源：所有进程直读直写 + 文件锁串行化，不依赖 daemon 存活，跨平台。GUI / CLI / MCP 可在创建时或对已有 Todo 独立管理附件；GUI 默认折叠附件摘要，支持新增区 / 每条 Pending Todo 拖入、新增输入框粘贴图片，以及选中行后粘贴剪贴板图片。单文件 ≤10 MiB 由 AskHuman 托管，>10 MiB 保留绝对路径引用（无稳定源路径的剪贴板图片超过阈值则拒绝），每条最多 20 个，并生成最长边 128 px 的图片缓存缩略图。用户可见写操作在落盘失败时必须报错。
- Agent 完成任务后必须调 `AskHuman --whats-next`（MCP 为 `whats_next` 工具）：固定提问 + 可选的 Agent 建议任务 + 待办 chip + 恒有「结束本轮」；顺序固定为建议任务、待办、结束，总选项最多 10 条。建议任务仅在确有建议时通过 `-o`/`-o!`（MCP `options`）传入，选择结果保持普通 Ask 的 `[selected_options]` 语义；待办派活为 `[user_input]`，准许结束为 `[selected_options]`，取消为 `[status]`。选中的待办按 id best-effort 出队（Coordinator 汇聚点统一处理）。标记为「自动执行」（⚡）的待办优先级不变：whats-next 时不发卡、直接按队列顺序派发最靠前一条。
- 送达面：whats-next / 普通提问 Popup 折叠待办区 / Stop 确认卡（兜底）都以选项形式呈现待办；GUI / IM 新建 Agent 任务也可直接执行待办。所有出口在执行前按卡片附件快照与最新 Todo 求交集，托管文件建立请求级交付副本，失效项转为 warning，选项只显示 `【N 个附件】`（支持富文本颜色时为灰色）而不向 IM 上传本地文件。输入面：CLI `todo` 子命令、Popup 内新增、GUI 待办窗口（托盘/AgentsView 入口）、IM `/todo`；IM 创建/管理首版仍只支持文字。
- IM `/todo`（管理卡：飞书代码卡自带输入表单，钉钉复用提问卡模板 `allow_input`，TG/Slack 文本 + 命令提示）、`/todo-rm`（复用单选卡逐条删除、就地刷新）与 `/todo-auto`（切换自动执行标记）仅 Unix，实现在 `daemon/unix_impl/todo.rs`。
- macOS 上待办窗口每条待办另有「创建任务」按钮：预选项目与该待办打开新建任务窗口，Terminal 启动成功后按快照出队（spec `docs/specs/gui-agent-task-launch.md`）。

## 菜单栏图标 + 统一 GUI Host（Unix 桌面）

> 需求 `docs/specs/menu-bar-tray.md`，计划 `docs/plans/menu-bar-tray.md`。

- Unix 每用户单实例 GUI Host；Daemon 保持无 GUI。Host 承载设置、历史、Agent、Interject 与托盘，所有入口路由到它以保证每类窗口唯一。
- Host 使用独立的 `gui-host.sock` / lock；即使 Daemon 未运行，设置和历史仍能打开。
- `general.menuBarIcon` 支持 `off|active|always`；Windows 不支持，Linux 桌面 best-effort。
- 托盘状态订阅不保活；只有打开窗口的独立连接给 Daemon 续命。`general.daemonLifecycle=keepalive` 是正交的常驻策略。
- `TrayState` 汇总待答、IM、Agent、更新与 drain；菜单可聚焦待答 Popup、打开 Agent/插话、控制 Daemon 和应用更新。
- GUI Host 启动与 daemon 停→运行时复用 Agents 设置页口径检查集成更新；待更新时菜单显示可点击警告，无待答时 template 图标显示右上实心圆，设置内更新成功后即时清除。
- GUI Host 只在无窗口时换到新二进制；GUI Host always 与 Daemon keepalive 的登录项分别管理。

## CLI 配置与 Agent 集成（headless / 无 GUI）

> 需求 `docs/specs/cli-config.md`，计划 `docs/plans/cli-config.md`。

- headless/SSH 可用 `channel`、`agents`、`config`、`doctor` 完成渠道配置与 Agent 集成；各子命令提供 help 和 JSON 输出。
- `channel set` 无 flag 走交互向导，有 flag 走脚本；密钥只从 env/file/stdin 或隐藏输入读取，不进 argv。
- `agents mode` 维护 None/CLI/MCP 整包；permission/stop 的 preference 独立保存，但只有 CLI/MCP mode 才安装交互接管 Hook，None 隐藏对应设置并卸载其 active 能力；lifecycle 始终正交，Grok 仅 None/MCP 且不支持 stop/interject。
- 上下文恢复 Hook 由当前 mode 独立托管，不依赖可关闭的 lifecycle tracking；CLI/MCP
  提示严格只指向用户当前选中的入口，缺失/过期并入当前 Hook 或 MCP 产物的更新状态。
- 全局交互协议把 Sub Agent 作为唯一例外并禁止其使用 AskHuman；Claude/Codex mode 另带 `SubagentStart` 提示 Hook，Cursor/Grok 只依赖协议文本。
- `agents update [<agent>]` 按当前 mode 重新 reconcile 单家或全部托管产物；重复设置相同 mode 也会完整更新，但不改独立保存的 capability 偏好。
- `config` 是通用键值兜底，`doctor` 汇总 Daemon、渠道和集成；两者复用同一配置与集成模块，不维护第二套逻辑。

## MCP 支持（CLI 之外的第二种集成形态）

> 需求 `docs/specs/mcp.md`，计划 `docs/plans/mcp.md`。

- `AskHuman mcp` 暴露 `ask`、`whats_next`、`show_last`、`todo_add`、`todo_list`、`todo_update`：`ask`/`whats_next` 每次调用 spawn 现有 CLI 流程，复用 Popup、IM、抢答、历史和 drain；`show_last` 只读恢复最近精确问答，三个 Todo 工具按当前项目直接读写 `todos.json`，其中 list/update 使用稳定 Todo 与附件 ID。
- `ask` 入参为 message/questions/files；输出为 CLI 同款结果区块文本（原样透传，无 output schema / structuredContent）加图片 ImageContent。MCP 取消会终止子 CLI，并通过 socket EOF 取消 Daemon 请求。
- Agent 自动集成是 None/CLI/MCP 互斥；Grok 仅 None/MCP，其 MCP 产物是 skill + config。
- Codex、Grok、Claude 分别写适配自身的长超时配置；Cursor MCP 超时不可配置，推荐 CLI 模式。
- MCP 没有通用的 Agent session 字段：Codex 使用每调用 `_meta.threadId`，Claude/Cursor
  使用 schema 隐藏的短命一次性 token，Grok 只在进程+项目分区内参数指纹候选唯一时认领；
  无可信 session 时才回退到当前 MCP instance + project。详见
  `docs/plans/agent-context-compaction-retention.md`。
- Codex 配置把 `mcp__askhuman` 加入 Code Mode direct-only namespace，确保 ask 顶层阻塞；最小编辑与所有权记录避免卸载用户原有配置。

## Agent 原生权限审批（Claude Code / Codex）

> 设计与实现计划见 `docs/plans/agent-permission-approval.md`。

- Unix 只支持 Claude Code / Codex 的原生 `PermissionRequest` Hook；基础设施或渠道失败、24h 到期时不输出裁决，让 Agent 回到原生审批。
- 固定动作是批准一次/拒绝；Claude 只原样回放本次请求携带且通过白名单的 allow suggestion，Codex 永不伪造 `updatedPermissions`。
- 权限请求走独立 Confirm 模型；Popup/四 IM 中首个 Ready 的合法回答胜出，其它端定格，轻量 tombstone 只保留到原 deadline。
- permission 是默认开启的正交 preference，不是第四种 mode；CLI/MCP mode 按 preference 管理 Hook，None 卸 Hook 但保留偏好。
- “已配置”不等于已生效；blocked policy 和其它 Hook 只做可读范围内的正向提示。Claude 决策可能互相覆盖，Codex 保持 deny-wins。

## Agent Stop 结束确认

> 规格 `docs/specs/agent-stop-confirmation.md`。

- 只处理 Claude Code、Codex、Cursor 的自然完成；Grok、错误和用户中断不能可靠 continuation，不发卡。
- 三家独立开关默认关；询问“继续/结束”，24h 或基础设施失败 fail-open，投放沿用活跃槽 ∪ watch 渠道。
- Stop preference 独立保存，但只在 CLI/MCP mode 生效；None 隐藏开关并卸载 confirm 能力，重新集成时自动恢复。若 lifecycle tracking 仍开，共享 Stop handler 只保留 `track`、不弹确认卡。
- 继续时使用各家原生 continuation，下一次自然 Stop 再询问，不做外部 resume。
- Stop 确认复用普通 Ask 单选链路但不写回复历史；它与 lifecycle 共用单一 Stop handler，避免并发 Hook 提前置空闲。
- `[user_confirmed_end_turn]` 出现在最后回复任意位置即表示用户已明确同意结束，命中后直接放行，避免再次确认。

## 接管 Claude 内置提问（AskUserQuestion）

> 规格 `docs/specs/claude-ask-user-question.md`。

- 只有 Claude Code 有内置选择题工具。`PreToolUse`（matcher 限定该工具）把题目交给 AskHuman 的多问题卡，作答后按官方 `allow` + `updatedInput.answers` 回传，终端不再弹原生选择框；headless 下同样可答。
- 独立 capability preference（`ask-question-preferences.json`，默认开），与 permission / stop 并列，同样只在 CLI/MCP mode 下安装 hook。
- 取消＝`deny` + 「必须重新询问」；弹窗与 IM 都不可用等基础设施失败则不输出，让位给原生选择框。
- 权限卡对该工具**一律让路**（不接管、不输出），与开关无关。

## 用户级 hooks + 弹窗提示音

- Unix 用户 hook 位于 `~/.askhuman/hooks/<event>`，是按事件命名的可执行脚本；摘要走环境变量，完整负载走 stdin JSON，非阻塞执行。
- `ask-received` 在 Daemon 接收一次提问时恰触发一次，与是否显示 Popup 或投放 IM 无关。
- Daemon 会生成不可执行的 `ask-received.sample`；复制去后缀并 `chmod +x` 即启用，新增事件沿用同一机制。
- `general.popupSound` 是独立便利功能：macOS 使用系统音名，Linux 有可用播放器时显示，Windows 不支持；它与用户 hooks 相互独立。

## 构建 / 开发 / 测试

> 完整的依赖、Dev Instance、发布与平台说明见 `docs/development.md`。

```bash
pnpm install
pnpm tauri dev
pnpm build && cargo build --release --manifest-path src-tauri/Cargo.toml --features custom-protocol
cargo test --manifest-path src-tauri/Cargo.toml
./scripts/install.sh                    # macOS / Linux
./scripts/install-windows.ps1           # Windows
node scripts/perf-popup.mjs             # 固定 canonical 弹窗性能场景
```

- 功能或逻辑变更后按项目规则运行安装脚本，再使用新安装的 `AskHuman` 验证。
- macOS 安装使用稳定签名身份以维持钥匙串信任；证书与会话边界见 `docs/specs/secret-storage-keychain.md`。
- 性能 harness、埋点与基线见 `docs/specs/popup-launch-performance.md` 和 `docs/perf/baseline.json`。

## 注意事项

- **stdout 洁净**：GUI 阶段把 stderr 重定向到 /dev/null（`app/mod.rs` 的 `stderr_redirect`，Unix），自身错误用 `eprintln_real` 走原 stderr。
- **首帧不白闪**：`src/index.html` 内联关键底色；macOS 建窗 URL 携带有效材质，Solid 首帧完整实色，Blur/Glass 首帧透明叠色罩。
- **macOS 窗口材质**依赖 `tauri` 的 `macos-private-api` feature 与 `macOSPrivateApi: true`；运行时原生层变更必须在主线程执行。
- **release 自包含**：前端资源在 `cargo build` 时由 `generate_context!` 嵌入，故安装后无需 dev server。
- Telegram 不接收图片；Cursor Hook 仅 mac/Linux（Windows 禁用并提示）。
