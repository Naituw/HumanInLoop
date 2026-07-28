//! 飞书长连接（WebSocket）：protobuf 帧（pbbp2）收事件 + 卡片回调（零公网）。
//!
//! 协议（对齐官方 SDK lark-oapi）：
//! 1. `POST {base}/callback/ws/endpoint {AppID,AppSecret}` → `data.URL`(wss，query 带 service_id) + `ClientConfig`。
//! 2. 连 wss；帧为 protobuf `PbFrame`。`method=0` 控制帧(ping/pong)，`method=1` 数据帧(JSON 业务)。
//! 3. 客户端按 `PingInterval` 主动发 ping 帧维持心跳；pong 帧 payload 可带新的 ClientConfig。
//! 4. 数据帧 header：`type`(event/card)、`message_id`、`sum`/`seq`(大消息分片，按 message_id 重组)。
//!    收到后须 **3 秒内回包**：回一个同 message_id 的帧，payload = `{"code":200[, "data":<base64(JSON)>]}`。
//!    事件回空 ACK（无 data）；卡片回调回 `{toast,...}`（base64 进 data）。

use super::FeishuError;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use prost::Message as ProstMessage;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

const GEN_ENDPOINT_URI: &str = "/callback/ws/endpoint";
const HEADER_TYPE: &str = "type";
const HEADER_MESSAGE_ID: &str = "message_id";
const HEADER_SUM: &str = "sum";
const HEADER_SEQ: &str = "seq";
// 帧 `type` header 取值："event" / "card" / "ping" / "pong"。业务路由以回包内 header.event_type
// 为准（卡片回调实测可能以 type=event 投递），仅把 MSG_TYPE_CARD 作为兜底。
const MSG_TYPE_CARD: &str = "card";
const MSG_TYPE_PING: &str = "ping";
const FRAME_CONTROL: i32 = 0;
const FRAME_DATA: i32 = 1;
/// 默认心跳间隔（ClientConfig 缺省时）。
const DEFAULT_PING_SECS: u64 = 120;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// 飞书长连接帧（pbbp2.proto）。用 `#[derive(prost::Message)]` 直接编解码，无需 protoc。
#[derive(Clone, PartialEq, ProstMessage)]
pub struct PbHeader {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(string, tag = "2")]
    pub value: String,
}

#[derive(Clone, PartialEq, ProstMessage)]
pub struct PbFrame {
    #[prost(uint64, tag = "1")]
    pub seq_id: u64,
    #[prost(uint64, tag = "2")]
    pub log_id: u64,
    #[prost(int32, tag = "3")]
    pub service: i32,
    #[prost(int32, tag = "4")]
    pub method: i32,
    #[prost(message, repeated, tag = "5")]
    pub headers: Vec<PbHeader>,
    #[prost(string, optional, tag = "6")]
    pub payload_encoding: Option<String>,
    #[prost(string, optional, tag = "7")]
    pub payload_type: Option<String>,
    #[prost(bytes = "vec", optional, tag = "8")]
    pub payload: Option<Vec<u8>>,
    #[prost(string, optional, tag = "9")]
    pub log_id_new: Option<String>,
}

impl PbFrame {
    fn header(&self, key: &str) -> &str {
        self.headers
            .iter()
            .find(|h| h.key == key)
            .map(|h| h.value.as_str())
            .unwrap_or("")
    }
}

/// 上抛给上层的业务事件（`data` 为已解析的 event JSON）。
pub enum WsEvent {
    /// 收到用户消息（`im.message.receive_v1` 的 `event`）。已自动 ACK。
    Message(Value),
    /// 卡片回传交互（`card.action.trigger` 的 `event`）。**未自动 ACK**：上层须 3 秒内
    /// 调 `respond_card` / `respond_ack`（带回原 `frame`）回包，否则飞书会重推。
    CardAction { data: Value, frame: PbFrame },
}

pub struct FeishuWs {
    http: reqwest::Client,
    base_url: String,
    app_id: String,
    app_secret: String,
    write: SplitSink<Ws, Message>,
    read: SplitStream<Ws>,
    service_id: i32,
    ping: tokio::time::Interval,
    /// message_id → 分片槽（大消息重组）。
    frag: HashMap<String, Vec<Option<Vec<u8>>>>,
}

