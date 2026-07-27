//! Telegram Bot API 客户端（手写 reqwest）。

pub mod confirm;
pub mod help;
pub mod markdown;
pub mod router;
pub mod select;
pub mod watch;

use serde_json::{json, Value};
use std::fmt;
use std::time::Duration;

#[derive(Debug)]
pub enum TelegramError {
    EmptyToken,
    EmptyChatId,
    InvalidChatId,
    Api(String),
    Network(String),
    BadResponse,
}

// 源语言(英文) Display：日志/技术细节统一英文；GUI 边界用 `localized()` 取本地化文案。
impl fmt::Display for TelegramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TelegramError::EmptyToken => write!(f, "Bot Token must not be empty"),
            TelegramError::EmptyChatId => write!(f, "Chat ID must not be empty"),
            TelegramError::InvalidChatId => {
                write!(f, "Invalid Chat ID format; enter a valid numeric ID")
            }
            TelegramError::Api(msg) => write!(f, "Telegram API error: {}", msg),
            TelegramError::Network(msg) => write!(f, "network error: {}", msg),
            TelegramError::BadResponse => write!(f, "failed to parse Telegram response"),
        }
    }
}

impl std::error::Error for TelegramError {}

/// 渠道健康登记（R7）：API 调用失败登记、成功清除。放在统一出口，覆盖发送/编辑/上传全部路径。
fn track<T>(r: Result<T, TelegramError>) -> Result<T, TelegramError> {
    match &r {
        Ok(_) => crate::channels::health::clear("telegram"),
        Err(e) => crate::channels::health::report("telegram", e.to_string()),
    }
    r
}

impl TelegramError {
    /// GUI 可见的本地化文案：校验类按界面语言翻译；技术细节(API/网络/解析)保留英文。
    pub fn localized(&self, lang: crate::i18n::Lang) -> String {
        use crate::i18n::tr;
        match self {
            TelegramError::EmptyToken => tr(lang, "err.tgEmptyToken").to_string(),
            TelegramError::EmptyChatId => tr(lang, "err.tgEmptyChatId").to_string(),
            TelegramError::InvalidChatId => tr(lang, "err.tgInvalidChatId").to_string(),
            _ => self.to_string(),
        }
    }
}

pub struct TelegramClient {
    token: String,
    chat_id: i64,
    api_base_url: String,
    http: reqwest::Client,
}

