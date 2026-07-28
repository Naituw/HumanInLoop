//! 飞书单聊机器人 OpenAPI 客户端（reqwest）。
//!
//! 鉴权统一用 `tenant_access_token`（Bearer）。发消息 `receive_id_type=open_id`。
//! 互动卡片直接以 JSON 下发（`msg_type=interactive`，content 即卡片 JSON）。

use super::token;
use super::FeishuError;
use crate::config::FeishuChannelConfig;
use serde_json::{json, Value};
use std::time::Duration;

const AUTH_ATTEMPTS: usize = 2;
const CARD_SEND_ATTEMPTS: usize = 2;
#[cfg(not(test))]
const CARD_RETRY_DELAY: Duration = Duration::from_millis(400);
#[cfg(test)]
const CARD_RETRY_DELAY: Duration = Duration::from_millis(1);

/// 渠道健康登记（R7）：API 调用失败登记、成功清除。放在统一出口，覆盖发送/编辑/上传全部路径。
fn track<T>(r: Result<T, FeishuError>) -> Result<T, FeishuError> {
    match &r {
        Ok(_) => crate::channels::health::clear("feishu"),
        Err(e) => crate::channels::health::report("feishu", e.to_string()),
    }
    r
}

#[derive(Clone)]
pub struct FeishuClient {
    app_id: String,
    app_secret: String,
    base_url: String,
    open_id: String,
    http: reqwest::Client,
}

