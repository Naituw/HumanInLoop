# Feishu / Lark channel setup

[简体中文](./feishu-setup.md) | English

This guide explains how to create and configure a custom enterprise app on the Feishu (or international Lark) open platform so AskHuman's "Feishu channel" works. The channel uses a **custom enterprise app + bot + long connection (WebSocket) + direct chat** model and needs **no public endpoint** to send/receive messages and card callbacks.

> Lark and Feishu follow the same steps; only the domain differs. The open platform is `open.larksuite.com`, and in AskHuman's "Service domain" field enter `https://open.larksuite.com` (leave it blank in China to use the default `https://open.feishu.cn`).

## 1. Create the app and enable the bot

There are two ways; **way 1 is recommended**: use Feishu's "one-click create a Feishu agent app" entry — it's **ready to use right after creation**, with no manual permission setup, event configuration, or publishing. If you use way 1, you can jump straight to "2. Fill in AskHuman".

### Way 1 (recommended): one-click create a Feishu agent app, no permission, event, or publishing setup

The Feishu Open Platform offers a "create a Feishu agent app" entry that **pre-configures the permissions and event / callback subscriptions an agent needs** and enables the bot (it can pair with open-source AI assistants such as OpenClaw and Hermes Agent). AskHuman can reuse it directly — just customize the name and avatar to get an App ID / App Secret with permissions already set.

1. Go to the Feishu Open Platform app list [open.feishu.cn/app](https://open.feishu.cn/app).
2. Find the "**Create a Feishu agent app**" banner at the top and click **Create now** on the right.
3. Fill in the app name and avatar (you can use the bundled avatar below), then finish.
4. Open the app's "Credentials & basic info" and copy the **App ID** (`cli_...`) and **App Secret** for AskHuman.

This entry already enables the bot, pre-configures the required permissions plus the long-connection subscriptions for "Receive message" and "Card callback interaction", and is **ready to use right after creation — no publishing needed** (none of the steps under "Way 2" below are required); just go to "2. Fill in AskHuman".

> Avatar: the repo ships two images you can use as the bot avatar (dark / light background), or for other uses: [dark](../../assets/avatars/bot-avatar-dark.jpg) · [light](../../assets/avatars/bot-avatar-light.jpg).

### Way 2: create a custom enterprise app manually

1. Go to the Feishu Open Platform → Developer Console → create a **custom enterprise app**.
2. Record the credentials: **App ID** (`cli_...`) and **App Secret** (enter both in AskHuman's settings).
3. Under "App capabilities → Add capability", enable the **Bot**.
4. Complete the manual setup per "Grant permissions", "Configure events and callbacks", and "Publish and availability" below.

#### Grant permissions (scopes)

In "Permissions → Enable permissions", use "Bulk import" and paste the following JSON:

```json
{
  "scopes": {
    "tenant": [
      "im:message:send_as_bot",
      "im:message.p2p_msg:readonly",
      "im:message:readonly",
      "im:resource"
    ],
    "user": []
  }
}
```

What each scope is for (cross-check against official requirements per the APIs this channel actually calls):

| Scope | Purpose | APIs |
| --- | --- | --- |
| `im:message:send_as_bot` (send messages as the app) | Send text / images / files / interactive cards to the user's direct chat, and PATCH-update the card on finalize | Send message, update app-sent message card |
| `im:message.p2p_msg:readonly` (read direct messages users send the bot) | Receive user direct-chat message events over the long connection | Event `im.message.receive_v1` |
| `im:message:readonly` (read direct/group messages) | Download images / files the user sends (human → AI attachments) | Get message resources |
| `im:resource` (get and upload image/file resources) | Upload media to get a key before sending `-f` attachments | Upload image, upload file |

Notes:

- A custom internal app uses `tenant_access_token`; leave `user` scopes empty.
- `im:resource` is an "advanced permission" and may require admin approval at some organizations.
- If you don't need "AI → human file sending / human → AI image-file return", you can skip `im:resource` and `im:message:readonly`; you'll lose those capabilities, but keeping them aligns Feishu with the other channels.

#### Configure events and callbacks (receive messages + card submit)

Scopes only decide "which APIs you can call". For the bot to actually **receive** messages and card submissions, you must configure the console as well.
**Key: Feishu puts "events" and "callbacks" on two separate pages, and each must be switched to the long connection. Configuring only one leads to "can receive messages but card submit does nothing / spins and reverts" or "receives no messages".**

> ⚠️ When switching either page to "long connection" and saving, the console requires that a long connection for this app is already online locally. To do this: first bring up AskHuman's connection (clicking "Auto-detect" in settings opens a connection and keeps it ~120s, or you're mid-question waiting), then click save in the console.

##### Event configuration (receive messages)

1. Go to "Dev config → Events & callbacks → **Event configuration**".
2. Edit subscription method → choose "**Receive events via long connection**" → save.
3. Under "Added events", **add the event "Receive message `im.message.receive_v1`"**: used to receive the text / images / files the user sends in the direct chat.
   > ⚠️ Per Feishu's official [bot FAQ](https://open.feishu.cn/document/faq/bot), the bot **only shows an input box in the direct chat after the "Receive message" event is subscribed and published**. Otherwise the bot opens to a "blank screen / no input box", and you can't use "Auto-detect" to send a code — the most common pitfall.

##### Callback configuration (card submit)

1. Go to "Dev config → Events & callbacks → **Callback configuration**" (this is **not the same page** as event configuration).
2. Edit subscription method → choose "**Receive callbacks via long connection**" → save.
3. Under "Subscribed callbacks", **add the callback "Card callback interaction `card.action.trigger`"**: when the user taps the card's "Submit", the checked options + extra text are returned via this callback.
   > ⚠️ You must use the **new** `card.action.trigger`; the legacy "Message card callback interaction `card.action.trigger_v1`" **does not support the long connection**.
   > If you miss this section, the card arrives but tapping "Submit" **spins for a few seconds, reverts, and fails**.

#### Publish and availability scope

1. Under "Version management & release", create a version and publish it (permissions, bot capability, and event subscriptions only take effect after publishing).
2. Make sure the bot's **availability scope** includes the target user, otherwise sending fails with `230013 Bot has NO availability to this user`.

## 2. Fill in AskHuman

Open AskHuman Settings → Channels → Feishu, enable it, and fill in:

| Field | Notes |
| --- | --- |
| App ID | App credential `cli_...` |
| App Secret | App credential secret |
| Open ID | The Open ID of the receiving / answering user (direct chat). Click "Auto-detect": it first validates App ID/Secret, then asks you to DM the bot a 4-digit code to fill it in accurately. **Prerequisite**: the bot has the "Receive message" capability (way 1 presets it; for way 2, subscribe the "Receive message" event and publish, see above), otherwise the bot's direct chat has no input box and you can't send the code (see FAQ) |
| Service domain | Blank = Feishu China `https://open.feishu.cn`; for international Lark enter `https://open.larksuite.com` |

Click "Test connection": it exchanges a token and sends a test message to that Open ID's direct chat. Receiving it in Feishu means you're set.

## 3. Behavior and fallback

- Questions are sent as **interactive cards** (Card JSON 2.0, delivered directly with no template to build), one per question: check predefined options (multi-select) in the card form, optionally add text, then tap "Submit" to finish (the callback goes over the long connection and must reply within 3s, handled automatically).
- After submitting, the card keeps the question and options in a **"submitted" state**: the checkers are disabled but keep your selection, your extra text is echoed in the input, and the button changes from "Submit" to a disabled "Submitted" (instead of being replaced by a one-line status text).
- **Images / files** sent in the chat while answering are accumulated into that question's answer; **plain text is ignored** (use the card's input field for text).
- For recoverable card-delivery failures such as a network interruption or Feishu internal error, AskHuman first retries once with the same idempotency key to prevent duplicate cards. If delivery still fails, it **falls back** to "plain text + numbered options": reply with numbers (comma-separated for multi-select, e.g. `1,3`), type text, or send images / files to answer.
- With multiple channels enabled, racing happens at the **whole-session** granularity: whichever side finishes all questions first wins, and the others wrap up (the Feishu card is PATCHed to an "answered on X" final state).