impl FeishuWs {
    /// 建立长连接：取 endpoint → 连 wss → 拆分读写 → 解析 service_id + 心跳间隔 → 首个 ping。
    pub async fn connect(
        http: reqwest::Client,
        base_url: &str,
        app_id: &str,
        app_secret: &str,
    ) -> Result<Self, FeishuError> {
        let (url, ping_secs) = open_endpoint(&http, base_url, app_id, app_secret).await?;
        let service_id = parse_query_i32(&url, "service_id").unwrap_or(0);
        let (ws, _resp) = connect_async(url)
            .await
            .map_err(|e| FeishuError::Network(format!("WebSocket connection failed: {}", e)))?;
        let (write, read) = ws.split();
        let dur = Duration::from_secs(ping_secs.max(10));
        let ping = tokio::time::interval_at(tokio::time::Instant::now() + dur, dur);
        let mut me = Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            app_id: app_id.to_string(),
            app_secret: app_secret.to_string(),
            write,
            read,
            service_id,
            ping,
            frag: HashMap::new(),
        };
        let _ = me.send_app_ping().await; // 首个 ping，校准心跳（与官方 SDK 一致）。
        Ok(me)
    }

    /// 收下一个业务事件；内部处理 ping/pong、分片重组、自动 ACK、断线重连。
    /// 返回 `None` 表示重连多次仍失败（上层据此结束）。
    pub async fn recv(&mut self) -> Option<WsEvent> {
        loop {
            enum Step {
                Frame(Vec<u8>),
                WsPing(Vec<u8>),
                Ignore,
                Dead,
                AppPing,
            }
            let step = tokio::select! {
                biased;
                msg = self.read.next() => match msg {
                    Some(Ok(Message::Binary(b))) => Step::Frame(b.to_vec()),
                    Some(Ok(Message::Ping(p))) => Step::WsPing(p.to_vec()),
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => Step::Dead,
                    Some(Ok(_)) => Step::Ignore,
                },
                _ = self.ping.tick() => Step::AppPing,
            };
            match step {
                Step::Frame(bytes) => {
                    if let Some(ev) = self.handle_frame(&bytes).await {
                        return Some(ev);
                    }
                }
                Step::WsPing(p) => {
                    let _ = self.write.send(Message::Pong(p.into())).await;
                }
                Step::AppPing => {
                    let _ = self.send_app_ping().await;
                }
                Step::Ignore => {}
                Step::Dead => {
                    if !self.reconnect().await {
                        return None;
                    }
                }
            }
        }
    }

    /// 处理一帧；业务帧返回事件，控制/分片未满/忽略类返回 None。
    async fn handle_frame(&mut self, bytes: &[u8]) -> Option<WsEvent> {
        let frame = PbFrame::decode(bytes).ok()?;
        if frame.method == FRAME_CONTROL {
            // ping/pong：维持心跳即可（pong 可带新的 ClientConfig，这里从简不动态调整）。
            return None;
        }
        if frame.method != FRAME_DATA {
            return None;
        }

        let msg_id = frame.header(HEADER_MESSAGE_ID).to_string();
        let sum: usize = frame.header(HEADER_SUM).parse().unwrap_or(1);
        let seq: usize = frame.header(HEADER_SEQ).parse().unwrap_or(0);

        let payload = frame.payload.clone().unwrap_or_default();
        let payload = if sum > 1 {
            match self.combine(&msg_id, sum, seq, payload) {
                Some(p) => p,
                None => return None, // 分片未满，等后续帧
            }
        } else {
            payload
        };

        let value: Value = serde_json::from_slice(&payload).ok()?;
        // 业务路由以 JSON 内的 header.event_type 为准（权威），不依赖帧 `type` header——
        // 卡片回调可能以 type="card" 或 type="event" 投递，统一按 event_type 分发更稳。
        let event_type = value
            .get("header")
            .and_then(|h| h.get("event_type"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let frame_type = frame.header(HEADER_TYPE).to_string();
        // 诊断：设 ASKHUMAN_FEISHU_DEBUG=1 时记录每个数据帧的类型，便于确认卡片回调是否到达。
        debug_log(&format!(
            "[feishu-ws] data frame: type={} event_type={}",
            frame_type, event_type
        ));

        if event_type == "card.action.trigger" || frame_type == MSG_TYPE_CARD {
            // 卡片回调：延迟 ACK——由上层算出回包后调 respond_*（须 3 秒内）。
            return Some(WsEvent::CardAction {
                data: value.get("event").cloned().unwrap_or(Value::Null),
                frame,
            });
        }

        // 其余事件：立即空 ACK 再按需上抛。
        self.respond_ack(&frame).await;
        if event_type == "im.message.receive_v1" {
            return Some(WsEvent::Message(
                value.get("event").cloned().unwrap_or(Value::Null),
            ));
        }
        None
    }

    /// 空 ACK（事件 / 非匹配卡片）：回 `{"code":200}`。
    pub async fn respond_ack(&mut self, frame: &PbFrame) {
        self.respond(frame, None).await;
    }

    /// 卡片回包：把业务响应体（如 `{toast,...}`）base64 进 `data`，回 `{"code":200,"data":..}`。
    pub async fn respond_card(&mut self, frame: &PbFrame, body: &Value) {
        let data = B64.encode(body.to_string().as_bytes());
        self.respond(frame, Some(data)).await;
    }

    /// 发送响应帧：复用收到的 data 帧（保留 headers/message_id/service），替换 payload 为 Response JSON。
    async fn respond(&mut self, frame: &PbFrame, data_b64: Option<String>) {
        let resp = match data_b64 {
            Some(d) => json!({ "code": 200, "data": d }),
            None => json!({ "code": 200 }),
        };
        let mut out = frame.clone();
        out.payload = Some(resp.to_string().into_bytes());
        out.payload_encoding = None;
        out.payload_type = None;
        let _ = self
            .write
            .send(Message::Binary(out.encode_to_vec().into()))
            .await;
    }

    /// 发送应用层 ping 帧（method=0, type=ping）。
    async fn send_app_ping(&mut self) -> Result<(), FeishuError> {
        let frame = PbFrame {
            seq_id: 0,
            log_id: 0,
            service: self.service_id,
            method: FRAME_CONTROL,
            headers: vec![PbHeader {
                key: HEADER_TYPE.to_string(),
                value: MSG_TYPE_PING.to_string(),
            }],
            payload_encoding: None,
            payload_type: None,
            payload: None,
            log_id_new: None,
        };
        self.write
            .send(Message::Binary(frame.encode_to_vec().into()))
            .await
            .map_err(|e| FeishuError::Network(e.to_string()))
    }

    /// 大消息分片重组：填入 seq 槽，集齐返回完整 payload，否则 None。
    fn combine(&mut self, msg_id: &str, sum: usize, seq: usize, bs: Vec<u8>) -> Option<Vec<u8>> {
        combine_frag(&mut self.frag, msg_id, sum, seq, bs)
    }

    /// 断线重连：重新取 endpoint + 连接。最多重试若干次。
    async fn reconnect(&mut self) -> bool {
        for attempt in 0..5u32 {
            tokio::time::sleep(Duration::from_millis(500 * (attempt as u64 + 1))).await;
            let opened =
                open_endpoint(&self.http, &self.base_url, &self.app_id, &self.app_secret).await;
            let Ok((url, ping_secs)) = opened else {
                continue;
            };
            let service_id = parse_query_i32(&url, "service_id").unwrap_or(0);
            if let Ok((ws, _)) = connect_async(url).await {
                let (write, read) = ws.split();
                self.write = write;
                self.read = read;
                self.service_id = service_id;
                let dur = Duration::from_secs(ping_secs.max(10));
                self.ping = tokio::time::interval_at(tokio::time::Instant::now() + dur, dur);
                self.frag.clear();
                let _ = self.send_app_ping().await;
                return true;
            }
        }
        false
    }
}

/// 取长连接 endpoint：返回 (wss URL, ping 间隔秒)。
async fn open_endpoint(
    http: &reqwest::Client,
    base_url: &str,
    app_id: &str,
    app_secret: &str,
) -> Result<(String, u64), FeishuError> {
    let url = format!("{}{}", base_url.trim_end_matches('/'), GEN_ENDPOINT_URI);
    let resp = http
        .post(&url)
        .header("locale", "zh")
        .json(&json!({ "AppID": app_id, "AppSecret": app_secret }))
        .send()
        .await
        .map_err(|e| FeishuError::Network(e.to_string()))?;
    let http_status = resp.status().as_u16();
    let log_id = super::response_log_id(resp.headers());
    let v: Value = resp
        .json()
        .await
        .map_err(|_| FeishuError::bad_response_with_metadata(Some(http_status), log_id.clone()))?;
    if v.get("code").and_then(|c| c.as_i64()) != Some(0) {
        let msg = v
            .get("msg")
            .and_then(|m| m.as_str())
            .unwrap_or("failed to obtain Feishu long-connection endpoint")
            .to_string();
        return Err(FeishuError::api_response(
            v.get("code").and_then(|code| code.as_i64()),
            msg,
            Some(http_status),
            log_id,
        ));
    }
    let data = v.get("data").ok_or_else(|| {
        FeishuError::bad_response_with_metadata(Some(http_status), log_id.clone())
    })?;
    let conn_url = data
        .get("URL")
        .and_then(|u| u.as_str())
        .ok_or_else(|| FeishuError::bad_response_with_metadata(Some(http_status), log_id.clone()))?
        .to_string();
    let ping_secs = data
        .get("ClientConfig")
        .and_then(|c| c.get("PingInterval"))
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_PING_SECS);
    Ok((conn_url, ping_secs))
}

