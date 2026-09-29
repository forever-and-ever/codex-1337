//! Sender attribution is captured at send time and retained with the tool output.

pub(super) const MAX_SENDER_NAME_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DelegatedMessage {
    pub(crate) source_thread_id: String,
    pub(crate) source_thread_name: Option<String>,
    pub(crate) sent_at: Option<i64>,
    pub(crate) input: String,
}

impl DelegatedMessage {
    pub(super) fn new(source: &codex_app_server_protocol::Thread, input: String) -> Self {
        Self {
            source_thread_id: source.id.clone(),
            source_thread_name: source
                .name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .map(|name| name.chars().take(MAX_SENDER_NAME_CHARS).collect()),
            sent_at: Some(chrono::Utc::now().timestamp_millis()),
            input,
        }
    }

    pub(super) fn encode(&self) -> String {
        let mut output = format!(
            "<codex_delegation>\n  <source_thread_id>{}</source_thread_id>",
            escape(&self.source_thread_id)
        );
        if let Some(name) = &self.source_thread_name {
            output.push_str(&format!(
                "\n  <source_thread_name>{}</source_thread_name>",
                escape(name)
            ));
        }
        if let Some(sent_at) = self.sent_at {
            output.push_str(&format!("\n  <sent_at>{sent_at}</sent_at>"));
        }
        output.push_str(&format!(
            "\n  <input>{}</input>\n</codex_delegation>",
            escape(&self.input)
        ));
        output
    }

    pub(super) fn parse(text: &str) -> Option<Self> {
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
