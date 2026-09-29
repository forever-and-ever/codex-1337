//! Preserve the desktop's async-question reply envelope through coalescing, display and dismissal.
//! Only complete envelopes, optionally following the standard IDE context prefix, are interpreted.

use codex_app_server_protocol::UserInput;
use codex_context_fragments::AnsweredQuestion;
use codex_context_fragments::ContextualUserFragment;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AsyncQuestionReply {
    pub(crate) question_item_id: String,
    question: String,
    answer: String,
}

fn split_ide_context(text: &str) -> (&str, &str) {
    // JSON escapes newlines, so a literal delimiter in an answer cannot match.
    if text
        .trim_start()
        .starts_with("# Context from my IDE setup:\n")
        && let Some((_, request)) = text.rsplit_once("\n## My request for Codex:\n")
    {
        let offset = text.len() - request.len();
        (&text[..offset], request)
    } else {
        ("", text)
    }
}

pub(crate) fn parse(text: &str) -> Option<Vec<AsyncQuestionReply>> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Replies {
        Many(Vec<AsyncQuestionReply>),
        One(AsyncQuestionReply),
    }
    let text = split_ide_context(text).1.trim();
    let (open, close) = AnsweredQuestion::type_markers();
    let json = text.strip_prefix(open)?.strip_suffix(close)?;
    let replies = match serde_json::from_str::<Replies>(json).ok()? {
        Replies::Many(replies) => replies,
        Replies::One(reply) => vec![reply],
    };
    (!replies.is_empty()).then_some(replies)
}

pub(crate) fn display_text(text: &str) -> Option<String> {
    Some(
        parse(text)?
            .iter()
            .map(|reply| format!("> {}\n\n{}", reply.question, reply.answer))
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

/// Extend the most recent answer, or collect another reply without nesting envelopes.
#[expect(
    clippy::expect_used,
    reason = "parse rejects empty replies; string fields always serialize"
)]
pub(crate) fn append(text: &str, appended: &str) -> Option<String> {
    let mut replies = parse(text)?;
    let (prefix, _) = split_ide_context(text);
    let (appended_prefix, appended_request) = split_ide_context(appended);
    let prefix = if appended_prefix.is_empty() {
        prefix
    } else {
        appended_prefix
    };
    if let Some(next) = parse(appended_request) {
        replies.extend(next);
    } else if !appended_request.is_empty() {
        let answer = &mut replies
            .last_mut()
            .expect("parsed replies are nonempty")
            .answer;
        answer.push('\n');
        answer.push_str(appended_request);
    }
    let json = serde_json::to_string(&replies).expect("question replies contain only strings");
    let (open, close) = AnsweredQuestion::type_markers();
    Some(format!("{prefix}{open}\n{json}\n{close}"))
}

pub(crate) fn append_input(input: &[UserInput], appended: &[UserInput]) -> Option<Vec<UserInput>> {
    let text = |items: &[UserInput]| {
        items
            .iter()
            .filter_map(|item| {
                if let UserInput::Text { text, .. } = item {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<String>()
    };
    let combined = append(&text(input), &text(appended))?;
    let mut combined = Some(combined);
    Some(
        input
            .iter()
            .chain(appended)
            .filter_map(|item| {
                if let UserInput::Text { .. } = item {
                    // JSON escaping invalidates UI byte ranges; decoded replies already render
                    // without them. Attachments and resolved mentions remain separate inputs.
                    combined.take().map(|text| UserInput::Text {
                        text,
                        text_elements: Vec::new(),
                    })
                } else {
                    Some(item.clone())
                }
            })
            .collect(),
    )
}

pub(crate) fn parse_input(input: &[UserInput]) -> Option<Vec<AsyncQuestionReply>> {
    let mut content = input
        .iter()
        .filter(|item| !matches!(item, UserInput::Skill { .. } | UserInput::Mention { .. }));
    let UserInput::Text { text, .. } = content.next()? else {
        return None;
    };
    if content.next().is_some() {
        return None;
    }
    parse(text)
}

#[cfg(test)]
#[path = "async_question_reply_tests.rs"]
mod tests;
