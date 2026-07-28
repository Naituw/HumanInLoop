# 需求：新增「飞书（Feishu / Lark）」通信渠道（Channel）

> 状态：待确认（review 后按计划实现）
> 关联计划：`docs/plans/feishu-channel.md`
> 关联既有：`docs/specs/dingtalk-channel.md` / `docs/specs/dingtalk-card-answers.md`（钉钉渠道总体设计与互动卡片预选答案；本需求按钉钉「完整双向交互 + 互动卡片」对齐到飞书）

## 1. 背景

`AskHuman` 现已支持三个 Channel：本地弹窗（GUI）、Telegram、钉钉（DingTalk）。其中钉钉与 Telegram 都已具备较完善的「互动卡片 + 文件收发 + 抢答」体验。本需求新增**第四个 Channel —— 飞书（Feishu / Lark）**，实现与钉钉同级的「**Agent 主动发问 → 人在飞书作答 → 结果回传**」完整双向交互。

飞书与钉钉的本质共性：要在本地（无公网）收消息与卡片回调，飞书提供与钉钉 Stream 等价的「**长连接（WebSocket）模式**」——零公网域名 / 零内网穿透即可接收事件与回调，**仅支持企业自建应用**，收到后须**3 秒内响应**。因此本需求选用「**企业自建应用 + 机器人 + 长连接模式 + 单聊**」方案，并**复用现有 `run_conversation` 公共驱动**与 `MessagingChannel` 抽象。

与钉钉的两点关键差异（影响实现）：

1. **长连接帧是 protobuf 协议（pbbp2）**：钉钉 Stream 是纯 JSON 帧（可手写解析）；飞书长连接是 protobuf 帧（`PbFrame`），Rust 无官方 SDK。本需求**新增 `prost` 运行时依赖、自定义 `PbFrame`**（不引入第三方飞书 crate、不需要 protoc / build.rs）。
2. **卡片可直接用 JSON 下发**：钉钉互动卡片高级版需先在后台搭建并发布模板（`cardTemplateId`）；飞书**消息卡片（卡片 JSON 2.0）可在发送时直接以 JSON 内容下发**，无需预先搭建模板。

## 2. 目标

用户在设置页「通信渠道」中配置飞书（AppId / AppSecret / OpenId / BaseUrl）并启用后：

```bash
AskHuman "请看看这个改动？" -f ./diff.patch -q "要继续吗？" -o "继续" -o "停止"
```

- 飞书机器人**主动私聊**用户：先发共享 Message（含 `-f` 文件），再逐题发**互动卡片**（卡片含标题、问题正文、可勾选的预定义选项【复选框/勾选器，平铺直接勾】、补充文字输入框、「提交」按钮）。
- 用户在飞书卡片内**勾选选项（多选）**、可**补充文字**、作答期间还可在聊天里**发图片/文件**，点「提交」完成该题。
- 多题逐条进行；全部完成后结果回传到 stdout（与弹窗/Telegram/钉钉同一契约）。
- 与弹窗 / Telegram / 钉钉 **并行抢答**：任一渠道率先完成即采纳，其余收尾。
- 无 GUI 时，飞书可作为 headless 渠道单独或与其它会话型渠道并行工作。

## 3. 已确认决策

> 决策来自需求澄清（2026-06-06，经 `AskHuman` 逐项确认）。

