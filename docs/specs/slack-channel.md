# 需求：新增「Slack」通信渠道（Channel）

> 状态：待确认（review 后按计划实现）
> 关联计划：`docs/plans/slack-channel.md`
> 关联既有：`docs/specs/feishu-channel.md` / `docs/specs/dingtalk-channel.md`（飞书 / 钉钉渠道总体设计；本需求按飞书「长连接 + 互动卡片 + 完整双向交互」对齐到 Slack）

## 1. 背景

`AskHuman` 现已支持四个 Channel：本地弹窗（GUI）、Telegram、钉钉（DingTalk）、飞书（Feishu / Lark）。其中钉钉 / 飞书 / Telegram 都已具备「互动卡片 + 文件收发 + 抢答」体验。本需求新增**第五个 Channel —— Slack**，实现与飞书同级的「**Agent 主动发问 → 人在 Slack 作答 → 结果回传**」完整双向交互。

Slack 与飞书的本质共性：要在本地（无公网）收消息与交互回调，Slack 提供与飞书长连接等价的「**Socket Mode**」——零公网域名 / 零内网穿透即可接收事件（events）与交互（interactivity）负载，收到后须**3 秒内 ack**（回 `envelope_id`），否则平台重推。因此本需求选用「**Slack App + Socket Mode + Bot + 单聊（DM）**」方案，并**复用现有 `channels::conversation::{MessagingChannel, run_conversation}` 公共驱动**。

与飞书的关键差异（影响实现）：

1. **双 token**：Slack 需要两个密钥——Bot Token（`xoxb-…`，调 Web API）与 App-Level Token（`xapp-…`，scope `connections:write`，仅用于建立 Socket Mode 连接）。飞书 / 钉钉是「appId/appSecret」单对凭据。
2. **Socket Mode 帧是 JSON**（非飞书 protobuf）：每帧含 `envelope_id`，ack 只需回 `{"envelope_id": id}`；ack 与「卡片更新」**解耦**（卡片更新走 Web API `chat.update`，不绑 3 秒窗口）。因此 Router 可**收帧即立即 ack**，比飞书的「延迟 ack 携回包」更简单。
3. **互动卡片用 Block Kit（消息内 `input` 块）**：经核实，Slack **消息内支持 `input` 块（多行 `plain_text_input` 文本输入 + `checkboxes` 复选框）**；用户点「提交」按钮产生的 `block_actions` 回调会**携带整条消息的 `state.values`**（含勾选项与输入框文本）。故可实现与飞书等价的「卡片内表单（多选 + 补充文字 + 提交）」体验，无需弹出式 Modal。
4. **无「禁用表单」终态**：Slack 消息无法像飞书那样把表单置灰保留控件；终态改为 `chat.update` 成**静态卡片**（回显已选项 + 补充文字 + 状态行，移除全部控件），形态更接近 Telegram 收尾。
5. **文件上传新流程**：旧 `files.upload` 已于 2025 弃用，改用 `files.getUploadURLExternal` + `files.completeUploadExternal` 两步上传并分享到 DM。
6. **Markdown 方言**：Slack 用 `mrkdwn`（`*粗*`/`_斜_`/`~删~`/`` `码` ``/```块```/`> 引`/`<url|文字>`），需新增「标准 Markdown → mrkdwn」转换（仿 `telegram/markdown.rs`）。

## 2. 目标

用户在设置页「通信渠道」中配置 Slack（Bot Token / App Token / User ID）并启用后：

```bash
AskHuman "请看看这个改动？" -f ./diff.patch -q "要继续吗？" -o "继续" -o "停止"
```

- Slack 机器人**主动私聊（DM）**用户：先发共享 Message（含 `-f` 文件），再逐题发**互动卡片**（Block Kit：标题 + 问题正文 + 可勾选的预定义选项【复选框，多选】+ 补充文字输入框 + 「提交」按钮）。
- 用户在卡片内**勾选选项（多选）**、可**补充文字**、作答期间还可在 DM 里**发图片/文件**，点「提交」完成该题。
- 多题逐条进行；全部完成后结果回传到 stdout（与弹窗/Telegram/钉钉/飞书同一契约）。
- 与弹窗 / Telegram / 钉钉 / 飞书 **并行抢答**：任一渠道率先完成即采纳，其余收尾。
- 无 GUI 时，Slack 可作为 headless 渠道单独或与其它会话型渠道并行工作。

## 3. 已确认决策

