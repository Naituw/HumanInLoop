# 需求：Todo 图片与文件附件

> 状态：已实现（2026-07-31）；自动化、生产构建、隔离 CLI 与安装验证通过。按既有授权边界未启动真实 Agent 或向真实 IM 派发附件。
> 关联计划：`docs/plans/todo-attachments.md`
> 依赖 / 复用：`docs/specs/todo-whats-next.md`（Todo 队列、执行历史与派发出口）、
> `docs/specs/file-attachments.md`（附件展示、打开、Quick Look 与回复 `[files]` 契约）、
> `docs/specs/gui-agent-task-launch.md` 与 `docs/specs/im-agent-task-launch.md`（新 Agent 任务启动）。

## 1. 背景与目标

项目 Todo 当前只有文字。很多稍后任务还依赖截图、设计稿、日志、报告或其它本地文件；只记录一句文字，
等 Agent 真正领取 Todo 时常已找不到当时的上下文。

本需求让 Todo 可以携带图片或任意普通文件，并覆盖两个管理时机：

1. **创建 Todo 时**同时添加附件；
2. **Todo 已创建后**继续添加或移除附件。

附件不是只供 Todo 窗口展示。Todo 经 whats-next、自动执行、Popup、Stop 卡或“新建 Agent 任务”
开始执行时，仍有效的附件必须与任务文字一起送达 Agent；已失效附件必须明确告知，不能静默丢失。

## 2. 已确认决策

| 编号 | 决策项 | 结论 |
|---|---|---|
| A1 | Todo 基本语义 | Todo **仍必须有非空文字**；附件只是补充上下文，不支持纯附件 Todo |
| A2 | 小文件所有权 | 单个附件大小 **≤ 10 MiB**（`10 * 1024 * 1024` 字节）时，复制到 AskHuman 托管目录 |
| A3 | 大文件所有权 | 单个附件大小 **> 10 MiB** 时，不复制原文件，只保存规范化后的绝对路径引用 |
| A4 | 数量上限 | 每条 Todo 最多 **20 个**附件；同一规范化源路径在同一 Todo 中重复添加时忽略 |
| A5 | 管理入口 | 首版覆盖 GUI 待办窗口、CLI 与 MCP；IM `/todo` 创建 / 管理卡首版仍只支持文字 |
| A6 | 修改范围 | GUI、CLI、MCP 都同时支持“创建时添加”和“给已有 Todo 增删附件” |
| A7 | 执行出口 | 所有 Todo 执行出口都送达附件：whats-next 手动 / 自动、普通 Popup Todo 区、Stop 卡、GUI / IM 新建 Agent 任务 |
| A8 | GUI 形态 | Pending Todo 默认只显示紧凑附件摘要（最多 3 张图片缩略图）；展开后显示完整列表并可直接移除，附件管理不进入文字编辑态；新增输入区与每条 Pending Todo 都可拖入文件 |
| A9 | 历史 | Todo 进入执行历史时保留附件；恢复后附件继续可用、可编辑 |
| A10 | 清理 | 托管原文件随 Todo / 执行历史生命周期清理；外部引用绝不删除用户源文件 |
| A11 | 删除竞态 | 附件从 Todo 移除、Todo 被删除或清空后**立即失效**；旧卡不为附件保留 24 小时快照 |
| A12 | 已派发文件 | 成功派发前为托管附件建立请求级交付副本；交付副本沿用现有 `temp/askhuman` 24 小时清理策略 |
| A13 | 失效引用 | 外部引用失效不阻塞 Todo 执行：继续派发文字和其它有效附件，并明确列出缺失名称与原路径 |
| A14 | MCP 定位 | 新增只读 `todo_list` 暴露稳定 Todo / 附件 ID；`todo_update` 按稳定 ID 修改，不能只靠易漂移的队列编号 |
| A15 | 图片缩略图 | Todo 图片加入时 best-effort 生成最长边 **128 px** 的托管 PNG 缓存；列表只读小图，预览 / 打开始终使用原文件 |
| A16 | 大图与不支持格式 | >10 MiB 图片仍只引用原图、只托管小缩略图；无法安全解码 / 不支持的格式不阻断附件，回退普通图片文件胶囊 |
| A17 | 选项提示 | whats-next / Stop 的 Todo 选项显示 `【N 个附件】`，支持富文本颜色的入口使用灰色，但不把本地附件主动上传到 IM |
| A18 | GUI 行选择与粘贴 | Pending Todo 可点击选中、上下键导航；选中行常显操作按钮。新增输入框与选中行都接收剪贴板图片粘贴。剪贴板图片没有稳定源路径，只接受 ≤10 MiB 并按托管附件保存 |

