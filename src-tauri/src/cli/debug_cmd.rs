//! 隐藏调试子命令组 `AskHuman debug …`（不进 help；PoC 后保留作回归工具）。
//!
//! `dd-watch-poc`：钉钉 watch 卡高频更新探针（`docs/plans/im-watch-channels.md` §4）。
//! 建卡投放后循环就地更新模板变量，逐次记录 OpenAPI 耗时与错误；同时自连一条 Stream
//! （仅卡片回调 topic）打印按钮回调。核心验证：高频 `PUT /v1.0/card/instances` 是否触发频控。

use super::cfgio;
use crate::config::AppConfig;
use crate::i18n::Lang;
use std::process::exit;

pub fn dispatch(args: &[String], _lang: Lang) {
    match args.first().map(|s| s.as_str()) {
        Some("dd-watch-poc") => dd_watch_poc(&args[1..]),
        _ => {
            eprintln!(
                "usage: AskHuman debug dd-watch-poc [--count N] [--interval-ms MS] [--template ID]\n       --count 0 = 只发一张卡看样式（不更新、不定格、不连 Stream）"
            );
            exit(1);
        }
    }
}

/// 钉钉 watch PoC 探针：建卡 + N 次间隔更新 + 终态定格；stdout 打印逐次耗时与统计。
fn dd_watch_poc(args: &[String]) {
    let mut count: usize = 60;
    let mut interval_ms: u64 = 2000;
    let mut template = crate::dingtalk::watch::DEFAULT_WATCH_CARD_TEMPLATE_ID.to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--count" if i + 1 < args.len() => {
                count = args[i + 1].parse().unwrap_or(count);
                i += 2;
            }
            "--interval-ms" if i + 1 < args.len() => {
                interval_ms = args[i + 1].parse().unwrap_or(interval_ms);
                i += 2;
            }
            "--template" if i + 1 < args.len() => {
                template = args[i + 1].clone();
                i += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                exit(1);
            }
        }
    }

    let config = AppConfig::load();
    let dd = config.channels.dingding.clone();
    let client = match crate::dingtalk::client::DingTalkClient::new(&dd) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("dingtalk config invalid: {e}");
            exit(1);
        }
    };

    println!("dd-watch-poc: template={template} count={count} interval={interval_ms}ms");
    println!("note: 若 daemon 正在运行，按钮回调可能被 daemon 的连接抢走（钉钉多连接轮询分发）；");
    println!("      验证按钮回调前建议先 `AskHuman daemon stop`。");

    exit(cfgio::block_on(run_poc(
        client,
        &dd,
        &template,
        count,
        interval_ms,
    )));
}

