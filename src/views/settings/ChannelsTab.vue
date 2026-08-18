<script setup lang="ts">
// 「通信渠道」tab：弹窗渠道尺寸 + 飞书 / Telegram / 钉钉 / Slack 四家 IM 渠道配置卡
// （含 R7 故障横幅、配置指南外链、连接测试与自动识别）。
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { useSettingsContext } from "./context";

const { t } = useI18n();
const ctx = useSettingsContext();
const {
  persist,
  secretsPresent,
  SECRET_PLACEHOLDER,
  clearSecret,
  channelIssueText,
  openChannelGuide,
  stepWidth,
  stepHeight,
  cancelDetect,
  telegramTesting,
  telegramMessage,
  telegramError,
  runTelegramTest,
  dingtalkTesting,
  dingtalkDetecting,
  dingtalkDetectCode,
  dingtalkMessage,
  dingtalkError,
  runDingtalkTest,
  runDingtalkDetect,
  feishuTesting,
  feishuDetecting,
  feishuDetectCode,
  feishuMessage,
  feishuError,
  runFeishuTest,
  runFeishuDetect,
  slackTesting,
  slackDetecting,
  slackDetectCode,
  slackMessage,
  slackError,
  runSlackTest,
  runSlackDetect,
} = ctx;
// 父组件仅在 config 加载后渲染本 tab，这里可安全断言非空。
const config = computed(() => ctx.config.value!);
</script>

