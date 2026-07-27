//! Telegram HTML renderer for structured IM help.

use crate::autochannel::{self, HelpQuestionState, HelpView};
use crate::i18n::Lang;
use crate::telegram::markdown::escape_html;

pub fn render(view: &HelpView, lang: Lang) -> String {
    let mut out = format!(
        "<b>{}</b>\n{}",
        escape_html(&view.title),
        escape_html(&view.intro)
    );
    for section in &view.sections {
        out.push_str("\n\n<b>");
        out.push_str(&escape_html(&section.title));
        out.push_str("</b>");
        for command in &section.commands {
            out.push_str("\n• <code>");
            out.push_str(&escape_html(&command.syntax));
            out.push_str("</code> — ");
            out.push_str(&escape_html(&command.description));
            if let Some(phrase) = &command.phrase {
                out.push_str("  <i>· ");
                out.push_str(&escape_html(&autochannel::help_phrase_hint(phrase, lang)));
                out.push_str("</i>");
            }
        }
    }
    out.push_str("\n\n────────\n<b>");
    match &view.question_state {
        HelpQuestionState::Active { instruction } => out.push_str(&escape_html(instruction)),
        HelpQuestionState::None { message } => out.push_str(&escape_html(message)),
    }
    out.push_str("</b>");
    if let Some(hint) = &view.switch_hint {
        out.push_str("\n<i>");
        out.push_str(&escape_html(hint));
        out.push_str("</i>");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_bulleted_html_help_with_escaped_syntax() {
        let view = autochannel::help_view(true, false, true, "/", Lang::Zh);
        let html = render(&view, Lang::Zh);
        assert!(html.contains("<b>Agent 管理</b>"));
        assert!(html.contains("• <code>/status [编号]</code>"));
        assert!(html.contains("<i>· 直接说「状态」</i>"));
        assert!(html.contains("<code>/msg-clear &lt;编号&gt;</code>"));
        assert!(html.chars().count() < 4096);
    }
}
