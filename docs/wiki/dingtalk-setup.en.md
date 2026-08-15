# DingTalk channel setup

[简体中文](./dingtalk-setup.md) | English

This guide shows how to create and configure a DingTalk "internal enterprise app + bot" so AskHuman's DingTalk channel works. The channel uses an **internal enterprise app + bot + Stream mode (WebSocket long connection) + direct chat** — **no public endpoint, domain, or certificate required**. Questions are delivered as interactive cards one at a time; you check options, add text, and submit to answer.

> Prerequisite: you have developer access in your DingTalk organization and can create an internal app (if your org hasn't completed enterprise verification on the Open Platform, do that first).

## 1. Create an app and enable the bot

There are two ways; **way 1 is recommended**: use DingTalk's "one-click create a DingTalk agent app" entry — it works out of the box with no manual permission setup. If you use way 1, you can jump straight to "2. Publish and availability".

### Way 1 (recommended): one-click create a DingTalk agent app (OpenClaw bot), no permission setup

The DingTalk Open Platform offers a "one-click create a DingTalk agent app" entry (earlier / some docs still call it "one-click create OpenClaw bot"; the agent app can pair with open-source AI assistants such as OpenClaw and Hermes Agent — OpenClaw itself is an open-source, self-hostable AI assistant, not a DingTalk product). The entry creates a **standard Stream-mode bot internal app** that AskHuman can reuse directly: it automatically creates the app, enables the bot, sets Stream mode, and **pre-grants the interactive-card and direct-message API permissions**, so it **usually works out of the box with no manual permission setup**.

1. Go to the [DingTalk Open Platform developer console](https://open-dev.dingtalk.com) → top menu "App Development → DingTalk Apps".
2. On the "DingTalk Apps" page, find the "one-click create a DingTalk agent app (OpenClaw bot)" banner at the top and click **Create now** on the right.
3. In the dialog, fill in the bot name / description / icon (you can use the bundled avatar below), then confirm.
4. After creation, copy the **ClientId (AppKey)** and **ClientSecret (AppSecret)** for AskHuman.

This entry already enables the bot, sets Stream mode, and pre-grants the required permissions, so **no permission setup is needed**; just go to "2. Publish and availability" to confirm the app is published and your user is in scope.

### Way 2: create an internal app manually

1. Go to the [DingTalk Open Platform developer console](https://open-dev.dingtalk.com), create an **internal enterprise app**, and record the credentials **ClientId (AppKey)** and **ClientSecret (AppSecret)**.
2. Under "App capabilities", add and enable the **Bot**. The bot's `robotCode` equals the app's AppKey, so no separate configuration is needed.
3. In the bot settings, set the **message-receiving mode** to **Stream mode** (pushed over a local long connection, no public callback).
4. Grant the required API permissions per "Grant permissions" below.
5. Publish a version under "Version management & release" (see "2. Publish and availability").

#### Grant permissions

> Only needed for manual creation; an app created via way 1 (one-click DingTalk agent app) already has the permissions below.

DingTalk permissions must be **searched and applied one by one** in the app's "Permission management" (there's no JSON batch import like Feishu); applying needs no approval and takes effect immediately, but **changing permissions requires re-publishing a version** to take effect. Based on the APIs this channel actually calls, grant:

| Permission to grant | Purpose | APIs involved |
| --- | --- | --- |
| **Internal bot message-sending permission** (企业内机器人发送消息权限, `qyapi_robot_sendmsg`) | Send text / image / file / card to the user in direct chat; download images / files the user sends (human → AI attachments) | `robot/oToMessages/batchSend`, `robot/messageFiles/download` |
| **`Card.Instance.Write`** (create and deliver card instances) | Deliver the advanced interactive card and update it on wrap-up / racing | `card/instances/createAndDeliver`, `card/instances` (update) |

Notes:

- **Media upload** (uploading `-f` attachments to get a mediaId) uses the base permission `qyapi_base`, which is **enabled by default for internal apps — no application needed**.
- In the "Permission management" search box, enter "企业内机器人发送消息权限" and "Card.Instance.Write" respectively to locate and apply them.

> Avatar: the repo ships two images you can use as the bot avatar (dark / light background), or for other uses: [dark](../../assets/avatars/bot-avatar-dark.jpg) · [light](../../assets/avatars/bot-avatar-light.jpg).

## 2. Publish and availability

1. Create and publish a version under "Version management & release" (bot capability, Stream mode, and permissions all take effect only **after publishing**).
2. Make sure the bot's **availability scope** includes your target user, otherwise sending fails with a "bot has no availability to this user" type error.

## 3. (Optional) Custom card template

AskHuman ships with a **built-in advanced interactive-card template** that works out of the box — no setup required. To customize the card, build and publish an **advanced** interactive-card template (same app) on the DingTalk card platform, then enter its template ID in the settings "Card template ID" field. Leave it blank to use the built-in default.

## 4. Fill in AskHuman

Open Settings → Channels → DingTalk, enable it, and fill in:

| Field | Notes |
| --- | --- |
| ClientId | App AppKey |
| ClientSecret | App AppSecret |
| UserId | The userId of the receiving / answering user (direct chat). Click "Auto-detect": it first validates ClientId/ClientSecret, then asks you to DM the bot a 4-digit code to accurately fill it in |
| Card template ID | Blank uses the built-in default; fill in to use your own advanced card template |

Click "Test connection": it exchanges a token and sends a test message to that userId's direct chat. Receiving it in DingTalk means you're set.

## 5. Behavior and fallback

- Questions are delivered as **advanced interactive cards**, one per question: check predefined options (multi-select), optionally add text, then tap "Submit" to finish that question (callbacks go over Stream, no public endpoint).
- **Images / files** sent in the chat while answering are accumulated into that question's answer (images into `[Images]`, files into `[Files]`); use the card's text field for plain text.
- If card delivery fails, it automatically **falls back** to "plain text + numbered options": reply with numbers (comma-separated for multi-select, e.g. `1,3`), type text, or send images / files to answer.
- Message attachments (`-f`) are uploaded as media and sent alongside the Message.
- With multiple channels enabled, racing happens at the granularity of the whole session: whichever side finishes all questions first wins, and the others wrap up.

## 6. FAQ

| Symptom | Likely cause / fix |
| --- | --- |
| Test connection fails | Wrong ClientId / ClientSecret or stray whitespace; or the app isn't published yet |
| No card received, falls back to plain text | A manually-created app is missing `Card.Instance.Write` (see "Way 2 · Grant permissions"); or the bot isn't enabled / published |
| Sending or downloading images / files fails with a permission error | A manually-created app is missing the "internal bot message-sending permission" (see "Way 2 · Grant permissions") |
| The bot receives no direct messages | Message-receiving mode isn't Stream; or the app isn't published; or the target user isn't in the availability scope |
| "Bot has no availability to this user" | The target user isn't in the bot's availability scope (see "2. Publish and availability") |
| "Auto-detect" never receives the code | DM the 4-digit code from your **target account**; make sure the bot is published and available to you |
