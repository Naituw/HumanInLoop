# IM `/diff` · `/stage` · `/transcript` 命令

> 在 IM 渠道为已追踪 agent 增加三个命令：查看未暂存 diff、确认后 stage 改动、导出完整会话 transcript。
> 无参时复用通用单选卡选 agent；有编号时直达。`/diff` 与 `/transcript` 产出附件（飞书 diff 为 `.diff`、transcript 为 `.md` / Telegram `.html` / Slack·钉钉 `.docx`）；`/stage` 经轻量确认卡二次确认后执行 `git add -A`（四渠道，钉钉专用双按钮模板）。
>
> 关联计划：`docs/plans/im-diff-stage-transcript.md`  
> 依赖：agent 生命周期追踪（`docs/specs/agent-lifecycle-tracking.md`）、通用单选卡（`docs/specs/im-select-card.md`）、四渠道文件发送能力。

## 1. 背景与目标

用户远程通过 IM 跟进多个 agent 时，常需要：

1. 看某个 agent 工作区里**尚未 stage 的本地改动**（便于审查进度）；
2. 把这些改动**一键 stage**（仍不 commit）；
3. 导出该 agent **完整会话记录**的可读渲染（含折叠思考、工具概览、用户输入、AskHuman 专项块）。

本需求把三者做成与 `/status` / `/watch` / `/msg` 同级的 slash 命令，挂在 daemon 入站分派上。

## 2. 决策记录（用户经 AskHuman 定案）

| 编号 | 决策项 | 结论 |
|---|---|---|
| D1 | 命令面 | `/diff [编号]`、`/stage [编号]`、`/transcript [编号]`；**仅英文**命令名（无中文别名）；Slack 仍可用 `!` 备用前缀 |
| D2 | 无参选 agent | 弹**通用单选卡**；选项范围 = **工作中 + 空闲**（同 `/status` 的 `agent_options`，**不是** `/watch` 的仅工作中） |
| D3 | 带编号 | 直达执行（不弹 agent 选择卡）；编号寻址复用 `/status` 稳定 `seq` |
| D4 | 已结束 agent | 编号存在且有 `cwd`/`session` → **仍允许**三命令；不存在 → 「未找到」文本（同 `/status`） |
| D5 | 渠道范围 | 四渠道都要：**飞书 / Telegram / Slack / 钉钉**。stage 确认卡四渠道均支持（飞书/TG/Slack 自由卡；钉钉专用模板 `dingtalk-confirm-card-template.json`） |
| D6 | `/diff` 内容 | **仅 unstaged**（`git diff`，不含 `git diff --staged`）；**含 untracked**（按新文件全文展示，类似 intent-to-add） |
| D7 | git 根 | 从 agent `cwd` **向上找 `.git` 根**，在仓库根执行 git（与 `project` 识别一致） |
| D8 | 空态 | 非 git 仓库 / 无未暂存改动 → **纯文本**提示（不发空附件） |
| D9 | 体积上限 | HTML 约 **2MB** 或 diff 累计约 **5000 行** 截断，页头/页脚说明；二进制/过大单文件标「binary / skipped」不贴内容 |
| D10 | `/stage` 范围 | `git add -A`（修改/删除 + untracked，与 `/diff` 展示范围对齐）；**不 commit** |
| D11 | `/stage` 确认 | **一律**先发确认卡（有编号也要）：列将 stage 的路径（最多前 30 +「另有 N 个」）+ **[确认暂存] [取消]**；确认后执行；取消定格 |
| D12 | Confirm 卡 | **新建**轻量 Confirm 抽象（**不**复用提问卡 / 单选卡）；不持久化；daemon 重启后旧卡静默无效；TTL ~30min（同 select） |
| D13 | 附件格式 | **Telegram：`.html`**（深色 + 显式前景/背景）；**飞书：diff 用 `.diff` unified-style 纯文本、transcript 用 `.md`**；**钉钉/Slack：`.docx`**（Slack 对 html/md 附件当源码，docx 可原生预览） |
| D14 | 发送形态 | 直接发送附件，不另发摘要文本消息 |
| D15 | 文件名 | `diff-{seq}-{project}.diff|.html|.docx`、`transcript-{seq}-{title-slug}.md|.html|.docx` |
| D16 | HTML 高亮 | 自包含轻量高亮（内嵌压缩脚本或按扩展名正则上色）+ **红绿行背景**；无外网依赖 |
| D17 | `/transcript` 形态 | **聊天式可滚动文档**（非截图）；尽量完整会话；超限从**最早**截断并页头说明 |
| D18 | 思考块 | 能识别则 **默认折叠**（HTML `<details>`；docx 用「思考」小节/缩写） |
| D19 | 工具块 | 概览：工具名 + 关键参数摘要；**默认折叠**详情（入参/结果摘要） |
| D20 | AskHuman 专项 | 识别 Bash/Shell 中的 AskHuman CLI 与 MCP `ask`，渲染独立「向人类提问」块（问题摘要 + 可解析的人类答复） |
| D21 | agent 格式 | 五家（Claude / Cursor / Codex / Grok / Pi）**best-effort**：统一中间事件模型；Pi 解析 v3 message/toolResult/compaction/branch_summary，自定义 sessionDir 使用 daemon 已验证路径；不识别的段落降级展示或跳过 |
| D22 | 门控 | 与 `/status` 相同：依赖 daemon + 生命周期追踪；**不另设**实验开关；无 agent 时文本提示 |
| D23 | `/help` | 动态 help 增加三条命令说明（含 `{p}` 前缀） |

