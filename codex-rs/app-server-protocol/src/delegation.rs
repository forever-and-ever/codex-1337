//! Sender attribution is captured at send time and retained with the tool output.

use std::fmt::Write as _;

pub const MAX_SENDER_NAME_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegatedMessage {
    pub source_thread_id: String,
    pub source_thread_name: Option<String>,
    pub sent_at: Option<i64>,
    pub input: String,
}

impl DelegatedMessage {
    /// Recognizes attribution only on named coordination outputs, including the
    /// persisted desktop namespace. Arbitrary tools cannot claim this envelope.
    #[must_use]
    pub fn from_tool_output(
        name: &str,
        namespace: Option<&str>,
        output: &codex_protocol::models::FunctionCallOutputBody,
    ) -> Option<Self> {
        if !matches!(namespace, Some("codex_tui" | "codex_app"))
            || !matches!(name, "create_thread" | "send_message_to_thread")
        {
            return None;
        }
        Self::parse(output.to_text().as_deref()?)
    }
    #[must_use]
    pub fn new(source: &crate::Thread, input: String) -> Self {
        Self {
            source_thread_id: source.id.clone(),
            source_thread_name: source
                .name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .map(|name| name.chars().take(MAX_SENDER_NAME_CHARS).collect()),
            sent_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok()),
            input,
        }
    }

    #[must_use]
    pub fn encode(&self) -> String {
        let mut output = format!(
            "<codex_delegation>\n  <source_thread_id>{}</source_thread_id>",
            escape(&self.source_thread_id)
        );
        if let Some(name) = &self.source_thread_name {
            let _ = write!(
                output,
                "\n  <source_thread_name>{}</source_thread_name>",
                escape(name)
            );
        }
        if let Some(sent_at) = self.sent_at {
            let _ = write!(output, "\n  <sent_at>{sent_at}</sent_at>");
        }
        let _ = write!(
            output,
            "\n  <input>{}</input>\n</codex_delegation>",
            escape(&self.input)
        );
        output
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let rest = text.strip_prefix("<codex_delegation>\n  <source_thread_id>")?;
        let (id, mut rest) = rest.split_once("</source_thread_id>")?;
        let source_thread_name = if let Some(named) = rest.strip_prefix("\n  <source_thread_name>")
        {
            let (name, tail) = named.split_once("</source_thread_name>")?;
            rest = tail;
            Some(unescape(name))
        } else {
            None
        };
        let sent_at = if let Some(dated) = rest.strip_prefix("\n  <sent_at>") {
            let (timestamp, tail) = dated.split_once("</sent_at>")?;
            rest = tail;
            Some(timestamp.parse().ok()?)
        } else {
            None
        };
        let input = rest
            .strip_prefix("\n  <input>")?
            .strip_suffix("</input>\n</codex_delegation>")?;
        Some(Self {
            source_thread_id: unescape(id),
            source_thread_name,
            sent_at,
            input: unescape(input),
        })
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}
