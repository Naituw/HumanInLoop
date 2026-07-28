# 开发计划：新增「飞书（Feishu / Lark）」通信渠道（Channel）

> 关联需求：`docs/specs/feishu-channel.md`
> 关联既有：`docs/plans/dingtalk-channel.md` / `docs/plans/dingtalk-card-answers.md`（钉钉渠道与互动卡片；飞书按其形态对齐，复用 `run_conversation` 公共驱动）
> 计划描述方案与技术/规则细节，具体代码以实现为准。

## 0. 方案总览

```
配置(设置页) ──► AppConfig.channels.feishu { enabled, appId, appSecret, openId, baseUrl }
                                   │
AskHuman "..." -q ... -o ...       ▼
   └─ run_ask 决策渠道：弹窗(若GUI) + 全部 active 会话型渠道(telegram/dingding/feishu)
        └─ 各会话型渠道 = 复用「公共驱动 run_conversation」+ 各自「MessagingChannel 实现」
             ├─ Telegram：长轮询
             ├─ 钉钉：DingTalk Stream(JSON 帧) + OpenAPI
             └─ 飞书：Feishu 长连接(protobuf 帧) 收 + OpenAPI 发
                  发：im/v1/messages(text 文本 · interactive 互动卡片 JSON · image/file 附件)
                      im/v1/images·im/v1/files(上传 -f 文件) · PATCH messages(收尾更新卡片)
                  收：长连接两类业务帧 —— 事件 im.message.receive_v1(文字忽略/图片/文件/识别码)
                       + 卡片回调 card.action.trigger(表单提交) ；resources 下载图片/文件
        └─ Coordinator 抢答：首个终态生效 → emit_result → 退出
```

核心两件事：①**新增飞书渠道**（长连接 protobuf 收 + OpenAPI 发 + JSON 互动卡片）；②**复用既有公共抽象**（`MessagingChannel` + `run_conversation`，钉钉引入时已就位，无需再抽象），编排泛化扩展到第三个会话型渠道。

---

## 1. 公共抽象复用（无新增抽象）

`channels/conversation.rs` 的 `MessagingChannel`（`id/open/send_message_prompt/ask_question/close`）与 `run_conversation`（单/多题编排、抢答返回 None、完成投递）**保持不变**。飞书仅：

- 新增传输实现 `FeishuSession`（实现 `MessagingChannel`，持 client + 长连接 + 跨题状态）。
- 新增薄外层 `FeishuChannel`（实现 `Channel`：`start` → spawn → `open` → `run_conversation`；`cancel_by_other` → `Preemption::cancel`），与 `TelegramChannel`/`DingTalkChannel` 同构。

> `QuestionCtx`（header/text/options/is_markdown/index/total/lang）与现状一致，飞书直接消费。

---

## 2. 飞书客户端层 `feishu/`（HTTP/OpenAPI + 长连接）

新增模块目录 `src-tauri/src/feishu/`，模块边界与钉钉 `dingtalk/` 对齐。

### 2.1 `feishu/mod.rs`：错误类型 + 模块声明
- `FeishuError`（`EmptyConfig(field)` / `Api(msg)` / `Network(msg)` / `BadResponse`），源语言英文 `Display` + `localized(lang)`（校验类本地化、技术细节英文），与 `DingTalkError` 同构。
- 子模块：`token` / `client` / `ws` / `card`。
- 备注：robot 发送统一用 `tenant_access_token`，无需单独 robotCode。

### 2.2 `feishu/token.rs`：tenant_access_token 缓存
- `get_token(http, base_url, app_id, app_secret) -> Result<String>`：`POST {base}/open-apis/auth/v3/tenant_access_token/internal {app_id, app_secret}` → `tenant_access_token` + `expire`（秒）。
- 进程内缓存（`OnceLock<Mutex<HashMap<app_id, (token, expire_at)>>>`，过期前留 60s 余量），形态照搬 `dingtalk/token.rs`。