## 3. 命令行为

### 3.1 解析（`autochannel::classify`）

| 输入 | 结果 |
|---|---|
| `/diff` · `/diff 3` | `Command::Diff(None \| Some(3))` |
| `/stage` · `/stage 3` | `Command::Stage(None \| Some(3))` |
| `/transcript` · `/transcript 3` | `Command::Transcript(None \| Some(3))` |
| 非数字第二 token | 视为无参（同 `/status` 宽松） |
| `!diff` 等 | 四渠道通用备用前缀（Slack 必需） |

### 3.2 无参 → 单选卡

- 选项：`select::agent_options`（工作中在前 + 空闲；已结束不列）。
- 动作：扩展 `SelectAction` / `PickerKind`：`Diff` / `Stage` / `Transcript`。
- 点选后：
  - **Diff / Transcript**：单选卡可定格或保持（推荐定格「已生成…」或不动 + 另发摘要与附件，实现取定格以免重复点刷屏）；执行对应导出。
  - **Stage**：**不立刻** `git add`；进入「对该 session 发 Confirm 卡」流程。
- 无可选 agent → 既有空态文案（需开启生命周期追踪）。

### 3.3 有编号 → 直达

1. 用 `seq` 在快照定位记录；找不到 → 未找到提示。
2. Diff / Transcript：直接导出。
3. Stage：直接发 Confirm 卡（仍不立刻 add）。

### 3.4 `/diff` 流水线

```
resolve agent → cwd → git root
  → 无 git 根：文本「非 git 仓库」
  → git status / diff 收集 unstaged + untracked
  → 空：文本「无未暂存改动」
  → 构建结构化 DiffModel → 按渠道渲染 diff / HTML / docx
  → 截断门控 → 写临时文件 → 上传发送 → 清临时文件
```

**Diff 语义细节**：

- 已跟踪未 stage：`git diff`（working tree vs index），按文件分节。
- Untracked：每个文件以「新文件」全文形式展示（二进制/过大则跳过内容）。
- **不包含**已 stage 未 commit 的改动。
- 二进制：标记 `binary file changed` / `binary file`，不贴 blob。
- 单文件过大：跳过内容并标注。
- 总行数 / HTML 体积超限：从**靠后文件或文件尾**裁切策略在实现中固定一种并在页头说明（计划写明：优先保证「文件列表完整 + 前面文件全文」，后面文件截断）。

### 3.5 `/stage` 流水线

```
resolve agent → cwd → git root
  → 预览将 stage 的路径列表（与 diff 同源：unstaged + untracked）
  → 空：文本「无未暂存改动」
  → 发 Confirm 卡（标题、仓库短名、路径列表≤30、确认/取消）
  → 确认：git add -A → 文本结果（文件数 + 路径摘要）
  → 取消：定格卡「已取消」
```

