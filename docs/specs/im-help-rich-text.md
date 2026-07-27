# IM 富文本帮助

> 状态：已实现（2026-07-27）。
> 关联现状：`docs/specs/im-always-respond.md` R3、`docs/specs/im-command-phrases.md`。

## 1. 背景

四个 IM 渠道目前共用 `autochannel::help_text`：它把标题、命令、自然语言短语和动态提示直接
拼成一段带项目符号的纯文本，再由 `reply_channel_text` 发送。结果有三个问题：

1. 命令语法、说明和自然语言短语挤在同一段纯文本里，层级不清晰；
2. Telegram、Slack、钉钉和飞书已有的富文本能力没有用于帮助消息；
3. i18n 词条包含 `•`、`—`、换行等版式，内容和渠道渲染耦合，难以按渠道优化。

帮助不只来自显式 `/help`：未知命令、无在途提问时的普通消息、严格模式中未接受的输入等
动态引导也复用同一内容。本规格覆盖所有这些入口。

## 2. 目标

- 每个正式命令是一个无序列表项并占一个主行，命令语法使用行内代码；
- 命令按功能分组；
- 命令说明保持简洁，但保留会影响预期或安全性的关键行为；
- 支持无前缀自然语言短语的命令，在同行末尾显示弱化提示；
- 各渠道采用原生最佳载体，保持统一语义层级，不要求像素一致；
- 飞书用灰色呈现自然语言短语；不支持任意文字颜色的渠道用斜体等非颜色方式降级；
- 保持原有动态门控、作答指引、自动切槽提示和 Slack `!` 前缀；
- 富文本发送失败时可靠回退到无标记的分组纯文本。

## 3. 已确认决策

| 编号 | 决策 | 结论 |
|---|---|---|
| D1 | 命令组织 | 按功能分组，不再使用单一平铺列表 |
| D2 | `/yolo` 归属 | 归入 **Agent 管理**，不放入“其他” |
| D3 | 自然语言短语 | 与命令同行，置于说明之后并弱化 |
| D4 | 渠道载体 | 使用渠道原生最佳载体，不强求统一为同一种 Markdown 消息 |
| D5 | 命令范围 | 列出完整正式命令集；`/unwatch`、`/msg-clear` 独立成行 |
| D6 | 覆盖入口 | 显式 `/help` 和所有隐式动态引导都发送同一份完整富文本帮助 |
| D7 | 文案密度 | 精简说明，但保留“下次工具调用送达”“确认后暂存”等关键行为 |
| D8 | 视觉一致性 | 一致的是命令/说明/短语的语义层级；颜色只是渠道允许时的增强 |
| D9 | 可访问性 | 不靠颜色单独传意；短语始终带“直接说”文字，颜色丢失后仍可理解 |
| D10 | i18n | 翻译只保存内容字段，不内嵌 Markdown、HTML、项目符号或换行布局 |
| D11 | 共享模型 | 先生成结构化 `HelpView`，再由各渠道 renderer 输出；不把某种 Markdown 方言作为领域模型 |
| D12 | 失败降级 | 任一富文本发送失败时，以同一 `HelpView` 渲染分组纯文本并重试一次 |
| D13 | 命令列表 | 每个命令必须渲染为真正的无序列表项，不能只靠换行排列 |

## 4. 信息结构

### 4.1 共享视图模型

在 `autochannel` 中用拥有所有字符串的结构化模型承载帮助语义；`help_text()` 仅保留为纯文本
兼容包装：

```rust
pub struct HelpView {
    pub title: String,
    pub intro: String,
    pub sections: Vec<HelpSection>,
    pub question_state: HelpQuestionState,
    pub switch_hint: Option<String>,
}

pub struct HelpSection {
    pub title: String,
    pub commands: Vec<HelpCommand>,
}

pub struct HelpCommand {
    pub syntax: String,
    pub description: String,
    pub phrase: Option<String>,
}

pub enum HelpQuestionState {
    Active { instruction: String },
    None { message: String },
}
```

`HelpView` 只表达语义，不包含反引号、`<font>`、Slack `mrkdwn` 或 Telegram HTML。所有字段使用
拥有所有权的 `String`，以便在会话事件循环的异步发送任务中安全移动。

生成入口保持现有动态参数：

```rust
pub fn help_view(
    auto_activation: bool,
    has_active_question: bool,
    watch_supported: bool,
    prefix: &str,
    lang: Lang,
) -> HelpView
```

### 4.2 分组与顺序

固定使用以下顺序：

