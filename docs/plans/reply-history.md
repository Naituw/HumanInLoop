# 开发计划：回复历史记录（reply-history）

> 关联需求：`docs/specs/reply-history.md`
> 计划描述方案与技术 / 规则细节，具体代码以实现为准。
> 2026-07-31 起，session 筛选、弹窗定位与精确删除行为由
> `docs/plans/reply-history-session-filter.md` 取代本计划中的旧项目级清空设计。

## 0. 方案总览

```
写入（旁路，覆盖所有渠道 / 模式）
  CLI 计算 project(向上找 .git 根，回退 cwd) ──┐
                                              ▼
  各渠道终态结果 → 每请求 Coordinator.finish() ──► render_result(落盘图片→得到图片路径)
                                              └─► history::record(entry, limit)  // limit=0 不记
                                                      └─ ~/.askhuman/history.jsonl（追加 + 裁剪到 N，文件锁 + 原子写）

读取 / 展示（独立窗口）
  弹窗导航栏「历史」按钮 → open_history（同进程新建窗口，类似设置窗）
  或 CLI：AskHuman --history [--all]（独立 GUI 进程，类似 --settings）
        └─ HistoryView：history_init(取当前 project/主题) + get_history(filter) + get_history_projects()
             ├─ 左列表（时间倒序 + 渠道徽标 + 动作状态 + 摘要）
             └─ 右详情：HistoryDetail（只读还原提问 UI + 状态横幅）  // 图片/文件 best-effort
```

核心三块：① **存储层**（jsonl + 项目识别 + 容量配置）；② **写入接线**（把 project/source 贯穿到每请求协调器，在 `finish()` 旁路记录，需 `render_result` 回吐图片路径）；③ **读取与独立窗口**（新 view + 只读详情组件 + 一组命令）。

---

## 1. 存储层与数据模型

### 1.1 项目识别 `src-tauri/src/project.rs`（新增）

- `pub fn detect() -> String`：取 `env::current_dir()`，从该目录向上逐级查找含 `.git` 的目录；命中则返回该目录、否则返回 cwd；对结果尽量 `canonicalize`（失败用原路径）；任何错误返回空串。返回值为项目 **key**（绝对路径）。
- `pub fn display_name(key: &str) -> String`：取 key 的 basename（空 key → 本地化「未知项目」）。
- 单测：构造临时目录树验证「子目录命中上层 `.git` 根」「无 `.git` 回退自身」。

### 1.2 历史模块 `src-tauri/src/history.rs`（新增）

数据模型（serde camelCase，对齐前端 TS）：

- `HistoryEntry`：
  - `id: String`、`timestamp_ms: i64`、`project: String`、`source: String`、`channel: String`（提交 / 取消端 id）、`action: ChannelAction`、`is_markdown: bool`
  - `message: MessagePrompt`（复用现有模型：文本 + `-f` 附件 `FileAttachment`，仅路径 / 名称 / 大小 / 是否图片）
  - `questions: Vec<Question>`（复用现有模型）
  - `answers: Vec<HistoryAnswer>`（取消时为空）
- `HistoryAnswer`：`selected_options: Vec<String>`、`user_input: Option<String>`、`images: Vec<String>`（**已落盘路径**，非 base64）、`files: Vec<String>`（路径）
- `ProjectInfo`：`key: String`、`name: String`、`count: usize`、`last_ms: i64`（下拉用）
- `ClearScope`：`All` | `Project(String)`

函数（写操作均加文件锁 + 原子写；读操作容错）：

