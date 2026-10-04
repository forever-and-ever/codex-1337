use serde::Deserialize;
use serde::Serialize;

/// The structured result of native `exec_command` and `write_stdin` calls.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeExecutionResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<String>,
    pub wall_time_seconds: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_token_count: Option<usize>,
    pub output: String,
}

#[derive(Clone, Copy)]
pub enum ExecutionOutputMetadata {
    Hidden,
    Included,
}

impl NativeExecutionResult {
    /// Contradictory status must retain its complete structured representation.
    pub fn render(&self, metadata: ExecutionOutputMetadata) -> Option<String> {
        let header = self.header(metadata)?;
        Some(match (header.is_empty(), self.output.is_empty()) {
            (true, _) => self.output.clone(),
            (false, true) => header,
            (false, false) => format!("{header}\n{}", self.output),
        })
    }

    /// Reject the entire batch on an incompatible item; the caller emits only
    /// after validation finishes. Items are consumed without retaining bodies.
    pub fn render_batch(
        results: impl IntoIterator<Item = Option<Self>>,
        metadata: ExecutionOutputMetadata,
    ) -> Option<String> {
        let mut output = String::new();
        let mut empty_start = None;
        let mut empty_end = 0;
        for (index, result) in results.into_iter().enumerate() {
            let result = result?;
            let ordinal = index + 1;
            if matches!(metadata, ExecutionOutputMetadata::Hidden)
                && result.exit_code == Some(0)
                && result.session_id.is_none()
                && result.output.is_empty()
            {
                empty_start.get_or_insert(ordinal);
                empty_end = ordinal;
                continue;
            }
            if let Some(start) = empty_start.take() {
                append_empty_span(&mut output, start, empty_end);
            }
            let header = result.header(metadata)?;
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(&format!("[{ordinal}]"));
            if !header.is_empty() {
                output.push(' ');
                output.push_str(&header);
            }
            if !result.output.is_empty() {
                output.push('\n');
                output.push_str(&result.output);
            }
        }
        if let Some(start) = empty_start {
            append_empty_span(&mut output, start, empty_end);
        }
        Some(output)
    }

    fn header(&self, metadata: ExecutionOutputMetadata) -> Option<String> {
        if self.exit_code.is_some() && self.session_id.is_some() {
            return None;
        }
        let header = match metadata {
            ExecutionOutputMetadata::Hidden => match (self.exit_code, self.session_id) {
                (Some(0), None) if self.output.is_empty() => "Completed".to_string(),
                (Some(0), None) => String::new(),
                (Some(exit), None) => format!("Exit {exit}"),
                (None, Some(session)) => format!("Running (session {session})"),
                (None, None) if self.output.is_empty() => {
                    "Execution status unavailable".to_string()
                }
                (None, None) => String::new(),
                (Some(_), Some(_)) => unreachable!(),
            },
            ExecutionOutputMetadata::Included => {
                let mut fields = Vec::new();
                if let Some(exit) = self.exit_code {
                    fields.push(format!("exit={exit}"));
                }
                if let Some(session) = self.session_id {
                    fields.push(format!("session={session}"));
                }
                fields.push(format!("wall={}s", self.wall_time_seconds));
                if let Some(chunk) = &self.chunk_id {
                    fields.push(format!("chunk={chunk}"));
                }
                if let Some(tokens) = self.original_token_count {
                    fields.push(format!("tokens≈{tokens}"));
                }
                fields.join(" ")
            }
        };
        Some(header)
    }
}

fn append_empty_span(output: &mut String, start: usize, end: usize) {
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    if start == end {
        output.push_str(&format!("[{start}] Completed"));
    } else {
        output.push_str(&format!("[{start}–{end}] Completed"));
    }
}