## 3. 数据模型与持久化

### 3.1 附件模型

新增 Todo 专用附件模型；不直接复用只有展示元数据的 `models::FileAttachment`：

```rust
#[serde(rename_all = "camelCase")]
struct TodoAttachment {
    id: String,                 // stable UUID
    name: String,               // original display name
    size: u64,                  // source size when attached
    is_image: bool,             // reuse the existing extension classifier
    source_path: String,        // canonical source path; dedupe / diagnostics
    path: String,               // effective managed or referenced absolute path
    storage: TodoAttachmentStorage, // managed | reference
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thumbnail_path: Option<String>, // managed 128 px PNG cache
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_modified_ms: Option<u64>, // reference thumbnail freshness
}
```

- `id` 是附件业务身份。重名文件可共存；删除不得用显示名或路径猜测。
- `source_path` 用于同一 Todo 内去重、CLI / GUI 诊断及引用变化检查；Agent 收到的是 `path`。
- `managed` 的 `path` 指向 AskHuman 托管原文件；`reference` 的 `path` 与 `source_path` 相同。
- `thumbnail_path` 始终是 AskHuman 自有缓存，即使原文件为外部引用；它只用于 UI，不送给 Agent。
- `source_modified_ms` 和 `size` 用于判断外部引用缩略图是否过期。重建失败时继续使用文件胶囊，
  不把过期缩略图冒充当前内容。

`TodoEntry` 与 `DoneTodoEntry` 都增加：

```rust
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub attachments: Vec<TodoAttachment>
```

旧 `todos.json` 缺字段时得到空数组，无需一次性迁移或重写。

### 3.2 目录布局与权限

```text
~/.askhuman/state/
  todos.json
  todos.lock
  todo-attachments/
    <todo-id>/
      files/
        <attachment-id>-<sanitized-original-name>
      thumbnails/
        <attachment-id>.png
      .stage-<uuid>/             # 未提交暂存；成功后消失
```

- 目录名只使用已校验 UUID；用户文件名仅作为经过净化的尾部展示信息，不能参与目录跳转。
- Unix 下附件根与 Todo 目录使用 `0700`，文件使用 `0600`；Windows 继承当前用户目录 ACL。
- 托管路径必须位于当前 `ASKHUMAN_HOME` 的附件根内；清理前再次 canonical / prefix 校验，绝不按
  未验证 JSON 路径递归删除。
- 外部引用可以位于任意本地绝对路径；AskHuman 只读元数据 / 内容并把路径交给 Agent，不修改或删除。

### 3.3 文件判定与复制

1. 展开 `~`，相对路径按调用入口 cwd 解析，canonicalize，且必须是可读普通文件；目录拒绝。
2. 在同一请求内先去重，再与现有附件 `source_path` 去重。
3. 加入后总数超过 20 时整次操作失败，不能只添加前 20 个。
4. 打开文件并以实际读取流执行 10 MiB 边界：
   - 读到 EOF 且总量 ≤10 MiB → 写入暂存文件，随后成为 `managed`；
   - 读到第 `10 MiB + 1` 字节 → 丢弃暂存副本，成为 `reference`；
   - 避免只信 attach 前 metadata 后执行无界 `copy`，防止源文件在竞态中增长造成意外大复制。
