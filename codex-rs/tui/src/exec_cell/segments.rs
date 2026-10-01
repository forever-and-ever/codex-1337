//! Source-preserving shell boundaries for presentation, not execution or safety decisions.

use codex_shell_command::bash::try_parse_shell;
use codex_shell_command::bash::try_parse_word_only_commands_sequence;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connector {
    Sequence,
    And,
    Or,
}

impl Connector {
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Sequence => ";",
            Self::And => " &&",
            Self::Or => " ||",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellSegment {
    pub source: Range<usize>,
    pub connector: Option<Connector>,
}

pub(super) fn summarize_shell_segment(
    script: &str,
) -> codex_protocol::parse_command::ParsedCommand {
    use codex_protocol::parse_command::ParsedCommand;
    if let Some(tree) = try_parse_shell(script)
        && let Some(commands) = try_parse_word_only_commands_sequence(&tree, script)
        && commands.len() == 1
    {
        let parsed = codex_shell_command::parse_command::parse_command(&[
            "bash".into(),
            "-c".into(),
            script.into(),
        ]);
        if let [action] = parsed.as_slice() {
            return action.clone();
        }
    }
    ParsedCommand::Unknown {
        cmd: script.to_owned(),
    }
}

/// Split only the outer command list. Pipelines, groups, substitutions and control flow
/// remain opaque; quoted punctuation and heredoc bodies never introduce boundaries.
pub fn shell_segments(script: &str) -> Vec<ShellSegment> {
    let mut boundaries = Vec::new();
    if let Some(tree) = try_parse_shell(script)
        && !tree.root_node().has_error()
    {
        let mut pending = vec![tree.root_node()];
        while let Some(node) = pending.pop() {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                let connector = match child.kind() {
                    ";" => Connector::Sequence,
                    "&&" => Connector::And,
                    "||" => Connector::Or,
                    "list" => {
                        pending.push(child);
                        continue;
                    }
                    _ => continue,
                };
                boundaries.push((child.byte_range(), connector));
            }
        }
    }
    boundaries.sort_by_key(|(range, _)| range.start);
    let mut segments = Vec::new();
    let mut start = 0;
    for (range, connector) in boundaries {
        segments.push(ShellSegment {
            source: start..range.start,
            connector: Some(connector),
        });
        start = range.end;
    }
    if start < script.len() || segments.is_empty() {
        segments.push(ShellSegment {
            source: start..script.len(),
            connector: None,
        });
    }
    segments
}

#[cfg(test)]
#[path = "segments_tests.rs"]
mod tests;
