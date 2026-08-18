# 需求：严格选择模式 + 结构化输出（`--select-only` / `--single` / `--output json`）

> 状态：方案设计（待评审）
> 关联计划：`docs/plans/strict-choice-and-structured-output.md`
> 影响面：CLI 入参（`cli/args.rs`、`cli/mod.rs`）、help 体系（`cli/help.rs`：重组 `--help`、新增 `--scripting-help`、`--agent-help` 字段改名、共享片段组装）、数据模型（`models.rs` `AskRequest` + TS 类型）、IPC（`ipc/mod.rs` `TaskRequest`）、结果渲染（`app/mod.rs` `render_result`：文本字段改名 + 附件字段合并 + 新增 JSON）、渠道公共层（`channels/conversation.rs`）、四个 IM 渠道卡片/文本回退（单选 / 严格 / 原生推荐）、钉钉互动卡片**模板**（新建并把内置默认模板 ID 升级为它）、弹窗前端（radio / 隐藏补充区）、i18n。
> **不改**：退出码语义（0/1/3）、stdout 洁净契约；daemon 协议仅字段增量演进且向后兼容。

## 1. 背景

现有 AskHuman 的提问总是允许用户在预设选项之外补充自由文本 / 附件，且结果以「随界面语言本地化的文本区块」输出。要把 AskHuman 当作**可被脚本 / 自动化调用的"选择器"**时存在两个缺口：

- 无法强制「只允许从预设答案中选择」——脚本需要受控、明确的选择结果；
- 文本区块按界面语言本地化、字段名不稳定，不利于程序解析。

本需求新增两个**正交**能力：①「严格选择模式」（禁补充）+ 单选 / 多选切换；②「结构化（JSON）输出」与「统一英文字段」。同时把「面向 Agent」与「面向脚本」的用法在 help 体系里分开呈现。

## 2. 目标

```bash
# 严格 + 单选 + JSON：脚本拿到明确、可解析的单一选择
AskHuman -q "要部署到哪个环境？" -o "staging" -o "production" --select-only --single --output json

# 严格 + 多选（默认多选）+ JSON
AskHuman -q "勾选要清理的缓存" -o "npm" -o "cargo" -o "docker" --select-only --output json

# 仅结构化输出（非严格，仍允许补充）
AskHuman "看看这个改动？" -q "继续吗？" -o "继续" -o "停止" --output json
```

- `--select-only`：只允许勾选预设项，禁用自由文本与附件；
- `--single`：单选（默认多选）；
- `--output json`：结果以 JSON 输出，字段稳定、英文、易解析；
- 三者可任意组合，也可各自单独使用。

## 3. 已确认决策

