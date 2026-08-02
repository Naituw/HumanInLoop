//! Agent 插话（Interject）队列：daemon 内存中按 session 维护「待送达消息 + composer 状态 +
//! 等待中的 hook」（spec `docs/specs/agent-interject.md` D2/D3/D4/D8）。
//!
//! 性能红线（spec §4）：hook 热路径零文件 IO——队列常驻内存（O(1) 查表）；
//! `~/.askhuman/state/interject.json` 只在**变更时**由调用方触发原子落盘（`persist`）、
//! daemon 启动 `load()` 读一次。composer 打开状态是连接态，不持久化。
//!
//! 交付语义（D3）：hook poll 时「有消息 → 原子出队（并发只交付一个）；composer 打开 → 挂起等待；
//! 都没有 → 立即放行」。提交时若有等待中的 hook，**只有一个**拿到消息、其余放行。

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::models::FileAttachment;
use crate::paths;

/// One queued interjection. Text may be empty when the entry only contains attachments.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InterjectEntry {
    text: String,
    attachments: Vec<FileAttachment>,
}

/// Exact batch removed from the queue for one hook delivery. Keeping the original entries lets a
/// failed socket write restore text and attachments without collapsing append boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterjectDelivery {
    entries: Vec<InterjectEntry>,
    pub text: String,
    pub attachments: Vec<FileAttachment>,
}

impl InterjectDelivery {
    fn from_entries(entries: Vec<InterjectEntry>) -> Self {
        let text = join_text(&entries);
        let attachments = entries
            .iter()
            .flat_map(|entry| entry.attachments.clone())
            .collect();
        Self {
            entries,
            text,
            attachments,
        }
    }
}

/// Hold 之后等待的结果（daemon 内部：提交/取消唤醒等待中的 hook 连接）。
#[derive(Debug)]
pub enum WaitOutcome {
    /// 有消息 → hook 输出 deny+消息。
    Message(InterjectDelivery),
    /// 放行（composer 取消 / 关窗 / 消息被并发的另一个 hook 拿走）。
    Release,
}

/// 一次 hook poll 的即时结果（AgentEvent `interject_poll=true` 的处理产物）。
pub enum PollOutcome {
    /// 无消息且 composer 未打开 → hook 立即放行。
    None,
    /// 有已提交消息 → 原子出队交付（调用方随后 `persist` + 广播徽标变化）。
    /// `receipt_channels`＝该批插话中「排队等待被消费」时登记的来源渠道（去重）：消费即代表
    /// agent 已读，调用方逐渠道回推「已阅读」回执后即消（spec agent-interject D9 追加）。
    Message {
        delivery: InterjectDelivery,
        receipt_channels: Vec<String>,
    },
    /// composer 打开中 → 挂起等待提交/取消（接收端由 hook 连接的处理任务持有）。
    Hold(oneshot::Receiver<WaitOutcome>),
}

/// 单 session 的插话状态。
#[derive(Default)]
struct Entry {
    /// 待送达条目（弹窗提交＝整体覆盖；IM `/msg`＝追加；消费时按空行拼接一次性送达）。
    entries: Vec<InterjectEntry>,
    /// 待「已阅读」回执的来源渠道（去重，spec agent-interject D9 追加）：仅 IM `/msg` **排队**
    /// 时登记；被消费（`poll`）时随消息取出、逐渠道回推后清空。不变式：仅当 `entries` 非空时可非空。
    receipt_channels: Vec<String>,
    /// composer 打开计数（连接态：连接断开＝关闭；正常每 session 至多 1，计数防重入）。
    composers: usize,
    /// 等待中的 hook（Hold 状态的连接）。提交时一个拿 Message、其余 Release；取消/关窗全 Release。
    waiters: Vec<oneshot::Sender<WaitOutcome>>,
}

impl Entry {
    fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.composers == 0 && self.waiters.is_empty()
    }
}

