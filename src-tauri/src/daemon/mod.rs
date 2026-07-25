//! 常驻 Daemon：子命令分发（run/start/stop/restart/status/logs）+ Phase 0 的空 Daemon 服务。
//!
//! Phase 0：起一个不承载任何渠道的空 Daemon，提供握手（含二进制指纹换新）、status、stop、
//! 单实例（flock）、自启、空闲退出。渠道 / 弹窗 / 提交将在后续 Phase 接入。

pub mod ask_dedup;
#[cfg(unix)]
pub mod config_watch;
pub mod lifecycle;
#[cfg(unix)]
pub mod popup_focus;
pub mod request;
#[cfg(unix)]
pub mod spawn;

/// `AskHuman daemon <sub>` 入口。永不返回（自行退出进程）。
pub fn dispatch(args: &[String]) -> ! {
    #[cfg(unix)]
    {
        std::process::exit(unix_impl::dispatch(args));
    }
    #[cfg(not(unix))]
    {
        let _ = args;
        eprintln!("AskHuman daemon is not supported on this platform yet.");
        std::process::exit(1);
    }
}

#[cfg(unix)]
mod unix_impl;