- `record(entry: HistoryEntry, limit: u32)`：`limit==0` 直接返回（不记录、不裁剪）；否则加锁 → 追加一行 → 若总行数 > limit 则只保留最近 limit 行 → 原子写回。**最佳努力**，出错仅 `eprintln_real` 警告，绝不向上传播影响主流程。
- `load(project: Option<&str>, all: bool) -> Vec<HistoryEntry>`：读全部行解析（坏行跳过），`all==false` 时按 `project` 过滤，按 `timestamp_ms` 倒序返回。
- `projects() -> Vec<ProjectInfo>`：聚合去重，按 `last_ms` 倒序。
- `count() -> usize`：总条数。
- `trim(limit: u32) -> usize`：裁剪到最近 limit 条（`limit==0` 视为不裁剪，返回当前条数），返回裁剪后条数（供设置页「立即清理」）。
- `clear(scope: ClearScope)`：按范围重写文件（全部 → 清空；某项目 → 滤除该项目）。
- 路径：`paths::history_file()` = `config_dir()/history.jsonl`；锁文件 `paths::history_lock()`。
- 锁实现：unix 复用 `daemon/lifecycle.rs` 既有 `flock` 思路（独立小封装）；非 unix 退化为「读改写 + 原子 rename」兜底。
- 单测：jsonl 往返、`trim` 到 N、`load` 项目过滤 / 全部、`clear` 两种范围、坏行跳过。

### 1.3 路径 `src-tauri/src/paths.rs`

- 新增 `history_file()` 与 `history_lock()`（位于 `~/.askhuman/`）。

### 1.4 配置 `src-tauri/src/config.rs`

- `GeneralConfig` 新增 `history_limit: u32`，`#[serde(default = "default_history_limit")]` → 200；文档注明 `0 = 停止新增记录`。
- 其余读写 / 容错逻辑不变（缺字段走默认、未知字段忽略）。

### 1.5 前端类型 `src/lib/types.ts`

- 新增 `HistoryEntry` / `HistoryAnswer` / `ProjectInfo`；`GeneralConfig` 增 `historyLimit: number`。

---

## 2. 写入接线（把上下文贯穿到记录点）

> 目标：在唯一汇聚点 `Coordinator.finish()` 旁路写历史，需要 `project` / `source`（来源名）与**已落盘图片路径**。

### 2.1 IPC 协议 `src-tauri/src/ipc/mod.rs`

- `TaskRequest` 新增 `project: String`（CLI 计算并上送；revisit A11）。
- `ShowPayload` 新增 `project: String`（Daemon 下发给 GUI Helper，供历史窗口过滤当前项目）。
- 仅新增字段，旧端忽略未知字段；协议版本视情况评估（新增可选字段一般无需 bump，实施时确认）。

### 2.2 CLI `src-tauri/src/cli/mod.rs`

- 提问分支：计算 `project = project::detect()`，写入 `TaskRequest.project`（unix 路径）。
- 非 unix 单进程路径：把 `project` 一并带入 `AskRequest` 流程（经 AppState，见 2.4）。

### 2.3 协调器 `src-tauri/src/app/coordinator.rs`

- `Coordinator` 增字段 `project: String`、`source: String`（来源名；`lang` 已有）。
- 三个构造器 `new` / `new_headless` / `new_ipc` 增 `project`、`source` 参数（更新所有调用点，见 2.4 / 2.5）。
- `finish()` 重构为：先 `render_result` 拿到 `RenderOutcome` **与各题图片路径**（见 2.6）；若存在结果（`Some`）→ 组装 `HistoryEntry`（来自 `inner.request` 的 message/questions/is_markdown/id + result 的 action/source_channel_id/answers + 图片路径 + project/source + 当前时间）→ 读 `AppConfig::load().general.history_limit` → `history::record(entry, limit)`；**随后**再按 exiter 分支（Ipc 回传 / 打印退出）。记录在三种模式下都只发生一次、且不影响后续输出。

### 2.4 单进程 / 设置 / Helper `src-tauri/src/app/mod.rs`

- `AppState` 增 `project: String`。
- `run_ask` / `run_headless`：`project = project::detect()`、`source = source_name()` 传入对应 `Coordinator` 构造器与 `AppState`。
- `run_gui_helper`：`AppState.project = show.project`（不参与本进程记录——记录由 Daemon 侧协调器完成；Helper 仅用 project 做历史窗口过滤）。
- `run_settings`：`project = project::detect()`（或空），供从设置窗口路径打开历史时过滤。
- 新增 `View::History` 与 `run_history(project, all, config)`（见 §3）。