### 2.3 `feishu/client.rs`：`FeishuClient`（reqwest）
- `new(config) -> Result<Self, FeishuError>`：校验非空（appId/appSecret/openId）；构造 `reqwest::Client`；持 `base_url`（去尾斜杠）。
- `http()` 暴露给 ws 复用连接池；`token()` 走 `token::get_token`。
- 统一调用助手 `call(method, path, body) -> Value`：`{base}/open-apis{path}`，header `Authorization: Bearer <token>`，按 `code==0`（飞书业务码）判定成功，失败取 `msg`/`code`。
- **发送单聊消息**：`send_message(msg_type, content_json_string) -> message_id`：
  - `POST /im/v1/messages?receive_id_type=open_id`，body `{receive_id: openId, msg_type, content}`（`content` 为 JSON 字符串）；返回 `data.message_id`。
  - 所有卡片创建额外携带 36 字符 `uuid`。瞬时故障（网络、畸形响应、HTTP 408/425/5xx、明确 `Internal Error`）复用同一 UUID 短退避后重试一次；频控、权限、参数和鉴权错误不重试，避免无效流量。业务错误保留 `code`、HTTP 状态与 `X-Tt-Logid` / `X-Request-Id` 供 daemon 日志排障。
  - 便捷封装：`send_text(text)`（`content={"text":..}`）、`send_interactive(card_json) `（`content=<card JSON 字符串>`，`msg_type=interactive`）、`send_image(image_key)`、`send_file(file_key)`。
- **上传媒体**：
  - `upload_image(path) -> image_key`：`POST /im/v1/images`，multipart：`image_type=message` + `image=<bytes,filename>`；取 `data.image_key`。
  - `upload_file(path, name) -> file_key`：`POST /im/v1/files`，multipart：`file_type`（按扩展名映射，未知用 `stream`）+ `file_name` + `file=<bytes>`；取 `data.file_key`。
- **下载用户消息资源**：`download_resource_to(message_id, file_key, kind: image|file, ext) -> 本地路径`：`GET /im/v1/messages/{message_id}/resources/{file_key}?type={kind}` → 字节落地临时目录（`askhuman-feishu/`），按真实类型修正扩展名（默认 `.file` → 实类型）。
- **更新卡片（收尾用）**：`patch_card(message_id, card_json) -> Result<()>`：`PATCH /im/v1/messages/{message_id}`，body `{content: <card JSON 字符串>}`。被抢答/断连时 best-effort 置终态。

### 2.4 `feishu/ws.rs`：长连接（protobuf 帧，钉钉 `stream.rs` 的飞书版）

职责与 `dingtalk/stream.rs` 对齐：取 endpoint → 连 wss → 帧循环（ping/pong + 分片重组 + ACK/回包 + 重连）→ 向上层抛业务事件。

- **PbFrame（自定义 prost 结构，§7 依赖）**：
  ```text
  PbFrame { seq_id:u64(1), log_id:u64(2), service:i32(3), method:i32(4),
            headers: Vec<PbHeader{key,value}>(5),
            payload_encoding:Option<String>(6), payload_type:Option<String>(7),
            payload:Option<Vec<u8>>(8), log_id_new:Option<String>(9) }
  ```
  - `method=0` = 控制帧（ping/pong）；`method=1` = 数据帧（payload 为 JSON 字符串 = LarkEvent）。
  - header 关键键：`type`（`ping`/`pong`/事件类型）、`message_id`、`sum`（分片总数）、`seq`（分片序号）。
