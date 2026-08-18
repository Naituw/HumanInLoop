<p align="center">
  <img src="assets/banner.jpg" alt="AskHuman" width="800">
</p>

<p align="center">English | <a href="./README.zh-CN.md">简体中文</a></p>

# AskHuman

A cross-platform human-in-the-loop tool. When an AI agent is about to end a conversation or needs confirmation, it runs the `AskHuman` CLI to pop up a window so you can ask follow-ups, pick options, add text, or attach images — and the result is returned to the agent.

- A single executable `AskHuman` that lets agents ask questions through the CLI
- Built on **Tauri 2 (Rust + Vue 3)**, supports **macOS / Linux / Windows**
- Multiple channels: local popup + Telegram + Slack + DingTalk + Feishu, independently toggleable and racing in parallel when several are on

## How it works

<p align="center">
  <img src="assets/overview.webp" alt="AskHuman bridges AI agents and humans: a Bash call fans out to the popup and IMs" width="900">
</p>

## Screenshots

Your agent's questions arrive at the local GUI popup and Telegram, Slack, DingTalk, and Feishu all at once, along with the key context, attachments, and preset options — so whether or not you're at your desk, you get notified and can reply anytime.

<p align="center">
  <img src="assets/channels.webp" alt="Reply to your agent from the local popup, Telegram, Slack, DingTalk, or Feishu" width="900">
</p>

The tool automatically records the recent history of agent questions and human answers, so you can refer back to it anytime while answering new questions. (If you don't need history, you can turn it off in the settings.)

<p align="center">
  <img src="assets/history.webp" alt="Browse message and reply history per project" width="680">
</p>

## Install

```bash
# npm (recommended): downloads only the one binary matching your platform
npm i -g askhuman
```

You can also download a platform archive from [GitHub Releases](https://github.com/Naituw/AskHuman/releases), extract it, and put `AskHuman` on your `PATH`. To build from source, see the [development guide](docs/development.md).

> Running the GUI popup on Linux needs system WebKitGTK (e.g. `libwebkit2gtk-4.1`); if it's missing and a session-based channel is configured, AskHuman uses that channel automatically.

## Usage

### 1. The AskHuman command

`AskHuman` is a command-line tool that AI agents use to ask you questions and get the result back. The most common usages:

```bash
# The most basic ask: the first argument is the question, -o adds an option
AskHuman "Continue?" -o "Continue" -o "Stop"

# With an image and multiple questions: the first argument is a shared description,
# -f attaches a file/image, each -q is a question, -o! marks the recommended answer
AskHuman "Take a look at this change?" -f ./diagram.png \
  -q "Continue?" -o! "Continue" -o "Stop" \
  -q "Run the tests?" -o "Run" -o "Skip"

# Strict choice for scripts: only preset options, single choice, machine-readable JSON
AskHuman -q "Deploy where?" -o "staging" -o! "production" --single --select-only --output json

# Long Markdown via stdin: quoted heredoc (write the heredoc last on the command line)
AskHuman -q "Continue?" -o "Continue" -o "Stop" --stdin <<'EOF'
# Review notes with `code`, $HOME, and "quotes"
EOF

# Other common ones
AskHuman --settings   # open the settings UI
AskHuman --history    # open reply history (add --all for every project)
```

For the full CLI usage, see `AskHuman --help`; for the asking usage tuned for AI agents, see `AskHuman --agent-help`; for scripting/automation (`--select-only` / `--single` / `--output json`), see `AskHuman --scripting-help`.

### 2. Integrate with your Agent

To make your agent call `AskHuman` on its own when finishing or needing confirmation, add the relevant prompt to the agent's global instructions. Run `AskHuman --settings`, open the **Agents** panel, and choose:

- **Manual integration** — copy the reference prompt and add it to your agent's global instructions yourself (e.g. Cursor Rules / `AGENTS.md` / `CLAUDE.md`).
- **Automatic integration** — one click installs global Rules for Cursor / Claude Code / Codex; you can also install the timeout Hook (when it detects a call to `AskHuman`, it extends the tool-call timeout to 24 hours so it isn't force-canceled while waiting for your reply).

### 3. Set up communication channels

The local popup works out of the box. You can also enable DingTalk, Feishu, Telegram, or Slack — so you get questions and can reply whether or not you're at your desk (multiple channels can run in parallel and race for the answer). Configure them in the **Channels** tab; for each channel's onboarding steps, see:

- [DingTalk](docs/wiki/dingtalk-setup.en.md)
- [Feishu / Lark](docs/wiki/feishu-setup.en.md)
- [Telegram](docs/wiki/telegram-setup.en.md)
- [Slack](docs/wiki/slack-setup.en.md)

### 4. General settings

For general preferences such as theme, window behavior, speech input, and reply history, see [General Settings](docs/wiki/settings.en.md).

## Advanced

### Program integration

Add `askhuman` to your project (`npm i askhuman`); `npm install` pulls the current platform's binary, and at runtime you resolve the path and call it:

```js
import { getBinaryPath, isAvailable } from "askhuman";
import { spawnSync } from "node:child_process";

if (isAvailable()) {
  const r = spawnSync(getBinaryPath(), ["Continue?", "-o", "Continue", "-o", "Stop"], { encoding: "utf8" });
  if (r.status === 3) { /* no available channel: degrade without blocking */ }
  else if (r.status === 0) { /* parse the result blocks from r.stdout */ }
}
```

> Exit codes: success / cancel is `0`; no available channel is `3`; other errors are `1`.
> Custom source name: set `ASKHUMAN_ENV_SOURCE_NAME=Agent`, and the popup title and channel message headers become `Question from Agent`.

### Environment variables

For the available environment variables, see [Environment Variables](docs/wiki/environment-variables.en.md).

## Development

For local build, test, and release workflow, see the [development guide](docs/development.md).

## License

[MIT](LICENSE) © Naituw
