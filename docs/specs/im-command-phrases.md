# IM 自然语言短语激活命令

> 状态：定案（2026-07-18）。实现：`autochannel::classify` 无前缀分支 + `COMMAND_PHRASES`。

## 1. 需求

用户在 IM 里除 `/new` / `!new` 等斜线（及 Slack 备用 `!`）命令外，可用**整句自然语言短语**触发同一无参命令，尤其方便语音输入（如「新建会话」→ `/new`）。

## 2. 规则

1. **斜线优先**：`trim` 后以 `/` 或 `!` 开头 → 现有 `classify_prefixed`，不做短语表。
2. **整句匹配**：对整条消息做规范化后与词表键**整串相等**；不做子串、不做参数抽取。
3. **规范化**（`textnorm::normalize_key`，与 whats-next 伪结束过滤同构）：
   - 去掉全部 whitespace；
   - 去掉全部非字母数字字符（含中文全角标点、ASCII 标点、连接符等）；
   - 英文字母小写；
   - 保留 Unicode 字母与数字（含汉字）。
4. **仅无参形态**：短语只映射到无编号、无正文的命令变体（如 `Status(None)`、`Msg(None, None)`、`New { has_args: false }`）。带参仍用斜线。
5. **命令优先**：短语命中与 slash 命令相同，**优先于**把消息当提问答案（有在途问时也执行命令）。
6. **不做** `/msg-clear` 短语；`?` 规范化后为空，仅保留 `/`/`!` 帮助。

## 3. 词表范围

- 每个支持短语的命令至少包含与**斜线命令名**及现有**中文 token 别名**对齐的键（如 `new` / `新建` / `新任务`）。
- 另收语音友好较长短语（如 `新建会话`、`查看状态`、`发消息`）。
- 词表见 `autochannel::COMMAND_PHRASES`；键必须唯一（单测保证）。

覆盖命令：`new`、`here`、`help`、`status`、`watch`、`unwatch`、`msg`、`diff`、`stage`、`transcript`、`todo`、`todo-rm`、`todo-auto`、`yolo`。

`yolo` 的全部短语（含 `关闭yolo`、`yolooff` 等关闭形态）一律指向**列表卡** `Yolo(List)`：卡上自带关闭按钮，与「仅无参形态」规则一致；按编号直关仍用斜线 `/yolo off <编号>`。

## 4. 非目标

- 模糊匹配 / 编辑距离 / LLM 意图；
- 「关注 3 号」类带参短语；
- 用户自定义短语；
- 答题场景下的二次确认。

## 5. 行为对照

| 输入 | 结果 |
|---|---|
| `新建会话` / `新建 会话！` / `新建` / `new` | `Command::New { has_args: false }` |
| `/new foo` | 仍带参 `New { has_args: true }` |
| `请帮我新建会话` | `Text`（非整句） |
| `status` / `状态` | `Status(None)` |
| `插话` / `发消息` | `Msg(None, None)` |