1. **Agent 管理**
2. **代码与记录**
3. **项目待办**
4. **渠道与帮助**

`/yolo` 放在 Agent 管理；`/help` 固定为最后一行。`/here` 关闭自动激活时不生成，因此关闭状态下
“渠道与帮助”只含 `/help`。

### 4.3 命令清单与中文文案

| 分组 | 语法 | 精简说明 | 同行短语 |
|---|---|---|---|
| Agent 管理 | `{p}status [编号]` | 列出 Agent；指定编号查看当前活动 | 状态 |
| Agent 管理 | `{p}new` | 在电脑上创建新的 Agent 任务 | 新任务 |
| Agent 管理 | `{p}watch [编号]` | 关注 Agent 的实时状态 | 关注 |
| Agent 管理 | `{p}unwatch [编号\|all]` | 取消一个或全部关注 | 取消关注 |
| Agent 管理 | `{p}msg [编号] [内容]` | 排队插话，下次工具调用时送达 | 插话 |
| Agent 管理 | `{p}msg-clear <编号>` | 撤回该 Agent 待送达的插话 | 无 |
| Agent 管理 | `{p}yolo [off [编号]]` | 查看或关闭 Codex YOLO 会话 | YOLO |
| 代码与记录 | `{p}diff [编号]` | 导出未暂存变更（附件） | 查看变更 |
| 代码与记录 | `{p}stage [编号]` | 确认后暂存未暂存改动 | 暂存 |
| 代码与记录 | `{p}transcript [编号]` | 导出完整会话记录（附件） | 导出会话 |
| 项目待办 | `{p}todo [内容]` | 选择项目查看待办或新增一条 | 待办 |
| 项目待办 | `{p}todo-rm` | 选择项目并删除待办 | 删待办 |
| 项目待办 | `{p}todo-auto [内容]` | 切换或新增自动执行待办 | 自动待办 |
| 渠道与帮助 | `{p}here` | 把后续提问切到此渠道 | 这里 |
| 渠道与帮助 | `{p}help` | 显示这份帮助 | 帮助 |

注意：

- `{p}` 在 Slack 为 `!`，其余渠道为 `/`；
- `watch_supported=false` 时同时移除 `/watch` 和 `/unwatch`；
- `auto_activation=false` 时移除 `/here`；
- `/msg-clear` 没有无前缀短语，不能显示“直接说「撤回」”。解析器仅支持带前缀的
  `/撤回 <编号>` 别名；
- 参数方括号表示可选，尖括号表示必填。`/msg` 的编号和内容确实都可省略；
- todo 的旧 Agent 编号兼容入口不在帮助中主推；
- 每个命令只列一个代表性短语，完整短语词表仍以 `COMMAND_PHRASES` 为准。

### 4.4 英文文案

| Group | Syntax | Description | Phrase |
|---|---|---|---|
| Agent management | `{p}status [n]` | List agents; add a number to view current activity | status |
| Agent management | `{p}new` | Create a new Agent task on your computer | new |
| Agent management | `{p}watch [n]` | Follow an agent with a live status card | watch |
| Agent management | `{p}unwatch [n\|all]` | Stop following one or all agents | unwatch |
| Agent management | `{p}msg [n] [text]` | Queue a message for the agent's next tool call | message |
| Agent management | `{p}msg-clear <n>` | Discard the agent's queued message | none |
| Agent management | `{p}yolo [off [n]]` | View or turn off Codex YOLO sessions | yolo |
| Code and records | `{p}diff [n]` | Export unstaged changes as an attachment | diff |
| Code and records | `{p}stage [n]` | Confirm, then stage unstaged changes | stage |
| Code and records | `{p}transcript [n]` | Export the full session transcript | transcript |
| Project todos | `{p}todo [text]` | Choose a project to view or add todos | todo |
| Project todos | `{p}todo-rm` | Choose a project and delete todos | delete todo |
| Project todos | `{p}todo-auto [text]` | Toggle or add auto-run todos | auto todo |
| Channel and help | `{p}here` | Route future questions to this channel | here |
| Channel and help | `{p}help` | Show this help | help |

## 5. 统一版式

帮助消息按以下层次排列：

1. 标题：`AskHuman 正在运行` / `AskHuman is running`
2. 简短说明：可使用命令，或直接发送每行末尾的短语
3. 四个命令分组；组内每个命令是一个无序列表项
4. 分隔
5. 当前提问状态和作答方式
6. 自动激活开启时的切槽提示

中文语义示意：