| 编号 | 决策项 | 结论 |
|---|---|---|
| D1 | 参数集 / 正交 | 新增 `--select-only`、`--single`、`--output <text\|json>`。三者**全局**（对所有问题生效，同 `--no-markdown`）、**正交独立**，可任意组合。`--single` 默认关闭（默认多选） |
| D2 | 严格语义 | `--select-only`：禁用自由文本输入 + 文件/图片**回复**附件，只能勾选预设项。**要求每个问题都有 `-o` 选项**，否则 CLI 报错退 1（无选项则无法作答）。注：`-f`（AI→人 的展示附件）不受影响 |
| D3 | 单选语义 | `--single`：使该题"恰好一个"选择。输出**仍用数组**（`selected_options`/`selected_indices` 长度 ≤ 1），单/多选共用同一套解析；单选脚本取 `[0]` 即可。`--single` 对无选项的问题为 no-op |
| D4 | 输出格式 | `--output text`（默认 = 现有区块，字段名见 D6）；`--output json`。非法值 → stderr 报错退 1 |
| D5 | 退出码 | **维持不变**：0（作答或取消）/ 1（参数·落盘错误）/ 3（连接·无渠道）。区分"作答 / 取消"只看 JSON 的 `action`（或文本的 `[status]`），信息全在输出里，不靠退出码 |
| D6 | 统一英文字段（文本侧） | 纯文本"**只改名 + 合并附件，不加其它字段**"：`[选择的选项]`→`[selected_options]`、`[用户输入]`→`[user_input]`、`[图片]`与`[文件]`**合并为** `[files]`（图片/文件/目录的本地路径，模型按后缀区分）、`[状态]`→`[status]`（取消）。**恒英文、不再随界面语言本地化**；多题仍 `# Q1` 分组。**不**在文本中新增 `selected_indices` / `action` / `channel`（这些仅 JSON 有） |
| D7 | JSON 结构 | snake_case、**美化多行**。顶层 `action`（`answer`\|`cancel`）+ `channel`（作答渠道 id）。`answers` 仅含**有作答**的题，每项 `question_index`（0 基，对应输入问题顺序）+ **仅非空字段**：`selected_options`、`selected_indices`、`user_input`、`files`。单选时数组长度 ≤ 1。取消 → `{ "action":"cancel", "channel":"<id>" }`。**无** `version` / `answered`（作答与否由字段有无推导） |
| D6b | 附件字段合并 | `[图片]`（落盘的图片路径）与 `[文件]`（透传的非图片路径）**统一合并为单一 `files`**（文本 `[files]`、JSON `files`），值为本地路径数组（图片/文件/目录），模型按后缀区分类型；渲染时顺序为「落盘图片路径 + 透传文件路径」拼接。此为对**既有输出契约**的有意变更，对所有输出（含默认文本与 JSON）生效；严格模式禁附件，故无此字段 |
| D8 | 字段语义 | `selected_indices`：按 `-o` 出现顺序的 0 基下标。推荐选项前缀不进 `selected_options`（保持原文），下标按原文匹配；重复文案取首个命中 |
| D9 | JSON 产出位置 | 由 Daemon 的 `render_result` 产出（macOS/Linux/Windows CLI 均仅转发 `Final.stdout`，保持瘦客户端）。 |
| D10 | help 体系 | `--help`/`-h` = **完整功能**，按「提问 / 管理」两块组织、列出新参数并指向另两者；`--agent-help` = **为 Agent 精调**的提问用法（仅把结果字段标记改名，不含脚本参数 / JSON）；`--scripting-help`（新）= **脚本/自动化用法**（`--select-only`/`--single`/`--output json` + JSON 结构 + 退出码，简洁精确、少量示例）。`--agent-help` 与 `--scripting-help` 用**共享片段 + 变量/条件组装**，不各写一份 |
| D11 | 弹窗 | `--single`→选项渲染为 radio（恰好一个）；`--select-only`→隐藏补充文本框 + 回复附件拖拽区，且**必须选中才能提交**（仍可取消）；推荐展示沿用现有绿色徽标 |
| D12 | Telegram | `--single`→inline 按钮互斥（选一个清其它，按钮 ✅ 高亮）+「提交」；`--select-only`→忽略卡片后聊天里的自由文字（不并入答案）；无选择就点提交 → `answerCallbackQuery` 弹 alert 提示。推荐展示 = **现状文字前缀**「【👍推荐】 」（平台按钮无法单独配色，沿用不变）。**demo 实测确认** |
| D13 | 飞书 | **多选** = 现有表单（checker + 提交），严格则**去掉 `input` 组件**；**单选** = 真 radio：checker **移出表单**、每个挂 `behaviors:callback`，每次点击走回调互斥（仅命中项 `checked`），由会话**自管选中态**，「提交」按钮收尾（接受点击延迟）。推荐展示 = **左侧彩色文字前缀** `<font color='green'>【👍推荐】</font> `（绿色含括号，checker `text` 用 `lark_md`）。**demo 实测：checker 无原生 `icon`（API 报 unknown property），`button_area` chip 只能固定在右侧，故弃用，改用左前缀彩色文字** |
| D14 | Slack | **多选** = `checkboxes`、**单选** = `radio_buttons`（原生单选）；严格 → 去掉 `plain_text_input` 块。推荐展示 = option 原生 `description`「👍 推荐」+ 选项文本 mrkdwn 加粗（checkbox / radio 的 `text` 与 `description` 均支持 mrkdwn；注意 75 字符上限）。**demo 实测确认** |
| D15 | 钉钉 | **统一新模板**（用户已搭 `d5dc7ac5-…`，**最后由用户发布**），靠变量条件渲染：`single`（单选列表 CheckboxList / 多选列表 CheckboxListMulti）、`allow_input`（补充输入框显隐）；选项用富文本 `options[].md` 渲染（字号 h5=15px，`<font sizeToken=common_h5_text_style__font_size>`），推荐项前缀 `<font sizeToken=… colorTokenV2=common_green1_color>【👍推荐】</font> `（绿色含括号，**demo 实测定稿**）。**新版把内置默认模板 ID 升级为该新模板**。卡片投放失败回退纯文本时同样遵守严格 / 单选 |
| D16 | 钉钉模板变量契约（demo 实测定稿） | 公有 cardParamMap（**值全为字符串**）：`title`、`markdown`、`options`(JSON 串 `[{id:下标(int), md:富文本}]`)、`single`("true"\|"false")、`allow_input`("true"\|"false")、`submit_status`；私有：`submitted`("false")、`private_input`("")。提交按钮 actionId=`submit_action`，回传 `params`：`{ user_input, selected_options }`，`selected_options` 装**选项 id**（多选=id 数组 `[0,2]`、单选=`{id}` 或单值，解析需兼容三态）；id=下标，按下标还原选项原文与 `selected_indices`。单/多选、输入框显隐全用同一模板靠变量切换。注：cardParamMap 只收字符串，布尔靠模板按变量类型还原；**下发真布尔会报「StringValue is mandatory」** |
| D17 | 推荐展示总览（demo 定稿） | 弹窗 = 现有绿色徽标；Slack = option `description`「👍 推荐」+ 加粗（原生控件内）；飞书 = 左侧彩色文字前缀 `<font color='green'>【👍推荐】</font> `；Telegram = 文字前缀「【👍推荐】 」；钉钉 = 选项 `md` 富文本左侧绿色前缀 `【👍推荐】`（`colorTokenV2=common_green1_color`）。**仅弹窗 / Slack 为原生控件展示；飞书 / Telegram / 钉钉为彩色 / 文字前缀**（平台能力所限） |
| D18 | 钉钉字号说明（demo 实测） | 钉钉**互动卡片**富文本 `<font>` 不支持自定义像素（`size=N` 仅旧版机器人消息有效，互动卡片会忽略），只认预设 `sizeToken`：footnote=12px、**h5=15px**、body=14px(PC)/17px(移动)。无 13px。选项定稿用 **h5(15px)**（介于 12 与 14/17 之间）|

