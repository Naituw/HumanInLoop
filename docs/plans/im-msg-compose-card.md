# 实现计划：IM `/msg` 一次性消息输入卡

> 需求定案见 `docs/specs/im-msg-compose-card.md`。本计划只描述实现落点，不改变既有插话队列与
> `/msg <内容>` 快捷发送语义。
>
> 状态：已实现。实际共享模型位于 `src-tauri/src/msg_card.rs`；四渠道发送、回调、TTL 与恢复编排
> 集中在 `daemon/runtime/select.rs`。

## M1 命令路由与状态模型

1. `daemon/runtime/mod.rs`
   - `PickerKind` 增 `MsgCompose`；`PickerEntry.payload` 用 JSON 保存目标 session id、过期时间与恢复态。
   - 更新 picker 注释、关停定格和穷尽 match；`MsgCompose` 不走通用 `send_agent_picker`。
   - 增加 `MsgComposeRecovery` 最小恢复台账，只保存 channel / message id / session id / expires_at；
     输入卡创建成功后落盘，任何终态后删除。
2. `daemon/runtime/inbound.rs`
   - `(None, None)`：唯一关注目标可发送时直接 `send_msg_compose`；否则发 `PickerKind::Msg`、
     `payload=None` 的 Agent 选择卡。
   - `(Some(n), None)`：工作中非 Grok → `send_msg_compose`；idle → 现有 `msg_echo_text`；其它错误不变。
   - `(None, Some)` / `(Some, Some)` 保持现有快捷路径。
3. `daemon/runtime/select.rs`
   - `PickerKind::Msg` 点选时按 payload 分流：`Some(content)` 继续现有立即发送；`None` 进入 compose。
   - 飞书 / Slack 可把选择卡就地改为输入卡；钉钉 / Telegram 先定格选择卡，再发送输入载体。
   - 提供 `take_msg_compose_picker(channel, message_id)`，有效提交原子消费，保证 first-submit-wins。

## M2 共享输入卡模型

在 daemon 层增加纯数据视图与纯函数，避免四渠道分别拼业务文案：

- `MsgComposeView`：目标 seq / title / project、pending_count、pending_preview、输入文案与错误状态。
- `build_view(record, pending_count, pending_text, error, lang)`：只从当前快照记录与队列快照读数据。
- `preview_pending(text, 1600)`：Unicode 安全；短文本原样，长文本首尾各保留预算并带省略字符数。
- `validate_input`：trim 与 3000 字上限；daemon 提交路径另行重验目标仍 working 且非 Grok。
- `take_msg_compose_picker` → `deliver_msg` → 返回现有 `select.msgSentCard` 文案；不调用
  `reply_channel_text`。

新增 i18n：输入卡标题、目标 / 待送达标题、空队列、输入 placeholder、空 / 超长校验、过期、发送失败。
复用已有 `autoChannel.msgDeliveredNow`、`autoChannel.msgQueued`、`select.msgTargetGone` 和
`select.msgSentCard`。

## M3 飞书

- `feishu/card.rs`：增加消息输入卡 builder；正文安全转义，form 仅含多行 input + primary Send。
- `daemon/runtime/select.rs`：在通用 select 回调前按消息 id + picker kind 识别 compose submit：
  - 无 / 超长输入：ACK 返回仍可编辑的错误卡；
  - 有效输入：原子消费台账、投递、ACK 返回一次性终态卡；
  - 重启恢复或 TTL 到期的 picker：ACK 返回“已过期、未发送”。
- Agent 选择卡可以通过 callback update 直接变身为输入卡，不另发消息。

测试：builder 结构、submit 解析、空 / 超长提示、目标漂移、重复 callback 只投递一次、过期回卡。

## M4 Slack

- `slack/blockkit.rs` 或 `slack/select.rs`：增加 pending section + multiline `plain_text_input` + Send actions。
- Slack select 路由识别 compose action，从 `state.values` 取输入；同一 message 就地更新为校验提示或终态。
- block id / action id 带 nonce，避免客户端草稿跨卡缓存；服务端仍按 message ts + picker 校验。

测试：blocks 布局、输入提取、nonce、一次性消费、过期和状态漂移。

## M5 钉钉

- Agent 选择继续走现有 select 模板；点选后先 `dd_finalize_select_card` + remove 旧 picker，再发送输入卡。
- 输入卡复用 `channels::dingding::effective_template_id` 指向的现有提问卡模板：
  - `options=[]`、`single=false`、`allow_input=true`；
  - `markdown` 放目标与待送达预览；
  - 创建成功后登记 `PickerKind::MsgCompose`。
- 复用 `dingtalk::card::parse_card_submit`。同步 ACK 先满足平台成功协议；异步处理：
  - 空 / 超长：更新 markdown / submit_status，并把私有 `submitted=false` 复位；
  - 有效：投递后写成功终态，保持 `submitted=true`，隐藏或禁用输入；
  - picker 缺失：写“已过期、未发送”。

不得新增 DingTalk template id / 设置项 / 模板 JSON；不得在选择未结束时投放输入卡。

测试：param map 复用既有模板契约、选择定格先于输入投放、空输入复位、有效提交一次、无新增模板常量。

## M6 Telegram

- 目标选择卡先就地定格，再发送一条 ForceReply 输入提示；直达目标只发 ForceReply。
- 在 daemon 的 Telegram select 路由登记 prompt message id；只接受
  `reply_to_message_id == prompt_message_id` 的非空文字，不使用“最新活动卡”自由文本兜底。
- 超过 3000 字时回复长度提示并保持 route；有效后原子消费 picker、投递，删除已用完的 bot prompt，
  再回复用户消息一条短终态（ForceReply 消息受 Telegram API 限制，不能就地编辑）。
- 不另发取消卡；忽略 / TTL 过期即可退出。picker / route 丢失后普通文字继续走 `autochannel`。

测试：精确 reply-to、普通文本不消费、超长保留、成功编辑终态、重复 reply 只发送一次。

## M7 生命周期、降级与文档

- `finalize_all_select_cards` 覆盖 `MsgCompose`，drain 时定格“服务已重启，未发送”。
- picker TTL 沿用 30 分钟；`MsgCompose` 不被普通 picker 软上限静默淘汰，过期由独立收尾任务处理。
- 新增 `paths.rs` 状态文件入口与原子读写；文件内容不含待送达预览、草稿或正文。daemon 启动时把遗留
  记录恢复成 expired tombstone route，并主动定格旧卡；编辑失败则保留 route 至成功或 TTL 到期。
- 恢复台账测试覆盖：原子往返、老 / 坏文件降级、终态清理、重启后只能过期不能发送、内容字段不可序列化。
- 输入卡发送失败走 spec §9 文本兜底，不泄露正文。
- 实现完成后更新：
  - `docs/overview-im-commands.md` 的 `/msg` 命令地图与 picker kind；
  - `docs/specs/agent-interject.md` 的 D9；
  - `docs/plans/im-msg-select-card.md` 顶部追加后续演进链接；
  - `docs/PROGRESS.md` 清除进行中标记。
- 主 `docs/overview.md` 的模块地图与跨模块不变量不变，不更新。

## M8 验证

1. 纯函数 / 四渠道 builder 与 parser / daemon 分派单测。
2. `cargo test` 全量通过。
3. `./scripts/install.sh` 编译并重启 daemon。
4. 四渠道真机矩阵：无参选择、唯一关注直达、编号直达、idle 回显、空 / 超长、状态漂移、成功终态、
   排队后已阅读、过期与重复提交。
5. 钉钉额外确认：没有新模板，选择阶段结束后才出现输入卡，任一时刻只有一张可操作卡。