- **取 endpoint**：`POST {base}/callback/ws/endpoint`（body 携带 app_id/app_secret，handshake 字段对齐官方 SDK；返回 wss `URL` + `ClientConfig{ ReconnectCount, ReconnectInterval, PingInterval, ... }`）。解析出 wss 地址与 `ping_interval`。
- **连接**：`tokio-tungstenite` 连 wss（已有依赖，rustls）。建连后立即发一个 ping 帧（与官方 SDK 一致）以校准 pong 的 `ping_interval`。
- **帧循环 `FeishuWs`**：
  - `recv() -> Option<WsEvent>`：循环读 `Message::Binary` → `PbFrame::decode`：
    - `method=0` 且 `type=pong`：解析 payload 里的 client_config 校准心跳；继续。
    - `method=1`（数据帧）：按 `message_id`/`sum`/`seq` 分片重组得到完整 payload（JSON）→ 解析 LarkEvent（`header.event_type` 或 schema 2.0 的 `header.event_type`/`event_type`）：
      - `im.message.receive_v1` → `WsEvent::Message(event_json)`（**自动回包空 ACK**，3 秒内）。
      - `card.action.trigger` → `WsEvent::CardAction { data, frame_headers }`（**延迟回包**：由上层算出 `{toast, card}` 后交给 Router，须 3 秒内写回）。
      - 其它事件 → 自动空 ACK，忽略。
  - `respond(frame_headers, body_json)`：回一个 `PbFrame`（同 `message_id`，`method=1`，payload = `body_json` 字符串）作为该数据帧的响应（飞书长连接「回包即响应」语义）。空 ACK = `respond(.., {})`。Router 在 `send().await` 完成后触发写回完成信号，供会导致进程结束的最终提交等待。
  - 定时（按 `ping_interval`）发 ping 帧；读错/Close/超时 → 重连（重新取 endpoint + 连接，最多若干次），重连失败 `recv()` 返回 `None`。
- 对上层暴露：`connect(http, base_url, app_id, app_secret)`、`recv()`、`respond(...)`。
- **去重**：同一 `message_id` 重复推送按已处理集合去重（避免回包慢导致重推时重复累积）。

> 与钉钉差异：① 帧是 protobuf 而非 JSON；② 不在建连请求里声明 topic（订阅由后台配置）；③ 数据帧响应直接回 payload（无需钉钉那层 `{"response": data}` 包裹），具体回包结构以真机联调为准并在实现处注释固定。

### 2.5 `feishu/card.rs`：卡片 JSON 2.0 组装 + 回调解析

- `build_question_card(header, text, options, is_markdown) -> Value`（卡片 JSON 2.0）：
  - `schema:"2.0"`；`header.title`（plain_text，取 header；空则省略或用兜底标题）。
  - body 元素：正文（`is_markdown` ? `markdown` 组件 : `plain_text` 组件，内容为 text）。
  - **表单容器**（`tag:"form"`, `name:"answer_form"`）内：
    - 每个选项一个 `checker`（`tag:"checker"`, `name:"opt_{i}"`, `checked:false`, 文本取选项原文）。无选项则省略。
    - 一个输入框 `input`（`tag:"input"`, `name:"user_input"`, placeholder 提示「补充说明（可选）」）。
    - 一个提交按钮 `button`（`form_action_type:"submit"`, `name:"submit"`, 文本「提交」, behaviors 含 callback 回传 `value:{action:"submit"}`）。
  - 选项下标 → checker name 的映射 `opt_{i}`，供回调里还原选了哪些选项（避免选项文案过长/重复问题）。
- `build_finalized_card(header, text, options, selected, status) -> Value`：终态卡片（去表单/禁用 + 追加状态行「已提交」/「已在 X 回答」），用于提交回包与被抢答 patch。
- `parse_card_submit(event) -> Option<CardSubmit>`：
  - 取 `event.operator.open_id`（=用户）与 `event.context.open_message_id`（=卡片消息 id）。
  - 取 `event.action`：要求 `form_value` 存在（表单提交）；从 `form_value` 读各 `opt_{i}` 的布尔（true 即选中）→ 还原选项文本数组；读 `user_input` 文本（空→None）。
  - `CardSubmit { open_id, message_id(open_message_id), selected_options, user_input }`。
  - 解析需健壮：缺字段不 panic，返回 None（非本类/非提交由会话层空 ACK 跳过）。
- 纯函数单测：卡片组装（含/不含选项、md/非 md）、`parse_card_submit`（勾选还原、空输入、缺字段）。

---

## 3. 飞书会话渠道 `channels/feishu.rs`