```markdown
# AskHuman 正在运行

可使用下列命令，或直接发送每行末尾的短语。

## Agent 管理
- `/status [编号]` — 列出 Agent；指定编号查看当前活动　· 直接说「状态」
- `/new` — 在电脑上创建新的 Agent 任务　· 直接说「新任务」
…

## 代码与记录
…

## 项目待办
…

## 渠道与帮助
…

---
当前暂无进行中的提问。
提示：发送任意文字即可把提问切到此渠道接收。
```

这是语义示意，不作为任何渠道的直接 payload。实际字号、颜色、分隔线和标题标签由渠道 renderer
决定。客户端窄屏自动换行是允许的，但 renderer 不主动把短语拆到第二行。

## 6. 渠道渲染

### 6.1 飞书

- 载体：消息卡片 JSON 2.0；
- 标题：蓝色 notation 标题行，可复用现有卡片视觉语言；
- 每个分组使用独立 Markdown 组件，分组名加粗；
- 每个命令使用 Markdown `- ` 无序列表项；
- 命令语法使用反引号；
- 同行短语使用
  `<font color='grey'>· 直接说「状态」</font>`；
- 动态状态前放 `hr`，次级提示使用灰色 notation 或灰色 Markdown；
- 使用 `feishu::card::build_help_card(&HelpView)`，不复用提问卡表单结构。

飞书是本设计中唯一保证短语使用不同文字颜色的渠道。

### 6.2 Slack

- 载体：Block Kit；
- 一个 `header` block；
- 每个分组一个 `section` block，内部使用 `mrkdwn`；
- 每个命令使用显式 `• ` 列表符；
- 命令语法使用行内代码；
- 同行短语使用斜体 `_· 直接说「状态」_`；
- 命令区和动态状态之间使用 `divider`；
- 动态状态可用 `context` block；若正文较长则使用普通 `section`；
- 顶层 `text` 使用完整纯文本 fallback，供通知和无障碍读取；
- 每个 section 独立，避免接近单个 section 的 3000 字符限制。

Slack 不支持任意文字颜色，不模拟不存在的颜色能力。

### 6.3 钉钉

- 载体：现有 `sampleMarkdown` 单聊主动消息；
- `title` 为本地化标题；
- 分组名使用 Markdown 加粗或三级标题；
- 每个命令使用 Markdown `- ` 无序列表项；
- 命令语法使用行内代码；
- 同行短语使用斜体；
- 动态状态前使用 Markdown 分隔线；
- 不依赖 `<font colorTokenV2>`：项目仅在互动卡片 Markdown 模板中验证过该扩展，
  `sampleMarkdown` 不以颜色作为验收条件。

不为帮助新增钉钉互动卡片模板，避免引入开发者后台导入、发布和默认模板 ID 的运维成本。

### 6.4 Telegram

- 载体：`sendMessage` + `parse_mode=HTML`；
- 标题和分组使用 `<b>`；
- 每个命令使用 `• ` 列表符；
- 命令语法使用 `<code>`；
- 同行短语使用 `<i>`；
- 所有文本节点通过现有 HTML 转义规则；
- 完整消息必须保持在 Telegram 4096 字符限制内。

使用帮助专用 HTML renderer；`HelpView` 本身不保存 Markdown。

### 6.5 纯文本降级

`render_help_plain(&HelpView)` 保留：

- 标题；
- 分组名；
- 每行一个带 `• ` 的命令；
- `· 直接说「…」`；
- 动态状态与切槽提示。

它不包含反引号、星号、HTML 标签或渠道专属 token。富文本 API 失败后只重试这一次；纯文本也
失败则沿用现有日志策略，不继续循环发送。

## 7. 发送与路由

### 7.1 观察者路径

`daemon/unix_impl/inbound.rs::handle_inbound` 中所有当前
`reply_channel_text(..., help_text(...))` 调用改为：

1. 生成 `HelpView`；
2. 调用 `reply_channel_help(channel_id, config, &view)`；
3. 按渠道发送富文本；
4. 失败时调用纯文本 fallback。

普通状态、确认回执、错误和其它命令结果仍走 `reply_channel_text`，不扩大本次范围。

### 7.2 活动会话路径

`channels::conversation::answer_inbound_reply` 当前只能返回字符串，无法区分确认回执和帮助。
改为：

```rust
pub enum InboundReply {
    Text(String),
    Help(HelpView),
}
```

- 内容被接受时返回 `InboundReply::Text`；
- 未被接受且需要引导时返回 `InboundReply::Help`；
- 命令或需要退避时仍返回 `None`。

四个渠道在已有 client 上发送：

