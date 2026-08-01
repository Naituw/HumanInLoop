# 开发计划：Todo 图片与文件附件

> spec：`docs/specs/todo-attachments.md`
> 状态：已完成（2026-07-31）。全量 Rust / Vitest / 前端生产构建、隔离 CLI 与本地安装验证通过；
> 真实 Agent / IM 启动验证按 spec 的授权边界未执行。
> 本计划扩展现有 Todo 队列、执行历史与派发链路；不改变 IM `/todo` 创建卡，不改变普通 Ask
> 回答图片 / `-f` 图片的现有缩略图行为。

## 0. 目标与当前缺口

当前 `TodoEntry` / `DoneTodoEntry` 只有文字、时间、来源与 auto 标记，`todos.json` 由
`src-tauri/src/todos.rs` 直接读写。现有附件能力分散在普通 Ask 请求 / 回复链路：

- `cli/file_attachment.rs` 负责路径解析与 `FileAttachment` 元数据；
- Popup / History 已有附件胶囊、图片 data URL、打开、Quick Look、右键菜单与拖出；
- 回复图片写到 `temp/askhuman/<request-id>/`，daemon 按 24 小时清理；
- `QuestionAnswer.files` 与 whats-next `[files]` 已能把本地路径送回 Agent。

缺口不只是存储字段：Todo 会从六类路径开始执行，而现有路径都只取文字：

1. whats-next 人工选择；
2. whats-next auto 接管；
3. 普通 Popup 的折叠 Todo 区；
4. Agent Stop 确认卡；
5. GUI 新建 Agent 任务；
6. IM `/new` 选 Todo。

实施目标是先建立唯一的附件所有权 / 原子 CRUD / delivery 核心，再让所有入口和出口复用；不得在每条
链路各自复制文件和拼警告。

## 1. 依赖、模型与路径

### 1.1 Tauri 文件选择依赖

修改：

- `src-tauri/Cargo.toml`：增加 `tauri-plugin-dialog = "2"`；
- `package.json` / lockfile：增加 `@tauri-apps/plugin-dialog`，版本与项目 Tauri 2 依赖兼容；
- `src-tauri/src/app/mod.rs`：在所有 GUI 角色共用的 Builder 注册 `tauri_plugin_dialog::init()`；
- `src-tauri/capabilities/default.json`：只增加 `dialog:allow-open`，不授予 save / message 等无关权限。