| 编号 | 决策项 | 结论 |
|---|---|---|
| F1 | 渠道能力 | **完整双向交互**（与钉钉对齐）：可发可收，在飞书内完成作答。范围含：互动卡片（多选 + 补充文字 + 提交）/ AI→人文件发送 / 人→AI图片文件接收 / 与弹窗·钉钉·TG 抢答 / headless 无 GUI / 多问题逐题 / 设置页（测试连接 + 自动识别）/ 中英 i18n / 卡片投放失败回退纯文本编号 / `prompts.rs`+`README` 文档 |
| F2 | 接入形态 | **企业自建应用 + 机器人 + 长连接（WebSocket）模式**（零公网/零域名/零内网穿透）。用户自建内部应用拿 AppId/AppSecret、开启机器人能力，并在开发者后台把「事件订阅」与「回调订阅」都设为**使用长连接接收**（见 §4 前置条件） |
| F3 | 会话场景 | **单聊**（人与机器人私聊，`chat_type="p2p"`），无需 @ |
| F4 | 收消息机制 | **长连接（WebSocket）**：`POST {baseUrl}/callback/ws/endpoint` 拿 wss 地址 + client_config → 连 wss；帧为 protobuf（pbbp2）。订阅由开发者后台配置（事件 `im.message.receive_v1` + 回调 `card.action.trigger`），**WS 建连本身不在请求里声明 topic**（与钉钉按 topic 订阅不同） |
| F5 | 帧协议实现 | **新增 `prost` 运行时依赖，自定义 `PbFrame`**（约 20–30 行，`#[derive(prost::Message)]`，无需 .proto/protoc/build.rs）。不引入第三方飞书 crate（体积/构建/维护/供应链考量，且与本项目手写钉钉 Stream 的风格一致） |
| F6 | 鉴权 | `POST {baseUrl}/open-apis/auth/v3/tenant_access_token/internal {app_id, app_secret}` → `tenant_access_token`（有效期约 7200s）。进程内缓存按 `baseUrl + appId + AppSecret SHA-256 指纹` 隔离并在过期前刷新；配置变更清除旧凭据缓存。所有 OpenAPI 调用 header 携带 `Authorization: Bearer <tenant_access_token>`；若返回明确的 tenant token 无效码，条件删除本次被拒 token、强制取新 token 并最多重试一次，刷新后仍失败则正常上报。 |
| F7 | 发送共享 Message | `POST {baseUrl}/open-apis/im/v1/messages?receive_id_type=open_id`，body `{receive_id: openId, msg_type, content(JSON 字符串)}`。文本用 `msg_type=text`（`content={"text":..}`）；Markdown 走互动卡片或文本（见 §3 F12） |
| F8 | 发送题目（互动卡片） | 卡片以 `msg_type=interactive` 直接发**卡片 JSON 2.0**（无需模板）。卡片含：标题 header + 正文（markdown/plain_text 组件）+ **表单容器**（内嵌每个选项一个 `checker` 勾选器 + 一个输入框 `input` + 一个 `form_action_type="submit"` 的提交按钮）。回调走长连接 `card.action.trigger` |
| F9 | 选项形态 | 预定义选项用**复选框/勾选器组（`checker`，平铺直接勾）**，置于表单容器内；提交时一次性回传所有勾选状态（`form_value`）。无预定义选项时省略选项区，仅留输入框 + 提交 |
| F10 | 收消息（长连接） | WS 收两类业务帧：① 事件 `im.message.receive_v1`（用户文字【作答期忽略】/图片/文件 / 自动识别码 / open_id 识别）；② 卡片回调 `card.action.trigger`（表单提交）。每条业务帧须 **3 秒内回包**（响应帧）：事件回空 ACK；卡片回调回 `{toast, card}`（更新卡片 + 轻提示）。最终提交由会话给出响应体、Router 写回；会话须等 Router 完成 WebSocket 写入尝试后才返回答案，避免单进程随答案完成而提前退出。控制帧 ping/pong（`method=0`）维持心跳；大消息按 `message_id`/`sum`/`seq` header 分片重组；断线重连（重新取 endpoint） |
| F11 | 每题完成方式 | 卡片上点「提交」即完成该题（勾选 + 补充文字一并回传）。回调 `event.action.form_value` 含各 `checker` 的勾选布尔与 `input` 文本；按勾选映射回选项文本，输入框文本作为补充输入 |
| F12 | Markdown | 卡片正文：`is_markdown=true` 用 `markdown` 组件（飞书 lark_md 子集，尽量原样传递、不做重转义）；`is_markdown=false` 用 `plain_text`。共享 Message 文本走 `msg_type=text`（飞书富文本不强求）。普通提问题首统一携带 source / Agent / 项目，见 `docs/specs/im-request-origin.md` |
| F13 | 作答-接收图片/文件（人→AI） | **支持**：作答期间累积用户在聊天里发的图片（`msg_type=image`）、文件（`msg_type=file`）；按 `message_id` + `file_key` 调 `GET {baseUrl}/open-apis/im/v1/messages/{message_id}/resources/{file_key}?type=image|file` 下载到本地临时文件（按真实类型修正扩展名）；图片进回答 `[图片]`、文件进回答 `[文件]`。**聊天里的纯文字忽略**（请用卡片输入框，避免双输入源冲突；与钉钉 DC8 一致） |
| F14 | 提问-发送 `-f` 文件（AI→人） | **支持上传**：图片先 `POST {baseUrl}/open-apis/im/v1/images`（multipart，`image_type=message`）拿 `image_key`，以 `msg_type=image` 发；其它文件先 `POST {baseUrl}/open-apis/im/v1/files`（multipart，`file_type` 取合适值/`stream`，带 `file_name`）拿 `file_key`，以 `msg_type=file` 发。**按 Telegram 风格直接原生收发，不做钉钉式「短文本内联 / 长文本转 docx」**（如后续飞书端文本预览有问题再议） |
| F15 | 接收人标识 + 自动识别 | 配置项 `openId`（用户 Open ID，稳定标识，发消息用 `receive_id_type=open_id`）。旁置「自动识别」：点击后程序随机生成 4 位数字提示「请私聊机器人发送：XXXX」，经长连接捕获 `content==XXXX` 的单聊消息，取 `event.sender.sender_id.open_id` 回填（带 ~120s 超时）。**前置校验**：AppId/AppSecret 为空或换 token 失败 → 立即中文报错、不进入识别 |
| F16 | 测试连接 | 校验 AppId/AppSecret 能换 token，并给配置的 openId **单聊发一条测试消息**，成功返回提示 |
| F17 | 抢答与退出 | 接入现有 Coordinator「首个终态生效，其余 `cancel_by_other` 收尾」；飞书被抢答 → 关闭长连接、不投递，并 **best-effort** 把当前卡片更新为「已在 X 回答」终态（`PATCH {baseUrl}/open-apis/im/v1/messages/{message_id}`，失败仅日志）。退出码语义不变（0/1/3） |
| F18 | 失败兜底 | 所有卡片创建携带飞书消息 `uuid` 幂等键；网络错误、响应无法解析、HTTP 408/425/5xx 或明确 `Internal Error` 时，复用同一 `uuid` 短退避后重试一次。仍失败或错误不可重试 → **自动回退**「纯文本 + 编号选项」B 方案问该题（用户回一条消息：回编号 / 文字 / 图片 / 文件），与钉钉一致；权限、参数、鉴权、频控错误不盲目重试 |
| F19 | 服务域名 | **新增 `baseUrl` 配置**（默认 `https://open.feishu.cn`），以同时支持 Lark 国际版（`https://open.larksuite.com`）。token/消息/上传/下载/卡片更新走 `{baseUrl}/open-apis/...`，长连接 endpoint 走 `{baseUrl}/callback/ws/endpoint` |
| F20 | 公共抽象复用 | 复用现有 `channels::conversation::{MessagingChannel, run_conversation}`（钉钉渠道引入时已抽象）；飞书仅新增「传输实现 `FeishuSession`」+「薄外层 `FeishuChannel`」，不改动公共驱动逻辑 |
| F21 | 文档同步 | 设置页 UI、`prompts.rs`、`README` 同步飞书配置与使用，并写明**前置条件**（自建应用、机器人、把事件/回调订阅设为长连接、所需权限） |
| F22 | 发送故障可观测性 | OpenAPI 错误日志保留业务 `code`、HTTP 状态与响应头 `X-Tt-Logid`（兼容 `X-Request-Id`）；一次重试即恢复时仅记 daemon 日志，不弹用户警告、不改变「下一次任意成功操作即清除渠道健康故障」的既有语义 |