### 3.1 外层 `FeishuChannel`（实现 `Channel`）
- 持 `FeishuChannelConfig + Arc<Preemption>`；`id()="feishu"`。
- `start`：spawn task → `FeishuSession::new(config)` → `open()`（失败 i18n 警告并跳过）→ `run_conversation`。
- `cancel_by_other(winner)` → `preempt.cancel(winner)`。

### 3.2 `FeishuSession`（实现 `MessagingChannel`）
- 持有：`FeishuClient`、`FeishuWs`（`open` 时建连）、`base_url`/`open_id`。
- `open()`：构造 client（校验三项非空）+ 取 token（隐式）+ 建长连接。失败 → `Err`（外层警告跳过）。
- `send_message_prompt(message, is_markdown, source, lang)`：
  - 头部「`「Message from {source}」`」+ 文本：`send_text`（或 markdown 走卡片/文本，沿用钉钉以文本为主的形态）。
  - `-f` 文件：图片 `upload_image`→`send_image`；其它 `upload_file`→`send_file`；失败 → i18n 警告 + 发一条含文件名的失败提示文本（不中断）。**不做钉钉式 inline/docx**（F14）。
  - 适当 settle 延迟（沿用钉钉 `MESSAGE_SETTLE_DELAY` 思路，保证「先 Message 后题目」的视觉顺序）。
- `ask_question(ctx, preempt)`：**卡片流程**
  1. `card = card::build_question_card(ctx.header|兜底, ctx.text, ctx.options, ctx.is_markdown)`；`message_id = client.send_interactive(card)`。
     - 可重试的瞬时错误先在 client 内以同一 `uuid` 重试一次；恢复成功仅记日志。
     - 不可重试或重试仍失败 → i18n 警告 → **回退 B 方案** `ask_question_text(...)`（纯文本编号选项，见 3.3）。
  2. 事件循环（每 `POLL_INTERVAL`≈1s 检查 `preempt.is_cancelled()`，`recv()` 加 timeout 以便分片检查抢答）：
     - `WsEvent::CardAction`：`parse_card_submit` 命中且 `open_message_id==message_id`、`open_id==配置 openId` →
       - 组装 `QuestionAnswer { selected_options, user_input, images(累积), files(累积) }`；
       - 把 `{toast:{type:"success", content:已提交}, card:{type:"raw"/"card_json", data: build_finalized_card(..,已提交)}}` 交给 Router，并等待其完成 WebSocket 写入尝试（3 秒内，避免报错 toast、并把卡片置终态）；
       - 写回完成屏障释放后返回 `Some(answer)`。
       - 否则（非本卡片/非提交/解析失败）→ `ws.respond(headers, {})` 空 ACK，继续。
     - `WsEvent::Message`：`sender.open_id==配置 openId` 且 `chat_type=="p2p"` →
       - `image` → 下载累积进 `images`（转 base64 `ImageAttachment`，沿用钉钉 `download_image` 形态）。
       - `file` → 下载落地累积进 `files`。
       - `text`/其它 → **忽略**（F13）。
     - `recv()` 返回 None（断连耗尽重连）→ 跳出收尾。
  3. `preempt.is_cancelled()` → best-effort `client.patch_card(message_id, build_finalized_card(.., 已在X回答))`（失败仅日志）→ 返回 `None`。
- `close()`：断开长连接（`self.ws = None`）。
- 累积状态（images/files）为**单题局部**变量，随每次 `ask_question` 重置（多题互不串味）。

### 3.3 回退 B 方案 `ask_question_text`（卡片失败时）
- 与钉钉同形：发「头部 + 正文 + 编号选项 + 作答提示」文本（markdown/纯文本），用户回一条消息完成该题。
- 复用解析：`parse_reply`（纯编号→映射选项，否则自由文本）、`message_to_answer`（text/image/file → 回答）。
- 这些纯文本辅助函数与钉钉 `channels/dingding.rs` 内的同名逻辑高度一致，可在飞书文件内各自实现（不强求跨渠道再抽公共函数；若实现期发现重复度高可顺手抽到 `conversation.rs` 的可选 helper，但非本计划要求）。

