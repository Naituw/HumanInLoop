//! CLI `todo` 子命令（spec todo-whats-next D6 + todo-attachments）：
//! add / list / attach / detach / rm / clear。
//!
//! 项目 key 取调用 cwd 的 git 根（`project::detect`，与回复历史同规则）；
//! 存储直读直写 `todos.json`（D1 第 9 轮定案，不依赖 daemon 存活，跨平台同一套代码）。

use super::print_line;
use crate::i18n::{self, Lang};
use std::process::exit;

/// `AskHuman todo <add|list|rm|clear> …` 入口（args 不含 "todo" 本身）。
pub fn dispatch(args: &[String], lang: Lang) -> ! {
    let project = crate::project::detect();
    if project.is_empty() {
        // cwd 都取不到（极端环境）：无法归属项目。
        eprintln!("{}cannot determine project", i18n::err_prefix(lang));
        exit(1);
    }
    match args.first().map(String::as_str) {
        Some("add") => add(&project, &args[1..], lang),
        // 无子命令时默认 list（顺手查看）。
        Some("list") | None => list(&project, lang),
        Some("rm") => rm(&project, &args[1..], lang),
        Some("attach") => attach(&project, &args[1..], lang),
        Some("detach") => detach(&project, &args[1..], lang),
        Some("clear") => clear(&project, &args[1..], lang),
        Some(other) => {
            eprintln!(
                "{}{}",
                i18n::err_prefix(lang),
                i18n::tr(lang, "todo.unknownSubcommand").replace("{cmd}", other)
            );
            exit(1);
        }
    }
}

fn add(project: &str, args: &[String], lang: Lang) -> ! {
    let (auto, files, text) = match parse_add_args(args) {
        Ok(parsed) => parsed,
        Err(message) => cli_error(lang, &message),
    };
    let agent = crate::agents::detect::detect_invoking_agent();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let added = crate::todos::add_with_attachments(project, &text, auto, agent, &files, &cwd);
    let entry = match added {
        Ok(entry) => entry,
        Err(crate::todos::AddError::EmptyInput) => {
            eprintln!(
                "{}{}",
                i18n::err_prefix(lang),
                i18n::tr(lang, "todo.missingText")
            );
            exit(1);
        }
        Err(crate::todos::AddError::Persist) => {
            eprintln!(
                "{}{}",
                i18n::err_prefix(lang),
                i18n::tr(lang, "todo.persistFailed")
            );
            exit(1);
        }
        Err(crate::todos::AddError::Attachment(message)) => {
            cli_error(lang, &message);
        }
    };
    // Prefer the real 1-based index of the new id (never report #0 on a hollow write).
    let n = crate::todos::index_of(project, &entry.id).unwrap_or(0);
    if n == 0 {
        eprintln!(
            "{}{}",
            i18n::err_prefix(lang),
            i18n::tr(lang, "todo.persistFailed")
        );
        exit(1);
    }
    let key = if auto { "todo.addedAuto" } else { "todo.added" };
    print_line(
        &i18n::tr(lang, key)
            .replace("{n}", &n.to_string())
            .replace("{text}", text.trim()),
    );
    exit(0);
}

fn parse_add_args(args: &[String]) -> Result<(bool, Vec<String>, String), String> {
    let mut auto = false;
    let mut files = Vec::new();
    let mut words = Vec::new();
    let mut i = 0;
    let mut text_only = false;
    while i < args.len() {
        let arg = &args[i];
        if text_only {
            words.push(arg.clone());
        } else {
            match arg.as_str() {
                "--" => text_only = true,
                "--auto" => auto = true,
                "-f" | "--file" => {
                    i += 1;
                    let Some(path) = args.get(i) else {
                        return Err(format!("{arg} requires a file path"));
                    };
                    files.push(path.clone());
                }
                _ if arg.starts_with('-') => {
                    return Err(format!("unknown todo add option: {arg} (use `--` before text that starts with '-')"));
                }
                _ => words.push(arg.clone()),
            }
        }
        i += 1;
    }
    Ok((auto, files, words.join(" ")))
}

fn list(project: &str, lang: Lang) -> ! {
    let entries = crate::todos::list(project);
    if entries.is_empty() {
        print_line(i18n::tr(lang, "todo.empty"));
        exit(0);
    }
    print_line(
        &i18n::tr(lang, "todo.listHeader")
            .replace("{project}", &crate::project::display_name(project)),
    );
    for (i, entry) in entries.iter().enumerate() {
        let auto_mark = if entry.auto {
            format!(" {}", i18n::tr(lang, "todo.autoMark"))
        } else {
            String::new()
        };
        print_line(&format!("{:>3}. {}{}", i + 1, entry.text, auto_mark));
        for (attachment_index, attachment) in entry.attachments.iter().enumerate() {
            let unavailable = if attachment.available() {
                ""
            } else {
                " [missing]"
            };
            let storage = match attachment.storage {
                crate::todo_attachments::TodoAttachmentStorage::Managed => "managed",
                crate::todo_attachments::TodoAttachmentStorage::Reference => "reference",
            };
            print_line(&format!(
                "       📎 {}. {} ({} bytes, {storage}){unavailable}",
                attachment_index + 1,
                attachment.name,
                attachment.size,
            ));
        }
    }
    exit(0);
}