async fn run_poc(
    client: crate::dingtalk::client::DingTalkClient,
    dd: &crate::config::DingTalkChannelConfig,
    template: &str,
    count: usize,
    interval_ms: u64,
) -> i32 {
    use crate::dingtalk::stream::{StreamConn, StreamEvent, TOPIC_CARD_CALLBACK};
    use crate::watch::CardMode;
    use serde_json::json;

    let lang = Lang::current();

    // 样式速览模式（--count 0）：只建一张卡就退出，不连 Stream、不更新、不定格。
    if count == 0 {
        let out_track_id = format!("watch-poc-{}", uuid::Uuid::new_v4());
        let started = now_secs();
        let param_map = crate::dingtalk::watch::build_watch_param_map(
            &poc_frame(1, started),
            CardMode::Active,
            started,
            lang,
        );
        return match client
            .create_and_deliver_card(&out_track_id, template, param_map, json!({}))
            .await
        {
            Ok(()) => {
                println!("created (style preview): otid={out_track_id}");
                0
            }
            Err(e) => {
                eprintln!("create card failed: {e}");
                1
            }
        };
    }

    // 自连一条 Stream（仅卡片回调 topic，不订 bot 消息，避免抢走 daemon 的聊天消息）。
    let http = reqwest::Client::new();
    match StreamConn::connect(
        http,
        dd.client_id.trim(),
        dd.client_secret.trim(),
        &[TOPIC_CARD_CALLBACK],
    )
    .await
    {
        Ok(mut conn) => {
            tokio::spawn(async move {
                while let Some(ev) = conn.recv().await {
                    if let StreamEvent::CardCallback { data, message_id } = ev {
                        match crate::dingtalk::watch::parse_watch_action(&data) {
                            Some((otid, action)) => {
                                println!("callback: action={action} otid={otid}");
                            }
                            None => println!(
                                "callback: (non-watch) {}",
                                serde_json::to_string(&data).unwrap_or_default()
                            ),
                        }
                        // 空回包仅确认；按钮端上表现（成功/失败 toast）由人观察记录。
                        conn.respond(&message_id, json!({})).await;
                    }
                }
                println!("stream: closed");
            });
            println!("stream: connected (card callback topic)");
        }
        Err(e) => println!("stream: connect failed ({e}); button callbacks won't be observed"),
    }

    let out_track_id = format!("watch-poc-{}", uuid::Uuid::new_v4());
    let started = now_secs();

    // 建卡投放。
    let t0 = std::time::Instant::now();
    let param_map = crate::dingtalk::watch::build_watch_param_map(
        &poc_frame(0, started),
        CardMode::Active,
        started,
        lang,
    );
    if let Err(e) = client
        .create_and_deliver_card(&out_track_id, template, param_map, json!({}))
        .await
    {
        eprintln!("create card failed: {e}");
        return 1;
    }
    println!(
        "created: otid={out_track_id} ({}ms)",
        t0.elapsed().as_millis()
    );

    // 高频就地更新。
    let mut lat_ms: Vec<u128> = Vec::new();
    let mut errors: usize = 0;
    for i in 1..=count {
        tokio::time::sleep(std::time::Duration::from_millis(interval_ms)).await;
        let now = now_secs();
        let map = crate::dingtalk::watch::build_watch_param_map(
            &poc_frame(i, started),
            CardMode::Active,
            now,
            lang,
        );
        let t = std::time::Instant::now();
        match client
            .update_card_private(&out_track_id, map, json!({}))
            .await
        {
            Ok(()) => {
                let ms = t.elapsed().as_millis();
                lat_ms.push(ms);
                println!("#{i:02} ok {ms}ms");
            }
            Err(e) => {
                errors += 1;
                println!("#{i:02} ERR {e}");
            }
        }
    }

    // 终态定格。
    let now = now_secs();
    let map = crate::dingtalk::watch::build_watch_param_map(
        &poc_frame(count + 1, started),
        CardMode::Final(crate::watch::FinalKind::Ended),
        now,
        lang,
    );
    match client
        .update_card_private(&out_track_id, map, json!({}))
        .await
    {
        Ok(()) => println!("finalized ok"),
        Err(e) => {
            errors += 1;
            println!("finalize ERR {e}");
        }
    }

    // 统计。
    lat_ms.sort_unstable();
    let pick = |p: f64| -> u128 {
        if lat_ms.is_empty() {
            return 0;
        }
        let idx = ((lat_ms.len() as f64 - 1.0) * p).round() as usize;
        lat_ms[idx]
    };
    println!(
        "summary: ok={} err={errors} latency min={}ms p50={}ms p90={}ms max={}ms",
        lat_ms.len(),
        lat_ms.first().copied().unwrap_or(0),
        pick(0.5),
        pick(0.9),
        lat_ms.last().copied().unwrap_or(0),
    );
    println!("(按钮回调 / 淹没后更新 / 终态渲染 请在钉钉端人工核对)");
    if errors > 0 {
        1
    } else {
        0
    }
}

/// 合成一帧演进中的 watch 内容：文字随 i 变化，足迹步在 进行中/已完成 间轮替，
/// 让端上能直观看到「流式」效果。
fn poc_frame(i: usize, started: u64) -> crate::watch::WatchFrame {
    use crate::agents::activity::{
        StepState, TodoItem, TodoState, ToolDisplay, ToolLabel, ToolStep,
    };
    let step = |label: ToolLabel, object: &str, state: StepState| ToolStep {
        tool: ToolDisplay {
            label,
            object: Some(object.to_string()),
        },
        state,
    };
    crate::watch::WatchFrame {
        seq: 99,
        kind_label: "PoC".into(),
        forked_from: None,
        title: Some("钉钉 watch 高频更新探针".into()),
        project: Some("HumanInLoop".into()),
        phase: crate::watch::WatchPhase::Working,
        text: Some(format!("流式更新 #{i}（验证就地编辑与频控）")),
        steps: vec![
            step(
                ToolLabel::Run,
                &format!("update #{}", i.saturating_sub(1)),
                StepState::Done,
            ),
            step(
                ToolLabel::Run,
                &format!("update #{i}"),
                if i % 5 == 4 {
                    StepState::Failed
                } else {
                    StepState::Running
                },
            ),
        ],
        steps_omitted: i.saturating_sub(2),
        todos: vec![
            TodoItem {
                content: "建卡投放".into(),
                state: TodoState::Completed,
            },
            TodoItem {
                content: format!("高频更新（第 {i} 次）"),
                state: TodoState::InProgress,
            },
            TodoItem {
                content: "终态定格".into(),
                state: TodoState::Pending,
            },
        ],
        active_elapsed_secs: Some(now_secs().saturating_sub(started)),
        at: Some(now_secs()),
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