/// 大消息分片重组（纯函数）：填入 seq 槽，集齐返回完整 payload，否则 None。
fn combine_frag(
    frag: &mut HashMap<String, Vec<Option<Vec<u8>>>>,
    msg_id: &str,
    sum: usize,
    seq: usize,
    bs: Vec<u8>,
) -> Option<Vec<u8>> {
    let slots = frag
        .entry(msg_id.to_string())
        .or_insert_with(|| vec![None; sum]);
    if seq < slots.len() {
        slots[seq] = Some(bs);
    }
    if slots.iter().any(|s| s.is_none()) {
        return None;
    }
    let full: Vec<u8> = slots.iter().flatten().flatten().copied().collect();
    frag.remove(msg_id);
    Some(full)
}

/// 是否开启飞书长连接诊断日志（环境变量 `ASKHUMAN_FEISHU_DEBUG` 非空且非 "0"；
/// 兼容旧变量名 `HUMANINLOOP_FEISHU_DEBUG`）。
pub fn debug_enabled() -> bool {
    let on = |name: &str| {
        std::env::var(name)
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
    };
    on("ASKHUMAN_FEISHU_DEBUG") || on("HUMANINLOOP_FEISHU_DEBUG")
}

/// 诊断日志：写入 `~/.askhuman/feishu-debug.log`（GUI 模式 stderr 被静默，文件更可靠），
/// 同时尽力写 stderr。仅当开启 `ASKHUMAN_FEISHU_DEBUG`（或兼容旧 `HUMANINLOOP_FEISHU_DEBUG`）时生效。
pub fn debug_log(msg: &str) {
    if !debug_enabled() {
        return;
    }
    use std::io::Write;
    let dir = crate::paths::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("feishu-debug.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{}] {}", ts, msg);
    }
    eprintln!("{}", msg);
}