API 与权限依据 [Tauri 2 dialog plugin 文档](https://v2.tauri.app/zh-cn/plugin/dialog/)；实现时以锁文件解析出的
同一 major 最新兼容版本为准，不在代码中手写平台文件选择器。

前端调用 `open({ multiple: true, directory: false })` 只取得路径。读文件、阈值判定、复制、缩略图与
持久化仍由 Rust command 完成，前端不能决定 `managed/reference`。

### 1.2 新模块与常量

新增 `src-tauri/src/todo_attachments.rs`，集中：

```rust
pub const MANAGED_MAX_BYTES: u64 = 10 * 1024 * 1024;
pub const MAX_ATTACHMENTS_PER_TODO: usize = 20;
pub const THUMBNAIL_MAX_EDGE: u32 = 128;

pub enum TodoAttachmentStorage { Managed, Reference }
pub struct TodoAttachment { /* spec §3.1 */ }
pub struct TodoAttachmentSnapshot { /* id + diagnostic display fields */ }
pub struct TodoDelivery { files: Vec<String>, warnings: Vec<...> }
```

职责拆分：

- 路径规范化、普通文件 / 控制字符校验、同源去重；
- 小文件有界流式暂存，大文件引用；
- 托管目录 / 文件权限与安全删除；
- 128 px PNG 缩略图生成、freshness 与 data URL；
- 请求级交付 hard-link / copy 与缺失诊断；
- 孤儿 / `.stage-*` 有界清理。

`cli/file_attachment.rs` 的 `expand_tilde`、图片扩展名判定和文件名净化应抽为可复用小函数，避免 Todo
重新维护另一份格式表；普通 Ask 行为保持原样。

### 1.3 路径函数

在 `src-tauri/src/paths.rs` 增加：

```rust
pub fn todo_attachments_dir() -> PathBuf;
pub fn todo_attachment_dir(todo_id: &str) -> PathBuf;
pub fn todo_delivery_dir(request_id: &str) -> PathBuf;
```

所有接收 ID 的路径 helper 先做 UUID 校验。安全删除 helper 只接受由根目录 + UUID 派生的路径；JSON 中的
`path` 仅用于核对和读，不直接作为 `remove_dir_all` 目标。

### 1.4 持久化模型与 DTO

在 `todos.rs`：

- `TodoEntry` / `DoneTodoEntry` 增加 `attachments`，serde default + empty skip；
- `take_at` 构造历史、`restore_at` 恢复时完整 clone / move 附件；
- 所有测试构造器补默认空数组，旧 JSON 兼容测试显式覆盖无字段输入。

持久化模型不存瞬时 `available`。在 `commands.rs` / MCP / CLI 展示边界构造 DTO：

```rust
TodoAttachmentView {
    id, name, size, is_image, storage,
    path, available, thumbnail_available
}
```

GUI 需要 path 以复用打开 / Quick Look；`thumbnail_path` 不直接交给前端，改由受控命令按
project + todo ID + attachment ID 读取，防止 UI 使用过期或已被移除的缓存。

### 1.5 快照传输模型

在 `models.rs` 给承载 Todo 的 `OptionItem` 增加兼容字段：

```rust
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub todo_attachments: Vec<TodoAttachmentSnapshot>;
```

- 普通 Option 恒空；`with_todo` 增加接收 entry / snapshots 的新构造器，旧构造器可保留给测试或一次性迁移。
- 反序列化旧 Option 对象 / 纯字符串时字段为空。
- `daemon/ask_dedup.rs` 指纹 feed 每个 snapshot 的稳定字段，至少含 Todo ID、附件 ID、storage 与 path；
  顺序与 Todo 中一致。

普通 Popup Todo 区不经过 `OptionItem`。给 `QuestionAnswer` 增加默认省略的结构化选择：

```rust
pub struct TodoSelection {
    pub todo_id: String,
    pub attachments: Vec<TodoAttachmentSnapshot>,
}

pub todo_selections: Vec<TodoSelection>
```

过渡期保留 `todo_ids` 读取兼容；新 Popup 同时只写 `todo_selections`，`ids_to_dequeue` 汇总两者并去重。
避免只增加 `HashMap<id, ids>`，因为警告还需要快照名称 / 路径。

## 2. 附件存储核心

### 2.1 暂存批次

实现 `stage_attachments(todo_id, raw_paths, existing, cwd)`：

1. 校验 todo UUID、路径非空且不含 NUL / `\r` / `\n`（`[files]` 是一行一路径，换行路径无法安全表达）；
2. `~` 展开、相对 cwd 拼接、canonicalize、metadata 是普通文件且可打开；
3. 请求内按 canonical source path 保序去重；已存在 source path 跳过；
4. 先应用本次 remove 集合，再计算添加后总数，>20 整批失败；
5. 对每个新文件做有界读取：最多读 `MANAGED_MAX_BYTES + 1`；
6. ≤阈值写 `<todo-id>/.stage-uuid/files/...`，>阈值丢弃暂存原文件并记录 reference；
7. 对图片 best-effort 生成 `.stage-uuid/thumbnails/<attachment-id>.png`；解码失败只记降级状态；
8. 返回 RAII `StagedAttachmentBatch`，未 commit drop 时清理 stage。

文件名用 attachment UUID + 净化后的原名，保留可识别扩展名但不依赖它定位。打开源文件后读取其 metadata，
减少 check/open 竞态；复制过程出错返回带原路径的本地化错误。

### 2.2 缩略图生成

基于现有 `image 0.25`：

- 显式设置 decoder limits / 最大像素数；先读取 dimensions，再解码；
- `thumbnail(128, 128)` 或等价保持比例算法，不放大小图；
- 编码 PNG，写临时文件后 rename；Unix 0600；
- 首版只承诺依赖已启用的 PNG / JPEG / WebP。不要为 spec 未承诺格式一次性打开所有 image feature；
- managed 图片从暂存 / 托管原文件读；reference 图片从当前 source 读；
- reference 缓存读取前比较源文件与缩略图 modified time（必要时再结合持久化 source metadata），
  过期则在 `spawn_blocking` 中重建；重建失败返回无缩略图，不返回旧图。

新增 Tauri 命令：

```text
todo_attachment_thumbnail(project, todo_id, attachment_id) -> Option<data-url>
```

命令先从当前 pending / history 查 membership，再读取缓存，避免已移除附件仍通过旧 path 被 UI 加载。

### 2.3 原子创建 / 更新 API

在 `todos.rs` 增加 path-parameterized、可测的核心 API；不要把复制逻辑塞进 Tauri command：

```rust
add_with_attachments(project, text, auto, agent_kind, raw_paths, cwd)
update_entry(project, id, expected_text, expected_attachment_ids,
             new_text, keep_attachment_ids, add_paths, cwd)
attach_by_index(project, todo_index, raw_paths, cwd)
detach_by_indexes(project, todo_index, attachment_indexes)
update_attachments_by_id(project, todo_id, add_paths, remove_ids, cwd)
```

`update_entry` 是 GUI 的原子 Save：

- 进入编辑时冻结原始 text + 有序 attachment IDs；保存时作为 expected 值；
- 锁内当前值不一致则返回 `Conflict`，保留前端草稿并提示刷新，不覆盖并发编辑；
- auto / 队列位置不在 expected 中，行内 ⚡ 或重排的并发变化保留；
- remove 后 add，因此“移除同源旧附件 + 重新加入”可刷新托管内容；
- keep IDs 必须是 expected/current 子集，不能通过伪造路径注入。

CLI index API 在同一把锁中解析 1 基 Todo / 附件编号并修改；MCP ID API 在锁中验证稳定 ID。

将当前只返回 bool / Option、可能忽略 `store_at=false` 的用户主动 mutator 逐步收敛为明确 `TodoError`，至少
新附件路径不能误报成功。既有内部 best-effort 调用可显式忽略 Result，但 Tauri / CLI / MCP 必须展示错误。

### 2.4 生命周期清理

扩展以下路径，在 JSON 成功后调用 `cleanup_owned`：

- `remove_at`：目标 pending Todo；
- `clear_at`：项目全部 pending Todo；
- `clear_history_at`：项目全部 done Todo；
- `take_at`：history limit 为 0 时 taken attachments；
- `take_at` 历史超限 trim：被 drain 的 DoneTodo attachments；
- `update_entry` / detach：被移除 attachments；
- 后续若新增单条历史删除，同样复用。

`take` 进入历史且未被 trim、`restore` 不清文件。清理失败写 stderr / daemon log 并留给 orphan GC，不回滚
已经正确提交的 JSON，也绝不删除外部 source。

为单测引入 `TodoStorePaths { json, lock, attachments_root, temp_root }` 或等价测试上下文，避免测试触碰真实
`~/.askhuman` 与系统 temp。

## 3. 请求级交付核心

### 3.1 `prepare_todo_delivery`

在 `todo_attachments.rs` 实现统一入口：

```rust
prepare_todo_delivery(store, project, todo_id, expected, request_id) -> TodoDelivery
```

算法：

1. 读取当前 Todo（pending 优先；必要时允许指定来源），按 attachment ID 与 `expected` 求交集；
2. expected 中当前不存在的项生成 `Removed` / `TodoMissing` warning，不因外部 source 尚在而送达；
3. managed 有效项在请求临时目录先尝试 `hard_link`，跨文件系统 / 不支持时 fallback `copy`；输出文件
   使用 `<attachment-id>-<safe-name>` 防重名；
4. reference 有效项重新 metadata / 可读性校验，直接返回 canonical path；
5. 找不到 / 复制失败项生成带 name + source path 的 warning，继续其它项；
6. 交付文件目录使用 0700 / 文件 0600；失败时删除该项半文件。

warning 结构化保存 kind / name / path，再由当前 `Lang` 渲染成统一任务段，例如：

```text
Attachment status:
- Unavailable: screenshot.png (/path/...)
```

不得使用 `[status]` marker。中文 UI 下可本地化标题与原因；绝对路径保持原文。

### 3.2 delivery 与 take 的线性化

删除必须立即使旧卡附件失效，同时 delivery 不能在 `take` 清理源文件后才复制。实现两阶段重验：

1. 锁外根据快照暂存 delivery；
2. 获取 `todos.lock`，重读并确认 Todo / expected attachment IDs 仍与准备时一致；
3. 若删除 / 编辑已先提交：丢弃相应 delivery 文件，按最新 membership 生成 warning；
4. 若一致：在同一临界区提交 `take` 或标记本次选择已开始执行；
5. JSON 成功后清 Todo 托管源（仅 history limit=0 / trim 场景），delivery inode / 副本继续存在 24 小时。

不要在锁内顺序复制 20×10 MiB。为减少正常路径 IO，managed 首选 hard link；测试必须覆盖 hard-link 失败的
copy fallback。

现有 `Coordinator.submit` 是同步 API 且在存 result 后立即 `take`。实现时抽出明确的“Todo 终态准备”阶段，
在持有 Coordinator 内部 mutex 之外做文件 IO，然后再存 winner / result、取消 losers。`terminal.try_set` 仍必须
先占首答，防止准备期间第二个渠道成为赢家。

## 4. CLI 与 MCP

### 4.1 CLI 解析与输出

修改 `src-tauri/src/cli/todo_cmd.rs`：

- 为 `add` 写局部、可测 parser，识别重复 `-f/--file`、`--auto` 与 `--`；缺 file value / text 报错；
- 新增 `attach` / `detach` 分支；
- `list` 在 Todo 行下输出附件编号与状态；多行 Todo 正文既有格式不改变；
- 成功输出包含实际添加 / 移除数量；重复源路径被忽略时明确说明，不虚报全部新增；
- 用户主动操作的 persist / conflict / invalid file / cap 错误走 stderr + exit 1；
- `cli/help.rs` 中普通 help / agent help 中英双语同步新用法。

给 `todo_cmd.rs` 增加 parser 与隔离存储测试；不要通过测试进程 `exit` 验证核心逻辑，把 parse / execute 尽量抽成
返回值函数。

### 4.2 MCP schema 与工具

修改 `src-tauri/src/mcp/ask.rs` / `mcp/mod.rs`：

- `TodoAddParams` 增 `files: Option<Vec<String>>`；
- 新增零参数 `TodoListParams`（保留隐藏 session token 兼容模式，如 MCP 基础设施要求）；
- 新增 `TodoUpdateParams { todo_id, add_files, remove_attachment_ids }`；
- 注册 `todo_list` / `todo_update`；更新 server 顶部文档和工具总数测试；
- `todo_list` annotations：read-only、non-destructive、closed-world；
- `todo_update` annotations：destructive hint 为 true（可移除附件并删除托管副本）、closed-world；
- `todo_update` 先走 `codex_thread_guard`，与 `todo_add` 相同；
- 文件处理放 `spawn_blocking`，避免 STDIO MCP async runtime 被最多 20 个文件的 IO / 图片解码阻塞；
- `todo_list` 返回模型易读文本，并附 structured content（如 rmcp output schema 成本可控）以稳定传递 UUID；
- `todo_add` 成功文本增加 Todo UUID，供立即跟进 update。

测试：schema 包含新字段、工具注册、空 update invalid params、未知 ID、guard 拒绝、相对 cwd、成功返回 ID、
托管 / reference 摘要与无附件旧调用兼容。

### 4.3 托管提示词边界

Todo 附件主要由人通过 GUI 管理，**不修改** `prompts.rs` 的 CLI / MCP reference，也不在 MCP server
全局 instructions 中宣传附件管理入口。Agent 的全局纪律只保留既有“用户要求记录延后任务时才创建
Todo”，不额外引导 `-f`、`todo attach/detach`、`todo_list` 或 `todo_update`。

CLI / MCP 入口仍为显式调用保留，并由各自 help / tool schema 描述参数；`todo_update` 的工具级说明继续
限制为人明确要求时才能增删文件。string tests 固定全局提示词不出现附件管理入口，防止后续回归。

## 5. GUI 待办窗口

### 5.1 类型与 IPC

`src/lib/types.ts`：

- 增加 `TodoAttachmentStorage`、`TodoAttachmentView`；
- `TodoEntry` / `TodoDoneEntry` 增加 `attachments`（前端容错用 `?? []`）；
- 增加 `TodosUpdatePayload` / 结构化 error（如 command 返回 code + message）。

`src/lib/ipc.ts`：

- `todosAdd(project, text, auto, filePaths, pastedImages)`；
- `todosUpdate(project, id, expectedText, expectedAttachmentIds, text, keepAttachmentIds, addPaths)`；
- `todosUpdateAttachments(project, id, addPaths, removeAttachmentIds)`，供非文字编辑态直接增删；
- `todosAttachPastedImages(project, id, images)`，把选中行收到的剪贴板图片写成托管附件；
- `todoAttachmentThumbnail(project, todoId, attachmentId)`；
- 既有 remove / clear / complete / history clear 的签名可保持，后端错误改为 reject 供 UI 展示。

`commands.rs`：

- `todos_list` / `todos_history` 映射 view DTO 并实时计算 availability；
- `todos_add` 接收 `file_paths` 与可选 `pasted_images`，先把剪贴板 blob 写到请求级临时目录，再把两类路径
  合并后在 `spawn_blocking` 调核心；无论成功失败都清临时目录；
- 新增 `todos_update`、`todos_update_attachments`、`todos_attach_pasted_images`、
  `todo_attachment_thumbnail`；
- remove / clear / history clear 传播用户可见 persist 错误；
- invoke handler 注册新命令。

### 5.2 可复用附件行组件

新增 `src/views/todos/TodoAttachmentList.vue`：

- props：attachments、removable、busy、confirmRemoveId；emit remove / cancelRemove；
- 28–40 px 图片位只调用 `todo_attachment_thumbnail`，失败显示文件图标；按可见性懒加载，避免一次读全列表；
- 默认折叠为数量摘要，最多加载 / 显示 3 张图片缩略图，剩余项显示 `+N`；展开后才加载完整资源列表；
- 摘要缩略图用小 gap 并排，不用负 margin 堆叠；
- 完整列表展示 name、格式化 size、unavailable，不显示 managed/reference；完整有效 path 仅作为 title /
  操作目标；
- 单击选中、Enter / 双击打开、空格 Quick Look、方向键在当前 Todo 内移动；
- macOS 右键 / 拖出复用 `showAttachmentMenu`、`startDrag`；unavailable 禁用操作；
- Quick Look 监听保持**窗口级单例**，不要让每个 Todo 行各注册 `preview-index` / `preview-closed`；
  子组件启动预览时上报按可用文件顺序排列的附件 ID，父窗口把原生 `preview-index` 映射回 ID，再通过
  `previewSelectedId` 驱动来源列表移动选中框与焦点；
- Pending Todo 展开列表里的移除按钮第一次只切换为“确认删除”，第二次才调用独立附件命令；点击其它
  位置取消确认。历史行不传 `removable`，保持只读。

可以从 `useAttachments.ts` 抽无 Popup 假设的 open / preview / format helper，但不能让 Popup 行为回归；若复用
成本高，先提取小型纯函数，Todo 保留自己的窗口级 controller。

### 5.3 新增草稿

`TodosView.vue` 增：

- `newFiles: string[]`、`newFileThumbs`、`newPastedImages`、选择 busy / error、隐藏 drop target 高亮；
- 回形针按钮调用 dialog open；选择 / 拖入保存 path，并为图片异步读取草稿缩略图，不自己决定托管类型；
- 同一路径草稿去重、20 数量前置提示；后端仍最终校验；
- `addEntry` 一次传 text / auto / paths / pasted images，成功才清空文字与附件草稿；失败保留草稿和
  明确错误；
- Todo 文字仍必填，只有附件不启用 Add。

原生 `getCurrentWebview().onDragDropEvent` 根据落点决定目标：新增 footer 或任意 Pending Todo 行；enter /
over 时持续显示命中反馈，未落入有效区域不添加。macOS / Linux 优先按逻辑坐标解析，Windows 优先按
物理坐标除 DPR，并以另一种解释兜底。

Tauri 原生 drag-drop handler 与 HTML5 排序 DnD 在 macOS 互斥，因此移除窗口上的
`disable_drag_drop_handler()`，排序手柄改用 Pointer Events；系统文件 drop 不触发排序。

### 5.4 已有行编辑与附件管理

- 文字“编辑”只建立文字 draft；行内回形针直接打开 picker，不改变文字编辑状态；
- 附件添加 / 移除调用 `todosUpdateAttachments`，成功即替换本地 row，无需再点 Save；失败在所属行显示；
- Save 仍调用 `todosUpdate`，但 keep IDs 使用当前附件快照且 add 为空；本窗口完成的附件变更同步更新
  expected IDs，外部并发变化仍返回 Conflict；
- Cancel 只丢弃文字草稿，不回滚已独立完成的附件操作；
- 删除 / 完成 / 切项目 / `todos-updated` 发现目标消失时取消；若目标仍在但内容并发改变，不静默覆盖草稿，
  标记 stale 并让 Save 得到 conflict；
- copy 仍只复制 Todo 文字，不隐式复制文件或路径；新建 Agent 任务按钮行为见 §7.5。

### 5.5 行选择、键盘与剪贴板

- paste 目标是新增 textarea 时，把 `image/*` 转为内存中的 `ImageAttachment` 草稿，与已选文件共享 20 个
  上限；提交时随 `todosAdd` 原子创建，失败保留草稿、成功清空；普通文字粘贴不拦截；
- 抽取 `ComposerAttachments.vue` 给 Popup 与 Todo 新增区共用；图片为 64px 缩略图 + 右上角删除按钮，
  普通文件为可删除 chip。Todo 的系统路径图片通过 `readImageDataUrl` 生成草稿预览，提交仍保留原始路径；
- `selectedTodoId` 独立于文字 `editingId`；点击 / 聚焦 Pending 行选择，上下键按当前队列顺序移动并
  `scrollIntoView({ block: "nearest" })`；编辑输入框、项目 select 和展开附件列表保留各自方向键语义；
- selected 行使用稳定 accent 边框，并把 hover-only 操作按钮常亮；完成 / 删除 / 项目切换或实时重载
  使目标消失时清空选择；
- 窗口级 paste 只在存在 selected Todo 且剪贴板含 `image/*` file item 时阻止默认；普通文字照常交给
  当前输入控件；
- 前端用现有 `fileToDataUrl` 构造 `ImageAttachment`，Rust 命令限制编码 / 解码大小、为同名多图生成
  唯一临时名、调用正常附件事务，并在成功 / 失败后清请求级临时目录；剪贴板图片 >10 MiB 拒绝，
  不允许退化成临时路径 reference。

### 5.6 历史与状态

- Done 行复用只读附件列表；恢复后 pending 行正常编辑；
- 清空确认文案无需列文件，但执行失败必须显示错误，不能关闭确认后假装成功；
- missing、摘要数量、拖入反馈、直接移除与附件上限增加中英文 i18n；managed/reference 仅保留在诊断
  数据与 CLI / MCP，不进入 GUI 展示；
- `todos-updated` 继续重载，缩略图 key 含 attachment ID + freshness，移除后清缓存 / 关闭涉及该文件的预览。

## 6. whats-next、Popup 与 Stop

### 6.1 Todo 选项标签

新增统一 formatter / parser，不继续靠只剥 `todoPrefix`：

```text
执行待办：修复登录 【2 个附件】
Run todo: Fix login 【2 attachments】
```

- `OptionItem` 内仍保存 raw Todo text / snapshots，输出不要从展示 label 逆向猜原文；
- 若为兼容暂时仍调用 `strip_todo_prefix`，同时可靠去掉只由内部生成的 attachment suffix；
- Feishu / DingTalk 现有 `【TODO】` 视觉替换保留，附件计数以灰色 `【N 个附件】` 展示；Slack / Telegram 显示同一无色计数；
- IM 不上传 Todo 本地附件。

优先给 `OptionItem` 增加 `todo_text: Option<String>`（serde default / skip）作为真正 raw 值，逐步停止从 label
剥前缀；旧请求没有该字段才 fallback parser。这会显著降低本次 suffix 和未来展示变化的脆弱性。

### 6.2 Coordinator 人工路径

调整 `app/coordinator.rs::submit`：

1. `terminal.try_set` 抢首答；
2. 从 request OptionItem snapshots 与 `result.todo_selections` 收集每题 Todo 选择；
3. 在 Coordinator mutex 外准备 delivery，并把有效路径 / warning 合入对应 `QuestionAnswer`；
4. 在 delivery / take 线性化 helper 中出队；
5. 再存最终 result、取消 losers、进入既有 finalize / render / history。

需保持：

- 取消不准备附件 / 不出队；
- 同一回答多处选择同一 Todo 去重；
- 普通 Popup Todo 文字仍进最后一题，文件 / warning 也进同一题；
- whats-next 选择的 Todo 文件进入最终 `[files]`；
- reply history 记录合入后的 delivery paths / warning；
- 用户回复自己附加的 files 与 Todo files 保序合并并按 canonical path 去重，不误删用户项。

### 6.3 自动路径

修改 `cli/mod.rs::try_whats_next_auto`：

- 生成请求 ID，按当前 entry attachments 构 snapshots；
- delivery + take 使用同一原子 helper；被并发拿走回落普通 whats-next；
- `whats_next_output` files 参数传实际 delivery；
- HistoryAnswer `files` / `user_input` 或选择摘要记录实际路径和警告；
- attachment count 不涉及 UI，因为 auto 不显示卡。

### 6.4 普通 Popup Todo 区

修改：

- `src/views/popup/whatsNextTodos.ts` / `TodoSection.vue`：Todo view 含附件数量，选中时冻结 snapshots；
- 普通 Todo 行显示灰色 `【N 个附件】`（本地 Popup 可选进一步显示附件胶囊，但 spec 首版只要求选择行计数；不要把
  Todo 管理窗口的完整预览塞进普通提问）；
- `usePopupCore.ts` submit 构造 `todoSelections`；已有 `todoIds` 前端停止写；
- 实时 reload 删除目标时清除选择；只移除部分附件时更新列表但不改已经冻结的 snapshot，派发端交集校验。

补前端纯函数测试：计数 label、snapshot 冻结、新增不混入、删除 Todo 清选择、select-only。

### 6.5 Stop 卡

修改 `agents/stop.rs`：

- `build_task` 用 entry 构带 raw text + attachment snapshots 的 OptionItem 和 `【N 个附件】` label；
- `parse_ask_decision` / continuation 构造接收 `QuestionAnswer.files` / delivery warnings，或改由 Coordinator
  已合入结果后读取；
- Claude 包裹、Codex / Cursor 裸 continuation 的既有分流不变；附件段追加在任务 / 补充之后；
- auto Todo 仍不自动，Grok 仍不支持 Stop，`record_history=false` 不变。

测试四分支之外新增：2 附件 label、有效 delivery、移除后 warning、外部 missing、结束 / 继续选项无附件。

## 7. 新建 Agent 任务

### 7.1 前端 / IM 快照

`NewTaskForm.vue` 的 `selectedTodo` 增附件 snapshots，并在只读 Todo 原文下复用精简附件列表 / 灰色 `【N 个附件】`；
`finalTask` 仍只计算用户正文，不提前拼路径。

IM `TaskInputSourcePayload` 已序列化 TodoEntry 快照；模型字段扩展后自然带 attachments。四个渠道的任务来源
选项显示 `【N 个附件】`（支持颜色时为灰色），不上传文件。

### 7.2 LaunchRecord

修改 `integrations/agent_launch.rs`：

```rust
LaunchRecord {
  ...,
  #[serde(default)] files: Vec<String>,
  payload_sha256: String, // task + ordered files; replace or complement task_sha256
}
```

- `create_record` 接受 delivery files；task trim / 3000 chars 校验仍只针对用户任务正文；
- path 数量 ≤20、无 NUL / CR / LF；记录 0600；
- `validate_claim` 校验 task + ordered paths payload hash；delivery 文件暂时缺失不在 helper 前崩溃为模糊 hash
  错误，而是构造 prompt warning / 清晰失败策略按 spec 保持“文字仍可执行”；
- `run_helper` 通过纯函数 `task_with_attachments(task, files, warnings)` 拼初始 prompt，再作为单个 argv 传 Agent；
- prompt 中 path 使用可逆引用 / JSON quoting，文件名含空格、引号时不产生歧义；绝不拼进 shell command。

### 7.3 GUI `new_task_launch`

- Tauri 入参增加 selected attachment snapshots（或后端通过 todo ID + request snapshot token 获取；不能只读最新并混入
  新附件）；
- `spawn_blocking` 内先按当前 membership 准备 delivery，再 create record / open Terminal。准备阶段获取 Todo 锁
  的先后顺序就是附件派发与手动删除的线性化点：删除先提交则只产生 warning；启动先冻结成功则后续删除
  不撤回这个已经发起的 launch；
- Terminal 打开失败：不 take Todo，删除未被 Agent 使用的 delivery 目录；
- 成功：原有 best-effort take 仍按 Todo ID 出队；期间若用户已手动删除，take 为空但已冻结的任务 / delivery
  照常启动，与既有“卡片快照仍执行”语义一致；
- delivery warning 进入 record prompt；成功后 popup active slot 行为不变。

### 7.4 IM `/new`

`daemon/unix_impl/inbound.rs` 的 task input submit：

- 选 Todo 时从 `TaskInputSourcePayload` 取 attachments snapshot；
- permission / workspace 流程结束、真正 launch 前做 delivery；
- 四家来源渠道的 PendingLaunchWatch / active slot / watch 语义不变；
- Terminal 成功后 take，失败保留；附件不回传 / 上传到 IM，只在成功 / 失败文案中可显示“含 N 个附件”。

### 7.5 文件可读性

统一绝对路径 prompt 是四家最低共同能力。对新启动 CLI：

- Codex / Claude / Cursor 均支持额外目录参数，但本需求首版不必为所有 delivery 文件扩大可写目录；先验证它们
  在现有 default / yolo 模式下可读系统 temp；
- 若某家默认沙箱确实不能读 temp，只在该 adapter 加最小只读 / add-dir 授权，不把差异泄露到 Todo 模型；
- Codex `-i/--image` 等原生图片 flag 可作为后续增强，不作为首版正确性的唯一路径；
- Grok 无统一 add-dir flag，真实验收必须覆盖至少一个附件读取；失败时先按其 permission rule / prompt 能力修 adapter。

任何真实 Agent 启动 / 可能计费的验证仍受 `im-agent-task-launch` D27：必须先经 AskHuman 明确批准。

## 8. i18n、文档与兼容

### 8.1 i18n

`src/i18n/zh.ts` / `en.ts` 增：附件、添加文件、托管 / 外部引用、文件不可用、最多 20、重复忽略、
保存冲突、缩略图降级等 Todo GUI 文案。

`src-tauri/src/i18n.rs` 增加 CLI / delivery warning / MCP 人类可读错误：缺文件、目录、超限、Todo / 附件 ID
不存在、persist / copy / conflict、managed/reference 状态。代码注释仍用英文。

### 8.2 文档

实现时同步：

- `docs/specs/todo-whats-next.md`：D1 数据模型、D2 / D3、D5、D7、D9、D11 与测试要求；
- `docs/specs/gui-agent-task-launch.md` / `im-agent-task-launch.md`：Todo 快照、LaunchRecord、任务 prompt 附件；
- `docs/specs/file-attachments.md`：引用本 spec，明确 Todo 缩略图是独立受控缓存，不改变普通 Ask；
- `docs/wiki/`、README（若现有 Todo CLI / MCP 用法在用户文档中出现）；
- `--help` / `--agent-help` 与 MCP tool descriptions；
- `docs/overview.md` 仅在新增 `todo_attachments.rs` / Todo 前端子目录成为长期模块后更新目录地图与一句跨模块
  不变量；不记录 Phase 进度。

实施完成后把 spec / plan 状态改“已实现（日期）”；不在 `docs/PROGRESS.md` 留已完成项目。若分阶段提交且
当前分支结束时仍有真实未完成部分，再按项目规则记录具体下一步。

### 8.3 序列化 / 升级兼容

- 旧 `todos.json`：attachments default 空；不批量迁移；
- 旧 `AskRequest` / `OptionItem` / `QuestionAnswer`：新 snapshot / selections default 空；
- 旧 `LaunchRecord`：files default 空；一次性 record TTL 5 分钟，升级跨版本只需安全失败 / 空兼容；
- MCP `todo_add` 新字段 optional；旧 client 不受影响；
- IPC 新字段全部 serde default，daemon drain / GUI Helper 跨版本窗口内不因缺字段崩溃；
- 前端所有 `attachments` 读取用默认空数组，避免旧宿主 payload undefined。

## 9. 自动化测试

### 9.1 Rust：存储与图片

在 `todo_attachments.rs` / `todos.rs` 隔离 temp root 覆盖：

- 0、1、恰好 10 MiB、10 MiB+1；copy 中源增长不越界托管；
- `~` / 相对路径 / symlink canonical、目录、缺失、不可读、NUL / CR / LF；
- 请求内重复、既有重复、remove+readd、20 / 21 个；
- managed 目录 / 文件权限（Unix）、文件名净化、同名文件不同 ID；
- PNG / JPEG / WebP 128 px、宽高比、不放大小图、透明通道；损坏 / 超像素 / 不支持格式降级；
- reference 缩略图过期重建、源缺失不展示旧缩略图；
- store 失败回滚 staged / final 文件，JSON 不出现半附件；崩溃孤儿 GC 只删安全根内目标；
- update expected conflict、CLI index 在锁内解析、MCP stable ID；
- remove / clear / take history=0 / history trim / clear history / restore 的精确清理；外部 source 始终存在；
- 旧 JSON roundtrip 与空字段省略。

### 9.2 Rust：delivery 与派发

- snapshot ∩ current：新加不混入、移除立即失效、整个 Todo 删除 warning；
- managed hard-link、copy fallback、交付重名、部分 copy 失败、reference valid / missing；
- delivery 成功后源 Todo 文件删除，临时路径仍可读；
- 删除先赢 / 执行先赢两种线性化顺序；
- whats-next 手动 / auto 输出 `[files]` 与 warning，失效路径不进入 files；
- 普通 Popup 最后一题合并、多个 Todo / 用户 reply files 去重；
- Stop continuation 四家既有语义；
- attachment label `【N 个附件】` 与 raw todo text 输出不含 suffix；Feishu / DingTalk 将计数单独渲染为灰色且 TODO 样式不回归；
- daemon dedup：同文字同 snapshots 合流，附件 ID / path 不同不合流；
- LaunchRecord payload hash、旧 record default、prompt path quoting、3000 字正文边界不计内部路径；
- Terminal 失败不 take / 清 unused delivery（用 mock，不打开真实 Terminal）。

### 9.3 CLI / MCP

- `todo add --auto -f a -f b -- -leading text`；缺 value / 缺 text / 重复 / cap；
- attach / detach 多编号、无效编号、并发重排不误改；list 状态；
- MCP tools/list schema、todo_add files optional、todo_list stable IDs、todo_update add/remove/empty/unknown；
- thread guard 拒绝副作用但允许 list；persist failure 不返回 success；
- Rules / agent help 保持既有主交互协议且不宣传附件管理入口；工具级 schema / help 自描述显式调用参数。

### 9.4 Frontend Vitest

- 新增 / 编辑 DraftFile 去重、20 上限、Save / Cancel、conflict 不丢草稿；
- TodoAttachmentList managed/reference/missing、缩略图成功 / fallback、remove emit；
- 文件 drop 落点映射到 footer / 编辑行，内部排序 drag 不误识别；
- keyboard open / preview、unavailable 禁用、单例 preview focus；
- history row read-only、restore 后 editable；
- Popup attachment count、snapshot 冻结、实时删除清选择、select-only；
- NewTask selected Todo 显示附件但 finalTask 字数只算正文。

## 10. 构建与手工验收

功能实现后按仓库规则执行：

1. `pnpm test`
2. `pnpm build`
3. `cargo fmt --all --manifest-path src-tauri/Cargo.toml -- --check`
4. `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --all-features -- -D warnings`
5. `cargo test --manifest-path src-tauri/Cargo.toml`
6. `./scripts/install.sh`

安装后只用可丢弃临时文件 / 隔离 `ASKHUMAN_HOME` 验收破坏性场景：

- GUI add / edit / cancel / drag / thumbnail / Quick Look；
- 10 MiB 边界、>10 MiB 引用、删除不碰 source、历史恢复 / 清空；
- CLI 与 MCP 创建 / 更新互相实时同步 GUI；
- whats-next manual / auto 与普通 Popup 的真实 `[files]`；
- 删除附件后旧卡执行出现 warning；
- IM 卡只显示 `【N 个附件】` 且不上传文件；
- 新建 Agent 任务真实读取文件只在 AskHuman 再次明确批准后执行；未批准前只测 mock LaunchRecord / helper。

验证系统 temp 的 24 小时清理由现有单测 / 缩短时钟测试覆盖，不为了验收真实等待 24 小时。

## 11. 实施顺序与提交边界

### Phase 1：模型、存储与生命周期

1. `todo_attachments.rs`、paths、模型 / serde 兼容；
2. 阈值复制、缩略图、权限、安全清理；
3. `todos.rs` 原子 add / update / remove / history lifecycle 与隔离测试。

建议提交：`feat(todos): add managed todo attachment storage`

### Phase 2：CLI / MCP 管理入口

1. CLI add `-f`、attach / detach / list；
2. MCP todo_add files、todo_list、todo_update + guard；
3. CLI / MCP help、i18n 与测试；全局 agent prompts 不新增附件引导。

建议提交：`feat(todos,mcp): manage todo attachments from cli and mcp`

### Phase 3：GUI 行内管理

1. dialog plugin / capability；
2. DTO / commands / IPC；
3. TodoAttachmentList、创建 / 编辑 draft、文件 drop、历史只读；
4. frontend tests / i18n。

建议提交：`feat(todos): add inline attachment management`

### Phase 4：统一 delivery 与既有 Agent 链路

1. snapshot 模型 / raw option text / dedup；
2. Coordinator manual + Popup + auto；
3. Stop；
4. LaunchRecord、GUI / IM new task；
5. `【N 个附件】` 四渠道展示（富文本入口灰色）与 delivery tests。

建议提交：`feat(todos): deliver attachments across todo execution paths`

### Phase 5：文档、安装与验收

1. 更新关联 specs / wiki / overview 边界；
2. 全量测试、clippy、build；
3. `./scripts/install.sh`；
4. 新安装 AskHuman 本地验收；真实 Agent / IM 场景另行获批。

文档-only 收尾可并入对应功能提交；如单独提交使用：
`docs(todos): document todo attachment behavior`

每个 Phase 必须保持旧无附件 Todo 完整可用。Phase 1–3 期间若 delivery 尚未接通，不应把功能暴露为“可执行
附件已支持”的正式入口；最安全的合并方式是同一分支完成 Phase 4 后统一交付。