impl TelegramClient {
    pub fn new(
        token: String,
        chat_id_string: String,
        api_base_url: String,
    ) -> Result<Self, TelegramError> {
        let token = token.trim().to_string();
        let chat = chat_id_string.trim().to_string();
        if token.is_empty() {
            return Err(TelegramError::EmptyToken);
        }
        if chat.is_empty() {
            return Err(TelegramError::EmptyChatId);
        }
        if chat.starts_with('@') {
            return Err(TelegramError::InvalidChatId);
        }
        let chat_id: i64 = chat.parse().map_err(|_| TelegramError::InvalidChatId)?;
        let base = api_base_url.trim();
        let api_base_url = if base.is_empty() {
            "https://api.telegram.org".to_string()
        } else {
            base.to_string()
        };
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| TelegramError::Network(e.to_string()))?;
        Ok(Self {
            token,
            chat_id,
            api_base_url,
            http,
        })
    }

    pub fn chat_id(&self) -> i64 {
        self.chat_id
    }

    /// 调用某方法，返回 `result` 字段（成功）或错误（`ok=false`/网络/解析）。
    /// 结果顺带登记渠道健康表（R7：失败可见化，成功即清）。
    async fn call(&self, method: &str, params: Value) -> Result<Value, TelegramError> {
        track(self.call_inner(method, params).await)
    }

    async fn call_inner(&self, method: &str, params: Value) -> Result<Value, TelegramError> {
        let url = format!("{}/bot{}/{}", self.api_base_url, self.token, method);
        let resp = self
            .http
            .post(&url)
            .json(&params)
            .send()
            .await
            .map_err(|e| TelegramError::Network(e.to_string()))?;
        let v: Value = resp.json().await.map_err(|_| TelegramError::BadResponse)?;
        if v.get("ok").and_then(|o| o.as_bool()) == Some(true) {
            Ok(v.get("result").cloned().unwrap_or(Value::Null))
        } else {
            let desc = v
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("request failed")
                .to_string();
            Err(TelegramError::Api(desc))
        }
    }

    /// 发送消息，返回 message_id。
    pub async fn send_message(
        &self,
        text: &str,
        parse_mode: Option<&str>,
        reply_markup: Option<Value>,
    ) -> Result<i64, TelegramError> {
        let mut params = serde_json::Map::new();
        params.insert("chat_id".into(), json!(self.chat_id));
        params.insert("text".into(), json!(text));
        if let Some(pm) = parse_mode {
            params.insert("parse_mode".into(), json!(pm));
        }
        if let Some(rm) = reply_markup {
            params.insert("reply_markup".into(), rm);
        }
        let result = self.call("sendMessage", Value::Object(params)).await?;
        Ok(result
            .get("message_id")
            .and_then(|m| m.as_i64())
            .unwrap_or(0))
    }

    pub async fn send_reply_message(
        &self,
        reply_to_message_id: i64,
        text: &str,
    ) -> Result<i64, TelegramError> {
        let params = json!({
            "chat_id": self.chat_id,
            "text": text,
            "reply_parameters": { "message_id": reply_to_message_id },
        });
        let result = self.call("sendMessage", params).await?;
        Ok(result
            .get("message_id")
            .and_then(Value::as_i64)
            .unwrap_or(0))
    }

    /// 删除 bot 发出的消息（ForceReply 输入提示完成或过期后的清理）。
    pub async fn delete_message(&self, message_id: i64) -> Result<(), TelegramError> {
        self.call(
            "deleteMessage",
            json!({ "chat_id": self.chat_id, "message_id": message_id }),
        )
        .await
        .map(|_| ())
    }

    /// 上传文件（multipart）。`method` 为 sendDocument/sendPhoto，`field` 为 document/photo。
    async fn send_file(
        &self,
        method: &str,
        field: &str,
        path: &str,
        filename: &str,
    ) -> Result<i64, TelegramError> {
        track(self.send_file_inner(method, field, path, filename).await)
    }

    async fn send_file_inner(
        &self,
        method: &str,
        field: &str,
        path: &str,
        filename: &str,
    ) -> Result<i64, TelegramError> {
        let bytes = std::fs::read(path)
            .map_err(|e| TelegramError::Network(format!("failed to read file: {}", e)))?;
        let part = reqwest::multipart::Part::bytes(bytes).file_name(filename.to_string());
        let form = reqwest::multipart::Form::new()
            .text("chat_id", self.chat_id.to_string())
            .part(field.to_string(), part);
        let url = format!("{}/bot{}/{}", self.api_base_url, self.token, method);
        let resp = self
            .http
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| TelegramError::Network(e.to_string()))?;
        let v: Value = resp.json().await.map_err(|_| TelegramError::BadResponse)?;
        if v.get("ok").and_then(|o| o.as_bool()) == Some(true) {
            Ok(v.get("result")
                .and_then(|r| r.get("message_id"))
                .and_then(|m| m.as_i64())
                .unwrap_or(0))
        } else {
            let desc = v
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("request failed")
                .to_string();
            Err(TelegramError::Api(desc))
        }
    }

    /// 以文档形式发送文件。
    pub async fn send_document(&self, path: &str, filename: &str) -> Result<i64, TelegramError> {
        self.send_file("sendDocument", "document", path, filename)
            .await
    }

    /// 以图片形式发送文件（可内联预览）。
    pub async fn send_photo(&self, path: &str, filename: &str) -> Result<i64, TelegramError> {
        self.send_file("sendPhoto", "photo", path, filename).await
    }

    /// 拉取更新。`timeout_secs` 为服务端长轮询挂起秒数（0 = 立即返回）。
    /// Router 用 25s 长轮询，既降负载又能近实时收到回调/消息。
    pub async fn get_updates(
        &self,
        offset: i64,
        timeout_secs: u64,
    ) -> Result<Vec<Value>, TelegramError> {
        let result = self
            .call(
                "getUpdates",
                json!({ "offset": offset, "timeout": timeout_secs }),
            )
            .await?;
        Ok(result.as_array().cloned().unwrap_or_default())
    }

    pub async fn answer_callback_query(&self, id: &str) {
        let _ = self
            .call("answerCallbackQuery", json!({ "callback_query_id": id }))
            .await;
    }

    /// 应答 callback 并弹出 alert 提示（用于严格模式下空提交的拦截）。
    pub async fn answer_callback_query_alert(&self, id: &str, text: &str) {
        let _ = self
            .call(
                "answerCallbackQuery",
                json!({ "callback_query_id": id, "text": text, "show_alert": true }),
            )
            .await;
    }

    pub async fn edit_message_reply_markup(&self, message_id: i64, markup: Value) {
        let _ = self
            .call(
                "editMessageReplyMarkup",
                json!({ "chat_id": self.chat_id, "message_id": message_id, "reply_markup": markup }),
            )
            .await;
    }

    /// 编辑消息文本（卡片终态 / watch 卡就地更新）。`editMessageText` 会整体替换消息：
    /// 不传 `reply_markup` 即移除按钮（终态用），传入则保留/替换按钮（watch 活动态用）。
    /// 返回 `Err` 表示服务端拒绝（如 HTML 解析失败），调用方可据此回退纯文本。
    pub async fn edit_message_text(
        &self,
        message_id: i64,
        text: &str,
        parse_mode: Option<&str>,
        reply_markup: Option<Value>,
    ) -> Result<(), TelegramError> {
        let mut params = serde_json::Map::new();
        params.insert("chat_id".into(), json!(self.chat_id));
        params.insert("message_id".into(), json!(message_id));
        params.insert("text".into(), json!(text));
        if let Some(pm) = parse_mode {
            params.insert("parse_mode".into(), json!(pm));
        }
        if let Some(rm) = reply_markup {
            params.insert("reply_markup".into(), rm);
        }
        self.call("editMessageText", Value::Object(params))
            .await
            .map(|_| ())
    }

    /// 发送测试消息验证配置（`lang` 决定远程消息与返回提示的语言）。
    pub async fn test_connection(&self, lang: crate::i18n::Lang) -> Result<String, TelegramError> {
        let text = crate::i18n::tr(lang, "cmd.tgTestRemote");
        self.send_message(text, None, None).await?;
        Ok(crate::i18n::tr(lang, "cmd.tgTestSent").to_string())
    }
}
