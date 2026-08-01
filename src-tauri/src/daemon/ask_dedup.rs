//! 重复提问收敛（spec `docs/specs/duplicate-ask-coalescing.md`）：会话键、提问指纹、
//! 答案重放缓存与重放标注。
//!
//! Agent 侧的一轮对话被中断（手动打断 / 连接失败）后往往会原样重发同一个提问，而被中断的
//! `AskHuman` 进程不会被杀掉，旧卡片仍然有效——于是同一个问题在人面前叠成多张卡。本模块提供
//! 判定「这两次调用是同一个提问」所需的纯逻辑：daemon 据此把新调用合流到在途请求上，或直接
//! 重放刚刚给过的答案。

use crate::app::RenderOutcome;
use crate::i18n::Lang;
use crate::ipc::TaskRequest;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// 重放窗口：答完这么久之内的相同提问直接拿旧答案（spec D5）。
pub const REPLAY_WINDOW: Duration = Duration::from_secs(300);

/// 重放缓存最多保留多少个会话（LRU 淘汰，spec D5）。
pub const REPLAY_MAX_SESSIONS: usize = 64;

/// 指纹字段分隔符：正常内容里不会出现的控制字符，避免相邻字段拼接产生歧义。
const SEP: &str = "\u{1}";

/// 会话键（spec D1）：真实 agent session 优先，退到 MCP 实例 id；都没有则不参与收敛。
/// 两类键带前缀，避免不同来源的字符串偶然相等。
pub fn session_key(task: &TaskRequest) -> Option<String> {
    let pick = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    if let Some(sid) = pick(&task.agent_session_id) {
        return Some(format!("sid:{sid}"));
    }
    pick(&task.mcp_instance_id).map(|id| format!("mcp:{id}"))
}

/// 提问指纹（spec D2）：共享 message、附件、每题文本与选项、全局模式位全部逐字一致才相同。
/// 只做首尾空白归一化，不做相似度匹配——两次 whats-next 的固定问题相同而报告不同，
/// 宽松匹配会把它们错误合流。
pub fn fingerprint(task: &TaskRequest) -> String {
    let mut hasher = Sha256::new();
    let mut feed = |value: &str| {
        hasher.update(value.trim().as_bytes());
        hasher.update(SEP.as_bytes());
    };

    feed(&task.message.text);
    for file in &task.message.files {
        feed(&file.path);
    }
    feed("questions");
    for question in &task.questions {
        feed(&question.message);
        for option in &question.predefined_options {
            feed(&option.text);
            feed(if option.recommended { "1" } else { "0" });
            feed(option.todo_id.as_deref().unwrap_or(""));
        }
        feed("/q");
    }
    feed("modes");
    feed(if task.is_markdown { "1" } else { "0" });
    feed(if task.select_only { "1" } else { "0" });
    feed(if task.single { "1" } else { "0" });
    feed(if task.whats_next { "1" } else { "0" });
    feed(match task.output_format {
        crate::models::OutputFormat::Text => "text",
        crate::models::OutputFormat::Json => "json",
    });

    format!("{:x}", hasher.finalize())
}

/// 一条已完成问答的重放记录。
#[derive(Clone)]
pub struct ReplayEntry {
    pub fingerprint: String,
    pub outcome: RenderOutcome,
    pub request_id: String,
    pub finished_at: Instant,
}

/// 命中重放时返回的内容：结果本体 + 距离作答过去了多久。
pub struct ReplayHit {
    pub outcome: RenderOutcome,
    pub request_id: String,
    pub ago: Duration,
}

/// 每会话只留最近一条已完成问答的重放缓存（仅内存，spec D5）。
#[derive(Default)]
pub struct ReplayCache {
    entries: HashMap<String, ReplayEntry>,
    /// 访问顺序（队尾最新），用于 LRU 淘汰。
    order: Vec<String>,
}

impl ReplayCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次真实作答的结果（覆盖该会话的上一条）。
    pub fn put(&mut self, session_key: &str, entry: ReplayEntry) {
        self.entries.insert(session_key.to_string(), entry);
        self.touch(session_key);
        while self.order.len() > REPLAY_MAX_SESSIONS {
            let oldest = self.order.remove(0);
            self.entries.remove(&oldest);
        }
    }

    /// 窗口内的同指纹命中；过期项顺手清理。
    pub fn get_fresh(
        &mut self,
        session_key: &str,
        fingerprint: &str,
        now: Instant,
    ) -> Option<ReplayHit> {
        let entry = self.entries.get(session_key)?;
        let ago = now.saturating_duration_since(entry.finished_at);
        if ago > REPLAY_WINDOW {
            self.entries.remove(session_key);
            self.order.retain(|key| key != session_key);
            return None;
        }
        if entry.fingerprint != fingerprint {
            return None;
        }
        let hit = ReplayHit {
            outcome: entry.outcome.clone(),
            request_id: entry.request_id.clone(),
            ago,
        };
        self.touch(session_key);
        Some(hit)
    }

    fn touch(&mut self, session_key: &str) {
        self.order.retain(|key| key != session_key);
        self.order.push(session_key.to_string());
    }
}