### 2.5 Daemon `src-tauri/src/daemon/request.rs`

- `create(task)`：把 `task.project` 传入 `Coordinator::new_ipc(..., project, source=task.source, lang)`，并写入 `ShowPayload.project`。

### 2.6 渲染回吐图片路径 `src-tauri/src/app/mod.rs`

- `render_result` 当前仅返回 `RenderOutcome{stdout,stderr,exit_code}`；改为**额外回吐各题已落盘图片路径** `Vec<Vec<String>>`（取消路径为空）。
  - 方式：返回 `(RenderOutcome, Vec<Vec<String>>)`，或在 `RenderOutcome` 增 `image_paths: Vec<Vec<String>>` 字段（实施时取其一，保持调用点最小改动）。
- `emit_result`（单进程打印路径）相应适配新签名，行为不变。
- `finish()`（2.3）据此为 `HistoryAnswer.images` 填入路径。

---

## 3. 独立历史窗口与 CLI 入口

### 3.1 CLI `--history` `src-tauri/src/cli/mod.rs`

- 新增分支 `"--history"`：解析其后是否带 `--all`；`project = project::detect()`；调 `app::run_history(project, all, AppConfig::load())`（独立 GUI 进程，**不经 Daemon**，与 `--settings` 同机制）。
- `help.rs` 的 `--help` 文案补 `--history [--all]`（agent-help 不涉及，属人类功能）。

### 3.2 窗口创建 `src-tauri/src/app/mod.rs`

- `launch` 支持 `View::History`：在 setup 中调 `create_history_window(app, &config, all)`。
- `create_history_window(manager, config, all)`：与 `create_settings_window` 同构（复用 `apply_surface` + 主题 + Liquid Glass）；窗口 URL `index.html?view=history`，`all` 时附 `&all=1`；尺寸约 820×600、最小 600×440。
- 关窗清理：macOS 下与 settings 一致清理 Liquid Glass 注册表条目（`clear_window_glass`）。

### 3.3 命令 `src-tauri/src/commands.rs`（注册进 `generate_handler` + `lib/ipc.ts`）

- `open_history(app)`：同进程新建历史窗口（默认当前项目，非 all），供弹窗导航栏调用。
- `history_init() -> { theme, project: { key, name } | null }`：返回本进程当前项目（来自 `AppState.project`）与主题 / 语言初值；`all` 由窗口 URL 参数读取。
- `get_history(project: Option<String>, all: bool) -> Vec<HistoryEntry>`。
- `get_history_projects() -> Vec<ProjectInfo>`。
- `history_count() -> usize`（设置页对比用）。
- `trim_history(limit: u32) -> usize`（设置页「立即清理」，返回裁剪后条数）。
- `clear_history(all: bool, project: Option<String>)`（清空全部 / 某项目）。
- 复用既有命令：`read_image_data_url`（缩略图）、`file_icon_data_url`、`open_path`、`preview_attachments`/`close_preview`、`set_theme`/`update_theme`（历史窗口随设置实时切主题可后续接 A12，本期至少初始主题正确）。

### 3.4 前端窗口

- `src/App.vue`：`view==='history'` → 渲染 `HistoryView`。
- `src/views/HistoryView.vue`（新增）：
  - 挂载：`historyInit()` 取主题 / 当前项目；读 URL `all`；初始 filter = `all ? 全部 : 当前项目.key`。
  - 加载 `getHistoryProjects()` 填下拉、`getHistory(filter)` 填列表。
  - 顶部：项目过滤下拉（当前项目 / 其他项目 / 全部）+ 「清空历史」（当前项目 / 全部，二次确认）。
  - 左列表：时间倒序；项每行显示相对时间、渠道徽标、动作状态（已提交 / 已取消）、消息或首题摘要；filter 为全部 / 其他项目时附项目名；点击选中。
  - 右详情：渲染所选 `HistoryDetail`；空态文案。