- `Text` 沿用现有纯文本路径；
- `Help` 调用本渠道帮助 renderer 和发送 API；
- 富文本失败后在同一个 client 上发送纯文本 fallback。

这样显式 `/help`、观察者 liveness 和作答期动态引导共用同一个语义模型，不会出现只有部分入口
升级成富文本的问题。

### 7.3 非阻塞约束

卡片作答期的即时回复仍必须在独立异步任务中发送，不能阻塞卡片提交和附件下载事件循环。结构化
视图必须在 spawn 前完成构建或拥有全部数据，不能借用 `QuestionCtx`。

## 8. i18n 重构

现有 `autoChannel.helpCmd*` 整行词条拆为以下类别：

- 标题、简介；
- 四个分组名；
- 每个命令的说明；
- 每个命令的代表性短语；
- “直接说「{phrase}」”包装文案；
- 当前提问状态、作答说明和切槽提示。

命令语法保存在 Rust 静态定义中，参数名按语言选择；前缀在生成 `HelpView` 时注入。翻译词条不得
包含：

- `•`、`—` 等布局符号；
- Markdown 反引号、粗体、斜体；
- HTML 或飞书 `<font>`；
- 主动换行形成的多个命令。

完整自然语言识别仍由 `COMMAND_PHRASES` 决定。帮助中的代表性短语必须有单测证明可在无前缀时
分类到对应命令；`/msg-clear` 明确例外，不显示短语。

## 9. 验收标准

### 9.1 共享模型

1. 中文和英文均按四个固定分组、固定命令顺序生成；
2. 一条 `HelpCommand` 只包含一个命令语法，不再把 `/status` 两种形式拼成两行；
3. `watch_supported=false` 同时隐藏 `/watch`、`/unwatch`；
4. `auto_activation=false` 隐藏 `/here` 和切槽提示；
5. 有/无在途提问时生成对应动态状态；
6. Slack 使用 `!`，其余渠道使用 `/`；
7. 完整命令集包含 `/msg-clear` 和 `/yolo`，且 `/yolo` 位于 Agent 管理；
8. 除 `/msg-clear` 外，所有显示的代表性短语均能被 `classify` 无前缀识别。

### 9.2 渠道 payload

1. 飞书卡片包含分组、Markdown 无序列表、行内代码和灰色短语；
2. Slack blocks 包含 header、四个 section、divider 和动态状态，每个命令有 `•`，短语为斜体；
3. 钉钉发送 `sampleMarkdown`，每个命令是无序列表项、命令为行内代码，短语为斜体；
4. Telegram 使用 HTML parse mode，每个命令有 `•`、命令为 `<code>`，短语为 `<i>`；
5. 所有 renderer 正确转义特殊字符，不把本地化文本解释成意外标记；
6. 中文/英文及所有动态组合均不超过各渠道消息/组件限制；
7. 纯文本 fallback 不含任何残留 markup。

### 9.3 端到端入口

四个渠道分别覆盖：

1. 显式 help 命令；
2. 未知 `/命令`；
3. 无在途提问时的普通消息；
4. 无在途提问时的图片/文件；
5. 作答期未被接受的文字；
6. 富文本发送失败后的纯文本回退；
7. 内容被接受时仍只发送既有简短确认，不误发完整帮助；
8. 单条入站消息仍至多产生一条业务回复。

## 10. 非目标

- 不修改命令解析语义或自然语言短语词表；
- 不给 Slack、Telegram 伪造任意文字颜色；
- 不新增钉钉互动卡片模板；
- 不把 `/status`、错误提示等其它普通文本一并升级为富文本；
- 不增加“精简帮助”和“完整帮助”两个模式；
- 不改变 Popup、CLI help、stdout、回答和抢答行为。

## 11. 实现落点

- `src-tauri/src/autochannel.rs`：`HelpView`、命令元数据、动态生成、纯文本 renderer；
- `src-tauri/src/i18n.rs`：拆分 help 内容词条；
- `src-tauri/src/daemon/unix_impl/inbound.rs`：观察者路径的富文本发送；
- `src-tauri/src/channels/conversation.rs`：`InboundReply::Text/Help`；
- `src-tauri/src/feishu/card.rs`：帮助卡片；
- `src-tauri/src/slack/blockkit.rs`：帮助 blocks；
- `src-tauri/src/dingtalk/`：帮助 Markdown renderer；
- `src-tauri/src/telegram/`：帮助 HTML renderer；
- 四个 `src-tauri/src/channels/*.rs`：活动会话中的富文本帮助发送与纯文本回退；
- 对应单元测试和渠道 payload 测试。