/// 持久化形态（只存 entries + 待回执来源渠道；composer/waiter 是连接态）。
#[derive(Default, Serialize, Deserialize)]
struct Persisted {
    /// Kept as a string array for backward compatibility with existing `interject.json` files.
    #[serde(default)]
    sessions: HashMap<String, Vec<String>>,
    /// Attachment arrays aligned by entry index with `sessions`. Missing in legacy files.
    #[serde(default)]
    attachments: HashMap<String, Vec<Vec<FileAttachment>>>,
    /// session → 待「已阅读」回执的来源渠道。老文件无此字段 → 默认空（向后兼容，不动 `sessions` 格式）。
    #[serde(default)]
    receipt_channels: HashMap<String, Vec<String>>,
}

/// daemon 内唯一的插话队列（线程安全）。
#[derive(Default)]
pub struct InterjectStore {
    inner: Mutex<HashMap<String, Entry>>,
}

/// 条目拼接分隔（多条按空行合成一条消息一次性送达，D2）。
const JOIN_SEP: &str = "\n\n";

fn join_text(entries: &[InterjectEntry]) -> String {
    entries
        .iter()
        .map(|entry| entry.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(JOIN_SEP)
}

fn normalize_attachments(attachments: Vec<FileAttachment>) -> Vec<FileAttachment> {
    let mut seen = std::collections::HashSet::new();
    attachments
        .into_iter()
        .filter(|attachment| {
            !attachment.path.trim().is_empty() && seen.insert(attachment.path.clone())
        })
        .collect()
}

impl InterjectStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 从 `interject.json` 还原待送达条目（缺失/解析失败 → 空）。已结束会话的残留条目
    /// 由 daemon 周期 tick 的 `retain_sessions` 与 session-end 事件清理。
    pub fn load() -> Self {
        Self::load_from(&paths::interject_file())
    }

    fn load_from(path: &std::path::Path) -> Self {
        let store = Self::new();
        let Ok(text) = std::fs::read_to_string(path) else {
            return store;
        };
        let Ok(parsed) = serde_json::from_str::<Persisted>(&text) else {
            return store;
        };
        let mut map = store.inner.lock().unwrap();
        for (sid, texts) in parsed.sessions {
            let attachment_groups = parsed.attachments.get(&sid).cloned().unwrap_or_default();
            let entries: Vec<InterjectEntry> = texts
                .into_iter()
                .enumerate()
                .filter_map(|(index, text)| {
                    let text = text.trim().to_string();
                    let attachments = normalize_attachments(
                        attachment_groups.get(index).cloned().unwrap_or_default(),
                    );
                    (!text.is_empty() || !attachments.is_empty())
                        .then_some(InterjectEntry { text, attachments })
                })
                .collect();
            if !sid.is_empty() && !entries.is_empty() {
                // 仅为仍有待送达条目的 session 恢复来源渠道（回执只在消息被消费时才有意义）。
                let receipt_channels: Vec<String> = parsed
                    .receipt_channels
                    .get(&sid)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| !c.trim().is_empty())
                    .collect();
                map.insert(
                    sid,
                    Entry {
                        entries,
                        receipt_channels,
                        ..Entry::default()
                    },
                );
            }
        }
        drop(map);
        store
    }

    /// 原子落盘（只在变更后由调用方触发；best-effort，失败静默）。
    pub fn persist(&self) {
        self.persist_to(&paths::interject_file());
    }

    fn persist_to(&self, path: &std::path::Path) {
        let data = {
            let map = self.inner.lock().unwrap();
            Persisted {
                sessions: map
                    .iter()
                    .filter(|(_, e)| !e.entries.is_empty())
                    .map(|(k, e)| {
                        (
                            k.clone(),
                            e.entries.iter().map(|entry| entry.text.clone()).collect(),
                        )
                    })
                    .collect(),
                attachments: map
                    .iter()
                    .filter(|(_, e)| {
                        !e.entries.is_empty()
                            && e.entries.iter().any(|entry| !entry.attachments.is_empty())
                    })
                    .map(|(k, e)| {
                        (
                            k.clone(),
                            e.entries
                                .iter()
                                .map(|entry| entry.attachments.clone())
                                .collect(),
                        )
                    })
                    .collect(),
                receipt_channels: map
                    .iter()
                    .filter(|(_, e)| !e.entries.is_empty() && !e.receipt_channels.is_empty())
                    .map(|(k, e)| (k.clone(), e.receipt_channels.clone()))
                    .collect(),
            }
        };
        let Ok(json) = serde_json::to_string_pretty(&data) else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension(format!("json.tmp-{}", uuid::Uuid::new_v4()));
        if std::fs::write(&tmp, json.as_bytes()).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// composer 提交：**整体覆盖**队列（D2）。文本和附件都为空＝清空。有等待中的 hook
    /// 时立即交付（一个拿 Message、其余 Release，队列即被消费）；无等待者则留队等下一次 poll。
    pub fn submit(&self, session_id: &str, text: &str, attachments: Vec<FileAttachment>) {
        let text = text.trim();
        let attachments = normalize_attachments(attachments);
        let mut map = self.inner.lock().unwrap();
        let e = map.entry(session_id.to_string()).or_default();
        // GUI 覆盖会丢弃排队的 IM 插话 → 那些消息永不被消费，清掉其待回执登记（Q2：不回执）。
        e.entries.clear();
        e.receipt_channels.clear();
        if !text.is_empty() || !attachments.is_empty() {
            e.entries.push(InterjectEntry {
                text: text.to_string(),
                attachments,
            });
        }
        Self::try_deliver(e);
        if e.is_empty() {
            map.remove(session_id);
        }
    }

    /// IM `/msg` 追加一条（D2）。交付语义同 `submit`。返回追加后的待送达条数（0＝已被
    /// 等待中的 hook 立即消费）。`receipt_channel`＝该条的来源渠道：**排队**（返回 >0）时登记，
    /// 以便被 agent 消费后回推「已阅读」回执（D9）；即时送达（返回 0）不登记（已有「已送达」回执）。
    pub fn append(
        &self,
        session_id: &str,
        text: &str,
        attachments: Vec<FileAttachment>,
        receipt_channel: Option<&str>,
    ) -> usize {
        let text = text.trim();
        let attachments = normalize_attachments(attachments);
        if text.is_empty() && attachments.is_empty() {
            return self.pending_count(session_id);
        }
        let mut map = self.inner.lock().unwrap();
        let e = map.entry(session_id.to_string()).or_default();
        e.entries.push(InterjectEntry {
            text: text.to_string(),
            attachments,
        });
        Self::try_deliver(e);
        if e.entries.is_empty() {
            // 恰有 hook 在等 → 立即送达：不登记回执（即时送达已由「已送达」覆盖，Q1）。
            e.receipt_channels.clear();
        } else if let Some(ch) = receipt_channel {
            let ch = ch.trim();
            if !ch.is_empty() && !e.receipt_channels.iter().any(|c| c == ch) {
                e.receipt_channels.push(ch.to_string());
            }
        }
        let n = e.entries.len();
        if e.is_empty() {
            map.remove(session_id);
        }
        n
    }

    /// 撤回：清空该 session 的待送达条目（不动 composer/waiter）。返回是否原有内容（供调用方
    /// 决定 persist + 广播）。
    pub fn clear(&self, session_id: &str) -> bool {
        let mut map = self.inner.lock().unwrap();
        let Some(e) = map.get_mut(session_id) else {
            return false;
        };
        let had = !e.entries.is_empty();
        // 撤回＝这些消息不会被消费 → 不回执（Q2）。
        e.entries.clear();
        e.receipt_channels.clear();
        if e.is_empty() {
            map.remove(session_id);
        }
        had
    }

    /// 待送达全文（composer 预填 / IM 回显）：条目按空行拼接；无则空串。
    pub fn full_text(&self, session_id: &str) -> String {
        let map = self.inner.lock().unwrap();
        map.get(session_id)
            .map(|e| join_text(&e.entries))
            .unwrap_or_default()
    }

    /// Flattened attachment snapshot for composer prefill and the console pending summary.
    pub fn attachments(&self, session_id: &str) -> Vec<FileAttachment> {
        let map = self.inner.lock().unwrap();
        map.get(session_id)
            .map(|entry| {
                entry
                    .entries
                    .iter()
                    .flat_map(|item| item.attachments.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 待送达条数（IM 回执用）。
    pub fn pending_count(&self, session_id: &str) -> usize {
        let map = self.inner.lock().unwrap();
        map.get(session_id).map(|e| e.entries.len()).unwrap_or(0)
    }

    /// 有待送达条目的 session 集合（AgentsState 快照注入 `pendingInterject` 徽标）。
    pub fn pending_sessions(&self) -> Vec<String> {
        let map = self.inner.lock().unwrap();
        map.iter()
            .filter(|(_, e)| !e.entries.is_empty())
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// hook poll（D3 三态）：有消息 → 原子出队；composer 打开 → 挂起；都没有 → 放行。
    /// Message 分支消费了队列，调用方需随后 `persist` + 广播徽标变化。
    pub fn poll(&self, session_id: &str) -> PollOutcome {
        let mut map = self.inner.lock().unwrap();
        let Some(e) = map.get_mut(session_id) else {
            return PollOutcome::None;
        };
        if !e.entries.is_empty() {
            let delivery = InterjectDelivery::from_entries(std::mem::take(&mut e.entries));
            // 消费＝agent 已读：取出待回执来源渠道随消息交给调用方回推（D9）。
            let receipt_channels = std::mem::take(&mut e.receipt_channels);
            if e.is_empty() {
                map.remove(session_id);
            }
            return PollOutcome::Message {
                delivery,
                receipt_channels,
            };
        }
        if e.composers > 0 {
            // 顺带清理已死的等待者（hook 连接断开后其接收端已 drop）。
            e.waiters.retain(|w| !w.is_closed());
            let (tx, rx) = oneshot::channel();
            e.waiters.push(tx);
            return PollOutcome::Hold(rx);
        }
        PollOutcome::None
    }

    /// composer 窗口打开（连接登记）。
    pub fn composer_opened(&self, session_id: &str) {
        let mut map = self.inner.lock().unwrap();
        map.entry(session_id.to_string()).or_default().composers += 1;
    }

    /// composer 窗口关闭（取消 / 关窗 / 连接断开）：放行所有等待中的 hook。
    pub fn composer_closed(&self, session_id: &str) {
        let mut map = self.inner.lock().unwrap();
        let Some(e) = map.get_mut(session_id) else {
            return;
        };
        e.composers = e.composers.saturating_sub(1);
        if e.composers == 0 {
            for w in e.waiters.drain(..) {
                let _ = w.send(WaitOutcome::Release);
            }
        }
        if e.is_empty() {
            map.remove(session_id);
        }
    }

    /// 会话结束：清空条目 + 放行所有等待者。composer 计数是连接态、保留（窗口连接断开时自行归零）。
    /// 返回是否清掉了待送达条目（供调用方决定 persist + 广播）。
    pub fn remove_session(&self, session_id: &str) -> bool {
        let mut map = self.inner.lock().unwrap();
        let Some(e) = map.get_mut(session_id) else {
            return false;
        };
        let had = !e.entries.is_empty();
        // 会话结束＝残留消息不会被消费 → 不回执（Q2）。
        e.entries.clear();
        e.receipt_channels.clear();
        for w in e.waiters.drain(..) {
            let _ = w.send(WaitOutcome::Release);
        }
        if e.is_empty() {
            map.remove(session_id);
        }
        had
    }

    /// 周期清理：不在 `keep` 集合（注册表活动 session）里的会话按 `remove_session` 处理
    /// （兜底漏 session-end 事件 / daemon 停机期间结束的会话）。返回是否有条目被清。
    pub fn retain_sessions(&self, keep: &[String]) -> bool {
        let sids: Vec<String> = {
            let map = self.inner.lock().unwrap();
            map.keys()
                .filter(|k| !keep.iter().any(|s| s == *k))
                .cloned()
                .collect()
        };
        let mut changed = false;
        for sid in sids {
            changed |= self.remove_session(&sid);
        }
        changed
    }

    /// Restore an exact failed delivery before messages queued while the socket write was in
    /// flight. Receipt channels are restored as well so a later successful poll still reports read.
    pub fn requeue_front(
        &self,
        session_id: &str,
        delivery: InterjectDelivery,
        receipt_channels: Vec<String>,
    ) {
        if delivery.entries.is_empty() {
            return;
        }
        let mut map = self.inner.lock().unwrap();
        let entry = map.entry(session_id.to_string()).or_default();
        let mut restored = delivery.entries;
        restored.append(&mut entry.entries);
        entry.entries = restored;
        let queued_channels = std::mem::take(&mut entry.receipt_channels);
        for channel in receipt_channels.into_iter().chain(queued_channels) {
            if !channel.trim().is_empty()
                && !entry.receipt_channels.iter().any(|value| value == &channel)
            {
                entry.receipt_channels.push(channel);
            }
        }
    }

    /// 若有存活的等待者则把当前队列整体交付给其中一个（其余 Release）并清空队列。
    /// 无等待者 / 队列为空则不动。
    fn try_deliver(e: &mut Entry) {
        if e.entries.is_empty() || e.waiters.is_empty() {
            // 无可交付内容时也顺带放行等待者？不——等待者由 composer 关闭事件放行；
            // 空提交（=撤回）不代表 composer 关闭。这里只清理已死连接。
            e.waiters.retain(|w| !w.is_closed());
            return;
        }
        let delivery = InterjectDelivery::from_entries(e.entries.clone());
        let mut delivered = false;
        for w in e.waiters.drain(..) {
            if !delivered {
                // send 失败（接收端已 drop）→ 尝试下一个等待者。
                if w.send(WaitOutcome::Message(delivery.clone())).is_ok() {
                    delivered = true;
                }
            } else {
                let _ = w.send(WaitOutcome::Release);
            }
        }
        if delivered {
            // 即时送达（有 hook 在等）走「已送达」回执，不留待「已阅读」回执（Q1）。
            e.entries.clear();
            e.receipt_channels.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment(path: &str) -> FileAttachment {
        FileAttachment {
            path: path.into(),
            name: path.rsplit('/').next().unwrap_or(path).into(),
            size: 42,
            is_image: path.ends_with(".png"),
        }
    }

    #[test]
    fn poll_none_when_empty() {
        let s = InterjectStore::new();
        assert!(matches!(s.poll("s1"), PollOutcome::None));
    }

    #[test]
    fn submit_then_poll_takes_atomically() {
        let s = InterjectStore::new();
        s.submit("s1", "调整方向", Vec::new());
        assert_eq!(s.pending_count("s1"), 1);
        // 首个 poll 拿到消息并清空。
        match s.poll("s1") {
            PollOutcome::Message { delivery, .. } => assert_eq!(delivery.text, "调整方向"),
            _ => panic!("expected message"),
        }
        // 第二个 poll（并发工具调用）拿不到 → 放行。
        assert!(matches!(s.poll("s1"), PollOutcome::None));
        assert_eq!(s.pending_count("s1"), 0);
    }

    #[test]
    fn submit_overwrites_whole_queue() {
        let s = InterjectStore::new();
        s.append("s1", "第一条", Vec::new(), None);
        s.append("s1", "第二条", Vec::new(), None);
        assert_eq!(s.pending_count("s1"), 2);
        assert_eq!(s.full_text("s1"), "第一条\n\n第二条");
        // 弹窗提交＝整体覆盖。
        s.submit("s1", "改成这个", Vec::new());
        assert_eq!(s.pending_count("s1"), 1);
        assert_eq!(s.full_text("s1"), "改成这个");
        // 空提交＝清空。
        s.submit("s1", "  ", Vec::new());
        assert_eq!(s.pending_count("s1"), 0);
    }

    #[test]
    fn append_joins_on_delivery() {
        let s = InterjectStore::new();
        assert_eq!(s.append("s1", "a", Vec::new(), None), 1);
        assert_eq!(s.append("s1", "b", Vec::new(), None), 2);
        match s.poll("s1") {
            PollOutcome::Message { delivery, .. } => assert_eq!(delivery.text, "a\n\nb"),
            _ => panic!("expected message"),
        }
    }

    #[test]
    fn clear_revokes_pending() {
        let s = InterjectStore::new();
        s.submit("s1", "x", Vec::new());
        assert!(s.clear("s1"));
        assert!(!s.clear("s1")); // 已空 → 无变化
        assert!(matches!(s.poll("s1"), PollOutcome::None));
    }

    #[test]
    fn composer_open_holds_then_submit_delivers_one() {
        let s = InterjectStore::new();
        s.composer_opened("s1");
        // 两个并发 hook 都进入等待。
        let PollOutcome::Hold(rx1) = s.poll("s1") else {
            panic!("expected hold")
        };
        let PollOutcome::Hold(rx2) = s.poll("s1") else {
            panic!("expected hold")
        };
        // 提交：恰一个拿到消息、另一个被放行；队列被消费。
        s.submit("s1", "停一下", Vec::new());
        let o1 = rx1.blocking_recv().unwrap();
        let o2 = rx2.blocking_recv().unwrap();
        let (msgs, releases): (Vec<_>, Vec<_>) = [o1, o2]
            .into_iter()
            .partition(|o| matches!(o, WaitOutcome::Message(_)));
        assert_eq!(msgs.len(), 1);
        assert_eq!(releases.len(), 1);
        match &msgs[0] {
            WaitOutcome::Message(delivery) => assert_eq!(delivery.text, "停一下"),
            _ => unreachable!(),
        }
        assert_eq!(s.pending_count("s1"), 0);
    }

    #[test]
    fn composer_close_releases_waiters() {
        let s = InterjectStore::new();
        s.composer_opened("s1");
        let PollOutcome::Hold(rx) = s.poll("s1") else {
            panic!("expected hold")
        };
        s.composer_closed("s1");
        assert!(matches!(rx.blocking_recv().unwrap(), WaitOutcome::Release));
        // 关闭后再 poll → 不再等待。
        assert!(matches!(s.poll("s1"), PollOutcome::None));
    }

    #[test]
    fn dead_waiter_skipped_on_delivery() {
        let s = InterjectStore::new();
        s.composer_opened("s1");
        let PollOutcome::Hold(rx_dead) = s.poll("s1") else {
            panic!("expected hold")
        };
        drop(rx_dead); // hook 连接死亡。
        let PollOutcome::Hold(rx_live) = s.poll("s1") else {
            panic!("expected hold")
        };
        s.submit("s1", "msg", Vec::new());
        // 消息不会丢在死连接上：交付给存活的等待者。
        match rx_live.blocking_recv().unwrap() {
            WaitOutcome::Message(delivery) => assert_eq!(delivery.text, "msg"),
            _ => panic!("expected message"),
        }
    }

    #[test]
    fn submit_without_waiters_stays_queued() {
        let s = InterjectStore::new();
        s.composer_opened("s1");
        s.submit("s1", "留队", Vec::new());
        s.composer_closed("s1");
        assert_eq!(s.pending_count("s1"), 1);
        match s.poll("s1") {
            PollOutcome::Message { delivery, .. } => assert_eq!(delivery.text, "留队"),
            _ => panic!("expected message"),
        }
    }

    #[test]
    fn remove_session_clears_and_releases() {
        let s = InterjectStore::new();
        s.submit("s1", "x", Vec::new());
        s.composer_opened("s1");
        let PollOutcome::Message { .. } = s.poll("s1") else {
            panic!("expected message")
        };
        let PollOutcome::Hold(rx) = s.poll("s1") else {
            panic!("expected hold")
        };
        assert!(!s.remove_session("s1")); // 条目已被消费 → 无条目变化，但等待者被放行
        assert!(matches!(rx.blocking_recv().unwrap(), WaitOutcome::Release));
        s.submit("s2", "y", Vec::new());
        assert!(s.remove_session("s2"));
        assert_eq!(s.pending_count("s2"), 0);
    }

    #[test]
    fn retain_sessions_prunes_unknown() {
        let s = InterjectStore::new();
        s.submit("alive", "a", Vec::new());
        s.submit("gone", "b", Vec::new());
        assert!(s.retain_sessions(&["alive".to_string()]));
        assert_eq!(s.pending_count("alive"), 1);
        assert_eq!(s.pending_count("gone"), 0);
        assert_eq!(s.pending_sessions(), vec!["alive".to_string()]);
        // 再清一遍无变化。
        assert!(!s.retain_sessions(&["alive".to_string()]));
    }

    #[test]
    fn persist_roundtrip() {
        // 用独立临时文件路径测试（不碰真实 `~/.askhuman/state/interject.json`）。
        let dir = std::env::temp_dir().join(format!("ah-interject-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("interject.json");
        let s = InterjectStore::new();
        s.append("s1", "第一条", Vec::new(), None);
        s.append("s1", "第二条", Vec::new(), None);
        s.submit("s2", "另一会话", Vec::new());
        s.persist_to(&path);
        let restored = InterjectStore::load_from(&path);
        assert_eq!(restored.pending_count("s1"), 2);
        assert_eq!(restored.full_text("s1"), "第一条\n\n第二条");
        assert_eq!(restored.full_text("s2"), "另一会话");
        // 消费后 persist：条目消失。
        let PollOutcome::Message { .. } = restored.poll("s1") else {
            panic!("expected message")
        };
        restored.persist_to(&path);
        let again = InterjectStore::load_from(&path);
        assert_eq!(again.pending_count("s1"), 0);
        assert_eq!(again.pending_count("s2"), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn queued_append_records_receipt_channels_deduped() {
        let s = InterjectStore::new();
        // 排队（无 hook 在等）→ 登记来源渠道；同渠道去重、不同渠道累加。
        assert_eq!(s.append("s1", "a", Vec::new(), Some("feishu")), 1);
        assert_eq!(s.append("s1", "b", Vec::new(), Some("feishu")), 2);
        assert_eq!(s.append("s1", "c", Vec::new(), Some("telegram")), 3);
        match s.poll("s1") {
            PollOutcome::Message {
                delivery,
                receipt_channels,
            } => {
                assert_eq!(delivery.text, "a\n\nb\n\nc");
                assert_eq!(
                    receipt_channels,
                    vec!["feishu".to_string(), "telegram".to_string()]
                );
            }
            _ => panic!("expected message"),
        }
        // 消费后再 poll 无回执渠道。
        assert!(matches!(s.poll("s1"), PollOutcome::None));
    }

    #[test]
    fn immediate_delivery_records_no_receipt() {
        let s = InterjectStore::new();
        s.composer_opened("s1");
        let PollOutcome::Hold(rx) = s.poll("s1") else {
            panic!("expected hold")
        };
        // 有 hook 在等 → 立即送达（返回 0），不登记回执。
        assert_eq!(s.append("s1", "hi", Vec::new(), Some("feishu")), 0);
        assert!(matches!(
            rx.blocking_recv().unwrap(),
            WaitOutcome::Message(_)
        ));
    }

    #[test]
    fn overwrite_revoke_end_drop_receipt_channels() {
        for op in ["submit", "clear", "remove"] {
            let s = InterjectStore::new();
            s.append("s1", "queued", Vec::new(), Some("feishu"));
            match op {
                "submit" => {
                    s.submit("s1", "gui text", Vec::new());
                    // 覆盖后仍有条目，但回执渠道被清空。
                    match s.poll("s1") {
                        PollOutcome::Message {
                            receipt_channels, ..
                        } => assert!(receipt_channels.is_empty(), "submit should drop receipts"),
                        _ => panic!("expected message"),
                    }
                }
                "clear" => {
                    assert!(s.clear("s1"));
                    assert!(matches!(s.poll("s1"), PollOutcome::None));
                }
                "remove" => {
                    assert!(s.remove_session("s1"));
                    assert!(matches!(s.poll("s1"), PollOutcome::None));
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn receipt_channels_persist_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ah-interject-rc-{}", uuid::Uuid::new_v4()));
        let path = dir.join("interject.json");
        let s = InterjectStore::new();
        s.append("s1", "a", Vec::new(), Some("feishu"));
        s.append("s1", "b", Vec::new(), Some("slack"));
        s.persist_to(&path);
        let restored = InterjectStore::load_from(&path);
        match restored.poll("s1") {
            PollOutcome::Message {
                receipt_channels, ..
            } => assert_eq!(
                receipt_channels,
                vec!["feishu".to_string(), "slack".to_string()]
            ),
            _ => panic!("expected message"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loads_legacy_file_without_receipt_channels() {
        // 老格式（只有 sessions、无 receipt_channels 字段）须能无损加载（向后兼容，不丢队列）。
        let dir =
            std::env::temp_dir().join(format!("ah-interject-legacy-{}", uuid::Uuid::new_v4()));
        let path = dir.join("interject.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, r#"{"sessions":{"s1":["旧消息"]}}"#).unwrap();
        let s = InterjectStore::load_from(&path);
        assert_eq!(s.pending_count("s1"), 1);
        match s.poll("s1") {
            PollOutcome::Message {
                delivery,
                receipt_channels,
            } => {
                assert_eq!(delivery.text, "旧消息");
                assert!(receipt_channels.is_empty());
            }
            _ => panic!("expected message"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn attachment_only_entry_round_trips_and_delivers() {
        let dir =
            std::env::temp_dir().join(format!("ah-interject-attachments-{}", uuid::Uuid::new_v4()));
        let path = dir.join("interject.json");
        let file = attachment("/tmp/screenshot.png");
        let s = InterjectStore::new();
        s.append("s1", "", vec![file.clone()], None);
        assert_eq!(s.pending_count("s1"), 1);
        assert_eq!(s.full_text("s1"), "");
        assert_eq!(s.attachments("s1"), vec![file.clone()]);
        s.persist_to(&path);

        let restored = InterjectStore::load_from(&path);
        match restored.poll("s1") {
            PollOutcome::Message { delivery, .. } => {
                assert!(delivery.text.is_empty());
                assert_eq!(delivery.attachments, vec![file]);
            }
            _ => panic!("expected message"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_delivery_requeues_exact_entries_at_front() {
        let s = InterjectStore::new();
        s.append(
            "s1",
            "first",
            vec![attachment("/tmp/a.png")],
            Some("feishu"),
        );
        let PollOutcome::Message {
            delivery,
            receipt_channels,
        } = s.poll("s1")
        else {
            panic!("expected message")
        };
        s.append("s1", "later", Vec::new(), Some("slack"));
        s.requeue_front("s1", delivery, receipt_channels);
        match s.poll("s1") {
            PollOutcome::Message {
                delivery,
                receipt_channels,
            } => {
                assert_eq!(delivery.text, "first\n\nlater");
                assert_eq!(delivery.attachments, vec![attachment("/tmp/a.png")]);
                assert_eq!(receipt_channels, vec!["feishu", "slack"]);
            }
            _ => panic!("expected message"),
        }
    }
}
