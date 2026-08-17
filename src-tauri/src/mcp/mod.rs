//! `AskHuman mcp`：以 STDIO 运行 MCP server，暴露 `ask`、`whats_next`、`show_last` 与 `todo_add`。
//!
//! `ask` / `whats_next` 为「薄壳」：每次工具调用都 spawn 一个现有的 `AskHuman …` 子进程（`ask` 带
//! `--output json`，`whats_next` 走文本模式），
//! 复用全部既有 ask 流程（弹窗 / IM / 抢答 / 历史 / 落盘 / 排空与自动重连），再把人类回复中的
//! 图片读回转成 MCP `ImageContent` 一并返回。`todo_add` 在 MCP 进程内直写 `todos.json`。
//! 全平台同一套；daemon 换新 / 重启后下一次 ask/whats_next 调用自动重连
//! （每次调用都是新起子进程、重新连接 daemon，因此 MCP server 进程可长期存活、跨 daemon 重启）。

pub(crate) mod ask;

use rmcp::{transport::stdio, ServiceExt};

// （`whats_next` 见 spec todo-whats-next D2：完成任务后必调，结果为下一个任务或「准许结束」。）

/// 进入 STDIO MCP server 事件循环（不返回）。
pub fn run() -> ! {
    let code = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt.block_on(serve()),
        Err(_) => 3,
    };
    std::process::exit(code);
}

/// 建 server、握手、等关闭。返回进程退出码。
async fn serve() -> i32 {
    #[cfg(windows)]
    let parent = mcp_parent_process();
    let server = ask::AskServer::new();
    server.register_instance().await;
    match server.serve(stdio()).await {
        Ok(service) => {
            #[cfg(windows)]
            let parent_watcher = parent.map(|parent| {
                let cancellation = service.cancellation_token();
                tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
                    wait_for_parent_exit(&parent).await;
                    // Cancelling the rmcp service also cancels every request child token. The ask
                    // handler then drops its kill-on-drop CLI child, producing the daemon socket
                    // EOF that finalizes popup and IM cancellation before this process exits.
                    cancellation.cancel();
                }))
            });
            let _ = service.waiting().await;
            #[cfg(windows)]
            drop(parent_watcher);
            0
        }
        // 握手失败（如非 MCP 客户端误启）：直接退出，stdout 不能有杂音。
        Err(_) => 3,
    }
}

#[cfg(windows)]
fn mcp_parent_process() -> Option<crate::agents::detect::ProcessIdentity> {
    let parent_pid = crate::agents::detect::parent_pid(std::process::id())?;
    crate::agents::detect::inspect_process(parent_pid)
}

#[cfg(windows)]
async fn wait_for_parent_exit(parent: &crate::agents::detect::ProcessIdentity) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        if !original_parent_alive(parent) {
            return;
        }
    }
}

#[cfg(windows)]
fn original_parent_alive(expected: &crate::agents::detect::ProcessIdentity) -> bool {
    if !crate::agents::detect::pid_alive(expected.pid) {
        return false;
    }
    let Some(current) = crate::agents::detect::inspect_process(expected.pid) else {
        // Access restrictions are not evidence of process death. Fail open until pid_alive says
        // otherwise rather than cancelling a healthy Agent session.
        return true;
    };
    process_instance_matches(expected, &current)
}

fn process_instance_matches(
    expected: &crate::agents::detect::ProcessIdentity,
    current: &crate::agents::detect::ProcessIdentity,
) -> bool {
    expected.pid == current.pid
        && match (expected.creation_time, current.creation_time) {
            (Some(expected), Some(current)) => expected == current,
            _ => true,
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(pid: u32, creation_time: Option<u64>) -> crate::agents::detect::ProcessIdentity {
        crate::agents::detect::ProcessIdentity {
            pid,
            parent_pid: 1,
            executable: None,
            command_line: None,
            session_id: None,
            creation_time,
        }
    }

    #[test]
    fn parent_identity_rejects_pid_reuse_and_accepts_unavailable_creation_time() {
        assert!(process_instance_matches(
            &identity(42, Some(100)),
            &identity(42, Some(100))
        ));
        assert!(!process_instance_matches(
            &identity(42, Some(100)),
            &identity(42, Some(101))
        ));
        assert!(!process_instance_matches(
            &identity(42, Some(100)),
            &identity(43, Some(100))
        ));
        assert!(process_instance_matches(
            &identity(42, None),
            &identity(42, Some(100))
        ));
    }
}