## 4. 约束与既有规则（不可破坏）

- **stdout 洁净契约不变**：结果仍只输出 `[选择的选项]`/`[用户输入]`/`[图片]`/`[文件]`/`[状态]` 区块；飞书答案经同一 `emit_result` 聚合输出。
- **现有功能契约不变**：弹窗、Telegram、钉钉、抢答、配置容错（缺字段走默认、未知字段忽略）、`--settings/--help/--version`、退出码（0/1/3）保持。
- **release 构建模式**：生产构建须 `--features custom-protocol`；TLS 沿用 rustls，不引入 OpenSSL；新增 `prost` 仅运行时（不引入 `prost-build`/protoc）。
- **配置容错**：新增 `channels.feishu` 与其字段用 `#[serde(default)]`；旧配置无该字段走默认。
- **3 秒响应**：长连接每条业务帧须 3 秒内回包，否则平台重推（沿用钉钉卡片回调的延迟 ACK + respond 模式思路）。
- **集群/单连接约束**：同一应用最多 50 个连接；推送为「集群模式」——同一应用多个 client 同时在线时，事件只随机投给其中一个 client。即「多开的 `AskHuman` 进程会相互抢消息」，与钉钉「同一 client-id 同一时刻一条 Stream」类似（见 §6 已知问题）。