> 决策来自需求澄清（2026-06-07，经 `AskHuman` 两轮逐项确认）。

| 编号 | 决策项 | 结论 |
|---|---|---|
| S1 | 渠道能力 | **完整双向交互**（与飞书对齐）：可发可收，在 Slack 内完成作答。范围含：互动卡片（多选 + 补充文字 + 提交）/ AI→人文件发送 / 人→AI图片文件接收 / 与弹窗·钉钉·飞书·TG 抢答 / headless 无 GUI / 多问题逐题 / daemon 常热单连接 / 设置页（测试连接 + 自动识别）/ 中英 i18n / 卡片投放失败回退纯文本编号 / `prompts.rs`+`README`+`wiki` 文档 |
| S2 | 接入形态 | **Slack App + Socket Mode + Bot + 单聊（DM）**（零公网/零域名/零内网穿透）。用户自建 Slack App、开启 Socket Mode、生成 App-Level Token、装机器人取 Bot Token，并开启 Interactivity 与订阅 `message.im` 事件（见 §4 前置条件） |
| S3 | 会话场景 | **单聊（DM）**：人与机器人私聊（`channel_type="im"`），无需 @ |
| S4 | 收消息机制 | **Socket Mode**：`POST https://slack.com/api/apps.connections.open`（App Token 走 `Authorization` 头）拿临时 wss URL → 连 wss；帧为 **JSON**。两类业务帧：`events_api`（事件，如 `message`）、`interactive`（交互，如 `block_actions`）；另有 `hello`（建连）、`disconnect`（要求重连）控制帧。每条含 `envelope_id` 的帧须 **3 秒内回 `{"envelope_id": id}`** ack（**收帧即 ack**，与卡片更新解耦） |
| S5 | 帧实现 | **JSON 帧**，复用现有 `tokio-tungstenite`(rustls) + `serde_json`，**不新增任何 crate**、不引入第三方 Slack SDK（与本项目手写钉钉/飞书长连接风格一致） |
| S6 | 鉴权 | 两个密钥：**Bot Token（`xoxb-…`）** 用于全部 Web API（`Authorization: Bearer <bot_token>`）；**App-Level Token（`xapp-…`，`connections:write`）** 仅用于 `apps.connections.open` 建 Socket Mode 连接。无 token 刷新概念（长期有效） |
| S7 | 发送共享 Message | `POST https://slack.com/api/chat.postMessage`，body `{channel, text, blocks?}`。文本走 `mrkdwn`（标准 Markdown 经 §S15 转换）；DM 频道由 §S11 解析 |
| S8 | 发送题目（互动卡片） | `chat.postMessage` 发 **Block Kit blocks**：section（标题加粗 + 正文）+ **`input` 块（`checkboxes` 复选框，每选项一项，`optional:true`，不 `dispatch_action`）** + **`input` 块（`plain_text_input` 多行输入框，`optional:true`，placeholder「补充说明（可选）」）** + actions 块（「提交」`button`，`action_id="submit"`）。返回消息 `ts` |
| S9 | 选项形态 | 预定义选项用 **`checkboxes` 复选框（多选，平铺直接勾）**；每选项 `value="opt_{i}"`（下标映射，规避选项文案过长/重复）。无预定义选项时省略复选框块，仅留输入框 + 提交 |
| S10 | 收消息（Socket Mode） | 收两类业务帧：① 事件 `message`（DM 内：用户文字【作答期忽略】/图片/文件 / 识别码）；② 交互 `block_actions`（点「提交」）。**收帧即 ack**（`{"envelope_id": id}`）。`hello` 忽略；`disconnect` 重连（重新 `apps.connections.open`）；WS ping 回 pong；socket 断开按重试上限重连 |
| S11 | 接收人标识 + DM 解析 | 配置项 `userId`（用户 `U…`，稳定标识）。发送前调 `conversations.open(users=<userId>)` 取 DM 频道 id 并**进程内缓存**，`chat.postMessage`/`chat.update` 用该 DM 频道。**前置校验**：Bot/App Token 或 userId 为空 → 立即中文报错 |
| S12 | 自动识别 userId | 旁置「自动识别」：点击随机生成 4 位数字提示「请私聊机器人发送：XXXX」，经 Socket Mode 捕获 DM 内 `text==XXXX` 的 `message` 事件，取 `event.user` 回填（~120s 超时）。复用 daemon 现有 `Detect` 通道（复用同 App Token 的活动连接，否则临时开连） |
| S13 | 每题完成方式 | 卡片点「提交」即完成该题。`block_actions` 回调读 `payload.state.values`：按各复选框 `value=opt_{i}` 还原选项文本数组；读 `plain_text_input` 文本作为补充输入（空→None）。`payload.container.message_ts`=卡片 ts、`payload.user.id`=用户 |
| S14 | 作答-接收图片/文件（人→AI） | **支持**：作答期间累积用户在 DM 里发的图片 / 文件（`message` 事件的 `files[]`）；用 `files[].url_private_download` 带 `Authorization: Bearer <bot_token>` 下载到本地临时文件（按 `mimetype`/`filetype` 修正扩展名）；图片进回答 `[图片]`、文件进回答 `[文件]`。**DM 里的纯文字忽略**（请用卡片输入框，避免双输入源冲突；与飞书一致） |
| S15 | 提问-发送 `-f` 文件（AI→人） | **支持上传**：用新版三步流程 `files.getUploadURLExternal`（取 upload_url + file_id）→ PUT 字节 → `files.completeUploadExternal`（带 `channel_id` 分享进 DM）。图片与文件均走此流程；**按 Telegram/飞书风格原生收发，不做钉钉式「短文本内联 / 长文本转 docx」** |
| S16 | Markdown | 卡片正文用 `mrkdwn`：`is_markdown=true` 走「标准 Markdown → mrkdwn」转换（新增 `slack/markdown.rs`，仿 `telegram/markdown.rs`：粗/斜/删/码/块/引/链 + 列表 • + 表格转等宽代码块 + 仅转义 `& < >`）；`is_markdown=false` 用纯文本。普通提问题首统一携带 source / Agent / 项目，见 `docs/specs/im-request-origin.md` |
| S17 | 终态卡片 | Slack 消息无「禁用表单」能力。提交 / 被抢答 / 取消收尾时用 `chat.update` 把卡片替换为**静态终态**：标题 + 正文 + 已选项打勾回显（如 `✓ 选项`）+ 补充文字回显 + 状态行（提交→「已提交」；被抢答→「已在 X 回答」；取消→「已取消 / 已被 X 取消」），并移除复选框/输入框/按钮。本端未作答（被抢答）时不回显勾选与文字 |
| S18 | 抢答与退出 | 接入现有 Coordinator「首个终态生效，其余 `interrupt` 收尾」；Slack 被抢答 → 不投递，并 best-effort `chat.update` 卡片为「已在 X 回答」终态（失败仅日志）。退出码语义不变（0/1/3） |
| S19 | 失败兜底 | 卡片投放失败（`chat.postMessage` 报错等）→ **自动回退**「纯文本 + 编号选项」B 方案问该题（用户回一条 DM：回编号 / 文字 / 图片 / 文件），与飞书一致 |
| S20 | 测试连接 | 校验 Bot Token（`auth.test` + 向 userId 发一条测试 DM）**并**校验 App Token（`apps.connections.open` 能拿到 wss 即通过，不保持长连），成功返回提示 |
| S21 | 公共抽象复用 | 复用现有 `channels::conversation::{MessagingChannel, run_conversation}`；Slack 仅新增「传输实现 `SlackSession`」+「薄外层 `SlackChannel`」+ 客户端层 `slack/`，不改公共驱动逻辑。daemon 集成与飞书同构（`ensure_slack_router` / `attach_im_channels` / config-watch 失效 / `status` 计入 / `Detect`） |