/// 给重放结果打上说明（spec D7）：复用既有的 `status` 载体，文本加 `[status]` 区块，
/// JSON 填 `status` 字段（`action` 保持原值——重放的是一次作答，不是取消）。
pub fn mark_replayed(
    outcome: &RenderOutcome,
    ago: Duration,
    json: bool,
    lang: Lang,
) -> RenderOutcome {
    let note = replay_note(ago, lang);
    let stdout = if json {
        match serde_json::from_str::<serde_json::Value>(&outcome.stdout) {
            Ok(serde_json::Value::Object(mut map)) => {
                map.insert("status".into(), serde_json::Value::String(note));
                serde_json::to_string(&serde_json::Value::Object(map))
                    .unwrap_or_else(|_| outcome.stdout.clone())
            }
            // 不是对象或解析失败：原样返回，绝不产出坏 JSON。
            _ => outcome.stdout.clone(),
        }
    } else {
        format!(
            "{}\n{}\n\n{}",
            crate::cli::output::MARKER_STATUS,
            note,
            outcome.stdout
        )
    };
    RenderOutcome {
        stdout,
        stderr: outcome.stderr.clone(),
        exit_code: outcome.exit_code,
    }
}

fn replay_note(ago: Duration, lang: Lang) -> String {
    let secs = ago.as_secs().max(1);
    crate::i18n::tr(lang, "status.replayed").replace("{n}", &secs.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{FileAttachment, MessagePrompt, OptionItem, OutputFormat, Question};

    fn attachment(path: &str) -> FileAttachment {
        FileAttachment {
            path: path.into(),
            name: "a.png".into(),
            size: 1,
            is_image: true,
        }
    }

    fn task() -> TaskRequest {
        TaskRequest {
            message: MessagePrompt {
                text: "hello".into(),
                files: vec![attachment("/tmp/a.png")],
            },
            questions: vec![Question {
                message: "go on?".into(),
                predefined_options: vec![OptionItem {
                    text: "yes".into(),
                    recommended: true,
                    todo_id: None,
                    todo_text: None,
                    todo_attachments: Vec::new(),
                }],
            }],
            is_markdown: true,
            source: "Cursor".into(),
            lang: "zh".into(),
            project: "/tmp/proj".into(),
            select_only: false,
            single: false,
            output_format: OutputFormat::Text,
            record_history: true,
            agent_kind: Some("cursor".into()),
            agent_session_id: Some("session-1".into()),
            mcp_instance_id: None,
            agent_pid: None,
            caller_pid: 0,
            from_mcp: false,
            whats_next: false,
            perf_id: String::new(),
            perf_autodismiss: false,
        }
    }

    fn outcome(text: &str) -> RenderOutcome {
        RenderOutcome {
            stdout: text.into(),
            stderr: None,
            exit_code: 0,
        }
    }

    #[test]
    fn session_key_prefers_agent_session_then_mcp_instance() {
        let mut t = task();
        assert_eq!(session_key(&t).as_deref(), Some("sid:session-1"));

        t.agent_session_id = Some("   ".into());
        t.mcp_instance_id = Some("inst-9".into());
        assert_eq!(session_key(&t).as_deref(), Some("mcp:inst-9"));

        t.mcp_instance_id = None;
        assert!(session_key(&t).is_none());

        // 两类键不互相碰撞。
        let mut a = task();
        a.agent_session_id = Some("x".into());
        let mut b = task();
        b.agent_session_id = None;
        b.mcp_instance_id = Some("x".into());
        assert_ne!(session_key(&a), session_key(&b));
    }

    #[test]
    fn fingerprint_ignores_only_surrounding_whitespace() {
        let base = fingerprint(&task());

        let mut padded = task();
        padded.message.text = "  hello \n".into();
        padded.questions[0].message = "go on?  ".into();
        assert_eq!(fingerprint(&padded), base);

        // 与提问内容无关的传输字段不进指纹。
        let mut other_caller = task();
        other_caller.source = "Codex".into();
        other_caller.project = "/tmp/elsewhere".into();
        other_caller.caller_pid = 4242;
        other_caller.perf_id = "perf".into();
        assert_eq!(fingerprint(&other_caller), base);
    }

    #[test]
    fn fingerprint_separates_every_content_and_mode_field() {
        let base = fingerprint(&task());

        let mut message = task();
        message.message.text = "hello!".into();
        assert_ne!(fingerprint(&message), base);

        let mut files = task();
        files.message.files = vec![attachment("/tmp/b.png")];
        assert_ne!(fingerprint(&files), base);

        let mut question = task();
        question.questions[0].message = "go on??".into();
        assert_ne!(fingerprint(&question), base);

        let mut option = task();
        option.questions[0].predefined_options[0].text = "no".into();
        assert_ne!(fingerprint(&option), base);

        let mut recommended = task();
        recommended.questions[0].predefined_options[0].recommended = false;
        assert_ne!(fingerprint(&recommended), base);

        let mut todo = task();
        todo.questions[0].predefined_options[0].todo_id = Some("t-1".into());
        assert_ne!(fingerprint(&todo), base);

        let mut markdown = task();
        markdown.is_markdown = false;
        assert_ne!(fingerprint(&markdown), base);

        let mut select_only = task();
        select_only.select_only = true;
        assert_ne!(fingerprint(&select_only), base);

        let mut single = task();
        single.single = true;
        assert_ne!(fingerprint(&single), base);

        let mut whats_next = task();
        whats_next.whats_next = true;
        assert_ne!(fingerprint(&whats_next), base);

        let mut json = task();
        json.output_format = OutputFormat::Json;
        assert_ne!(fingerprint(&json), base);
    }

    #[test]
    fn fingerprint_does_not_confuse_field_boundaries() {
        let mut split = task();
        split.message.text = "ab".into();
        split.questions[0].message = "c".into();
        let mut merged = task();
        merged.message.text = "a".into();
        merged.questions[0].message = "bc".into();
        assert_ne!(fingerprint(&split), fingerprint(&merged));
    }

    fn entry(fp: &str, text: &str, finished_at: Instant) -> ReplayEntry {
        ReplayEntry {
            fingerprint: fp.into(),
            outcome: outcome(text),
            request_id: "req".into(),
            finished_at,
        }
    }

    #[test]
    fn replay_cache_hits_only_within_window_and_same_fingerprint() {
        let now = Instant::now();
        let mut cache = ReplayCache::new();
        cache.put("sid:a", entry("fp", "answer", now));

        let hit = cache.get_fresh("sid:a", "fp", now).expect("fresh hit");
        assert_eq!(hit.outcome.stdout, "answer");
        assert!(cache.get_fresh("sid:a", "other-fp", now).is_none());
        assert!(cache.get_fresh("sid:b", "fp", now).is_none());

        let expired = now + REPLAY_WINDOW + Duration::from_secs(1);
        assert!(cache.get_fresh("sid:a", "fp", expired).is_none());
        // 过期项已被清理。
        assert!(cache.get_fresh("sid:a", "fp", now).is_none());
    }

    #[test]
    fn replay_cache_keeps_one_entry_per_session_and_evicts_lru() {
        let now = Instant::now();
        let mut cache = ReplayCache::new();
        cache.put("sid:a", entry("fp1", "first", now));
        cache.put("sid:a", entry("fp2", "second", now));
        assert!(cache.get_fresh("sid:a", "fp1", now).is_none());
        assert_eq!(
            cache.get_fresh("sid:a", "fp2", now).unwrap().outcome.stdout,
            "second"
        );

        for i in 0..REPLAY_MAX_SESSIONS {
            cache.put(&format!("sid:{i}"), entry("fp", "x", now));
        }
        // `sid:a` 是最久未访问的，已被淘汰。
        assert!(cache.get_fresh("sid:a", "fp2", now).is_none());
        assert!(cache
            .get_fresh(&format!("sid:{}", REPLAY_MAX_SESSIONS - 1), "fp", now)
            .is_some());
    }

    #[test]
    fn mark_replayed_prepends_status_block_in_text_mode() {
        let marked = mark_replayed(
            &outcome("[user_input]\nyes"),
            Duration::from_secs(12),
            false,
            Lang::Zh,
        );
        assert!(marked.stdout.starts_with("[status]\n"));
        assert!(marked.stdout.contains("12 秒前"));
        assert!(marked.stdout.ends_with("[user_input]\nyes"));
        assert_eq!(marked.exit_code, 0);
    }

    #[test]
    fn mark_replayed_fills_status_field_in_json_mode() {
        let marked = mark_replayed(
            &outcome(r#"{"action":"answer","answers":[]}"#),
            Duration::from_secs(3),
            true,
            Lang::En,
        );
        let value: serde_json::Value = serde_json::from_str(&marked.stdout).expect("valid json");
        assert_eq!(value["action"], "answer");
        assert!(value["status"].as_str().unwrap().contains("3s ago"));
    }

    #[test]
    fn mark_replayed_leaves_unparsable_json_untouched() {
        let marked = mark_replayed(&outcome("not json"), Duration::from_secs(1), true, Lang::En);
        assert_eq!(marked.stdout, "not json");
    }
}