- Confirm **不持久化**；payload 至少含：`channel`、`message_id`、`session_id`、`git_root`（或 cwd）、创建时路径快照哈希可选（确认时若工作树剧变可提示「文件列表已变化，请重试」——**建议做**：确认前重新列举，与发卡时集合不一致则拒绝并提示重新 `/stage`）。
- 钉钉 Confirm：**P2**（新模板）；P1 钉钉 `/stage` 可降级为「纯文本列表 + 提示暂不支持卡片确认 / 或暂回报『钉钉确认卡开发中』」——计划中写死降级策略（见计划）。

### 3.6 `/transcript` 流水线

```
resolve agent → kind + session_id → transcript_path
  → 无文件：文本「找不到会话记录」
  → 流式/分块解析 jsonl → 归一为 TranscriptEvent 序列
  → 渲染 HTML/docx → 截断门控 → 摘要 + 附件
```

## 4. 渲染模型

### 4.1 DiffModel（传输无关）

- 元信息：agent seq/kind/title、git root、生成时间、截断标志。
- `files[]`：`path`、`kind`（modified / added / deleted / untracked / binary）、`hunks` 或 `full_text` 行列表（每行 `Equal|Insert|Delete` + 文本）。

**HTML**：深浅色友好 CSS；文件锚点目录；每文件一块；行号可选；插入行绿底、删除行红底；按扩展名 token 高亮（尽力）。

**docx（钉钉）**：Markdown 风格结构或直接 OOXML：H1 标题、每文件 H2、代码等宽段落，插入/删除用颜色或 `+`/`-` 前缀；无语法高亮。

### 4.2 TranscriptModel（传输无关）

归一事件（示意）：

| 类型 | 展示 |
|---|---|
| `UserText` | 用户气泡 |
| `AssistantText` | 助手气泡（Markdown→HTML） |
| `Thinking` | 默认折叠 |
| `ToolCall` | 概览条 + 折叠详情（name、args 摘要、result 摘要、error） |
| `AskHuman` | 独立块：问题（message/questions）+ 人类答复（若 tool_result/stdout 可解析） |
| `System/Meta` | 弱化或跳过（environment_context 等注入块） |
| `Unknown` | 可选折叠「原始事件」或静默跳过 |

**解析策略（best-effort）**：

- 复用并扩展 `agents/activity.rs` / `title.rs` 的路径与家族分支，但改为**整文件有界读取**（非仅尾部 256KB；上限如 2MB，优先尾部窗口保证「最近完整」时从文件末向前取）。
- Claude / Cursor：message content 数组（text / tool_use / tool_result / thinking 若有）。
- Codex：`event_msg / user_message` 是现代 rollout 的权威用户消息；紧邻的
  `response_item / message(role=user)` 模型输入副本去重，独立 `<skill>` / Hook 等上下文跳过，
  仅为旧格式保留非上下文 response_item 回退；reasoning 作 Thinking。
- Grok：assistant / user / tool_result；reasoning 字段作 Thinking。
- **CLI vs MCP**：同一 session 通常同一 jsonl；AskHuman 专项同时匹配：
  - 命令行含 `AskHuman` / `askhuman`；
  - 工具名 `ask` 且像 MCP AskHuman；
  - 从 arguments/command 抽问题文本；从 tool_result 抽 `[user_input]` / JSON `user_input` / `selected_options` 等。

无法完整解析时：页头注明「部分事件未能解析」；能解析多少展示多少。

## 5. Confirm 卡抽象

与提问卡、单选卡并列的第三类轻量交互：

```
ConfirmView {
  title,
  body_markdown,   // 文件列表等
  confirm_label,
  cancel_label,
  // 回调只带短 token：confirm|cancel + 由 daemon 台账还原上下文
}
```

- **飞书**：卡片 JSON 2.0，markdown + 两 button（callback `confirm:ok` / `confirm:cancel`）。
- **Telegram**：HTML + inline keyboard。
- **Slack**：section + actions。
- **钉钉（P2）**：专用模板 + 变量（正文 markdown、两按钮、finalized 定格）。