## 4. 约束与既有规则（不可破坏）

- **stdout 洁净契约不变**：结果仍只输出 `[选择的选项]`/`[用户输入]`/`[图片]`/`[文件]`/`[状态]` 区块；Slack 答案经同一 `emit_result`/`render_result` 聚合输出。
- **现有功能契约不变**：弹窗、Telegram、钉钉、飞书、抢答、配置容错（缺字段走默认、未知字段忽略）、`--settings/--history/--help/--version`、退出码（0/1/3）保持。
- **release 构建模式**：生产构建须 `--features custom-protocol`；TLS 沿用 rustls，不引入 OpenSSL；**不新增任何 crate**。
- **配置容错**：新增 `channels.slack` 与其字段用 `#[serde(default)]`；旧配置无该字段走默认。
- **密钥安全**：两个密钥默认迁入系统钥匙串（`config.json` 留空），沿用现有 `secrets` + `SECRET_SPECS` 策略；钥匙串不可用时回退明文。
- **3 秒 ack**：Socket Mode 每条业务帧须 3 秒内回 `envelope_id` ack（**收帧即 ack**），否则平台重推。
- **集群/单连接约束**：同一 App 可建多条 Socket Mode 连接，事件**只投递给其中一条**（负载均衡）。因此 macOS、Linux 与 Windows 的所有公开入口都经 shared daemon，以**单条常热共享连接**根治多进程抢消息；不得恢复产品级逐请求连接。