- `src/components/HistoryDetail.vue`（新增，**只读**）：
  - 顶部**状态横幅**：动作 + 渠道（`channel.sourceX` 本地化）+ 时间（相对 + 绝对），附项目 / 来源名。
  - 主体还原：message（按 `isMarkdown` 渲染）+ `-f` 附件区（复用打开 / 预览，缺失置灰）+ 每题（题干 + 选项已选高亮且不可点）+ 我填写的回复文本（只读呈现）+ 图片缩略图（`read_image_data_url`，失败显示占位）+ 回复文件胶囊（缺失置灰）；取消的记录显示横幅 + 无作答区；多题未答的题显示「未作答」。
  - **样式自带**（scoped），视觉与弹窗一致但不依赖、不改动 `PopupView.vue`（U8）。
- `src/lib/ipc.ts`：新增上述命令封装。
- `src/views/PopupView.vue`：仅在导航栏「设置」按钮旁新增「历史」按钮，`@click` 调 `openHistory()`；不动其余逻辑。

---

## 4. 设置页（条数配置 + 超额提示 + 立即清理）

`src/views/SettingsView.vue`（通用 Tab）：

- 新增「历史记录条数」数字输入，绑定 `config.general.historyLimit`（默认 200；0 = 停止新增并清理已有记录，附帮助说明）。
- 进入设置时调 `historyCount()` 取现有条数；当 `现有条数 > 输入值`（含输入 0）时，在该项下方显示一行小字：「已有日志超过该条数，将在下次调用 AskHuman 时清理」，并提供**链接样式「立即清理」**按钮：点击调 `trimHistory(输入值)`，成功后刷新计数、隐藏提示。
- 条数随既有「保存」流程持久化（保存本身不强制立即裁剪——裁剪发生在下次 `AskHuman` 调用或点「立即清理」，与提示文案一致）。
- i18n：新增标签 / 帮助 / 提示 / 「立即清理」文案。

---

## 5. 文案与文档

- i18n `src/i18n/*`：新增历史相关键——窗口标题、导航按钮 tooltip、列表 / 详情标签、状态横幅模板（提交 / 取消 + 渠道 + 时间）、项目过滤（当前项目 / 全部 / 其他）、空态、清空二次确认、相对时间词、设置项文案；渠道显示名复用既有 `channel.sourceX`。
- `src-tauri/src/cli/help.rs`：`--help` 增 `--history [--all]`。
- `docs/overview.md`：补充 history 模块 / 命令 / 独立窗口 / `historyLimit` / `history.jsonl` 存储 / 项目识别 / A11 revisit 说明 / 新增 view 路由。
- `docs/wiki/configuration.md`、`configuration.en.md`：补 `historyLimit` 配置项与 `AskHuman --history [--all]` 用法（中英）。
- `README.md` / `README.en.md`：简述「回复历史」功能（按既有详尽度酌情）。

---

## 6. 涉及文件清单

- 新增：`src-tauri/src/project.rs`、`src-tauri/src/history.rs`、`src/views/HistoryView.vue`、`src/components/HistoryDetail.vue`、`docs/specs/reply-history.md`、`docs/plans/reply-history.md`。
- 修改（Rust）：`paths.rs`、`config.rs`、`models.rs`（如需 `HistoryAnswer` 放此或独立）、`ipc/mod.rs`、`cli/mod.rs`、`cli/help.rs`、`app/mod.rs`、`app/coordinator.rs`、`daemon/request.rs`、`commands.rs`、`main.rs`（声明新模块）。
- 修改（前端）：`App.vue`、`views/PopupView.vue`（仅加按钮）、`views/SettingsView.vue`、`lib/ipc.ts`、`lib/types.ts`、`i18n/*`。
- 文档：`docs/overview.md`、`docs/wiki/configuration.md(.en)`、`README.md(.en)`、`docs/PROGRESS.md`。

## 7. 任务顺序