impl FeishuClient {
    /// 构造客户端：校验 AppId/AppSecret（open_id 允许为空，自动识别流程不需要）。
    pub fn new(config: &FeishuChannelConfig) -> Result<Self, FeishuError> {
        let app_id = config.app_id.trim().to_string();
        let app_secret = config.app_secret.trim().to_string();
        let base_url = {
            let b = config.base_url.trim().trim_end_matches('/');
            if b.is_empty() {
                "https://open.feishu.cn".to_string()
            } else {
                b.to_string()
            }
        };
        if app_id.is_empty() {
            return Err(FeishuError::EmptyConfig("AppId".into()));
        }
        if app_secret.is_empty() {
            return Err(FeishuError::EmptyConfig("AppSecret".into()));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| FeishuError::Network(e.to_string()))?;
        Ok(Self {
            app_id,
            app_secret,
            base_url,
            open_id: config.open_id.trim().to_string(),
            http,
        })
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub fn open_id(&self) -> &str {
        &self.open_id
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    pub fn app_secret(&self) -> &str {
        &self.app_secret
    }

    async fn token(&self) -> Result<String, FeishuError> {
        token::get_token(&self.http, &self.base_url, &self.app_id, &self.app_secret).await
    }

    fn invalidate_rejected_token(&self, rejected_token: &str) {
        token::invalidate_if_matches(
            &self.base_url,
            &self.app_id,
            &self.app_secret,
            rejected_token,
        );
    }

    async fn refresh_rejected_token(&self, rejected_token: &str) -> Result<String, FeishuError> {
        self.invalidate_rejected_token(rejected_token);
        self.token().await
    }

    /// 仅校验凭据（换取一次 token）。供「测试连接」用。
    pub async fn verify(&self) -> Result<(), FeishuError> {
        self.token().await.map(|_| ())
    }

    /// 通用 JSON 调用：Bearer 鉴权 + 业务码 code==0 判定成功。结果顺带登记渠道健康表（R7）。
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Value,
    ) -> Result<Value, FeishuError> {
        track(self.call_inner(method, path, body).await)
    }

    async fn call_inner(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Value,
    ) -> Result<Value, FeishuError> {
        let mut access_token = self.token().await?;
        for attempt in 0..AUTH_ATTEMPTS {
            let resp = self
                .http
                .request(method.clone(), format!("{}{}", self.base_url, path))
                .bearer_auth(&access_token)
                .json(&body)
                .send()
                .await
                .map_err(|e| FeishuError::Network(e.to_string()))?;
            let http_status = resp.status().as_u16();
            let log_id = super::response_log_id(resp.headers());
            let value: Value = resp.json().await.map_err(|_| {
                FeishuError::bad_response_with_metadata(Some(http_status), log_id.clone())
            })?;
            match api_result(value, "request failed", Some(http_status), log_id) {
                Ok(value) => return Ok(value),
                Err(error) if attempt == 0 && error.is_invalid_tenant_token() => {
                    access_token = self.refresh_rejected_token(&access_token).await?;
                }
                Err(error) => {
                    if error.is_invalid_tenant_token() {
                        self.invalidate_rejected_token(&access_token);
                    }
                    return Err(error);
                }
            }
        }
        unreachable!("Feishu authentication attempts are bounded")
    }

    // ===== 单聊主动发送（im/v1/messages, receive_id_type=open_id）=====

    /// 发送一条消息，返回 message_id（卡片后续 PATCH 收尾用）。`content` 会被序列化为 JSON 字符串。
    async fn send_message(
        &self,
        msg_type: &str,
        content: &Value,
        retry_transient: bool,
    ) -> Result<String, FeishuError> {
        let mut body = json!({
            "receive_id": self.open_id,
            "msg_type": msg_type,
            "content": content.to_string(),
        });
        if retry_transient {
            // Feishu deduplicates message creations with the same UUID for one hour. Reuse this
            // value across attempts so an ambiguous transport failure cannot create two cards.
            body["uuid"] = Value::String(uuid::Uuid::new_v4().to_string());
        }

        let result = async {
            for attempt in 0..CARD_SEND_ATTEMPTS {
                match self
                    .call_inner(
                        reqwest::Method::POST,
                        "/open-apis/im/v1/messages?receive_id_type=open_id",
                        body.clone(),
                    )
                    .await
                {
                    Ok(value) => return Ok(value),
                    Err(error)
                        if retry_transient
                            && attempt == 0
                            && error.is_transient_message_create_failure() =>
                    {
                        eprintln!(
                            "[feishu] transient card creation failure; retrying once after {}ms: {}",
                            CARD_RETRY_DELAY.as_millis(),
                            error
                        );
                        tokio::time::sleep(CARD_RETRY_DELAY).await;
                    }
                    Err(error) => return Err(error),
                }
            }
            unreachable!("Feishu card creation attempts are bounded")
        }
        .await;
        let v = track(result)?;
        Ok(v.get("data")
            .and_then(|d| d.get("message_id"))
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub async fn send_text(&self, text: &str) -> Result<String, FeishuError> {
        self.send_message("text", &json!({ "text": text }), false)
            .await
    }

    /// 发送互动卡片（卡片 JSON 直接作为 content）。返回 message_id。
    pub async fn send_card(&self, card: &Value) -> Result<String, FeishuError> {
        self.send_message("interactive", card, true).await
    }

    pub async fn send_image(&self, image_key: &str) -> Result<String, FeishuError> {
        self.send_message("image", &json!({ "image_key": image_key }), false)
            .await
    }

    pub async fn send_file(&self, file_key: &str) -> Result<String, FeishuError> {
        self.send_message("file", &json!({ "file_key": file_key }), false)
            .await
    }

    /// PATCH 更新已发送的卡片消息（收尾灰显 / 抢答收尾）。`card` 为完整卡片 JSON。
    pub async fn patch_card(&self, message_id: &str, card: &Value) -> Result<(), FeishuError> {
        let body = json!({ "content": card.to_string() });
        self.call(
            reqwest::Method::PATCH,
            &format!("/open-apis/im/v1/messages/{}", message_id),
            body,
        )
        .await?;
        Ok(())
    }

    // ===== 媒体上传（multipart）=====

    /// 上传图片，返回 image_key。
    pub async fn upload_image(&self, path: &str) -> Result<String, FeishuError> {
        let bytes = std::fs::read(path)
            .map_err(|e| FeishuError::Network(format!("failed to read file: {}", e)))?;
        let name = file_name_of(path);
        let v = self
            .upload("/open-apis/im/v1/images", || {
                let part = reqwest::multipart::Part::bytes(bytes.clone()).file_name(name.clone());
                reqwest::multipart::Form::new()
                    .text("image_type", "message")
                    .part("image", part)
            })
            .await?;
        v.get("data")
            .and_then(|d| d.get("image_key"))
            .and_then(|m| m.as_str())
            .map(|s| s.to_string())
            .ok_or_else(FeishuError::bad_response)
    }

    /// 上传文件，返回 file_key。
    /// `file_type`：飞书要求枚举（opus/mp4/pdf/doc/xls/ppt/stream）；docx/xlsx/pptx 用
    /// doc/xls/ppt 以利预览，其余 `stream`。
    pub async fn upload_file(&self, path: &str, file_name: &str) -> Result<String, FeishuError> {
        let bytes = std::fs::read(path)
            .map_err(|e| FeishuError::Network(format!("failed to read file: {}", e)))?;
        let ext = std::path::Path::new(file_name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let file_type = match ext.as_str() {
            "pdf" => "pdf",
            "doc" | "docx" => "doc",
            "xls" | "xlsx" => "xls",
            "ppt" | "pptx" => "ppt",
            "mp4" => "mp4",
            "opus" => "opus",
            _ => "stream",
        };
        let v = self
            .upload("/open-apis/im/v1/files", || {
                let part =
                    reqwest::multipart::Part::bytes(bytes.clone()).file_name(file_name.to_string());
                reqwest::multipart::Form::new()
                    .text("file_type", file_type)
                    .text("file_name", file_name.to_string())
                    .part("file", part)
            })
            .await?;
        v.get("data")
            .and_then(|d| d.get("file_key"))
            .and_then(|m| m.as_str())
            .map(|s| s.to_string())
            .ok_or_else(FeishuError::bad_response)
    }

    async fn upload<F>(&self, path: &str, build_form: F) -> Result<Value, FeishuError>
    where
        F: FnMut() -> reqwest::multipart::Form,
    {
        track(self.upload_inner(path, build_form).await)
    }

    async fn upload_inner<F>(&self, path: &str, mut build_form: F) -> Result<Value, FeishuError>
    where
        F: FnMut() -> reqwest::multipart::Form,
    {
        let mut access_token = self.token().await?;
        for attempt in 0..AUTH_ATTEMPTS {
            let resp = self
                .http
                .post(format!("{}{}", self.base_url, path))
                .bearer_auth(&access_token)
                .multipart(build_form())
                .send()
                .await
                .map_err(|e| FeishuError::Network(e.to_string()))?;
            let http_status = resp.status().as_u16();
            let log_id = super::response_log_id(resp.headers());
            let value: Value = resp.json().await.map_err(|_| {
                FeishuError::bad_response_with_metadata(Some(http_status), log_id.clone())
            })?;
            match api_result(value, "upload failed", Some(http_status), log_id) {
                Ok(value) => return Ok(value),
                Err(error) if attempt == 0 && error.is_invalid_tenant_token() => {
                    access_token = self.refresh_rejected_token(&access_token).await?;
                }
                Err(error) => {
                    if error.is_invalid_tenant_token() {
                        self.invalidate_rejected_token(&access_token);
                    }
                    return Err(error);
                }
            }
        }
        unreachable!("Feishu authentication attempts are bounded")
    }

    // ===== 接收消息资源下载 =====

    /// 下载消息里的图片/文件资源到临时文件，返回本地路径。
    /// `kind` 为 `image` / `file`；`key` 为 image_key / file_key；`ext` 为期望扩展名。
    pub async fn download_resource_to(
        &self,
        message_id: &str,
        key: &str,
        kind: &str,
        ext: &str,
    ) -> Result<String, FeishuError> {
        let url = format!(
            "{}/open-apis/im/v1/messages/{}/resources/{}?type={}",
            self.base_url, message_id, key, kind
        );
        let bytes = self.download_resource(&url).await?;

        let dir = std::env::temp_dir().join("askhuman-feishu");
        std::fs::create_dir_all(&dir)
            .map_err(|e| FeishuError::Network(format!("failed to create temp dir: {}", e)))?;
        let ext = ext.trim_start_matches('.');
        let name = if ext.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            format!("{}.{}", uuid::Uuid::new_v4(), ext)
        };
        let dest = dir.join(name);
        std::fs::write(&dest, &bytes)
            .map_err(|e| FeishuError::Network(format!("failed to write temp file: {}", e)))?;
        Ok(dest.to_string_lossy().to_string())
    }

    async fn download_resource(&self, url: &str) -> Result<Vec<u8>, FeishuError> {
        let mut access_token = self.token().await?;
        for attempt in 0..AUTH_ATTEMPTS {
            let resp = self
                .http
                .get(url)
                .bearer_auth(&access_token)
                .send()
                .await
                .map_err(|e| FeishuError::Network(e.to_string()))?;
            let status = resp.status();
            let log_id = super::response_log_id(resp.headers());
            let is_json = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.contains("json"));
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| FeishuError::Network(e.to_string()))?;

            let api_error = if is_json || !status.is_success() {
                serde_json::from_slice::<Value>(&bytes)
                    .ok()
                    .and_then(|value| {
                        api_error(
                            &value,
                            "resource download failed",
                            Some(status.as_u16()),
                            log_id.clone(),
                        )
                    })
            } else {
                None
            };
            match api_error {
                Some(error) if attempt == 0 && error.is_invalid_tenant_token() => {
                    access_token = self.refresh_rejected_token(&access_token).await?;
                    continue;
                }
                Some(error) => {
                    if error.is_invalid_tenant_token() {
                        self.invalidate_rejected_token(&access_token);
                    }
                    return Err(error);
                }
                None if !status.is_success() => {
                    return Err(FeishuError::api_response(
                        None,
                        format!("resource download failed: HTTP {}", status),
                        Some(status.as_u16()),
                        log_id,
                    ));
                }
                None => return Ok(bytes.to_vec()),
            }
        }
        unreachable!("Feishu authentication attempts are bounded")
    }
}

fn api_result(
    value: Value,
    fallback: &str,
    http_status: Option<u16>,
    log_id: Option<String>,
) -> Result<Value, FeishuError> {
    match api_error(&value, fallback, http_status, log_id.clone()) {
        Some(error) => Err(error),
        None if value.get("code").and_then(|code| code.as_i64()) == Some(0) => Ok(value),
        None => Err(FeishuError::bad_response_with_metadata(http_status, log_id)),
    }
}

fn api_error(
    value: &Value,
    fallback: &str,
    http_status: Option<u16>,
    log_id: Option<String>,
) -> Option<FeishuError> {
    let code = value.get("code").and_then(|code| code.as_i64())?;
    if code == 0 {
        return None;
    }
    let message = value
        .get("msg")
        .and_then(|message| message.as_str())
        .unwrap_or(fallback);
    Some(FeishuError::api_response(
        Some(code),
        message,
        http_status,
        log_id,
    ))
}

fn file_name_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    struct MockResponse {
        status: &'static str,
        content_type: &'static str,
        headers: Vec<(&'static str, &'static str)>,
        body: Vec<u8>,
    }