### 前置条件（用户侧，需在 Slack App 后台一次性配置）

1. 在 <https://api.slack.com/apps> 创建一个 Slack App（from scratch），选择目标 Workspace。
2. **Socket Mode**：Settings → Socket Mode → 开启；在 Basic Information → App-Level Tokens 生成一个带 `connections:write` scope 的 App Token（`xapp-…`）。
3. **Bot Token Scopes**（OAuth & Permissions）：至少 `chat:write`（发消息）、`im:write`（开 DM）、`im:history`（读 DM 消息，供自动识别 / 接收文件）、`files:read`（下载用户文件）、`files:write`（上传文件）。安装 App 到 Workspace 后取 **Bot User OAuth Token（`xoxb-…`）**。
4. **Interactivity**：Interactivity & Shortcuts → 开启（Socket Mode 下无需填 Request URL）。
5. **Event Subscriptions**：开启；在「Subscribe to bot events」订阅 `message.im`（Socket Mode 下无需 Request URL）。
6. **与机器人建立 DM**：用户在 Slack 客户端打开与机器人的私聊，再点设置页「自动识别」并按提示私聊发送识别码完成 userId 回填。

## 5. 验收标准

1. 设置页可填 Bot Token / App Token / User ID 并启用 Slack；「测试连接」能通过 `auth.test`、收到一条单聊测试 DM，且 `apps.connections.open` 校验通过。
2. 「自动识别」给出 4 位数字，按提示私聊后 userId 被精确回填。
3. 启用 Slack + 弹窗后发起提问：Slack 机器人 DM 先发 Message（含 `-f` 文件，图片/文件可发），再逐题发互动卡片（标题 / 正文 / 可勾选选项 / 补充输入框 / 提交按钮）。
4. 卡片多选勾选 + 可补充文字，点「提交」完成该题；提交后卡片显示静态「已提交」终态（回显选择与文字、无控件）；多题依次进行。
5. 作答期间在 DM 发的图片/文件被收进答案；DM 里的纯文字被忽略。
6. 完成后 stdout 正确输出选项/文字/图片/文件区块；被弹窗 / Telegram / 钉钉 / 飞书抢答时 Slack 中止、不重复投递，并尽力把卡片置终态「已在 X 回答」。
7. 卡片投放失败时自动回退「纯文本 + 编号选项」仍可完成该题。
8. 无 GUI 时仅启用 Slack 也能完成整套问答（headless）；与其它会话型渠道同开时可并行抢答。
9. 弹窗 / Telegram / 钉钉 / 飞书行为与现状一致（不回归）。
10. `daemon status` 在 Slack 已建连时把 `slack` 计入 `im conns`；改配置（禁用 / 改 token / 改 userId）经 config-watch 即时失效旧连接，下个请求按新配置重连。
11. 设置页、`prompts.rs`、`README`、`docs/wiki/slack-setup.*` 反映 Slack 用法与前置条件。

## 6. 已知问题与风险（预登记）

- **【已解决】Socket Mode 多开互抢**：同一 App 多条连接在线时事件只投随机一条；shared daemon 在三平台统一独占一条常热连接并按请求路由，连续/并发提问不再各建连接。
- **复选框上限**：Slack `checkboxes` 单元素最多 10 个选项。超 10 个时**拆分为多个 `input` 复选框块**（每块 ≤10），提交时各块 `state.values` 合并；实现处注释固定。
- **section 文本长度**：Block Kit `section` 文本上限约 3000 字符；超长正文截断或拆多块（实现处处理，不崩溃）。
- **消息内 input 块取值**：依赖「点按钮的 `block_actions` 携带整条消息 `state.values`」这一行为；以真机联调确认字段路径（`state.values[block_id][action_id]`）并在 `blockkit.rs` 注释锁定。
- **文件上传新流程**：`files.getUploadURLExternal` + `files.completeUploadExternal` 三步；`completeUploadExternal` 需带 `channel_id` 才会分享进 DM；以真机联调固定字段。
- **mrkdwn 方言**：与标准 Markdown 不同；先做基础转换，渲染异常再补；表格/嵌套列表退化为等宽块/项目符号（仿 telegram）。
- **媒体大小限制**：上传/下载有上限；超限走失败提示，不崩溃。
- **配置容错**：旧配置无 `slack` 走默认（`#[serde(default)]`）。
- **release 构建**：须 `--features custom-protocol`；不新增 crate。