5. 一个创建 / 更新请求中任一文件校验或持久化失败，全部附件修改回滚；缩略图生成是唯一的
   best-effort 旁路，失败不回滚有效附件。

### 3.4 缩略图

- 对 `is_image=true` 的 Todo 附件，在阻塞线程中 best-effort 解码，保持宽高比缩到最长边 128 px，
  输出 PNG；不得放大原本更小的图片。
- 解码器设置像素 / 内存上限，先读尺寸再分配，避免压缩炸弹。首版只保证当前 `image` 依赖可安全
  解码的 PNG / JPEG / WebP；GIF / BMP / SVG / HEIC / TIFF 等未覆盖格式允许回退文件胶囊。
- GUI 列表只读取 `thumbnail_path`，不得为了 28–40 px 的行内图标把原文件整体 base64 化。
- 预览、打开、右键菜单、拖出始终使用 `path`，不使用缩略图。
- 外部引用的 size / mtime 改变时，旧缓存标为过期并 best-effort 重建；源文件不存在时删除 / 忽略
  旧缩略图展示并标记附件不可用。
- 本需求不顺带改变普通回答图片或 `Ask -f` 图片现有的整图读取行为；公用缩略图治理另行设计。

### 3.5 原子性与跨进程并发

`todos.json` 仍是单一业务真相源，写入仍持有 `todos.lock`。文件系统和 JSON 无法形成真正的跨资源事务，
因此使用以下提交顺序：

**新增 / 更新附件**：

1. 锁外解析源路径、暂存托管副本与缩略图；
2. 获取 `todos.lock`，重新读取最新 JSON；
3. 校验目标 Todo 仍存在、附件 ID / 数量前置条件仍成立；并发变化时丢弃暂存结果并返回冲突，
   不覆盖对方修改；
4. 把暂存文件原子 rename 到目标 Todo 目录；
5. 原子写 `todos.json`；写失败则删除本次新文件并保持旧 JSON；
6. JSON 成功后清理本次被移除且不再引用的托管原文件 / 缩略图。

**删除 / 清空 / 历史裁剪**：

1. 持锁确定将失去引用的附件 ID；
2. 先原子提交新的 `todos.json`；
3. 再 best-effort 删除严格位于托管根内的对应文件 / 空目录；外部引用只删 JSON 元数据。

进程崩溃可能留下没有 JSON 引用的 `.stage-*` 或附件目录。每次附件写入与 GUI Host / daemon 启动时
可做有界孤儿 GC：只处理 UUID 目录和超过安全年龄的暂存项，绝不扫描或删除托管根之外的路径。

## 4. 生命周期

| 动作 | Todo JSON | 托管原文件 / 缩略图 | 外部源文件 |
|---|---|---|---|
| 创建 / 添加附件 | 写 pending 条目 | 创建 | 只引用 |
| 编辑移除附件 | 移除附件元数据 | JSON 成功后立即删除 | 不删除 |
| 手动删除 Todo / 清空待办 | 删除 pending 条目 | JSON 成功后立即删除 | 不删除 |
| 执行 `take` 且保留历史 | pending → history | 保留，由 history 继续拥有 | 继续引用 |
| 执行 `take` 且历史上限为 0 | 删除 pending 条目 | 交付副本成功后立即删除 | 不删除 |
| 历史上限淘汰 / 清空历史 | 删除 history 条目 | JSON 成功后立即删除 | 不删除 |
| 恢复历史 | history → pending，同一 id | 原目录继续使用 | 继续引用 |

执行前产生的请求级交付副本不属于 Todo / 历史存储，位于系统 `temp/askhuman/<request-id>/todo-files/`；
它由现有 daemon 启动 + 每小时的 24 小时临时目录清理回收。

## 5. 创建与管理入口

### 5.1 GUI 待办窗口

#### 创建

- 新增输入区增加回形针按钮；使用跨平台原生文件对话框，可多选普通文件，不选目录。
- 整个新增输入区是系统文件拖入目标；拖入经过输入区时显示明确的虚线边框、上传图标和目标文案。
  路径来自 Tauri `onDragDropEvent`，不依赖 DOM `File.path`。