### 前置条件（用户侧，需在飞书开发者后台一次性配置）

1. 创建**企业自建应用**，开通**机器人**能力；拿到 AppId（`cli_...`）、AppSecret。
2. 开通所需权限：发送/获取单聊消息（`im:message`）、获取与上传图片或文件资源（`im:resource`）、更新消息（`im:message:update`）、获取用户 user ID（可选，仅在需要 user_id 时；本方案用 open_id 不强制）。
3. 把**事件订阅**方式设为「使用长连接接收事件」，订阅 `im.message.receive_v1`；把**回调订阅**方式设为「使用长连接接收回调」，订阅 `card.action.trigger`。
   - 注意：飞书要求**保存长连接订阅方式时，本地必须有一条该应用的长连接处于在线**。可在设置页点「自动识别」（其会建立并保持长连接约 120s）期间到后台保存订阅方式，再私聊机器人发送识别码。
   - 「消息卡片回传交互（旧）`card.action.trigger_v1`」**不支持**长连接，必须用**新版** `card.action.trigger`。

## 5. 验收标准

1. 设置页可填 AppId/AppSecret/OpenId/BaseUrl 并启用飞书；「测试连接」能换到 token 且收到一条单聊测试消息。
2. 「自动识别」给出 4 位数字，按提示私聊后 openId 被精确回填（且建连期间可用于后台保存长连接订阅）。
3. 启用飞书 + 弹窗后发起提问：飞书机器人私聊先发 Message（含 `-f` 文件，图片/文件可发），再逐题发互动卡片（标题 / 正文 / 可勾选选项 / 补充输入框 / 提交按钮）。
4. 卡片多选勾选 + 可补充文字，点「提交」完成该题；提交后卡片显示「已提交」终态、无报错 toast；多题依次进行。
5. 作答期间在聊天发的图片/文件被收进答案；聊天里的纯文字被忽略。
6. 完成后 stdout 正确输出选项/文字/图片/文件区块；被弹窗 / Telegram / 钉钉抢答时飞书中止、不重复投递，并尽力把卡片置终态。
7. 卡片投放失败时自动回退「纯文本 + 编号选项」仍可完成该题。
8. 无 GUI 时仅启用飞书也能完成整套问答（headless）；与钉钉/Telegram 同开时可并行抢答。
9. 弹窗 / Telegram / 钉钉行为与现状一致（不回归）。
10. 设置页、`prompts.rs`、`README` 反映飞书用法与前置条件。
11. 飞书在 token 名义有效期内提前拒绝旧 token 时，JSON 请求、媒体上传和资源下载均自动刷新并最多重试一次；并发旧请求不得清除其它请求已刷新的 token。
12. 所有卡片创建带唯一 `uuid`；瞬时失败时复用同一值重试一次且不重复投递，非瞬时失败不重试；最终错误日志包含飞书业务码、HTTP 状态与可用的请求日志 ID。

## 6. 已知问题与风险（预登记）

