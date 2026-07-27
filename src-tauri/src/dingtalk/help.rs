//! DingTalk `sampleMarkdown` renderer for structured IM help.

use crate::autochannel::{self, HelpQuestionState, HelpView};
use crate::i18n::Lang;

pub fn render(view: &HelpView, lang: Lang) -> String {
    let mut out = escape_text(&view.intro);
    for section in &view.sections {
        out.push_str("\n\n**");
        out.push_str(&escape_text(&section.title));
        out.push_str("**");
        for command in &section.commands {
            out.push_str("\n- `");
            out.push_str(&escape_code(&command.syntax));
            out.push_str("` — ");
            out.push_str(&escape_text(&command.description));
            if let Some(phrase) = &command.phrase {
                out.push_str("  *· ");
                out.push_str(&escape_text(&autochannel::help_phrase_hint(phrase, lang)));
                out.push('*');
            }
        }
    }
    out.push_str("\n\n---\n\n**");
    match &view.question_state {
        HelpQuestionState::Active { instruction } => out.push_str(&escape_text(instruction)),
        HelpQuestionState::None { message } => out.push_str(&escape_text(message)),
    }
    out.push_str("**");
    if let Some(hint) = &view.switch_hint {
        out.push_str("\n\n*");
        out.push_str(&escape_text(hint));
        out.push('*');
    }
    out
}

fn escape_code(text: &str) -> String {
    text.replace('`', "\\`")
}

fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(
            ch,
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '!'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_bulleted_markdown_help() {
        let view = autochannel::help_view(true, false, true, "/", Lang::Zh);
        let markdown = render(&view, Lang::Zh);
        assert!(markdown.contains("**Agent 管理**"));
        assert!(markdown.contains("- `/status [编号]`"));
        assert!(markdown.contains("*· 直接说「状态」*"));
        assert!(markdown.contains("- `/msg-clear <编号>`"));
        assert!(!markdown.contains("msg-clear <编号>` — 撤回该 Agent 待送达的插话  *·"));
        assert!(markdown.contains("---"));
    }
}