## 4. help 体系与示例（供评审）

> 三套 help 的实际拟定文案（英文示例；中文同形）。`--agent-help` 与 `--scripting-help` 中**重复的片段**（调用式、参数说明、结果字段、示例）在实现时抽成共享常量 / 函数组装。

### 4.1 `--help` / `-h`（完整功能，按「提问 / 管理」分块）

```
AskHuman - Human-in-the-loop interaction tool

Usage:
  AskHuman <message> [options]      Ask a human and collect their response

Asking (agents: see --agent-help · scripts/automation: see --scripting-help):
  -q, --question <text>   Ask a question; repeatable
  -o, --option <text>     Add a predefined answer option after a question
  -o!, --option! <text>   Same as -o, marks it as your recommended answer
  -f, --file <path>       Attach a file/image to the message; repeatable
  --stdin                 Read the message from stdin
  --no-markdown           Disable Markdown rendering
  --single                Single choice (default: multiple choice)
  --select-only           Choice only: forbid free text/attachments (each question must have options)
  --output <text|json>    Output format (default: text)

Management:
  --settings              Open the settings window
  --history [--all]       Open the reply history window (current project; --all for every project)
  daemon <sub>            Manage the background daemon: status/stop/restart/start/logs

Help:
  --agent-help            Concise usage tuned for AI agents (asking)
  --scripting-help        Usage for scripts/automation (choice-only, single, JSON output)
  --help, -h              Show this help
  --version, -v           Show version
```

