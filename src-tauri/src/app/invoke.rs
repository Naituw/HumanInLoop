//! Tauri IPC command routing.
//!
//! Keep each generated handler in a separate non-inlined function. A single handler containing
//! every command makes LLVM reserve stack space for the largest command future on every invoke.
//! That exhausts the Windows UI thread's default stack while WebView2 is still on the call stack.

use tauri::ipc::Invoke;
use tauri::Wry;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Group {
    Core,
    Integration,
    Channel,
    History,
    Todo,
    Task,
    Update,
}

pub(super) fn handle(invoke: Invoke<Wry>) -> bool {
    match group_for(invoke.message.command()) {
        Group::Core => core(invoke),
        Group::Integration => integration(invoke),
        Group::Channel => channel(invoke),
        Group::History => history(invoke),
        Group::Todo => todo(invoke),
        Group::Task => task(invoke),
        Group::Update => update(invoke),
    }
}

fn group_for(command: &str) -> Group {
    if command.starts_with("todos_")
        || matches!(command, "todo_attachment_thumbnail" | "open_todos")
    {
        Group::Todo
    } else if command.starts_with("new_task_")
        || command.starts_with("fork_task_")
        || matches!(
            command,
            "open_new_task" | "open_fork_task" | "project_key_of"
        )
    {
        Group::Task
    } else if command.starts_with("update_")
        || matches!(command, "get_app_version" | "restart_settings")
    {
        Group::Update
    } else if command.starts_with("history_")
        || command.starts_with("agents_")
        || command.starts_with("console_")
        || command.starts_with("interject_")
        || matches!(
            command,
            "open_history"
                | "open_interject"
                | "focus_request"
                | "get_history"
                | "get_history_projects"
                | "trim_history"
                | "delete_history_entries"
                | "clear_all_history"
                | "resolve_history_session_titles"
        )
    {
        Group::History
    } else if command.ends_with("_test")
        || command.contains("_detect_")
        || matches!(command, "detect_cancel" | "channel_health")
    {
        Group::Channel
    } else if command.starts_with("agent_")
        || command.starts_with("cursor_hook_")
        || command.starts_with("claude_hook_")
        || command.starts_with("mcp_")
        || matches!(
            command,
            "permission_rules_panel"
                | "collaboration_style_defaults"
                | "collaboration_style_apply_integrations"
                | "focus_agent_terminal"
        )
    {
        Group::Integration
    } else {
        Group::Core
    }
}

#[inline(never)]
fn core(invoke: Invoke<Wry>) -> bool {
    let handler: fn(Invoke<Wry>) -> bool = tauri::generate_handler![
        crate::commands::popup_init,
        crate::commands::enrich_permission_diff,
        crate::commands::perf_mark,
        crate::commands::popup_agent_terminal,
        crate::commands::popup_agent_resolved,
        crate::commands::popup_show_window,
        crate::commands::submit_popup,
        crate::commands::submit_confirm_action,
        crate::commands::confirm_popup_ready,
        crate::commands::cancel_popup,
        crate::commands::open_path,
        crate::commands::preview_attachments,
        crate::commands::close_preview,
        crate::commands::read_image_data_url,
        crate::commands::file_icon_data_url,
        crate::commands::show_attachment_menu,
        crate::commands::get_settings,
        crate::commands::save_settings,
        crate::commands::get_prompt,
        crate::commands::open_test_popup,
        crate::commands::popup_sound_support,
        crate::commands::play_popup_sound,
        crate::commands::set_theme,
        crate::commands::update_theme,
        crate::commands::open_settings,
        crate::commands::open_agent_console,
        crate::commands::popup_im_tip_visible,
        crate::commands::popup_im_tip_dismiss,
        crate::commands::apply_window_effect,
        crate::commands::start_speech,
        crate::commands::stop_speech,
        crate::commands::flush_speech,
        crate::commands::speech_available,
        crate::commands::popup_update_state,
    ];
    handler(invoke)
}

#[inline(never)]
fn integration(invoke: Invoke<Wry>) -> bool {
    let handler: fn(Invoke<Wry>) -> bool = tauri::generate_handler![
        crate::commands::permission_rules_panel,
        crate::commands::agent_task_workspaces,
        crate::commands::agent_task_workspace_add,
        crate::commands::agent_task_workspace_pin,
        crate::commands::agent_task_workspace_hide,
        crate::commands::agent_task_workspace_forget,
        crate::commands::agent_task_readiness,
        crate::commands::agent_task_test_terminal,
        crate::commands::collaboration_style_defaults,
        crate::commands::collaboration_style_apply_integrations,
        crate::commands::cursor_hook_status,
        crate::commands::cursor_hook_install,
        crate::commands::cursor_hook_update,
        crate::commands::cursor_hook_uninstall,
        crate::commands::cursor_hook_reveal,
        crate::commands::claude_hook_status,
        crate::commands::claude_hook_install,
        crate::commands::claude_hook_update,
        crate::commands::claude_hook_uninstall,
        crate::commands::claude_hook_reveal,
        crate::commands::agent_rule_status,
        crate::commands::agent_rule_install,
        crate::commands::agent_rule_update,
        crate::commands::agent_rule_uninstall,
        crate::commands::agent_rule_reveal,
        crate::commands::agent_rule_open,
        crate::commands::agent_mode_status,
        crate::commands::agent_mode_set,
        crate::commands::agent_mode_update,
        crate::commands::agent_mode_update_artifact,
        crate::commands::agent_permission_set,
        crate::commands::agent_stop_set,
        crate::commands::agent_ask_question_set,
        crate::commands::mcp_config_reveal,
        crate::commands::mcp_config_open,
        crate::commands::mcp_command_path,
        crate::commands::agent_hook_reveal,
        crate::commands::agent_hook_open,
        crate::commands::agent_lifecycle_status,
        crate::commands::agent_lifecycle_install,
        crate::commands::agent_lifecycle_uninstall,
        crate::commands::focus_agent_terminal,
        crate::commands::agent_force_idle,
    ];
    handler(invoke)
}