### 3.4 事件字段解析（关键）
- `im.message.receive_v1`：`event.sender.sender_id.open_id`、`event.message.{message_id, chat_type, message_type, content(JSON 字符串)}`；
  - `message_type=text`：`content={"text":..}`。
  - `message_type=image`：`content={"image_key":..}`（下载用 `message_id` + `image_key`，`type=image`）。
  - `message_type=file`：`content={"file_key":.., "file_name":..}`（下载用 `message_id` + `file_key`，`type=file`，扩展名取 file_name）。
- `card.action.trigger`：见 §2.5 `parse_card_submit`。

---

## 4. 配置、命令与设置页 UI

### 4.1 配置 `config.rs` / 类型 `types.ts`
- 新增 `FeishuChannelConfig { enabled:bool, app_id:String, app_secret:String, open_id:String, base_url:String }`（serde camelCase + `#[serde(default)]`）。
  - `Default`：`enabled=false`，三项空串，`base_url="https://open.feishu.cn"`。
- `ChannelsConfig` 增 `feishu: FeishuChannelConfig`。
- `config.rs` 单测补充飞书默认值 + 旧 JSON 无 `feishu` 仍可加载。
- TS `types.ts` 同步 `FeishuChannelConfig` + `ChannelsConfig.feishu` + 命令参数类型（见 4.2）。

### 4.2 命令 `commands.rs`（+ `app/mod.rs` 注册 + `ipc.ts`）
- `feishu_test(args{appId,appSecret,openId}) -> Result<String,String>`：openId 空给中文提示；换 token（校验）+ `send_text` 一条测试消息到 openId；成功返回提示。
- `feishu_detect_prepare(args{appId,appSecret,baseUrl}) -> Result<String,String>`：前置校验（appId/appSecret 空 → 中文错误；换 token 失败 → 错误）；通过返回随机 4 位识别码。
- `feishu_detect_wait(args{appId,appSecret,baseUrl,code}) -> Result<String,String>`：建长连接，等到 `im.message.receive_v1` 且 `content.text==code` 的单聊消息，返回其 `sender.open_id`；~120s 超时报错。
  - 实现期与钉钉 `dingtalk_detect_wait` 对齐；`WsEvent` 的 `CardAction` 分支直接空 ACK 跳过。
  - 兼具「保持长连接在线」副作用，便于用户此期间在后台保存长连接订阅方式（见 spec §4 前置条件）。
- 全部 `async` 命令；在 `app/mod.rs` 的 `invoke_handler` 注册；`ipc.ts` 增对应封装。

### 4.3 设置页 `SettingsView.vue`（「通信渠道」tab）
- 仿钉钉卡片新增「飞书」卡片：`enabled` 开关 + 字段 AppId / AppSecret(password) / OpenId（旁置「自动识别」按钮，沿用钉钉 detect 两段式：prepare 取码 → 展示「请私聊机器人发送：XXXX」→ wait 回填）/ BaseUrl（默认 `https://open.feishu.cn`，占位提示可填 Lark 国际版）。
- 「测试连接」按钮 → `feishu_test`，复用现有 `result ok/err` 展示。
- 复用现有 detect/test 的 loading/错误状态写法（`feishuTesting`/`feishuDetecting`/`feishuDetectCode`/`feishuMessage`/`feishuError`）。
- 当前渠道设置中，飞书作为第一个外部 IM 渠道展示，并带“推荐”标记。

---

## 5. 运行编排泛化（`app/mod.rs`）

- 新增 `is_feishu_active(config)`：`enabled` 且 `FeishuClient::new(&config.channels.feishu).is_ok()`（三项非空）。
- `has_active_messaging` 增加 `|| is_feishu_active`。
- `active_messaging_channels`：active 时 push `FeishuChannel::new(config.channels.feishu.clone())`。
- `run_headless`：`messaging_count` 增加 `is_feishu_active as usize`；新增飞书分支（与钉钉同构：`FeishuSession::open` 成功后 `run_conversation`）。
- `coordinator.rs::display_name`：增加 `"feishu" => channel.sourceFeishu`。
- 退出码 / 抢答语义不变。

---

## 6. i18n

