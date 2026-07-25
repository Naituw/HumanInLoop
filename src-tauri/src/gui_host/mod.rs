//! 统一 GUI 宿主进程的「自有 IPC」协议与客户端（spec D2/D3/D13）。
//!
//! 宿主进程（`AskHuman --gui-host`）单实例承载托盘图标 + 设置/历史/待办/Agent 窗口。它另起一条
//! **与 daemon 解耦**的 Unix socket（`~/.askhuman/gui-host.sock`），接收来自 CLI（`--settings`
//! /`--history`/`--todos`/`agents monitor`）与弹窗导航按钮的「打开窗口」请求，从而保证每类窗口全局唯一。
//!
//! 传输复用 `ipc::codec` 的 NDJSON 编解码；协议见 `HostMsg`。客户端入口为 `host_open`。
//! 宿主侧的监听 / 窗口管理 / 托盘逻辑见 `app::gui_host`。

use serde::{Deserialize, Serialize};

/// 要打开的窗口类型。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum WindowKind {
    Settings,
    History,
    Agents,
    /// 插话 composer（spec agent-interject D7）：每 session 全局唯一，`OpenWindow.session` 必填。
    Interject,
    /// 项目待办窗口（spec todo-whats-next D9）：全局唯一；`project` 为预选项目 key（可空）。
    Todos,
    /// 「新建 Agent 任务」窗口（spec gui-agent-task-launch）：全局唯一；
    /// `project` 为预选项目 key、`todo` 为预选待办 id（均可空）。
    NewTask,
}

/// CLI / 弹窗 → 宿主 的消息。
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HostMsg {
    /// 打开（或聚焦已存在的）指定窗口。`all` 仅历史窗口使用（默认展示全部项目）；
    /// `project` 按窗口类型复用：历史窗口=调用方项目 key（空串=未知项目，宿主里的历史窗口
    /// 默认过滤到该项目而非宿主自身 cwd）；设置窗口=初始定位 tab（如 "channel"）。
    /// `session` 用于 Agent 窗口预选或插话窗口唯一键；`agent`/`cwd` 仅插话窗口使用：
    /// 家族（头部胶囊）与工作目录（头部项目名）。`todo` 仅新建任务窗口使用：预选待办 id。
    /// 旧宿主忽略未知字段（serde default 兼容）。
    OpenWindow {
        kind: WindowKind,
        #[serde(default)]
        all: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        todo: Option<String>,
    },
    /// 探活（保留；当前 `host_open` 不依赖回包）。
    Ping,
    /// 请求宿主退出（mode→off 时可由设置/守护进程触发）。
    Shutdown,
}

/// 插话窗口的额外参数（session 必填；agent/cwd 用于头部展示）。
#[derive(Clone, Debug)]
pub struct InterjectTarget {
    pub session: String,
    pub agent: Option<String>,
    pub cwd: Option<String>,
}

/// 插话 composer 的窗口 label：每 session 全局唯一。session_id 可能含 label 非法字符，
/// 用哈希编码（仅进程内聚焦去重用，无需跨进程/跨版本稳定）。
pub fn interject_label(session_id: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    session_id.hash(&mut h);
    format!("interject-{:016x}", h.finish())
}

#[cfg(unix)]
pub use unix_impl::{bind, host_open, spawn_detached, spawn_detached_from};

#[cfg(unix)]
mod unix_impl {
    use super::{HostMsg, InterjectTarget, WindowKind};
    use crate::ipc;
    use crate::paths::gui_host_sock;
    use std::io::{Error, ErrorKind};
    use std::time::{Duration, Instant};
    use tokio::io::BufReader;
    use tokio::net::{UnixListener, UnixStream};

    /// 宿主侧绑定监听 `gui-host.sock`。调用前应已持有 `gui-host.lock`（flock），
    /// 故可安全删除残留 socket 再 bind。权限 0600。
    pub fn bind() -> std::io::Result<UnixListener> {
        use std::os::unix::fs::PermissionsExt;
        let path = gui_host_sock();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        Ok(listener)
    }