- **【已知问题 · 拟暂不修】长连接「集群模式」多开互抢**：同一应用多 client 在线时事件只投随机一个 client；当前每次 `AskHuman` 进程各开一条长连接，**连续/并发提问可能相互干扰**（与钉钉单 Stream 问题同源）。拟沿用钉钉处置：**仅记录，暂不修**（未来修复路线同钉钉：文件锁串行化 / 常驻 daemon）。
- **长连接 protobuf 帧**：需保证帧编解码、ping/pong 心跳、分片重组、断线重连、3 秒回包正确；参考钉钉 `stream.rs` 形态 + 飞书 `pbbp2` 帧结构落地。
- **后台订阅保存的「鸡生蛋」**：保存长连接订阅方式时需要在线连接——通过设置页「自动识别」保持连接窗口完成；文档需写清顺序。
- **卡片更新凭证/时限**：被抢答时用 `PATCH /im/v1/messages/{message_id}` 更新卡片（应用自身发送的卡片，14 天内可更新）；提交时优先在 `card.action.trigger` 回包里直接返回更新后的卡片（3 秒窗口内）。

## 7. 反馈意见

（review 中产生的调整意见追加于此，标注日期。）

- **2026-06-06｜修复：卡片回调收不到（点提交转圈回弹）**：实测飞书卡片回调 `card.action.trigger` 经长连接投递时，帧头 `type` 为 `event`（非 `card`）。原实现按帧 `type` 路由，把它当普通事件丢弃。改为**以回包内 `header.event_type` 为准**路由（兼容 `type=event`/`card`），并新增环境变量 `HUMANINLOOP_FEISHU_DEBUG=1` 时写 `~/.humaninloop/feishu-debug.log` 的诊断日志（默认关闭）。
- **2026-06-06｜终态卡片改为「钉钉模式」**：原终态（类 Telegram）把整张卡片换成「正文 + 一行 ✅ 已提交」，丢弃选项与按钮。改为**复刻钉钉**：同一表单结构下，勾选器 `disabled` 且按用户选择 `checked`、输入框 `default_value` 回显补充文字且 `disabled`、提交按钮 `disabled` 并改文案（提交→「已提交」；被抢答→「已在 {渠道} 回答」且勾选器不勾）。选中项仅禁用并保留高亮，不加删除线。
- **2026-07-15｜修复：Windows 最终提交显示「未响应」**：Windows 单进程回退路径可能在会话把最终答案交给 Coordinator 后立即退出；原 oneshot 只保证响应体送到 Router，不保证 Router 已把响应帧写入 WebSocket，形成退出竞态。卡片 ACK 改为「响应体 + 写回完成」两阶段握手，最终提交等待 Router 完成写入尝试后再返回答案；中间卡片操作仍只提交响应体，不增加等待。
- **2026-07-17｜修复：tenant token 提前失效后需重启 daemon**：缓存键从单一 `app_id` 改为服务域名、App ID 与 Secret 不可逆指纹，并在配置变化时清除旧条目。OpenAPI 错误保留数值 `code`；JSON 请求、multipart 上传和资源下载遇到 tenant token 无效码时，只有缓存仍等于本次被拒 token 才删除并刷新，随后最多重试一次，避免并发旧请求误删新 token 或形成无限重试。
- **2026-07-25｜终态卡的补充文字改为可选中**：`input` 组件一旦 `disabled`，在飞书客户端里既不能编辑也**不能选中复制**，而人常需要回头引用自己写过的话。终态遂把补充文字改为「灰色小标题（`channel.fsNoteLabel`）+ markdown 引用块」；无补充时整段不渲染（不再留空输入框）。勾选器与置灰按钮维持「钉钉模式」不变；交互态仍是 `input`。
- **2026-07-28｜修复：飞书 `Internal Error` 直接降级纯文本**：所有 `send_card` 创建请求加入飞书原生 `uuid` 幂等键；网络错误、畸形响应、HTTP 408/425/5xx 或明确的 `Internal Error` 短退避后复用同一 UUID 重试一次，仍失败才执行 F18 文本回退。权限、参数、鉴权和频控错误不重试。错误显示同时保留业务码、HTTP 状态及 `X-Tt-Logid` / `X-Request-Id`，恢复成功只写 daemon 日志。