## 4. FAQ

| Symptom | Likely cause |
| --- | --- |
| **Bot direct chat is blank / has no chat page / no input box** | The "Receive message" event (`im.message.receive_v1`, requires `im:message.p2p_msg:readonly`) is not subscribed or not published. Per the official FAQ, the input box only appears after subscribing and publishing that event |
| `230006 / 234007 Bot ability is not activated` | Bot capability not enabled, or not effective after publishing |
| `230013 Bot has NO availability to this user` | The target user is outside the bot's availability scope |
| Send / upload permission error | The corresponding scope is not enabled or not published; `im:resource` may await admin approval |
| Card arrives but tapping "Submit" spins and reverts, submit fails | On the **Callback configuration** page, the subscription method isn't set to long connection, or "Card callback interaction `card.action.trigger`" isn't added (see Way 2 · Configure events and callbacks · Callback configuration). Note it's a separate page from Event configuration — a common miss |
| No user messages received / no input box in direct chat | On the **Event configuration** page, `im.message.receive_v1` isn't subscribed, the method isn't long connection, or it's not published (see Way 2 · Configure events and callbacks · Event configuration) |
| Want to confirm whether the callback arrived | Run with `ASKHUMAN_FEISHU_DEBUG=1`, tap submit, then check `~/.askhuman/feishu-debug.log`: an `event_type=card.action.trigger` line means the callback arrived |
| Test connection fails | Wrong App ID / App Secret, or wrong service domain (China vs international) |
