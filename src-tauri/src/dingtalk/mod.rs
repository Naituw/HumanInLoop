//! 钉钉 OpenAPI / Stream 客户端层。
//!
//! 形态：企业内部应用 + 机器人 + Stream 模式 + 单聊。
//! - 鉴权：`token`（access_token 缓存，新旧接口同一 token）。
//! - 发送：`client`（单聊文本/图片/文件、媒体上传、消息文件下载、互动卡片发送/更新）。
//! - 卡片：`card`（StandardCard cardData 构造 + 回调解析）。
//! - 接收：`stream`（Stream 长连接：bot 消息 + 卡片回调）。
//!
//! robotCode 不单独配置：企业内部应用机器人 robotCode = clientId(AppKey)。

pub mod card;
pub mod client;
pub mod confirm;
pub mod docx;
pub mod help;
pub mod image_convert;
pub mod router;
pub mod select;
pub mod stream;
pub mod textfile;
pub mod token;
pub mod watch;

use std::fmt;

/// DingTalk OpenAPI base (`https://api.dingtalk.com`). Overridable via `ASKHUMAN_DINGTALK_API_BASE`
/// for tests/CI (the perf harness points it at a local mock IM); unset → the real endpoint, so
/// production behaviour is unchanged. Covers token, Stream `connections/open` and card OpenAPI calls.
pub fn api_base() -> String {
    std::env::var("ASKHUMAN_DINGTALK_API_BASE")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "https://api.dingtalk.com".to_string())
}

#[derive(Debug)]
pub enum DingTalkError {
    /// 配置缺失（附字段名提示）。
    EmptyConfig(String),
    /// 钉钉接口返回错误（错误信息）。
    Api(String),
    /// 网络错误。
    Network(String),
    /// 响应无法解析。
    BadResponse,
}

// 源语言(英文) Display：日志/技术细节统一英文；GUI 边界用 `localized()` 取本地化文案。
impl fmt::Display for DingTalkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DingTalkError::EmptyConfig(field) => write!(f, "{} must not be empty", field),
            DingTalkError::Api(msg) => write!(f, "DingTalk API error: {}", msg),
            DingTalkError::Network(msg) => write!(f, "network error: {}", msg),
            DingTalkError::BadResponse => write!(f, "failed to parse DingTalk response"),
        }
    }
}

impl std::error::Error for DingTalkError {}

impl DingTalkError {
    /// GUI 可见的本地化文案：校验类按界面语言翻译；技术细节(API/网络/解析)保留英文。
    pub fn localized(&self, lang: crate::i18n::Lang) -> String {
        match self {
            DingTalkError::EmptyConfig(field) => {
                crate::i18n::tr(lang, "err.ddEmptyConfig").replace("{field}", field)
            }
            _ => self.to_string(),
        }
    }
}
