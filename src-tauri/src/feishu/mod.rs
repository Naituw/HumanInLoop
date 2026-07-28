//! 飞书（Feishu / Lark）OpenAPI / 长连接客户端层。
//!
//! 形态：企业自建应用 + 机器人 + 长连接(WebSocket)模式 + 单聊。
//! - 鉴权：`token`（tenant_access_token 缓存）。
//! - 发送：`client`（单聊文本/图片/文件、互动卡片 JSON、媒体上传、消息资源下载、卡片更新）。
//! - 卡片：`card`（卡片 JSON 2.0 组装 + card.action.trigger 回调解析）。
//! - 接收：`ws`（长连接：protobuf 帧 pbbp2；事件 im.message.receive_v1 + 卡片回调 card.action.trigger）。
//!
//! 与钉钉差异：长连接帧是 protobuf（非 JSON），且订阅 topic 由开发者后台配置（建连不声明 topic）。

pub mod card;
pub mod client;
pub mod router;
pub mod token;
pub mod ws;

use std::fmt;

#[derive(Debug)]
pub enum FeishuError {
    /// 配置缺失（附字段名提示）。
    EmptyConfig(String),
    /// 飞书接口返回业务错误；保留数值 code 供鉴权恢复等逻辑可靠判断。
    Api {
        code: Option<i64>,
        message: String,
        http_status: Option<u16>,
        log_id: Option<String>,
    },
    /// 网络错误。
    Network(String),
    /// 响应无法解析。
    BadResponse {
        http_status: Option<u16>,
        log_id: Option<String>,
    },
}

// 源语言(英文) Display：日志/技术细节统一英文；GUI 边界用 `localized()` 取本地化文案。
impl fmt::Display for FeishuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FeishuError::EmptyConfig(field) => write!(f, "{} must not be empty", field),
            FeishuError::Api {
                code,
                message,
                http_status,
                log_id,
            } => {
                write!(f, "Feishu API error")?;
                write_response_metadata(f, *code, *http_status, log_id.as_deref())?;
                write!(f, ": {}", message)
            }
            FeishuError::Network(msg) => write!(f, "network error: {}", msg),
            FeishuError::BadResponse {
                http_status,
                log_id,
            } => {
                write!(f, "failed to parse Feishu response")?;
                write_response_metadata(f, None, *http_status, log_id.as_deref())
            }
        }
    }
}

impl std::error::Error for FeishuError {}

impl FeishuError {
    pub(crate) fn api_response(
        code: Option<i64>,
        message: impl Into<String>,
        http_status: Option<u16>,
        log_id: Option<String>,
    ) -> Self {
        Self::Api {
            code,
            message: message.into(),
            http_status,
            log_id,
        }
    }

    pub(crate) fn bad_response() -> Self {
        Self::bad_response_with_metadata(None, None)
    }

    pub(crate) fn bad_response_with_metadata(
        http_status: Option<u16>,
        log_id: Option<String>,
    ) -> Self {
        Self::BadResponse {
            http_status,
            log_id,
        }
    }

    /// These codes identify an invalid tenant token for APIs that AskHuman always calls with a
    /// tenant token. Keep this code-based: Feishu documents `msg` as unstable display text.
    pub(crate) fn is_invalid_tenant_token(&self) -> bool {
        matches!(
            self,
            Self::Api {
                code: Some(99991663 | 99991665),
                ..
            }
        )
    }

    /// Message creation retries must remain narrow: retry only failures that can plausibly clear
    /// without changing the payload. Rate limits are deliberately excluded.
    pub(crate) fn is_transient_message_create_failure(&self) -> bool {
        match self {
            Self::Network(_) | Self::BadResponse { .. } => true,
            Self::Api {
                code,
                message,
                http_status,
                ..
            } => {
                if matches!(code, Some(230020 | 11232 | 11233 | 99991400)) {
                    return false;
                }
                matches!(http_status, Some(408 | 425 | 500..=599))
                    || message
                        .trim()
                        .trim_end_matches(['.', ';'])
                        .to_ascii_lowercase()
                        .starts_with("internal error")
            }
            Self::EmptyConfig(_) => false,
        }
    }

    /// GUI 可见的本地化文案：校验类按界面语言翻译；技术细节(API/网络/解析)保留英文。
    pub fn localized(&self, lang: crate::i18n::Lang) -> String {
        match self {
            FeishuError::EmptyConfig(field) => {
                crate::i18n::tr(lang, "err.fsEmptyConfig").replace("{field}", field)
            }
            _ => self.to_string(),
        }
    }
}

fn write_response_metadata(
    f: &mut fmt::Formatter<'_>,
    code: Option<i64>,
    http_status: Option<u16>,
    log_id: Option<&str>,
) -> fmt::Result {
    let mut fields = Vec::new();
    if let Some(code) = code {
        fields.push(format!("code={code}"));
    }
    if let Some(http_status) = http_status {
        fields.push(format!("http={http_status}"));
    }
    if let Some(log_id) = log_id.filter(|value| !value.is_empty()) {
        fields.push(format!("log_id={log_id}"));
    }
    if fields.is_empty() {
        Ok(())
    } else {
        write!(f, " ({})", fields.join(", "))
    }
}

pub(crate) fn response_log_id(headers: &reqwest::header::HeaderMap) -> Option<String> {
    ["x-tt-logid", "x-request-id"].into_iter().find_map(|name| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

#[cfg(test)]
mod tests {
    use super::FeishuError;

    #[test]
    fn transient_message_create_failures_are_narrowly_classified() {
        assert!(FeishuError::Network("timeout".into()).is_transient_message_create_failure());
        assert!(
            FeishuError::bad_response_with_metadata(Some(502), Some("log-1".into()))
                .is_transient_message_create_failure()
        );
        assert!(FeishuError::api_response(
            Some(230001),
            "Internal Error",
            Some(400),
            Some("log-2".into()),
        )
        .is_transient_message_create_failure());
        assert!(
            FeishuError::api_response(Some(1), "upstream", Some(503), None)
                .is_transient_message_create_failure()
        );

        assert!(!FeishuError::api_response(
            Some(230020),
            "This operation triggers the frequency limit",
            Some(500),
            None,
        )
        .is_transient_message_create_failure());
        assert!(!FeishuError::api_response(
            Some(230006),
            "Bot ability is not activated",
            Some(400),
            None,
        )
        .is_transient_message_create_failure());
    }

    #[test]
    fn response_metadata_is_rendered_for_diagnostics() {
        let error = FeishuError::api_response(
            Some(230001),
            "Internal Error",
            Some(400),
            Some("20260728083612ABC".into()),
        );
        assert_eq!(
            error.to_string(),
            "Feishu API error (code=230001, http=400, log_id=20260728083612ABC): Internal Error"
        );
    }
}