/// 从 URL query 中取某个整型参数（如 `service_id`）。
fn parse_query_i32(url: &str, key: &str) -> Option<i32> {
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    for pair in query.split('&') {
        if let Some((k, val)) = pair.split_once('=') {
            if k == key {
                return val.parse::<i32>().ok();
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pbframe_round_trip() {
        let frame = PbFrame {
            seq_id: 7,
            log_id: 0,
            service: 3,
            method: FRAME_DATA,
            headers: vec![
                PbHeader {
                    key: "type".into(),
                    value: "event".into(),
                },
                PbHeader {
                    key: "message_id".into(),
                    value: "m1".into(),
                },
            ],
            payload_encoding: None,
            payload_type: None,
            payload: Some(b"{\"a\":1}".to_vec()),
            log_id_new: None,
        };
        let bytes = frame.encode_to_vec();
        let back = PbFrame::decode(bytes.as_slice()).unwrap();
        assert_eq!(back.service, 3);
        assert_eq!(back.method, FRAME_DATA);
        assert_eq!(back.header("type"), "event");
        assert_eq!(back.header("message_id"), "m1");
        assert_eq!(back.header("missing"), "");
        assert_eq!(back.payload.as_deref(), Some(&b"{\"a\":1}"[..]));
    }

    #[test]
    fn parse_query_extracts_service_id() {
        let url = "wss://host/ws?device_id=abc&service_id=42&x=1";
        assert_eq!(parse_query_i32(url, "service_id"), Some(42));
        assert_eq!(parse_query_i32(url, "device_id"), None); // 非整型
        assert_eq!(parse_query_i32("wss://h/ws", "service_id"), None);
    }

    #[test]
    fn combine_reassembles_fragments() {
        let mut frag: HashMap<String, Vec<Option<Vec<u8>>>> = HashMap::new();
        assert_eq!(combine_frag(&mut frag, "m", 2, 0, b"ab".to_vec()), None);
        assert_eq!(
            combine_frag(&mut frag, "m", 2, 1, b"cd".to_vec()),
            Some(b"abcd".to_vec())
        );
        // 重组完成后槽位清除。
        assert!(!frag.contains_key("m"));
    }
}