### 4.2 `--agent-help`（为 Agent 精调；仅结果字段改名）

```
AskHuman — ask a human and collect their response.

Invocation:
  AskHuman "<Message>" [-f "<file>" ...] [-q "<question>" [-o "<option>" ...] ...] [--no-markdown]

Arguments:
  <Message>             Shared description for all questions (optional)
  --stdin               Read the <Message> from stdin instead of the argument
  -f, --file <path>     Attach a file or image to the Message; repeatable
  -q, --question <text> Ask a question; repeatable
  -o, --option <text>   Add a predefined answer option after a question
  -o!, --option! <text> Same as -o, and marks that option as your recommended answer
  --no-markdown         Disable Markdown rendering (applies to all descriptions/questions)

User response:
  [selected_options]  Predefined options the user checked
  [user_input]        Free-form text the user typed
  [files]             Local paths the user attached (images/files/dirs; tell type by extension)
  [status]            Shown when the user cancels; follow its instructions to keep asking

Multi-question output:
  Each question is grouped under "# Qn", with questions separated by "---"

Examples:
  AskHuman -q "Proceed with deploy?" -o! "Proceed" -o "Stop"
  AskHuman "Review this change?" -f ./diff.patch -q "Continue?" -o "Continue" -o "Stop"
```

### 4.3 `--scripting-help`（脚本 / 自动化用法；新）

```
AskHuman — scripting / automation usage (machine-readable choices).

Invocation:
  AskHuman [<Message>] -q "<question>" -o "<option>" ... --select-only [--single] --output json

Script options (combine freely):
  --select-only   Choice only: forbid free text and attachments; each question must have options
  --single        Single choice (default: multiple choice); selection has at most one item
  --output <fmt>  Output format: text (default) | json

Exit codes:
  0  the user answered, or cancelled
  1  usage or local I/O error
  3  could not collect a response (daemon/connection, or no channel available)

JSON output (--output json) — snake_case, pretty-printed:
  {
    "action": "answer",            // "answer" | "cancel"
    "channel": "popup",            // channel that answered
    "answers": [                    // only answered questions; empty fields omitted
      { "question_index": 0, "selected_options": ["staging"], "selected_indices": [0] }
    ]
  }
  - single choice -> arrays have at most one element.
  - non-strict answers may also carry "user_input" / "files".
  - "files": local paths the user attached (images/files/dirs).
  - cancel -> { "action": "cancel", "channel": "popup" }

Example:
  AskHuman -q "Deploy where?" -o "staging" -o "production" --select-only --single --output json
```

## 5. 各渠道行为（严格 / 单选 / 推荐展示）

| 渠道 | 多选（默认） | 单选（`--single`） | 严格（`--select-only`） | 推荐展示 |
|---|---|---|---|---|
| 本地弹窗 | checkbox（现状） | radio | 隐藏补充文本框 + 回复附件拖拽区；必须选中才可提交 | 绿色徽标（现状） |
| Telegram | inline 多选 +「提交」 | 按钮互斥 ✅ 高亮 +「提交」 | 忽略聊天自由文字 | 文字前缀「【👍推荐】 」 |
| 飞书 | 表单 checker +「提交」 | checker 移出表单 + 回调互斥（真 radio）+「提交」 | 去掉 `input` 组件 | 左侧彩色前缀 `<font color='green'>【👍推荐】</font> `（lark_md） |
| Slack | checkboxes | radio_buttons | 去掉 `plain_text_input` 块 | option `description`「👍 推荐」+ 加粗 |
| 钉钉 | 模板多选列表 | 模板单选列表（`single` 条件渲染） | `allow_input=false` 隐藏输入框 | 选项 `md` 富文本左侧绿色前缀 `【👍推荐】` |