## 7. 反馈意见

（review 中产生的调整意见追加于此，标注日期。）

- **2026-06-07（真机联调）**：发现安装文档遗漏「启用 App Home 私聊」这一必备步骤。Slack 自 2021 起默认禁止用户主动 DM 机器人（opt-in），不开则 DM 输入框置灰、提示「Sending messages to this app has been turned off」，导致自动识别的 4 位码与作答期回传图片/文件被挡。需在 App 配置 **Features → App Home → Show Tabs** 打开 **Messages Tab** 并勾选 **Allow users to send Slash commands and messages from the messages tab**，保存后刷新/重装。仅文档修订（`docs/wiki/slack-setup.md` 与 `.en.md` 新增「启用 App Home 私聊」一节 + 故障排查行），不涉及代码逻辑变更。

- **2026-06-07（真机联调 · bug 修复）**：多题连问时，新一题卡片的输入框/复选框会回填上一题的内容。根因是 Slack 客户端按 `block_id`(+`action_id`) 缓存 `input` 块的草稿状态，而原实现各题卡片的 input 块 block_id 为固定常量（`userinput` / `opts_k`）。修法：每张卡片生成唯一 nonce（毫秒时间戳 + 进程内自增序号）拼入各 input 块 block_id，使 Slack 视为全新控件、输入恢复空白；`action_id` 保持不变，故 `parse_submit`（选项按 `opt_` 前缀 / 文本按类型 / 提交按 `submit`）与既有单测不受影响。改动：`slack/blockkit.rs`（`build_question_card` 增 `nonce` 参数）+ `channels/slack.rs`（`next_card_nonce()` 生成并传入）。已加单测 `input_block_ids_carry_nonce`，真机验证连问两题第二题卡片无残留。

- **2026-06-07（真机联调 · 卡片样式增强）**：提问/消息标题为普通文本、与中文正文区隔不明显。
  - 评估「颜色」：Slack `section` 文本不支持任意字色；唯一「颜色」是 legacy attachment 左侧彩色竖条。真机实测彩条内嵌交互 `input` 表单**技术可行**（可渲染、可交互，推翻了网上「input 不可入 attachment」的过时说法），但 attachment 布局观感差、视觉层级偏低，**经确认放弃彩条**。
  - 最终采用（顶层 blocks、无颜色）：① 提问标题改用 `header` 块（大号粗体）并加 `❓` 前缀（仅问题标题加图标，选项/补充 label 不加）；② 「消息」头（`✉️ Message from {source} · {agent} · {project}`，按存在性与去重规则裁剪）同样改用大号 `header`，并加 `✉️` 前缀（来源标题为多渠道共用文案，图标仅在 Slack 发送处追加，不影响其它渠道）。`header` 限 ≤150 字符，超长自动回退普通加粗 section。改动：`slack/blockkit.rs`（抽出 `header_block` 通用函数；`title_block` 用之；新增 `build_message_blocks`）+ `channels/slack.rs`（`send_message_prompt` 改发大号 header blocks）。已加单测 `title_rendered_as_header_with_question_icon`。

- **2026-06-07（真机联调 · 发图片/附件顺序修复）**：`-f` 附带的图片/文件出现在「问题卡片之后」，破坏「message → 附件 → question」顺序。实测根因：`files.completeUploadExternal` 约 0.5s 即返回 ok，但文件真正分享进 DM 时间线是**异步**的（图片需生成缩略图，实测 ~1.4–6.5s 后才出现 `shares`），分享消息的 ts 按真正分享时刻计；原来仅等 `MESSAGE_SETTLE_DELAY`(500ms) 就发问题卡片，导致文件排在问题之后。飞书/钉钉无此问题（用原生 image/file 消息，同步即时有序）。修法：`slack/client.rs::upload_file` 在 `completeUploadExternal` 后新增 `wait_until_shared`——轮询 `files.info` 直到 `shares` 出现目标频道再返回，超时上限 15s（best-effort，不卡死）。代价：发图时多等数秒。已加单测 `shared_detected_in_private_and_public` / `not_shared_when_empty_or_other_channel`。
