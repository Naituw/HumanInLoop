//! 钉钉 Stream 模式长连接：WebSocket 收机器人消息 + 卡片回调（零公网）。
//!
//! 流程：`POST /v1.0/gateway/connections/open` 拿 `endpoint`+`ticket` → 连 `endpoint?ticket=…` →
//! 循环收帧：SYSTEM ping 回 200；CALLBACK 先 3 秒内 ACK 再上抛事件；断线自动重连。

use super::DingTalkError;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::time::{self, Instant, Interval};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

pub const TOPIC_BOT_MESSAGE: &str = "/v1.0/im/bot/messages/get";
pub const TOPIC_CARD_CALLBACK: &str = "/v1.0/card/instances/callback";

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// 上抛给上层的事件（`data` 为已解析的 JSON）。
pub enum StreamEvent {
    /// 机器人收到用户消息（文字/图片/文件/富文本）。已自动 ACK。
    BotMessage(Value),
    /// 卡片回调（按钮点选/提交）。**未自动 ACK**：上层须在 3 秒内调用 `respond(message_id, ..)`
    /// 回包（更新卡片）或空回包，否则钉钉会重推。
    CardCallback { data: Value, message_id: String },
}

pub struct StreamConn {
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
    topics: Vec<String>,
    ws: Ws,
    heartbeat: Interval,
    awaiting_pong: bool,
}

impl StreamConn {
    /// 建立连接并订阅 topics（CALLBACK）。
    pub async fn connect(
        http: reqwest::Client,
        client_id: &str,
        client_secret: &str,
        topics: &[&str],
    ) -> Result<Self, DingTalkError> {
        let topics: Vec<String> = topics.iter().map(|t| t.to_string()).collect();
        let ws = open_ws(&http, client_id, client_secret, &topics).await?;
        Ok(Self {
            http,
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            topics,
            ws,
            heartbeat: heartbeat_interval(),
            awaiting_pong: false,
        })
    }

    /// 收下一个业务事件；内部处理 SYSTEM ping / ACK / 断线重连。
    /// 返回 `None` 表示重连多次仍失败（上层据此结束）。
    pub async fn recv(&mut self) -> Option<StreamEvent> {
        loop {
            let frame = tokio::select! {
                frame = self.ws.next() => Some(frame),
                _ = self.heartbeat.tick() => None,
            };
            match frame {
                None => {
                    // A sleeping Mac or a network handoff can leave the TCP socket half-open:
                    // REST sends keep working while this reader waits forever. Probe the WebSocket
                    // explicitly and rebuild it when the previous probe received no response.
                    if self.awaiting_pong
                        || self
                            .ws
                            .send(Message::Ping(Vec::new().into()))
                            .await
                            .is_err()
                    {
                        if !self.reconnect().await {
                            return None;
                        }
                    } else {
                        self.awaiting_pong = true;
                    }
                }
                Some(Some(Ok(Message::Text(txt)))) => {
                    self.awaiting_pong = false;
                    if let Some(ev) = self.handle_frame(txt.as_str()).await {
                        return Some(ev);
                    }
                }
                Some(Some(Ok(Message::Ping(p)))) => {
                    self.awaiting_pong = false;
                    let _ = self.ws.send(Message::Pong(p)).await;
                }
                Some(Some(Ok(Message::Pong(_)))) => {
                    self.awaiting_pong = false;
                }
                // Cannot collapse into a match guard: `.await` is not allowed there.
                #[allow(clippy::collapsible_match)]
                Some(Some(Ok(Message::Close(_)))) | Some(Some(Err(_))) | Some(None) => {
                    if !self.reconnect().await {
                        return None;
                    }
                }
                _ => {}
            }
        }
    }

    /// 处理一帧；业务帧返回事件，控制帧（ping/系统）返回 None。
    async fn handle_frame(&mut self, txt: &str) -> Option<StreamEvent> {
        let frame: Value = serde_json::from_str(txt).ok()?;
        let typ = frame.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let headers = frame.get("headers").cloned().unwrap_or(Value::Null);
        let topic = headers.get("topic").and_then(|t| t.as_str()).unwrap_or("");
        let message_id = headers
            .get("messageId")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let data: Value = frame
            .get("data")
            .and_then(|d| d.as_str())
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);