- 各渠道**提交值恒为选项原文**；推荐前缀 / 标记只进显示。
- 各渠道**文本回退**模式（钉钉卡片投放失败等）同样遵守严格（编号清单 + 忽略自由文字）/ 单选（仅接受一个编号）。
- 严格 + 单选在各渠道一律「先选一个再点提交」（不做"点一下即提交"，与现有收尾 / 多题流程一致）。

## 6. 约束与既有规则（不可破坏）

- **stdout 洁净**：仍只输出结果（文本区块或 JSON），所有日志 / 警告走 stderr。
- **退出码语义不变**：0 / 1 / 3 含义同现状（D5）。
- **提交值为原文**：所有渠道（含文本回退）回传的 `selected_options` 为不带前缀的原始选项文本。
- **向后兼容**：不带新参数时，文本输出除「字段标记改为英文且不本地化」（D6）与「`[图片]`+`[文件]` 合并为 `[files]`」（D6b）这两处刻意变更外，行为不变；daemon 协议新增字段均带 serde 默认值，新旧二进制短暂并存可解析。
- **解析为纯函数**：`parse_ask` 保持无 IO 副作用、可单测（含新参数与"严格需有选项"校验）。
- **历史兼容**：回复历史落盘 / 还原不受影响（历史用结构化数据，不解析文本标记）。

## 7. 验收标准

1. `--select-only`：弹窗与各渠道隐藏补充输入 / 附件，仅能勾选；某题无 `-o` → CLI 报错退 1。
2. `--single`：各渠道单选（恰好一个）；输出数组长度 ≤ 1；与多选共用解析。
3. `--output json`：输出符合 D7 结构（snake_case、省略空字段、`answers` 仅含已作答题、取消为 `{action,channel}`、美化多行）；`--output text` 默认输出按 D6 改名后的英文字段。
4. 退出码：作答 / 取消 = 0、参数·落盘错误 = 1、连接·无渠道 = 3，与现状一致。
5. 三者正交：任意组合（如 `--single --output json` 非严格、`--select-only` 但 `--output text`）均行为正确。
6. help：`--help` 分「提问 / 管理」两块且含新参数与指引；`--agent-help` 字段标记已改英文且不含脚本参数；`--scripting-help` 含脚本参数 / JSON / 退出码、简洁；两者共享片段组装无重复。
7. 各 IM 渠道单选 / 严格 / 推荐展示按 §5 生效；提交回传原文；钉钉用升级后的内置默认模板。
8. 既有用法（无新参数）回归正常；文本输出字段名改英文，且 `[图片]`+`[文件]` 已合并为单一 `[files]`（JSON 同为 `files`）。

## 8. 交付流程（demo 先行）

卡片若干「暂定」项（钉钉内联「推荐」样式、飞书单选 radio 与推荐 icon、Slack 单选 / 推荐 description、Telegram 单选高亮）**只能看真实效果才能敲定**，故采用 demo 先行：

1. 写卡片原型（最小可发卡代码 / demo），经**各真实渠道**把预计样式发出来给评审；
2. 据真实观感**敲定卡片待定项**（推荐展示、单选交互细节），必要时迭代；
3. 再进入正式编写 + 单元测试；
4. 最后实测验证（`./scripts/install.sh` 后用新 `AskHuman` 全链路验证）。

详细任务拆解见计划文档 `docs/plans/strict-choice-and-structured-output.md`。

## 9. 反馈意见

（后续 review 中产生的调整意见追加于此，标注日期。）