台账：`ConfirmEntry { channel, message_id, kind: Stage, session_id, git_root, paths_fingerprint, created_at }`，TTL 30min，不落盘。

## 6. 渠道交付

| 渠道 | diff / transcript | stage 确认 |
|---|---|---|
| 飞书 | 直接 `upload_file`/`send_file`；diff 为 `.diff`，transcript 为 `.md` | Confirm 卡（P1） |
| Telegram | 直接 `send_document`（.html） | Confirm 卡（P1） |
| Slack | 直接 `upload_file`（.docx） | Confirm 卡（P1） |
| 钉钉 | 直接上传发送 docx | Confirm 模板 **P2**；P1 文本降级 |

临时文件：写在 `paths::temp` 下，发送后删除（失败 best-effort 清理）。

## 7. 非目标

- 不 commit、不 push、不 `git stash`。
- 不做交互式逐文件 stage / partial hunk stage。
- 不做真·长图截图导出。
- macOS、Linux 与 Windows 共用 lifecycle/daemon 能力；不为其它未支持平台增加单独降级实现。
- 不保证 100% 还原所有 agent 私有事件类型；best-effort。
- 钉钉 HTML 预览、PDF 转换不在本期。

## 8. 风险与降级

| 风险 | 降级 |
|---|---|
| 各家 transcript 格式漂移 | 家族解析独立；失败跳过事件；页头 partial 提示 |
| 超大 diff/会话 | 截断上限；二进制跳过 |
| git 并发（agent 同时写） | 只读 diff / 确认时重检列表；add 失败回 stderr 文本 |
| 钉钉无确认模板（P1） | `/stage` 文本说明暂不可用或仅预览列表不执行（计划二选一写死） |
| 渠道附件大小限制 | 截断后仍过大 → 文本报错建议本地查看 |
| 无 cwd | 文本「无工作目录」 |

## 9. 验收要点

1. `/diff 3` 在有 unstaged 时收到附件：飞书为易读的 unified-style `.diff`，Telegram 为 html，钉钉/Slack 为 docx；staged-only 改动不出现。
2. untracked 文件出现在 diff 中。
3. `/stage 3` 先出确认卡；确认后 `git status` 显示已 stage；取消不改动 index。
4. `/transcript 3` 含用户/助手/折叠思考/工具概览；AskHuman 调用呈独立块（有样本会话时）。
5. 无参三条命令均弹出工作中+空闲 agent 单选卡；点选行为正确。
6. 非 git / 空 diff / 无 transcript 均为清晰文本，不发空文件。
7. `/help` 列出新命令；Slack 展示 `!` 前缀。

## 10. 反馈意见

（评审 / 实现中的修改意见追加到此处，标注日期。）

- **2026-07-10**：飞书打开 `.html` 附件显示为源码，无法当页面预览。
- **2026-07-10（续）**：试发 pure md：飞书可预览 md，但内嵌 HTML 不生效。定案：飞书发 **`.md`**（` ```diff ` 等）；钉钉仍 **docx**；TG/Slack 仍 **HTML**。
- **2026-07-10（着色）**：飞书 ` ```diff ` 着色不稳定（简单行整行红绿，含反引号/`##` 等易变 token 高亮）。曾尝试转义净化，用户定案：**不改写代码内容**（准确优先），接受飞书着色差异。
- **2026-07-10（Slack/TG）**：Slack 打开 HTML/MD 均当源码 → 改 **docx**（与钉钉同）。Telegram HTML 可渲染；深色模式白底浅字 → HTML **深色优先** + 显式 color/background。
- **2026-07-10（transcript 聚焦）**：导出聚焦 agent 行为——保留用户真实输入、助手输出；工具调用 **watch 同款一行**（读取/写入/运行 + 对象），**不展示 result**、**不单独解析 AskHuman**；跳过 system 注入；md 工具行不用列表（避免双圆点）。
- **2026-07-17（飞书 diff）**：飞书 `/diff` 改为直接发送 **`.diff`** 文件；内容接近 `git diff`，以易读为目标，不要求可被 `git apply` 应用。保留现有 untracked 展示和安全截断说明；飞书 transcript 继续使用 `.md`。