<template>
  <div class="card">
    <div class="row">
      <p class="card-title">{{ t("settings.channels.popupTitle") }}</p>
      <span class="spacer"></span>
      <label class="switch">
        <input
          type="checkbox"
          v-model="config.channels.popup.enabled"
          @change="persist"
        />
        <span class="track"></span>
      </label>
    </div>

    <template v-if="config.channels.popup.enabled">
      <hr class="divider" />
      <div class="row">
        <span class="label">{{ t("settings.channels.rememberSize") }}</span>
        <span class="spacer"></span>
        <label class="switch">
          <input
            type="checkbox"
            v-model="config.channels.popup.rememberSize"
            @change="persist"
          />
          <span class="track"></span>
        </label>
      </div>
      <div class="row">
        <span class="label">{{ t("settings.channels.defaultWidth") }}</span>
        <span class="spacer"></span>
        <div class="stepper">
          <button
            type="button"
            :disabled="config.channels.popup.width <= 360"
            @click="stepWidth(-20)"
          >
            −
          </button>
          <span class="val">{{ Math.round(config.channels.popup.width) }}</span>
          <button
            type="button"
            :disabled="config.channels.popup.width >= 1200"
            @click="stepWidth(20)"
          >
            +
          </button>
        </div>
      </div>
      <div class="row">
        <span class="label">{{ t("settings.channels.defaultHeight") }}</span>
        <span class="spacer"></span>
        <div class="stepper">
          <button
            type="button"
            :disabled="config.channels.popup.height <= 360"
            @click="stepHeight(-20)"
          >
            −
          </button>
          <span class="val">{{
            Math.round(config.channels.popup.height)
          }}</span>
          <button
            type="button"
            :disabled="config.channels.popup.height >= 1400"
            @click="stepHeight(20)"
          >
            +
          </button>
        </div>
      </div>
    </template>
  </div>

  <!-- 引导到「高级 · 按需发送」：独立 tip 卡，走 .card+.card 间距；仅未开启时显示（默认关时的发现性） -->
  <div
    v-if="!config.channels.autoActivation"
    class="card channels-tip"
  >
    <p class="channels-tip-title">
      {{ t("settings.channels.autoActivationChannelsHintTitle") }}
    </p>
    <p class="channels-tip-body">
      {{ t("settings.channels.autoActivationChannelsHint") }}
    </p>
  </div>

  <div class="card channel-card channel-card-telegram">
    <div class="row">
      <p class="card-title">{{ t("settings.channels.telegramTitle") }}</p>
      <a class="link guide-link" href="#" @click.prevent="openChannelGuide('telegram')">
        {{ t("settings.channels.setupGuide") }} ↗
      </a>
      <span class="spacer"></span>
      <label class="switch">
        <input
          type="checkbox"
          v-model="config.channels.telegram.enabled"
          @change="persist"
        />
        <span class="track"></span>
      </label>
    </div>
    <!-- R7 渠道故障横幅：daemon 侧最近未恢复的错误（修复后自动消失） -->
    <p v-if="channelIssueText('telegram')" class="card-desc warn">
      {{ channelIssueText("telegram") }}
    </p>

    <template v-if="config.channels.telegram.enabled">
      <hr class="divider" />
      <div class="field">
        <label>{{ t("settings.channels.botToken") }}</label>
        <div class="row">
          <input
            class="input"
            style="flex: 1"
            type="password"
            :placeholder="
              secretsPresent.telegramToken ? SECRET_PLACEHOLDER : ''
            "
            v-model="config.channels.telegram.botToken"
            @change="persist"
          />
          <button
            v-if="secretsPresent.telegramToken"
            class="btn"
            type="button"
            @click="clearSecret('telegram')"
          >
            {{ t("settings.channels.clearSecret") }}
          </button>
        </div>
      </div>
      <div class="field">
        <label>{{ t("settings.channels.chatId") }}</label>
        <input
          class="input"
          v-model="config.channels.telegram.chatId"
          @change="persist"
        />
      </div>
      <div class="field">
        <label>{{ t("settings.channels.apiBaseUrl") }}</label>
        <input
          class="input"
          v-model="config.channels.telegram.apiBaseUrl"
          @change="persist"
        />
      </div>
      <div class="row">
        <button
          class="btn"
          type="button"
          :disabled="telegramTesting"
          @click="runTelegramTest"
        >
          {{
            telegramTesting
              ? t("settings.channels.testing")
              : t("settings.channels.testConnection")
          }}
        </button>
        <span class="spacer"></span>
      </div>
      <p
        v-if="telegramMessage"
        class="result"
        :class="telegramError ? 'err' : 'ok'"
      >
        {{ telegramMessage }}
      </p>
    </template>
  </div>

  <div class="card channel-card channel-card-dingtalk">
    <div class="row">
      <p class="card-title">{{ t("settings.channels.dingtalkTitle") }}</p>
      <a class="link guide-link" href="#" @click.prevent="openChannelGuide('dingding')">
        {{ t("settings.channels.setupGuide") }} ↗
      </a>
      <span class="spacer"></span>
      <label class="switch">
        <input
          type="checkbox"
          v-model="config.channels.dingding.enabled"
          @change="persist"
        />
        <span class="track"></span>
      </label>
    </div>
    <p v-if="channelIssueText('dingding')" class="card-desc warn">
      {{ channelIssueText("dingding") }}
    </p>

    <template v-if="config.channels.dingding.enabled">
      <hr class="divider" />
      <div class="field">
        <label>{{ t("settings.channels.clientId") }}</label>
        <input
          class="input"
          v-model="config.channels.dingding.clientId"
          @change="persist"
        />
      </div>
      <div class="field">
        <label>{{ t("settings.channels.clientSecret") }}</label>
        <div class="row">
          <input
            class="input"
            style="flex: 1"
            type="password"
            :placeholder="
              secretsPresent.dingdingSecret ? SECRET_PLACEHOLDER : ''
            "
            v-model="config.channels.dingding.clientSecret"
            @change="persist"
          />
          <button
            v-if="secretsPresent.dingdingSecret"
            class="btn"
            type="button"
            @click="clearSecret('dingding')"
          >
            {{ t("settings.channels.clearSecret") }}
          </button>
        </div>
      </div>
      <div class="field">
        <label>{{ t("settings.channels.userId") }}</label>
        <div class="row">
          <input
            class="input"
            style="flex: 1"
            v-model="config.channels.dingding.userId"
            @change="persist"
          />
          <button
            class="btn"
            type="button"
            :disabled="dingtalkDetecting"
            @click="runDingtalkDetect"
          >
            {{
              dingtalkDetecting
                ? t("settings.channels.detecting")
                : t("settings.channels.autoDetect")
            }}
          </button>
          <button
            v-if="dingtalkDetecting"
            class="btn"
            type="button"
            @click="cancelDetect"
          >
            {{ t("settings.channels.detectCancel") }}
          </button>
        </div>
      </div>
      <i18n-t
        v-if="dingtalkDetectCode"
        keypath="settings.channels.detectHint"
        tag="p"
        class="result ok"
      >
        <template #code><b>{{ dingtalkDetectCode }}</b></template>
      </i18n-t>
      <div class="row">
        <button
          class="btn"
          type="button"
          :disabled="dingtalkTesting"
          @click="runDingtalkTest"
        >
          {{
            dingtalkTesting
              ? t("settings.channels.testing")
              : t("settings.channels.testConnection")
          }}
        </button>
        <span class="spacer"></span>
      </div>
      <p
        v-if="dingtalkMessage"
        class="result"
        :class="dingtalkError ? 'err' : 'ok'"
      >
        {{ dingtalkMessage }}
      </p>
      <hr class="divider" />
      <div class="field">
        <label>{{ t("settings.channels.cardTemplateId") }}</label>
        <input
          class="input"
          v-model="config.channels.dingding.cardTemplateId"
          :placeholder="t('settings.channels.cardTemplateIdPlaceholder')"
          @change="persist"
        />
      </div>
      <div class="field">
        <label>{{ t("settings.channels.permissionCardTemplateId") }}</label>
        <input
          class="input"
          v-model="config.channels.dingding.permissionConfirmCardTemplateId"
          :placeholder="t('settings.channels.cardTemplateIdPlaceholder')"
          @change="persist"
        />
      </div>
      <hr class="divider" />
      <div class="row">
        <label>{{ t("settings.channels.inlineSmallText") }}</label>
        <span class="spacer"></span>
        <label class="switch">
          <input
            type="checkbox"
            v-model="config.channels.dingding.inlineSmallText"
            @change="persist"
          />
          <span class="track"></span>
        </label>
      </div>
      <p class="card-desc" style="margin-top: 0">
        {{ t("settings.channels.inlineSmallTextHint") }}
      </p>
      <div class="row">
        <label>{{ t("settings.channels.convertTextToDocx") }}</label>
        <span class="spacer"></span>
        <label class="switch">
          <input
            type="checkbox"
            v-model="config.channels.dingding.convertTextToDocx"
            @change="persist"
          />
          <span class="track"></span>
        </label>
      </div>
      <p class="card-desc" style="margin-top: 0">
        {{ t("settings.channels.convertTextToDocxHint") }}
      </p>
    </template>
  </div>

  <div class="card channel-card channel-card-feishu">
    <div class="row">
      <p class="card-title">{{ t("settings.channels.feishuTitle") }}</p>
      <span class="channel-recommended-badge">
        {{ t("settings.channels.recommendedBadge") }}
      </span>
      <a class="link guide-link" href="#" @click.prevent="openChannelGuide('feishu')">
        {{ t("settings.channels.setupGuide") }} ↗
      </a>
      <span class="spacer"></span>
      <label class="switch">
        <input
          type="checkbox"
          v-model="config.channels.feishu.enabled"
          @change="persist"
        />
        <span class="track"></span>
      </label>
    </div>
    <p v-if="channelIssueText('feishu')" class="card-desc warn">
      {{ channelIssueText("feishu") }}
    </p>

    <template v-if="config.channels.feishu.enabled">
      <hr class="divider" />
      <div class="field">
        <label>{{ t("settings.channels.appId") }}</label>
        <input
          class="input"
          v-model="config.channels.feishu.appId"
          @change="persist"
        />
      </div>
      <div class="field">
        <label>{{ t("settings.channels.appSecret") }}</label>
        <div class="row">
          <input
            class="input"
            style="flex: 1"
            type="password"
            :placeholder="
              secretsPresent.feishuSecret ? SECRET_PLACEHOLDER : ''
            "
            v-model="config.channels.feishu.appSecret"
            @change="persist"
          />
          <button
            v-if="secretsPresent.feishuSecret"
            class="btn"
            type="button"
            @click="clearSecret('feishu')"
          >
            {{ t("settings.channels.clearSecret") }}
          </button>
        </div>
      </div>
      <div class="field">
        <label>{{ t("settings.channels.openId") }}</label>
        <div class="row">
          <input
            class="input"
            style="flex: 1"
            v-model="config.channels.feishu.openId"
            @change="persist"
          />
          <button
            class="btn"
            type="button"
            :disabled="feishuDetecting"
            @click="runFeishuDetect"
          >
            {{
              feishuDetecting
                ? t("settings.channels.detecting")
                : t("settings.channels.autoDetect")
            }}
          </button>
          <button
            v-if="feishuDetecting"
            class="btn"
            type="button"
            @click="cancelDetect"
          >
            {{ t("settings.channels.detectCancel") }}
          </button>
        </div>
      </div>
      <i18n-t
        v-if="feishuDetectCode"
        keypath="settings.channels.feishuDetectHint"
        tag="p"
        class="result ok"
      >
        <template #code><b>{{ feishuDetectCode }}</b></template>
      </i18n-t>
      <div class="field">
        <label>{{ t("settings.channels.feishuBaseUrl") }}</label>
        <input
          class="input"
          v-model="config.channels.feishu.baseUrl"
          :placeholder="t('settings.channels.feishuBaseUrlPlaceholder')"
          @change="persist"
        />
      </div>
      <div class="row">
        <button
          class="btn"
          type="button"
          :disabled="feishuTesting"
          @click="runFeishuTest"
        >
          {{
            feishuTesting
              ? t("settings.channels.testing")
              : t("settings.channels.testConnection")
          }}
        </button>
        <span class="spacer"></span>
      </div>
      <p
        v-if="feishuMessage"
        class="result"
        :class="feishuError ? 'err' : 'ok'"
      >
        {{ feishuMessage }}
      </p>
    </template>
  </div>

  <div class="card channel-card channel-card-slack">
    <div class="row">
      <p class="card-title">{{ t("settings.channels.slackTitle") }}</p>
      <a class="link guide-link" href="#" @click.prevent="openChannelGuide('slack')">
        {{ t("settings.channels.setupGuide") }} ↗
      </a>
      <span class="spacer"></span>
      <label class="switch">
        <input
          type="checkbox"
          v-model="config.channels.slack.enabled"
          @change="persist"
        />
        <span class="track"></span>
      </label>
    </div>
    <p v-if="channelIssueText('slack')" class="card-desc warn">
      {{ channelIssueText("slack") }}
    </p>

    <template v-if="config.channels.slack.enabled">
      <hr class="divider" />
      <div class="field">
        <label>{{ t("settings.channels.slackBotToken") }}</label>
        <div class="row">
          <input
            class="input"
            style="flex: 1"
            type="password"
            :placeholder="
              secretsPresent.slackBotToken ? SECRET_PLACEHOLDER : 'xoxb-…'
            "
            v-model="config.channels.slack.botToken"
            @change="persist"
          />
          <button
            v-if="secretsPresent.slackBotToken"
            class="btn"
            type="button"
            @click="clearSecret('slackBot')"
          >
            {{ t("settings.channels.clearSecret") }}
          </button>
        </div>
      </div>
      <div class="field">
        <label>{{ t("settings.channels.slackAppToken") }}</label>
        <div class="row">
          <input
            class="input"
            style="flex: 1"
            type="password"
            :placeholder="
              secretsPresent.slackAppToken ? SECRET_PLACEHOLDER : 'xapp-…'
            "
            v-model="config.channels.slack.appToken"
            @change="persist"
          />
          <button
            v-if="secretsPresent.slackAppToken"
            class="btn"
            type="button"
            @click="clearSecret('slackApp')"
          >
            {{ t("settings.channels.clearSecret") }}
          </button>
        </div>
      </div>
      <div class="field">
        <label>{{ t("settings.channels.slackUserId") }}</label>
        <div class="row">
          <input
            class="input"
            style="flex: 1"
            v-model="config.channels.slack.userId"
            @change="persist"
          />
          <button
            class="btn"
            type="button"
            :disabled="slackDetecting"
            @click="runSlackDetect"
          >
            {{
              slackDetecting
                ? t("settings.channels.detecting")
                : t("settings.channels.autoDetect")
            }}
          </button>
          <button
            v-if="slackDetecting"
            class="btn"
            type="button"
            @click="cancelDetect"
          >
            {{ t("settings.channels.detectCancel") }}
          </button>
        </div>
      </div>
      <i18n-t
        v-if="slackDetectCode"
        keypath="settings.channels.slackDetectHint"
        tag="p"
        class="result ok"
      >
        <template #code><b>{{ slackDetectCode }}</b></template>
      </i18n-t>
      <div class="row">
        <button
          class="btn"
          type="button"
          :disabled="slackTesting"
          @click="runSlackTest"
        >
          {{
            slackTesting
              ? t("settings.channels.testing")
              : t("settings.channels.testConnection")
          }}
        </button>
        <span class="spacer"></span>
      </div>
      <p
        v-if="slackMessage"
        class="result"
        :class="slackError ? 'err' : 'ok'"
      >
        {{ slackMessage }}
      </p>
    </template>
  </div>

</template>
