<p align="center">
  <img src="assets/banner.jpg" alt="AskHuman" width="800">
</p>

<p align="center"><a href="./README.md">English</a> | 简体中文</p>

# AskHuman

跨平台的「Human-in-the-loop」交互工具。当 AI Agent 准备结束对话或需要确认时，调用命令行 `AskHuman` 弹出窗口，让你继续提问、勾选选项、补充文字或附带图片，并把结果回传给 AI。

- 单一可执行文件 `AskHuman`，允许 Agent 通过 CLI 方式调用提问
- 基于 **Tauri 2（Rust + Vue 3）**，支持 **macOS / Linux / Windows**
- 多通信渠道：本地弹窗 + 钉钉 + 飞书 + Telegram + Slack，可独立开关、多开并行「抢答」

## 工作原理

<p align="center">
  <img src="assets/overview.webp" alt="AskHuman 在 AI Agent 与人之间架起桥梁：从 Bash 调用分发到弹窗与各 IM" width="900">
</p>

## 功能预览

Agent 的提问会同时送达本地 GUI 弹窗与钉钉、飞书、Telegram、Slack，并提供关键上下文、附件及预选项，无论你是否在电脑前，都能随时收到通知并回复。

<p align="center">
  <img src="assets/channels.webp" alt="在本地弹窗、钉钉、飞书、Telegram、Slack 等多渠道回复 Agent" width="900">
</p>

工具会自动记录最近的 Agent 提问及人类回答历史，在回答新问题时，可以随时参考。（若不需要历史记录，可以在设置中关闭）

<p align="center">
  <img src="assets/history.webp" alt="按项目查看消息与回复历史" width="680">
</p>

## 安装

```bash
# npm（推荐）：只下载与当前平台匹配的一个二进制
npm i -g askhuman
```

也可从 [GitHub Releases](https://github.com/Naituw/AskHuman/releases) 下载对应平台压缩包，解压后把 `AskHuman` 放入 `PATH`。从源码构建见[开发文档](docs/development.md)。

> Linux 运行 GUI 弹窗需系统具备 WebKitGTK（如 `libwebkit2gtk-4.1`）；缺失且配置了会话型渠道时会自动改走该渠道。

## 使用

### 一、AskHuman 命令

`AskHuman` 是一个命令行工具，AI Agent 通过它向你提问并取回结果。几个最常见的用法：

```bash
# 最基础的提问：第一个参数即问题，-o 添加预选项
AskHuman "要继续吗？" -o "继续" -o "停止"

# 带图片、多问题：第一个参数是共享描述，-f 附带文件/图片，每个 -q 是一题，-o! 标记推荐答案
AskHuman "看看这个改动？" -f ./diagram.png \
  -q "要继续吗？" -o! "继续" -o "停止" \
  -q "需要跑测试吗？" -o "跑" -o "跳过"

# 脚本场景的严格选择：只允许预设选项、单选、机器可解析的 JSON
AskHuman -q "部署到哪个环境？" -o "staging" -o! "production" --single --select-only --output json

# 长 Markdown 走 stdin：带引号 heredoc（heredoc 写在命令最后）
AskHuman -q "要继续吗？" -o "继续" -o "停止" --stdin <<'EOF'
# 含 `code`、$HOME 与 "引号" 的说明
EOF

# 其它常用
AskHuman --settings   # 打开设置界面
AskHuman --history    # 打开回复历史（加 --all 看全部项目）
```

整个 CLI 的完整用法见 `AskHuman --help`，面向 AI 的提问用法见 `AskHuman --agent-help`，面向脚本/自动化（`--select-only` / `--single` / `--output json`）见 `AskHuman --scripting-help`。

### 二、集成到 Agent 中

为了让 Agent 在结束或需要确认时主动调用 `AskHuman`，需要把相应提示词加入 Agent 的全局提示词。运行 `AskHuman --settings` 打开设置，进入 **Agents** 面板，按需选择：

- **手动集成**：复制参考提示词，自行加入你的 Agent 全局提示词（如 Cursor Rules / `AGENTS.md` / `CLAUDE.md`）。
- **自动集成**：一键为 Cursor / Claude Code / Codex 安装全局 Rules；还可安装超时 Hook（检测到调用 `AskHuman` 时，自动把工具调用超时延长到 24 小时，避免等待你回应时被强制取消）。

### 三、配置沟通渠道

默认即有本地弹窗。你也可以开启钉钉、飞书、Telegram、Slack 等渠道——这样无论是否在电脑前，都能收到提问并回复（多个渠道可同时开启并行「抢答」）。在设置的 **通信渠道** Tab 配置，每个渠道的接入步骤见：

- [钉钉](docs/wiki/dingtalk-setup.md)
- [飞书 / Lark](docs/wiki/feishu-setup.md)
- [Telegram](docs/wiki/telegram-setup.md)
- [Slack](docs/wiki/slack-setup.md)

### 四、通用设置

主题、窗口、语音输入、回复历史等通用偏好见[通用设置](docs/wiki/settings.md)。

## 高级用法

### 程序集成

把 `askhuman` 加入项目依赖（`npm i askhuman`），`npm install` 会自动装上当前平台二进制，运行时解析路径并调用：

```js
import { getBinaryPath, isAvailable } from "askhuman";
import { spawnSync } from "node:child_process";

if (isAvailable()) {
  const r = spawnSync(getBinaryPath(), ["要继续吗？", "-o", "继续", "-o", "停止"], { encoding: "utf8" });
  if (r.status === 3) { /* 无任何可用 channel：降级，不阻塞流程 */ }
  else if (r.status === 0) { /* 解析 r.stdout 的结果区块 */ }
}
```

> 退出码：成功 / 取消为 `0`；无任何可用 channel 为 `3`；其它异常为 `1`。
> 自定义来源名：设环境变量 `ASKHUMAN_ENV_SOURCE_NAME=Agent`，弹窗标题与渠道消息头变为 `Question from Agent`。

### 环境变量

可用的环境变量见[环境变量](docs/wiki/environment-variables.md)。

## 开发

本地构建、测试与发布流程见[开发文档](docs/development.md)（English）。

## 许可

[MIT](LICENSE) © Naituw