fn attach(project: &str, args: &[String], lang: Lang) -> ! {
    let (entry, index) = todo_by_index(project, args.first().map(String::as_str), lang);
    let mut paths = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-f" | "--file" => {
                i += 1;
                let Some(path) = args.get(i) else {
                    cli_error(lang, "-f/--file requires a file path");
                };
                paths.push(path.clone());
            }
            other => cli_error(lang, &format!("unexpected todo attach argument: {other}")),
        }
        i += 1;
    }
    if paths.is_empty() {
        cli_error(lang, "todo attach requires at least one -f/--file path");
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    match crate::todos::update_attachment_ids(project, &entry.id, &paths, &[], &cwd) {
        Ok(updated) => {
            print_line(&format!(
                "Updated todo #{index}: {} attachment(s)",
                updated.attachments.len()
            ));
            exit(0);
        }
        Err(error) => cli_error(lang, &error.to_string()),
    }
}

fn detach(project: &str, args: &[String], lang: Lang) -> ! {
    let (entry, index) = todo_by_index(project, args.first().map(String::as_str), lang);
    let mut positions = Vec::new();
    for raw in args.iter().skip(1) {
        let Some(position) = raw
            .parse::<usize>()
            .ok()
            .filter(|position| (1..=entry.attachments.len()).contains(position))
        else {
            cli_error(lang, &format!("invalid attachment number: {raw}"));
        };
        if !positions.contains(&position) {
            positions.push(position);
        }
    }
    if positions.is_empty() {
        cli_error(lang, "todo detach requires at least one attachment number");
    }
    let ids: Vec<String> = positions
        .iter()
        .map(|position| entry.attachments[*position - 1].id.clone())
        .collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    match crate::todos::update_attachment_ids(project, &entry.id, &[], &ids, &cwd) {
        Ok(updated) => {
            print_line(&format!(
                "Updated todo #{index}: {} attachment(s)",
                updated.attachments.len()
            ));
            exit(0);
        }
        Err(error) => cli_error(lang, &error.to_string()),
    }
}

fn todo_by_index(project: &str, raw: Option<&str>, lang: Lang) -> (crate::todos::TodoEntry, usize) {
    let raw = raw.unwrap_or("");
    let entries = crate::todos::list(project);
    let Some(index) = raw
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=entries.len()).contains(n))
    else {
        eprintln!(
            "{}{}",
            i18n::err_prefix(lang),
            i18n::tr(lang, "todo.invalidIndex")
                .replace("{n}", raw)
                .replace("{prog}", &super::help::program_name())
        );
        exit(1);
    };
    (entries[index - 1].clone(), index)
}

fn cli_error(lang: Lang, message: &str) -> ! {
    eprintln!("{}{message}", i18n::err_prefix(lang));
    exit(1)
}

fn rm(project: &str, args: &[String], lang: Lang) -> ! {
    let raw = args.first().map(String::as_str).unwrap_or("");
    let entries = crate::todos::list(project);
    // 编号为 `todo list` 显示的 1 基序号。
    let index = raw
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=entries.len()).contains(n));
    let Some(index) = index else {
        eprintln!(
            "{}{}",
            i18n::err_prefix(lang),
            i18n::tr(lang, "todo.invalidIndex")
                .replace("{n}", raw)
                .replace("{prog}", &super::help::program_name())
        );
        exit(1);
    };
    let entry = &entries[index - 1];
    // 并发下条目可能已被别处删除（best-effort，与 D11 一致）：仍按成功报告，最终状态一致。
    if let Err(error) = crate::todos::remove_checked(project, &entry.id) {
        cli_error(lang, &error.to_string());
    }
    print_line(
        &i18n::tr(lang, "todo.removed")
            .replace("{n}", &index.to_string())
            .replace("{text}", &entry.text),
    );
    exit(0);
}

fn clear(project: &str, args: &[String], lang: Lang) -> ! {
    let entries = crate::todos::list(project);
    if entries.is_empty() {
        print_line(i18n::tr(lang, "todo.empty"));
        exit(0);
    }
    // 交互确认（D6：clear 需确认），`--yes`/`-y` 跳过（脚本用）。
    let skip_confirm = args.iter().any(|a| a == "--yes" || a == "-y");
    if !skip_confirm && !confirm_clear(entries.len(), lang) {
        print_line(i18n::tr(lang, "todo.clearAborted"));
        exit(0);
    }
    let removed = match crate::todos::clear_checked(project) {
        Ok(removed) => removed,
        Err(error) => cli_error(lang, &error.to_string()),
    };
    print_line(&i18n::tr(lang, "todo.cleared").replace("{n}", &removed.to_string()));
    exit(0);
}

/// 读取一行确认（y/yes 才通过）；stdin 不可读 / EOF 视为取消。
fn confirm_clear(count: usize, lang: Lang) -> bool {
    use std::io::{BufRead, Write};
    let prompt = i18n::tr(lang, "todo.clearConfirm").replace("{n}", &count.to_string());
    let mut out = std::io::stdout();
    let _ = write!(out, "{prompt}").and_then(|_| out.flush());
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn add_parser_supports_repeated_files_auto_and_dash_text() {
        let parsed = parse_add_args(&args(&[
            "--auto", "-f", "a.md", "--file", "b.png", "--", "-leading", "task",
        ]))
        .unwrap();
        assert!(parsed.0);
        assert_eq!(parsed.1, vec!["a.md", "b.png"]);
        assert_eq!(parsed.2, "-leading task");
    }

    #[test]
    fn add_parser_rejects_missing_file_value_and_unknown_flags() {
        assert!(parse_add_args(&args(&["task", "-f"])).is_err());
        assert!(parse_add_args(&args(&["--wat", "task"])).is_err());
    }
}