    impl MockResponse {
        fn json(body: &'static str) -> Self {
            Self {
                status: "200 OK",
                content_type: "application/json",
                headers: Vec::new(),
                body: body.as_bytes().to_vec(),
            }
        }

        fn json_error(
            status: &'static str,
            body: &'static str,
            headers: Vec<(&'static str, &'static str)>,
        ) -> Self {
            Self {
                status,
                content_type: "application/json",
                headers,
                body: body.as_bytes().to_vec(),
            }
        }

        fn bytes(body: &'static [u8]) -> Self {
            Self {
                status: "200 OK",
                content_type: "application/octet-stream",
                headers: Vec::new(),
                body: body.to_vec(),
            }
        }
    }

    async fn mock_server(
        responses: Vec<MockResponse>,
    ) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let request = read_request(&mut socket).await;
                recorded.lock().unwrap().push(request);
                let extra_headers = response
                    .headers
                    .iter()
                    .map(|(name, value)| format!("{name}: {value}\r\n"))
                    .collect::<String>();
                let header = format!(
                    "HTTP/1.1 {}\r\nContent-Type: {}\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    response.status,
                    response.content_type,
                    extra_headers,
                    response.body.len()
                );
                socket.write_all(header.as_bytes()).await.unwrap();
                socket.write_all(&response.body).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        (base_url, requests, task)
    }

    async fn read_request(socket: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..read]);
            let Some(header_end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if bytes.len() >= header_end + 4 + content_length {
                break;
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn json_body(request: &str) -> Value {
        serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap()
    }

    fn config(base_url: &str) -> FeishuChannelConfig {
        FeishuChannelConfig {
            enabled: true,
            app_id: "app-client-test".into(),
            app_secret: "secret-client-test".into(),
            open_id: "user-client-test".into(),
            base_url: base_url.into(),
        }
    }

    #[tokio::test]
    async fn invalid_token_refreshes_and_retries_json_request_once() {
        let (base_url, requests, server) = mock_server(vec![
            MockResponse::json(r#"{"code":0,"tenant_access_token":"old-token","expire":7200}"#),
            MockResponse::json(
                r#"{"code":99991663,"msg":"Invalid access token for authorization"}"#,
            ),
            MockResponse::json(r#"{"code":0,"tenant_access_token":"fresh-token","expire":7200}"#),
            MockResponse::json(r#"{"code":0,"data":{"message_id":"message-1"}}"#),
        ])
        .await;
        let client = FeishuClient::new(&config(&base_url)).unwrap();

        assert_eq!(client.send_text("hello").await.unwrap(), "message-1");
        server.await.unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert!(requests[0].contains("/open-apis/auth/v3/tenant_access_token/internal"));
        assert!(requests[1]
            .to_ascii_lowercase()
            .contains("authorization: bearer old-token"));
        assert!(requests[2].contains("/open-apis/auth/v3/tenant_access_token/internal"));
        assert!(requests[3]
            .to_ascii_lowercase()
            .contains("authorization: bearer fresh-token"));
    }

    #[tokio::test]
    async fn second_invalid_token_is_reported_without_another_retry() {
        let (base_url, requests, server) = mock_server(vec![
            MockResponse::json(r#"{"code":0,"tenant_access_token":"old-token","expire":7200}"#),
            MockResponse::json(r#"{"code":99991663,"msg":"token invalid"}"#),
            MockResponse::json(r#"{"code":0,"tenant_access_token":"fresh-token","expire":7200}"#),
            MockResponse::json(r#"{"code":99991663,"msg":"still invalid"}"#),
        ])
        .await;
        let client = FeishuClient::new(&config(&base_url)).unwrap();

        let error = client.send_text("hello").await.unwrap_err();
        assert!(matches!(
            error,
            FeishuError::Api {
                code: Some(99991663),
                ..
            }
        ));
        server.await.unwrap();
        assert_eq!(requests.lock().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn transient_card_failure_retries_once_with_the_same_uuid() {
        let (base_url, requests, server) = mock_server(vec![
            MockResponse::json(r#"{"code":0,"tenant_access_token":"token","expire":7200}"#),
            MockResponse::json_error(
                "400 Bad Request",
                r#"{"code":230001,"msg":"Internal Error"}"#,
                vec![("X-Tt-Logid", "log-internal-error")],
            ),
            MockResponse::json(r#"{"code":0,"data":{"message_id":"card-1"}}"#),
        ])
        .await;
        let client = FeishuClient::new(&config(&base_url)).unwrap();

        assert_eq!(
            client
                .send_card(&serde_json::json!({"schema":"2.0"}))
                .await
                .unwrap(),
            "card-1"
        );
        server.await.unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        let first = json_body(&requests[1]);
        let second = json_body(&requests[2]);
        let request_uuid = first["uuid"].as_str().unwrap();
        assert_eq!(request_uuid.len(), 36);
        assert_eq!(second["uuid"], request_uuid);
        assert_eq!(first["msg_type"], "interactive");
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn permanent_card_failure_is_not_retried_and_keeps_response_metadata() {
        let (base_url, requests, server) = mock_server(vec![
            MockResponse::json(r#"{"code":0,"tenant_access_token":"token","expire":7200}"#),
            MockResponse::json_error(
                "400 Bad Request",
                r#"{"code":200621,"msg":"Failed to create card content"}"#,
                vec![("X-Tt-Logid", "log-invalid-card")],
            ),
        ])
        .await;
        let client = FeishuClient::new(&config(&base_url)).unwrap();

        let error = client
            .send_card(&serde_json::json!({"schema":"2.0"}))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Feishu API error (code=200621, http=400, log_id=log-invalid-card): Failed to create card content"
        );
        server.await.unwrap();
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn upload_and_download_rebuild_requests_after_token_refresh() {
        let (base_url, requests, server) = mock_server(vec![
            MockResponse::json(r#"{"code":0,"tenant_access_token":"upload-old","expire":7200}"#),
            MockResponse::json(r#"{"code":99991663,"msg":"token invalid"}"#),
            MockResponse::json(r#"{"code":0,"tenant_access_token":"upload-fresh","expire":7200}"#),
            MockResponse::json(r#"{"code":0,"data":{"file_key":"file-1"}}"#),
            MockResponse::json(r#"{"code":99991663,"msg":"token invalid"}"#),
            MockResponse::json(
                r#"{"code":0,"tenant_access_token":"download-fresh","expire":7200}"#,
            ),
            MockResponse::bytes(b"downloaded-file"),
        ])
        .await;
        let client = FeishuClient::new(&config(&base_url)).unwrap();
        let upload = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(upload.path(), b"upload-body").unwrap();

        assert_eq!(
            client
                .upload_file(upload.path().to_str().unwrap(), "report.txt")
                .await
                .unwrap(),
            "file-1"
        );
        assert_eq!(
            client
                .download_resource(&format!("{base_url}/download"))
                .await
                .unwrap(),
            b"downloaded-file"
        );
        server.await.unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 7);
        assert!(requests[1].contains("POST /open-apis/im/v1/files"));
        assert!(requests[1]
            .to_ascii_lowercase()
            .contains("authorization: bearer upload-old"));
        assert!(requests[3]
            .to_ascii_lowercase()
            .contains("authorization: bearer upload-fresh"));
        assert!(requests[4].contains("GET /download"));
        assert!(requests[4]
            .to_ascii_lowercase()
            .contains("authorization: bearer upload-fresh"));
        assert!(requests[6]
            .to_ascii_lowercase()
            .contains("authorization: bearer download-fresh"));
    }
}