        match typ {
            "SYSTEM" => {
                // ping/KEEPALIVE 等：回 200 维持连接。
                self.ack(&message_id, json!({})).await;
                None
            }
            "CALLBACK" | "EVENT" => match topic {
                TOPIC_BOT_MESSAGE => {
                    // bot 消息：立即空 ACK 再上抛。
                    self.ack(&message_id, json!({})).await;
                    Some(StreamEvent::BotMessage(data))
                }
                TOPIC_CARD_CALLBACK => {
                    // 卡片回调：延迟 ACK——由上层算出回包后调 respond()（须 3 秒内）。
                    Some(StreamEvent::CardCallback { data, message_id })
                }
                _ => {
                    self.ack(&message_id, json!({})).await;
                    None
                }
            },
            _ => None,
        }
    }

    /// 回复一个卡片回调（带回包，用于更新卡片）。须在收到回调后 3 秒内调用。
    /// `data` 为业务响应体（如 `{cardUpdateOptions, cardData, userPrivateData}`）；空对象即
    /// 「仅确认、不更新」。按钉钉 Stream 协议，CALLBACK 的响应体须再包一层 `{"response": data}`
    /// 才能被网关识别（否则卡片端会提示「提交失败」且不更新）。
    pub async fn respond(&mut self, message_id: &str, data: Value) {
        self.ack(message_id, json!({ "response": data })).await;
    }

    /// 发送 ACK / 响应帧（带回原 messageId）。
    async fn ack(&mut self, message_id: &str, data: Value) {
        let frame = json!({
            "code": 200,
            "headers": { "contentType": "application/json", "messageId": message_id },
            "message": "OK",
            "data": data.to_string(),
        });
        let _ = self.ws.send(Message::Text(frame.to_string().into())).await;
    }

    /// 断线重连：重新 open 拿新 ticket 再连。最多重试若干次。
    async fn reconnect(&mut self) -> bool {
        for attempt in 0..5u32 {
            tokio::time::sleep(std::time::Duration::from_millis(500 * (attempt as u64 + 1))).await;
            if let Ok(ws) = open_ws(
                &self.http,
                &self.client_id,
                &self.client_secret,
                &self.topics,
            )
            .await
            {
                self.ws = ws;
                self.heartbeat = heartbeat_interval();
                self.awaiting_pong = false;
                return true;
            }
        }
        false
    }
}

fn heartbeat_interval() -> Interval {
    let mut interval = time::interval_at(Instant::now() + HEARTBEAT_INTERVAL, HEARTBEAT_INTERVAL);
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
    interval
}

/// 注册长连接 + 建 WebSocket。
async fn open_ws(
    http: &reqwest::Client,
    client_id: &str,
    client_secret: &str,
    topics: &[String],
) -> Result<Ws, DingTalkError> {
    let subscriptions: Vec<Value> = topics
        .iter()
        .map(|t| json!({ "type": "CALLBACK", "topic": t }))
        .collect();
    let body = json!({
        "clientId": client_id,
        "clientSecret": client_secret,
        "subscriptions": subscriptions,
        "ua": "askhuman/0.2",
        "localIp": "127.0.0.1",
    });
    let resp = http
        .post(format!(
            "{}/v1.0/gateway/connections/open",
            super::api_base()
        ))
        .json(&body)
        .send()
        .await
        .map_err(|e| DingTalkError::Network(e.to_string()))?;
    let status = resp.status();
    let v: Value = resp.json().await.map_err(|_| DingTalkError::BadResponse)?;
    if !status.is_success() {
        let msg = v
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("failed to establish Stream connection")
            .to_string();
        return Err(DingTalkError::Api(msg));
    }
    let endpoint = v
        .get("endpoint")
        .and_then(|e| e.as_str())
        .ok_or(DingTalkError::BadResponse)?;
    let ticket = v
        .get("ticket")
        .and_then(|t| t.as_str())
        .ok_or(DingTalkError::BadResponse)?;
    let url = format!("{}?ticket={}", endpoint, ticket);
    let (ws, _resp) = connect_async(url)
        .await
        .map_err(|e| DingTalkError::Network(format!("WebSocket connection failed: {}", e)))?;
    Ok(ws)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn missed_heartbeats_preserve_full_pong_window() {
        let mut heartbeat = heartbeat_interval();

        // Model a suspended runtime that misses more than two heartbeat periods.
        time::advance(HEARTBEAT_INTERVAL * 3).await;
        heartbeat.tick().await;

        // recv() sends Ping on the overdue tick above. The following tick is the
        // earliest point at which awaiting_pong can trigger a reconnect, so it must
        // remain pending for the entire response window.
        let mut next_tick = Box::pin(heartbeat.tick());
        time::advance(HEARTBEAT_INTERVAL - Duration::from_millis(1)).await;
        tokio::select! {
            biased;
            _ = &mut next_tick => panic!("heartbeat fired before the pong window elapsed"),
            _ = tokio::task::yield_now() => {}
        }

        time::advance(Duration::from_millis(1)).await;
        next_tick.await;
    }
}