- 未提交附件以紧凑草稿项显示文件名，每项可移除；最终缩略图只在后端建立 Todo 附件后读取。
- 文字为空、附件处理失败、总数超限或正在提交时不能创建。
- 点击“添加”后文字、auto 标记和附件一次性写入；成功才清空草稿。

#### 已有 Todo

- 每条 Pending Todo 整行都是文件拖入目标；拖入经过目标行时显示虚线边框、上传图标和“添加到此
  待办”的明确反馈，松手后直接保存附件。
- 默认只显示一个紧凑摘要按钮：附件数量 + 最多 3 张图片缩略图 + 剩余数量，不让多附件持续撑高列表。
  缩略图之间保留小间距、不互相堆叠；点击摘要才在行内展开完整列表。
- 完整列表显示图片缩略图或原生文件图标、文件名、大小和缺失状态；不显示 `managed/reference`
  等存储实现细节。
- 展开列表可移除附件，行 hover 的回形针可直接多选添加；两者都不进入文字编辑态，也不需要再点
  “保存”。已保存附件不可撤销，故删除按钮第一次点击改为“确认删除”，第二次才调用独立附件命令；
  点击其它位置取消确认态。
- 文字“编辑”只编辑文字；附件变化和文字草稿的 UI / 操作入口解耦。文字保存仍以原始文字和最新
  附件 ID 做并发校验，避免覆盖外部变化。
- 新增 Todo 输入框收到剪贴板图片时，把图片加入尚未创建的附件草稿；普通文字粘贴
  保持 textarea 原行为。创建时后端把文件路径与剪贴板图片放在同一附件事务中，失败保留前端草稿，
  成功后才清空；两类附件合计最多 20 个。草稿展示必须复用 Popup 输入框的附件组件：图片显示带右上角
  删除按钮的缩略图，普通文件显示可删除 chip；选择或拖入的本地图片也读取缩略图，不能另做纯文字列表。
- 点击 Pending Todo 的正文 / 空白区可选中该行；`↑` / `↓` 在 Pending 行间移动选择。选中行用稳定
  高亮边框表示，并让删除、auto、复制、编辑、添加附件和新建任务等 hover 操作持续可见。
- 当某行选中时，粘贴事件只提取剪贴板中的图片项并直接添加到该 Todo；普通文字粘贴不拦截。粘贴
  图片经 data URL 送到受控 Rust 命令，先写请求级临时文件，再按普通附件事务托管，成功 / 失败后都
  清理临时文件。由于剪贴板 blob 没有可持续引用的源路径，解码后 >10 MiB 时明确拒绝。
- 单击选中、双击 / Enter 打开、空格 Quick Look、右键菜单与拖出行为复用现有附件交互。
- Quick Look 内用方向键切换文件时，窗口级 `preview-index` 监听把返回索引映射到本次预览的可用附件
  ID，并同步来源 Todo 列表的蓝色选中框；不能让高亮停留在启动预览的旧附件上，也不能为每个 Todo
  行重复注册原生事件监听。
- Todo 排序改用 Pointer Events 手柄；窗口保留 Tauri 原生 drag-drop handler，使排序与系统文件拖入
  不再互斥。
- 同时只能编辑一行文字；切换项目、完成 / 删除该 Todo 或收到使目标消失的实时更新时取消文字草稿。

#### 执行历史

- 历史行只读展示附件与缺失状态，仍可打开 / Quick Look。
- 历史行复用折叠摘要与完整列表，但不显示移除按钮；点击恢复后附件随同 Todo 回到 pending，重新
  允许直接管理。