- Rust `i18n.rs` 新增键（中/英，沿用 `channel.*`/`cmd.*`/`err.*`/`app.*` 命名）：
  - `channel.sourceFeishu`（「飞书」/「Feishu」）。
  - `channel.fsConfigInvalidSkip` / `fsMessageSendFailed` / `fsFileSendFailedLog` / `fsQuestionSendFailed` / `fsCardDeliverFailed` / `fsImageDownloadFailed` / `fsFileDownloadFailed` / `fsSubmitted` / `fsAnsweredVia` / `fsTitleFallback` / `fsHintFree` / `fsHintOptions`（多数可直接借鉴钉钉 `dd*` 文案）。
  - `app.feishuInvalid`（headless 警告）。
  - `cmd.fsTestRemote` / `cmd.fsTestSent` / `cmd.fillAppIdSecret` / `cmd.fillOpenId`（detect/test）；`err.fsEmptyConfig`。
  - detect 复用现有 `cmd.detectTimeout` / `cmd.detectCodeInvalid` / `cmd.streamDisconnected`（与钉钉共用）。
- 前端 `src/i18n/zh.ts` / `en.ts` 设置页键：`settings.channels.feishuTitle` / `appId` / `appSecret` / `openId` / `baseUrl`（含占位）等；detect/test 复用现有 `detecting`/`autoDetect`/`detectHint`/`detected`/`testing`/`testConnection`。

---

## 7. 依赖（`Cargo.toml`）

- 新增：`prost`（运行时，约束版本与生态一致，如 `0.13`）。**仅运行时**：用 `#[derive(prost::Message)]` 标注自定义 `PbFrame`，**不引入 `prost-build`、不需要 protoc / build.rs**。
- 复用：`tokio-tungstenite`(rustls,已有)、`futures-util`(已有)、`reqwest`(json/multipart/rustls,已有)、`serde_json`、`base64`、`tokio`、`uuid`、`async-trait`。
- 不引入第三方飞书 SDK / `lark-websocket-protobuf`（见 spec F5 理由）。

---

## 8. 涉及文件清单

新增：
- `src-tauri/src/feishu/{mod.rs, token.rs, client.rs, ws.rs, card.rs}`。
- `src-tauri/src/channels/feishu.rs`：`FeishuChannel` + `FeishuSession`（+ 回退 `ask_question_text`）。
- `docs/specs/feishu-channel.md` / `docs/plans/feishu-channel.md`（本两份）。

改动：
- `src-tauri/src/channels/mod.rs`：`pub mod feishu;`。
- `src-tauri/src/config.rs` / `src/lib/types.ts`：`FeishuChannelConfig` + `ChannelsConfig.feishu` + 单测。
- `src-tauri/src/commands.rs` / `src/lib/ipc.ts`：`feishu_test` / `feishu_detect_prepare` / `feishu_detect_wait`（+ 注册）。
- `src-tauri/src/app/mod.rs`：active 判定、`active_messaging_channels`、headless 分支与计数、命令注册。
- `src-tauri/src/app/coordinator.rs`：`display_name` 增飞书。
- `src-tauri/src/i18n.rs` / `src/i18n/zh.ts` / `src/i18n/en.ts`：飞书文案。
- `src/views/SettingsView.vue`：飞书配置卡片 + 自动识别/测试。
- `src-tauri/src/prompts.rs` / `README.md` / `docs/overview.md`：文档（含前置条件）。
- `src-tauri/Cargo.toml`：`prost` 依赖。

---

## 9. 任务顺序