    /// 客户端连接到宿主 socket。
    pub async fn connect() -> std::io::Result<UnixStream> {
        UnixStream::connect(gui_host_sock()).await
    }

    /// 后台拉起宿主进程（`AskHuman --gui-host`，detach 新会话脱离调用方终端）。
    /// 单实例由宿主自身的 flock 去重——重复 spawn 的多余进程会因抢锁失败而立即退出。
    pub fn spawn_detached() -> std::io::Result<()> {
        let exe = std::env::current_exe()?;
        spawn_detached_from(&exe)
    }

    /// Start GUI Host from a caller-supplied stable disk path.
    ///
    /// The running executable can be replaced during self-update. In particular, Linux may then
    /// report `current_exe()` as a deleted inode path, so the old Host passes its launch path here.
    pub fn spawn_detached_from(exe: &std::path::Path) -> std::io::Result<()> {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        let mut cmd = Command::new(exe);
        cmd.arg("--gui-host")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        cmd.spawn().map(|_| ())
    }

    /// 把「打开窗口」请求路由到宿主（spec D3）。同步阻塞，内部在独立线程跑一个 current-thread
    /// 运行时，避免在 Tauri 命令（可能已处于某 tokio 运行时上下文）中嵌套运行时而 panic。
    ///
    /// 流程：连宿主 → 发 `OpenWindow` → 返回；连不上则 `spawn --gui-host` 后轮询重连。
    /// 全程失败返回 `Err`，调用方据此回退到「本进程直接建窗」兜底（保证至少能打开窗口）。
    /// `target.session` 也用于 Agent 窗口预选；`target.agent/cwd` 仅插话窗口使用；
    /// `todo` 仅新建任务窗口使用，其余窗口传 `None`。
    pub fn host_open(
        kind: WindowKind,
        all: bool,
        project: Option<String>,
        target: Option<InterjectTarget>,
        todo: Option<String>,
    ) -> std::io::Result<()> {
        let handle = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(host_open_async(kind, all, project, target, todo))
        });
        match handle.join() {
            Ok(r) => r,
            Err(_) => Err(Error::other("host_open worker panicked")),
        }
    }

    async fn host_open_async(
        kind: WindowKind,
        all: bool,
        project: Option<String>,
        target: Option<InterjectTarget>,
        todo: Option<String>,
    ) -> std::io::Result<()> {
        // 1. 宿主已在 → 直接发送。
        if let Ok(stream) = connect().await {
            return send_open(stream, kind, all, project, target, todo).await;
        }
        // 2. 宿主不在 → 拉起后轮询重连（最多约 6 秒，覆盖 Tauri 进程启动 + socket 就绪）。
        spawn_detached()?;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(6) {
            tokio::time::sleep(Duration::from_millis(80)).await;
            if let Ok(stream) = connect().await {
                return send_open(stream, kind, all, project, target, todo).await;
            }
        }
        Err(Error::new(
            ErrorKind::TimedOut,
            "gui-host did not become ready in time",
        ))
    }

    async fn send_open(
        stream: UnixStream,
        kind: WindowKind,
        all: bool,
        project: Option<String>,
        target: Option<InterjectTarget>,
        todo: Option<String>,
    ) -> std::io::Result<()> {
        let (r, mut w) = stream.into_split();
        let (session, agent, cwd) = match target {
            Some(t) => (Some(t.session), t.agent, t.cwd),
            None => (None, None, None),
        };
        // 写出请求并 flush；内核缓冲该行，即便随后关闭连接，宿主仍能读到。
        ipc::write_msg(
            &mut w,
            &HostMsg::OpenWindow {
                kind,
                all,
                project,
                session,
                agent,
                cwd,
                todo,
            },
        )
        .await?;
        // 读一行作为「已受理」回执（宿主收到后回 Ping 作为 ack）；超时也按成功处理（已写入内核缓冲）。
        let mut reader = BufReader::new(r);
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            ipc::read_msg::<_, HostMsg>(&mut reader),
        )
        .await;
        Ok(())
    }
}