文件选择使用 [Tauri 2 官方 dialog plugin](https://v2.tauri.app/zh-cn/plugin/dialog/)；
`multiple=true`、`directory=false`。插件只负责选择路径，
所有信任边界、复制与持久化仍在 Rust 命令中完成。

### 5.2 CLI

```bash
AskHuman todo add [--auto] [-f <path>]... [--] <text>
AskHuman todo list
AskHuman todo attach <todo-number> -f <path>...
AskHuman todo detach <todo-number> <attachment-number>...
```

- `-f` / `--file` 可重复；`--` 后全部视为 Todo 文字，支持以 `-` 开头的文本。
- `attach` 至少一个文件；`detach` 至少一个附件编号，重复编号去重。
- `list` 在每条 Todo 下缩进列附件编号、名称、格式化大小、托管 / 引用及缺失状态。
- 人类界面继续使用 1 基编号；解析 Todo / 附件编号与修改必须在同一 `todos.lock` 临界区完成，
  不能先无锁 `list` 再按可能漂移的编号修改。
- 路径解析、数量上限、整批原子失败和本地化错误与 GUI 共用 Rust 核心。

### 5.3 MCP

`todo_add` 增加：

```json
{
  "text": "…",
  "auto": false,
  "files": ["relative/or/absolute/path"]
}
```

成功结果除 1 基位置和文本外返回稳定 Todo ID 与附件摘要。

新增只读工具 `todo_list`：无公开 project 参数，仍按 MCP server cwd 的 git 根归项目；返回待办稳定 ID、
文字、auto、来源和附件稳定 ID / 名称 / 大小 / storage / available，供模型先定位再更新。

新增副作用工具 `todo_update`：

```json
{
  "todo_id": "<uuid>",
  "add_files": ["path"],
  "remove_attachment_ids": ["<uuid>"]
}
```

- 至少有一个实际添加 / 移除操作；空操作报 invalid params。
- 删除 ID 不属于该 Todo 时返回明确错误，不按名称猜测；并发下目标 Todo 已不存在同样报错。
- 相对路径按 MCP server cwd 解析。
- `todo_update` 与 `todo_add` 使用同一 Codex thread guard；ambient suggestion 等后台线程不能借新工具
  修改用户 Todo。`todo_list` 是只读工具，不产生该副作用。
- 托管 Rules / Agent help 更新为：用户明确要求创建或修改 Todo 附件时才调用对应工具 / CLI，
  不得把 Agent 自己的工作文件擅自附入 Todo。

### 5.4 IM 创建入口（非目标）

飞书、钉钉、Telegram、Slack 的 `/todo` 管理卡首版不接收附件上传，也不新增附件编辑卡。它们继续
创建纯文字 Todo。但这些渠道上的 whats-next / Stop / `/new` 若执行了由 GUI、CLI 或 MCP 创建的
带附件 Todo，仍按 §6 送达附件。

## 6. 执行与附件送达

### 6.1 统一准备函数

新增统一的 delivery 层，例如：

```rust
prepare_todo_delivery(
    project: &str,
    todo_id: &str,
    expected_attachments: &[TodoAttachmentSnapshot],
    request_id: &str,
) -> TodoDelivery
```

`TodoAttachmentSnapshot` 至少包含附件稳定 ID、名称与卡片打开时路径信息；`TodoDelivery` 返回：

- `files`：已准备、Agent 可读的绝对路径；
- `warnings`：已从 Todo 移除、Todo 已删除、源文件不存在、复制失败等明确诊断；
- 出队所需的稳定 Todo ID / 状态。

竞态规则：

- 卡片 / 新建任务表单冻结当时的附件 ID 集合；后续新增附件不自动混入旧卡。
- 派发时把快照 ID 与当前 Todo 的附件 ID 求交集。已移除附件即使外部原文件仍存在也不能送达，并生成
  “附件已从 Todo 移除”的警告；这落实 A11 的立即失效。
- 托管附件先复制到 `request_temp_dir(request_id)/todo-files/`，文件名带附件 ID 防重名；外部引用仅校验
  当前可读性后直接返回原路径。
- 准备交付与 `take` 必须具备线性化顺序：删除先提交则执行看见缺失；执行先成功冻结 / 交付则视为已经
  开始执行。实现可锁外暂存、锁内重验版本 / 附件 ID 后提交，不能长时间持锁复制 20 个文件。
- 交付副本失败不让整条 Todo 卡死：有效文件继续送达，失败项进入 warnings；文字始终执行。

警告进入任务文字中的明确附件状态段，不使用普通 Ask 的取消 / 重放 `[status]` 语义，避免 Agent
误判整次请求已取消。不存在的路径不能放入 `[files]` 冒充有效附件。

### 6.2 whats-next 手动选择

- `OptionItem` 的 Todo 载荷增加附件快照（至少稳定 ID + 诊断元数据），并纳入 daemon ask 去重指纹；
  同文本但附件快照不同的请求不能错误合流。
- Todo 选项展示文本在既有前缀 / auto 规则之外增加 `【N 个附件】`；解析给 Agent 的任务正文时必须同时剥掉
  该展示后缀，不能污染 Todo 原文。IM 只展示数量，不上传本地文件。
- 选中 Todo 后，在 Coordinator 首个终态收敛点、`take` 之前准备交付；有效路径追加到赢家回答的
  `QuestionAnswer.files`，故现有 whats-next 渲染自然输出 `[files]`。
- Todo 文字仍使用卡片快照，保持 `todo-whats-next` D11；附件按本 spec 的交集 / 立即失效规则。
- 回复历史记录实际交付路径与警告，使用户能核对 Agent 当时收到的内容。

### 6.3 whats-next 自动执行

- `first_auto` 取得条目后先准备全部当前附件，再执行原子出队；并发下已被拿走则回落正常提问。
- stdout 使用现有 `[user_input]` + `[files]` 契约；缺失警告拼入任务文字。
- 自动路径的回复历史 `files` 不再恒为空，记录实际交付路径。

### 6.4 普通 Popup 的 Todo 区

- 前端选中 Todo 时同时冻结附件稳定 ID；提交时继续把 Todo 文本并入最后一题 `user_input`，并把快照
  传回后端。
- 后端按当前 Todo 交集准备交付，把有效路径并入最后一题 `files`、警告并入同题文字，再由
  Coordinator 出队。
- select-only 下仍只能删除、不能点选 Todo，附件不改变该规则。

### 6.5 Stop 卡

- Stop 卡 Todo 选项携带附件快照；解析选中项后准备交付。
- 带附件选项显示 `【N 个附件】`，Popup / 飞书 / 钉钉将计数渲染为灰色，Slack / Telegram 使用无色文本；四个 IM adapter 只渲染计数，不上传附件。
- continuation 保持现有各 Agent 包裹规则，在 Todo 原文 / 用户补充后追加统一附件路径段与缺失警告。
- 有效路径同时写入结构化内部结果，便于测试与日志；Stop 卡仍不写普通回复历史、不自动执行 auto Todo。

### 6.6 GUI / IM 新建 Agent 任务

- GUI `selectedTodo` 与 IM `TaskInputSourcePayload` 增加附件快照；表单中只读展示所选 Todo 附件。
- 点击启动时后端按当前 Todo 求交集并准备请求级交付副本；若 Todo / 附件已删除，任务文字快照仍可启动，
  但追加明确警告。
- 启动准备与手动删除按取得 Todo 锁的顺序线性化：删除先提交则该附件失效；启动先冻结交付成功则视为
  已经发起，之后的删除不撤回这次 launch。Terminal 打开失败仍清交付副本并保留尚未被用户删除的 Todo。
- `LaunchRecord` 增加交付文件列表及其完整性摘要；一次性 claim 校验必须覆盖文件列表，防止 record 被篡改。
- helper 构造实际初始 prompt 时追加统一 `Attachments` 路径段。用户任务正文现有 3000 字限制不把内部
  路径段计入；仍检查 NUL 与系统 argv 安全边界。
- 首版不依赖单家 Agent 的原生图片 flag：四家都收到可读绝对路径和明确提示。可在不改变 Todo 契约的
  前提下，后续 adapter 对支持原生图片输入的 Agent 做增强。
- Terminal 成功打开后才 `take`；失败保留 Todo 与托管附件，清理本次未使用的交付暂存。

## 7. 错误与安全边界

- 所有公开创建 / 更新入口都拒绝空文字（创建）、空路径、目录、不可读文件、包含 NUL / CR / LF 的路径、
  超过 20 个附件；换行路径无法进入“一行一个路径”的 `[files]` 契约。
- 文件路径、名称与错误不得输出文件内容；MCP 返回路径是用户明确传入或 `todo_list` 明确请求的本地状态。
- 缩略图解码在阻塞线程并受尺寸 / 内存限制；失败降级，不 panic、不阻塞 Todo。
- UI 的打开 / Quick Look 命令只接收后端返回的有效附件 path；不可用状态不触发打开。
- 托管文件清理不跟随 symlink，不接受宽泛目录目标；任何前缀校验失败都宁可留孤儿，不误删用户文件。
- delivery 写入失败不能让 Todo 被当成完整附件已送达；结果必须包含 warning，且仍允许文字执行。
- 跨进程更新冲突不做 last-write-wins 覆盖；GUI 保留草稿并提示刷新后重试。

## 8. 非目标

- 不支持纯附件 Todo。
- 不在 IM `/todo` 创建 / 管理卡上传或编辑附件。
- 不复制 >10 MiB 原文件，不提供“强制托管大文件”开关。
- 不做附件内容索引、全文搜索、云同步、跨设备下载或内容去重。
- 不让多个 Todo 共享同一托管副本；每条 Todo 独立拥有自己的附件生命周期。
- 不顺带改变普通 Ask 回答图片、`-f` 展示图片的内存 / 缩略图实现。
- 不保证所有图片格式都能生成缩略图；原文件仍作为普通附件可打开 / 预览 / 送达。
- 不为旧卡保留已删除附件的快照副本。

## 9. 验收标准

1. GUI 创建 Todo 时可多选、拖入或在输入框粘贴图片和文件；新增区与 Popup 输入框复用同一个附件组件：
   图片显示可删除缩略图，普通文件显示可删除 chip；新增区有明确落点反馈，保存后重开窗口仍显示，
   文字为空不能创建。
2. 每条已有 Pending Todo 都可直接拖入 / 多选添加附件；默认摘要最多显示 3 张图片缩略图，展开后
   删除附件必须两次点击确认，不进入文字编辑态、不显示托管 / 引用技术细节；文字编辑只处理文字。
3. Pending Todo 可点击选中并用上下键导航；选中行的 hover 操作常显。选中后粘贴一个或多个图片会
   直接形成托管附件，普通文本粘贴不被截获，超过 10 MiB 的剪贴板图片不留下失效临时引用。
4. 单文件恰好 10 MiB 被托管，10 MiB + 1 字节只引用；每条第 21 个附件被整批拒绝。
5. 托管原文件位于 Todo 私有目录；>10 MiB 外部文件不被复制或删除。
6. 可解码图片显示 128 px 缓存缩略图；打开 / Quick Look 使用原文件；不支持 / 损坏图片回退胶囊。
7. CLI `add -f`、`attach`、`detach` 与带附件的 `list` 工作；编号解析在并发重排下不修改错条目。
8. MCP `todo_add(files)`、`todo_list`、`todo_update` 按稳定 ID 工作，副作用 guard 生效。
9. Todo 经 whats-next 手动 / 自动、普通 Popup、Stop 卡、GUI / IM 新建任务执行时，有效附件全部送达；
   不存在的附件不出现在 `[files]`，任务中有明确警告。
10. 旧卡打开后附件被移除 / Todo 被删除：文字仍按既有快照语义执行，已删除附件立即失效并警告；
   卡打开后新加的附件不混入旧卡。
11. 执行前为托管附件生成 24 小时请求级交付副本；随后删除 Todo、清空历史或 history limit 为 0，
    已派发路径仍可被 Agent 读取。
12. 执行进入历史后原附件保留；恢复后可用；清空 / 淘汰历史后托管原文件与缩略图清理，外部源文件不动。
13. 旧 `todos.json` 无附件字段仍正常读取；持久化失败、复制失败和跨进程冲突不产生半条目或误报成功。
14. GUI 中英文、键盘打开 / Quick Look、右键、拖出、实时 `todos-updated`、auto 标记、排序、完成与
    新建任务既有行为无回归。
15. whats-next / Stop 的带附件 Todo 选项显示 `【N 个附件】`（富文本入口为灰色），选中后送给 Agent 的任务正文不包含该展示后缀；
    IM 不因展示计数而上传本地文件。

## 10. 反馈记录

- **2026-07-31**：初版定案。小文件（≤10 MiB）托管、大文件引用；每条最多 20 个；GUI + CLI / MCP
  同时覆盖创建与已有 Todo 修改；所有执行出口送达；行内管理；历史持有并随生命周期清理；删除附件
  立即失效；成功派发使用现有 24 小时临时交付副本；失效引用继续执行并明确警告；MCP 用稳定 ID +
  `todo_list`；文字仍必填。
- **2026-07-31（缩略图补充）**：Todo 图片统一生成最长边 128 px 的托管缓存缩略图，列表不整图读取；
  预览始终打开托管原文件或外部原文件。现有回答图片 / `-f` 图片链路不纳入本需求。
- **2026-07-31（选项提示补充）**：whats-next / Stop 的 Todo 选项显示 `【N 个附件】`，仅提示数量，不把
  本地附件主动上传到 IM。
- **2026-07-31（附件计数视觉纠正）**：所有待办 / 任务选项移除回形针 emoji，统一使用括号计数；
  Popup、飞书和钉钉使用灰色，Slack 与 Telegram 保留无色文本。
- **2026-07-31（实现完成）**：GUI / CLI / MCP 管理、托管与引用存储、128 px 缩略图、历史生命周期、
  六类执行出口、请求级交付副本、失效 warning、路径安全与删除/派发竞态重验全部接通；真实 Agent / IM
  启动验证因可能产生外部副作用或费用，按授权边界未执行。
- **2026-07-31（GUI 交互重做）**：按实机反馈把附件管理与文字编辑解耦；新增输入区和每条 Pending
  Todo 都支持带命中反馈的系统文件拖入；默认折叠为数量 + 最多 3 张图片缩略图，展开完整列表后可直接
  移除；用户界面不再暴露 managed/reference。为恢复原生文件拖入，排序手柄从 HTML5 DnD 改为
  Pointer Events。
- **2026-07-31（选择与粘贴补充）**：摘要缩略图改为保留细小间距；Pending Todo 增加点击选择与
  上下键导航，选中时常显行内操作；剪贴板图片可直接附入选中 Todo，并以 ≤10 MiB 托管限制避免把
  临时粘贴文件保存成失效引用。
- **2026-07-31（新增框粘贴补充）**：新增 Todo 输入框同样接收剪贴板图片，先保存为前端附件草稿；
  `todos_add` 在一次创建事务中处理文件路径与粘贴图片，完成后清理请求级临时文件。
- **2026-07-31（附件交互统一）**：新增 Todo 与 Popup 输入框抽取并复用同一附件组件，图片统一为带 ×
  的缩略图、普通文件统一为 chip；已保存 Todo 附件的删除改为两次点击确认。
- **2026-07-31（Quick Look 高亮同步）**：Todo 窗口增加单例 `preview-index` / `preview-closed` 监听，
  原生预览切换文件时同步对应附件行的蓝色选中框。
- **2026-07-31（Agent 提示词边界纠正）**：附件管理主要面向人，不在 CLI / MCP 全局 Agent reference
  或 MCP server instructions 中新增附件操作引导；显式 CLI / MCP 能力只由对应 help / tool schema 描述。