#[inline(never)]
fn channel(invoke: Invoke<Wry>) -> bool {
    let handler: fn(Invoke<Wry>) -> bool = tauri::generate_handler![
        crate::commands::telegram_test,
        crate::commands::dingtalk_test,
        crate::commands::dingtalk_detect_prepare,
        crate::commands::dingtalk_detect_wait,
        crate::commands::feishu_test,
        crate::commands::feishu_detect_prepare,
        crate::commands::feishu_detect_wait,
        crate::commands::slack_test,
        crate::commands::slack_detect_prepare,
        crate::commands::slack_detect_wait,
        crate::commands::detect_cancel,
        crate::commands::channel_health,
    ];
    handler(invoke)
}

#[inline(never)]
fn history(invoke: Invoke<Wry>) -> bool {
    let handler: fn(Invoke<Wry>) -> bool = tauri::generate_handler![
        crate::commands::open_interject,
        crate::commands::interject_init,
        crate::commands::interject_submit,
        crate::commands::interject_cancel,
        crate::commands::interject_clear,
        crate::commands::open_history,
        crate::commands::history_init,
        crate::commands::agents_init,
        crate::commands::agents_start_subscription,
        crate::commands::agents_focus,
        crate::commands::focus_request,
        crate::commands::interject_append,
        crate::commands::interject_peek,
        crate::commands::console_transcript,
        crate::commands::console_diff_stat,
        crate::commands::console_diff_file,
        crate::commands::console_stage,
        crate::commands::get_history,
        crate::commands::get_history_projects,
        crate::commands::history_count,
        crate::commands::trim_history,
        crate::commands::delete_history_entries,
        crate::commands::clear_all_history,
        crate::commands::resolve_history_session_titles,
    ];
    handler(invoke)
}

#[inline(never)]
fn todo(invoke: Invoke<Wry>) -> bool {
    let handler: fn(Invoke<Wry>) -> bool = tauri::generate_handler![
        crate::commands::todos_list,
        crate::commands::todos_add,
        crate::commands::todos_update,
        crate::commands::todos_update_attachments,
        crate::commands::todos_attach_pasted_images,
        crate::commands::todo_attachment_thumbnail,
        crate::commands::todos_remove,
        crate::commands::todos_complete,
        crate::commands::todos_clear,
        crate::commands::todos_reorder,
        crate::commands::todos_set_auto,
        crate::commands::todos_set_text,
        crate::commands::todos_history,
        crate::commands::todos_restore,
        crate::commands::todos_history_clear,
        crate::commands::todos_init,
        crate::commands::todos_projects,
        crate::commands::todos_projects_enriched,
        crate::commands::open_todos,
    ];
    handler(invoke)
}

#[inline(never)]
fn task(invoke: Invoke<Wry>) -> bool {
    let handler: fn(Invoke<Wry>) -> bool = tauri::generate_handler![
        crate::commands::open_new_task,
        crate::commands::open_fork_task,
        crate::commands::new_task_init,
        crate::commands::new_task_projects,
        crate::commands::new_task_projects_refreshed,
        crate::commands::project_key_of,
        crate::commands::new_task_launch,
        crate::commands::fork_task_init,
        crate::commands::fork_task_launch,
    ];
    handler(invoke)
}

#[inline(never)]
fn update(invoke: Invoke<Wry>) -> bool {
    let handler: fn(Invoke<Wry>) -> bool = tauri::generate_handler![
        crate::commands::get_app_version,
        crate::commands::update_check,
        crate::commands::update_get_notes,
        crate::commands::update_get_version_notes,
        crate::commands::update_apply,
        crate::commands::update_prepare,
        crate::commands::update_dismiss,
        crate::commands::restart_settings,
    ];
    handler(invoke)
}

#[cfg(test)]
mod tests {
    use super::{group_for, Group};

    #[test]
    fn routes_stack_heavy_commands_away_from_core() {
        assert_eq!(group_for("feishu_detect_prepare"), Group::Channel);
        assert_eq!(group_for("feishu_detect_wait"), Group::Channel);
        assert_eq!(group_for("todos_projects_enriched"), Group::Todo);
        assert_eq!(group_for("new_task_projects_refreshed"), Group::Task);
        assert_eq!(group_for("agent_task_workspaces"), Group::Integration);
    }

    #[test]
    fn routes_representative_commands_to_each_group() {
        assert_eq!(group_for("popup_init"), Group::Core);
        assert_eq!(group_for("channel_health"), Group::Channel);
        assert_eq!(group_for("console_transcript"), Group::History);
        assert_eq!(group_for("todos_add"), Group::Todo);
        assert_eq!(group_for("fork_task_launch"), Group::Task);
        assert_eq!(group_for("update_apply"), Group::Update);
    }
}