1. **依赖 + 帧**：加 `prost`；`feishu/ws.rs` 定义 `PbFrame`，落地取 endpoint / 连接 / ping-pong / 分片重组 / 回包 / 重连骨架。
2. **配置/类型**：`FeishuChannelConfig`（Rust + TS）+ 默认值单测。
3. **客户端层**：`token` → `client`（send_text/interactive/image/file + upload_image/file + download_resource + patch_card）。
4. **联调收发底座**：先用 `feishu_test` 验证发；用 `feishu_detect_prepare/wait` + 长连接验证收（事件 `im.message.receive_v1`），并完成后台「长连接订阅」保存。
5. **互动卡片**：`card.rs` 组装 + `card.action.trigger` 回调解析；打通「勾选 + 补充文字 + 提交 + 回包终态」。
6. **飞书会话渠道**：`FeishuSession` 串起发 Message / 逐题卡片 / 收图片文件 / 完成；接入 `run_conversation`；卡片失败回退 `ask_question_text`。
7. **编排泛化**：`app/mod.rs` active 判定 + headless 分支/计数；`coordinator` display_name。
8. **设置页 UI + 命令注册 + ipc/types + i18n**。
9. **文档**：`prompts.rs` / `README` / `docs/overview.md`（含前置条件与已知问题）。
10. **构建**（`pnpm build && cargo build --release --features custom-protocol`）+ 端到端实测。

---

## 10. 测试策略

- Rust 单测：
  - `config` 飞书默认值 / 容错（旧 JSON 无 feishu）。
  - `card` 纯函数（卡片组装含/不含选项与 md 切换；`parse_card_submit` 勾选还原、空输入、缺字段）。
  - `ws` 可纯测部分：`PbFrame` 编解码 round-trip、header 读取、分片重组逻辑。
  - `client` 纯函数（msg content JSON 组装、file_type/扩展名映射）。
- 手动 / 端到端（需真实自建应用 + 已配置长连接订阅）：
  - 测试连接 / 自动识别 openId（含「建连期间保存后台订阅」流程）。
  - 单题 / 多题 / 带 Message + `-f` 文件（图片、文件可发可见）。
  - 卡片多选勾选 + 补充文字 + 提交完成；提交后卡片终态、无报错 toast。
  - 作答期间发图片/文件被收进答案；聊天纯文字被忽略。
  - 卡片投放失败回退纯文本编号选项。
  - 卡片创建首次返回瞬时错误、第二次成功：两次请求 UUID 相同，渠道层不得进入纯文本回退；非瞬时错误只请求一次。
  - OpenAPI 错误日志包含业务码、HTTP 状态与响应日志 ID。
  - 与弹窗 / Telegram / 钉钉 抢答（飞书先答 / 被抢答中止并尽力置卡片终态）。
  - headless（无 GUI）单跑飞书；与钉钉/Telegram 并行。
  - 弹窗 / Telegram / 钉钉 回归。

---

## 11. 风险与注意

- **protobuf 帧编解码 / 回包结构**：以真机联调为准固定 endpoint 请求体、数据帧响应体（事件空 ACK vs 卡片回调 `{toast, card}`）的确切字段，并在 `ws.rs` 注释锁定；参考钉钉 `stream.rs` 的 ACK/重连骨架。
- **3 秒回包**：每条业务帧收到后尽快回包（卡片提交先组装响应体，Router 写回并释放完成屏障；事件先空 ACK）。最终提交不得只以 oneshot「响应体已移交」作为完成条件，否则 Windows 单进程可能先退出。
- **集群模式多开互抢**（spec §6 已知问题）：维持不修，仅记录；连续/并发提问可能相互干扰。
- **后台订阅保存的在线要求**：文档写清「先点自动识别保持连接 → 后台保存长连接订阅 → 私聊发码」的顺序。
- **断线重连**：长等待期间 WS 可能断；重连后继续等本卡片回调/消息，不丢「当前题等待中」状态。
- **被抢答更新卡片**：用 `PATCH /im/v1/messages/{message_id}`（应用自身发送的卡片、14 天内可更新）；失败仅日志，不影响主流程。
- **媒体大小限制**：上传图片/文件、下载资源（≤100MB）有上限；超限走失败提示，不崩溃。
- **markdown**：飞书 `markdown` 组件为 lark_md 子集；先尽量原样传递，若出现渲染异常再加轻量适配（不在本计划强求）。
- **配置容错**：旧配置无 `feishu` 走默认（`#[serde(default)]` + `base_url` 默认值）。
- **release 构建**：须 `--features custom-protocol`；`prost` 仅运行时（无 protoc）。
