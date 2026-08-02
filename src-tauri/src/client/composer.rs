//! 插话 composer 窗口 ↔ daemon 的连接管理（spec agent-interject D7）。
//!
//! 每个插话窗口持一条到 daemon 的专属连接（`InterjectComposer` 登记「composer 打开中」，
//! 此后该 session 的 PreToolUse hook 会挂起等待）；**连接生命周期与窗口一致**——窗口销毁 /
//! 取消即断开，daemon 视为 composer 关闭并放行等待中的 hook。进程内按窗口 label 索引
//! （同一 GUI 宿主可同时开多个 session 的 composer）。
//!
//! 所有函数 best-effort：daemon 不可达时窗口仍可用（提交走一次性连接兜底）。

use crate::ipc::{self, ClientMsg, ServerMsg};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc;

/// 活跃 composer 连接（key = 窗口 label；drop 发送端即关连接）。
static CONNS: OnceLock<Mutex<HashMap<String, mpsc::UnboundedSender<ClientMsg>>>> = OnceLock::new();

fn conns() -> &'static Mutex<HashMap<String, mpsc::UnboundedSender<ClientMsg>>> {
    CONNS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 预填查询的等待上限（daemon 在本机，正常毫秒级；超时不致命——仅缺预填）。
const QUERY_TIMEOUT: Duration = Duration::from_secs(3);

/// 打开（或重开）某 session 的 composer 连接：登记「composer 打开」+ 查询待送达全文作预填。
/// 返回全文、条数与附件；daemon 不可达 / 查询超时返回空（窗口仍可提交，走兜底连接）。
/// 须在 tokio 运行时上下文内调用（Tauri async 命令满足）。
pub async fn open(session_id: &str) -> (String, usize, Vec<crate::models::FileAttachment>) {
    let label = crate::gui_host::interject_label(session_id);
    close_by_label(&label); // 同窗重开：先关旧连接（daemon 侧 composer 计数配平）。

    let Ok((mut reader, mut writer)) = super::open_for_subscribe().await else {
        return (String::new(), 0, Vec::new());
    };
    let register = ClientMsg::InterjectComposer {
        session_id: session_id.to_string(),
    };
    if ipc::write_msg(&mut writer, &register).await.is_err() {
        return (String::new(), 0, Vec::new());
    }

    // 预填查询（同连接请求-响应）。
    let mut text = String::new();
    let mut entries = 0usize;
    let mut attachments = Vec::new();
    let query = ClientMsg::InterjectQuery {
        session_id: session_id.to_string(),
    };
    if ipc::write_msg(&mut writer, &query).await.is_ok() {
        if let Ok(Some((t, n, files))) =
            tokio::time::timeout(QUERY_TIMEOUT, read_interject_state(&mut reader)).await
        {
            text = t;
            entries = n;
            attachments = files;
        }
    }

    // 常驻任务：把窗口侧消息（提交等）串行写往 daemon；读端仅探测 daemon 断开。
    // 发送端全部 drop（关窗/取消）→ 排空剩余消息后退出 → 连接关闭 = composer 关闭。
    let (tx, mut rx) = mpsc::unbounded_channel::<ClientMsg>();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                m = rx.recv() => match m {
                    Some(msg) => {
                        if ipc::write_msg(&mut writer, &msg).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
                r = ipc::read_msg::<_, ServerMsg>(&mut reader) => {
                    if matches!(r, Ok(None) | Err(_)) {
                        break; // daemon 断开（重启/换新）：提交将走一次性连接兜底。
                    }
                }
            }
        }
    });
    conns().lock().unwrap().insert(label, tx);
    (text, entries, attachments)
}

/// 提交插话文本与附件（整体覆盖该 session 的待送达队列；两者都为空＝清空，spec D2）。
/// 优先走 composer 连接（保证 daemon 先见提交、后见关窗，等待中的 hook 能当场拿到消息）；
/// 连接已死（daemon 重启等）则用一次性连接兜底，消息不丢。
pub async fn submit(session_id: &str, text: &str, attachments: Vec<crate::models::FileAttachment>) {
    let label = crate::gui_host::interject_label(session_id);
    let msg = ClientMsg::InterjectSubmit {
        session_id: session_id.to_string(),
        text: text.to_string(),
        attachments,
    };
    let sent = conns()
        .lock()
        .unwrap()
        .get(&label)
        .map(|tx| tx.send(msg.clone()).is_ok())
        .unwrap_or(false);
    if !sent {
        one_shot(msg).await;
    }
}

/// 关闭某 session 的 composer 连接（取消 / 提交后收尾）。幂等。
pub fn close(session_id: &str) {
    close_by_label(&crate::gui_host::interject_label(session_id));
}

/// 按窗口 label 关闭（窗口 Destroyed 事件兜底用，label 无法反推 session）。幂等。
pub fn close_by_label(label: &str) {
    conns().lock().unwrap().remove(label);
}

/// 一次性连接发送（composer 连接不可用时的兜底；`InterjectSubmit`/`InterjectClear`
/// 在 daemon 控制环即发即走处理）。
pub async fn one_shot(msg: ClientMsg) {
    let _ = super::ensure_running().await;
    if let Ok((_reader, mut writer)) = super::connect_split().await {
        let _ = ipc::write_msg(&mut writer, &msg).await;
    }
}

/// 读到下一帧 `InterjectState`（跳过其它帧）；EOF/错误返回 None。
async fn read_interject_state<R>(
    reader: &mut R,
) -> Option<(String, usize, Vec<crate::models::FileAttachment>)>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    loop {
        match ipc::read_msg::<_, ServerMsg>(reader).await {
            Ok(Some(ServerMsg::InterjectState {
                text,
                entries,
                attachments,
            })) => return Some((text, entries, attachments)),
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// close 幂等：未注册的 label 关闭不 panic。
    #[test]
    fn close_unknown_is_noop() {
        close("no-such-session");
        close_by_label("interject-deadbeef");
    }

    /// 注册后 close 移除发送端（任务收到 None 退出 → 连接关闭）。
    #[tokio::test]
    async fn close_drops_sender() {
        let (tx, mut rx) = mpsc::unbounded_channel::<ClientMsg>();
        conns().lock().unwrap().insert("interject-test".into(), tx);
        close_by_label("interject-test");
        assert!(rx.recv().await.is_none());
    }
}
