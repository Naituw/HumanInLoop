// HumanInLoop / AskHuman —— Rust + Tauri 跨平台实现入口。
//
// 注意：本程序既是 CLI（向 stdout 输出结果）又会按需弹出 GUI 窗口。
// 因此不设置 `windows_subsystem = "windows"`，以保证 Windows 上也能向终端写 stdout。
// （代价是 GUI 模式在 Windows 上可能伴随控制台窗口，后续单独处理。）

// 开发期：部分核心 API 在后续步骤（弹窗/设置/Telegram）才会接线，先放宽未使用告警。
#![allow(dead_code)]

mod agents;
mod app;
mod ask_question;
mod autochannel;
mod channels;
mod cli;
mod client;
mod commands;
mod config;
mod confirm;
mod context_binding;
mod daemon;
mod dev_instance;
mod dev_presets;
mod dingtalk;
mod export;
mod feishu;
mod file_lock;
mod gitutil;
mod gui_host;
mod history;
mod hooks;
mod i18n;
mod integrations;
mod ipc;
mod local_time;
#[cfg(target_os = "macos")]
mod macos_dock_icon;
#[cfg(target_os = "macos")]
mod macos_menu;
#[cfg(target_os = "macos")]
mod macos_quicklook;
#[cfg(target_os = "macos")]
mod macos_window_anim;
#[cfg(target_os = "macos")]
mod macos_window_order;
mod mcp;
mod models;
mod msg_card;
mod path_identity;
mod paths;
mod perf;
mod permission_diff;
mod permission_memory;
mod permission_rules;
mod permission_shell;
mod permissions;
mod project;
mod prompts;
mod secrets;
mod select;
mod shell_safety;
mod show_last;
mod slack;
mod sound;
#[cfg(target_os = "macos")]
mod speech;
mod telegram;
mod textnorm;
mod todo_attachments;
mod todos;
mod uistate;
mod update;
mod watch;

fn main() {
    // Capture process birth ASAP so the `cli.start` perf milestone is accurate.
    perf::record_start();
    cli::dispatch();
}