1. **存储核心**：`paths`（history 路径）、`project.rs`（detect + 单测）、`history.rs`（模型 + record/load/projects/count/trim/clear + 锁 + 单测）、`config.history_limit` + 默认、TS 类型。
2. **写入接线**：`render_result` 回吐图片路径 + `emit_result` 适配；`Coordinator` 增 project/source 与 `finish()` 旁路记录；`TaskRequest/ShowPayload.project`；CLI 计算 project；`daemon/request.rs`、`app/mod.rs`(run_ask/headless/gui_helper/settings)、`AppState.project` 全部接通。
3. **命令层**：`open_history`/`history_init`/`get_history`/`get_history_projects`/`history_count`/`trim_history`/`clear_history` + 注册 + `ipc.ts`。
4. **CLI 与窗口**：`--history [--all]` 分发、`run_history` + `View::History` + `create_history_window`（URL all 参数）、`help.rs`。
5. **前端窗口**：`App.vue` 路由、`HistoryView.vue`（列表 + 过滤 + 清空）、`HistoryDetail.vue`（只读还原 + 状态横幅）、样式、i18n。
6. **弹窗入口**：`PopupView.vue` 导航栏「历史」按钮 → `open_history`。
7. **设置页**：`historyLimit` 输入 + 超额提示 + 「立即清理」+ i18n。
8. **文档**：overview / wiki / README / help / PROGRESS。
9. **验证**：`pnpm build` + `cargo build` + `cargo test` + `./scripts/install.sh`，再用新装 `AskHuman` 端到端实测（见 §8）。

## 8. 测试策略

- **Rust 单测**：`project::detect`（git 根上溯 / 回退）；`history` jsonl 往返、`trim` 到 N、`load` 项目过滤 / 全部、`clear` 两范围、坏行跳过、`limit=0` 不记不裁。
- **手动 / 端到端**：
  - 弹窗回复 → 历史出现（渠道=弹窗）；如配置 IM，经钉钉 / 飞书 / Telegram 回复 → 渠道标注正确。
  - 用户主动取消（关弹窗）被记录；系统性取消（超时 / 断连 / `daemon stop`）不记录。
  - 多题部分未答：详情只读还原，未答题显示「未作答」。
  - 图片缩略图正常；临时图片被清理后显示占位、不报错。
  - `AskHuman --history` 默认当前项目；`--history --all` 全部；下拉切换项目 / 全部。
  - 设置 `historyLimit`：默认 200；设 0 停新增并清理已有记录；调小且超额（含填 0）→ 提示 + 「立即清理」即时裁剪；正常调用 `AskHuman` 自动裁剪到上限。
  - 清空（当前项目 / 全部）生效。
  - 历史窗口与弹窗并存对照；主题 / 玻璃与设置窗一致；全平台冒烟。
  - **回归**：stdout 结果区块、退出码（0/1/3）、多渠道抢答、`--settings`/`--help` 不受影响；记录失败不影响主流程。

## 9. 风险与注意

- **revisit A11（新增 project 上下文）**：历史需要项目维度，故 CLI 计算并上送 project（既有 A11 曾决定「不传 cwd」）。仅**新增**字段，不影响 `-f` 绝对路径与「文件不存在即退 1」语义。
- **`render_result` / `Coordinator` 构造器签名变更**：牵动 `emit_result`、`daemon/request.rs`、`app/mod.rs` 多个调用点，需一次性改齐编译。
- **旁路记录绝不污染主流程**：`record` 出错只警告；stdout / 退出码 / 输出时机一律不受影响。
- **图片 / 文件时效**：临时图片受 24h 清理影响，历史按路径 best-effort，缺失显示占位（既定）。
- **并发写**：多请求 / 多进程并发需文件锁 + 原子写；非 unix 退化为原子 rewrite，容忍极少数竞态。
- **短命 Helper 窗口**：Daemon 拉起的弹窗进程在作答 / 被抢答后退出，其同进程历史窗口随之关闭（可接受）。
- **`historyLimit=0` 语义**：停止新增**新**记录，但已有记录仍按与 `>0` 相同时机（`record` / 下次 `AskHuman`，以及「立即清理」）裁剪到 0（清空）；裁剪对 0 不再特判。
