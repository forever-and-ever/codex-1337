//! Literal read recognition, not shell evaluation. Unrecognized commands pass through.

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ReadRange {
    Whole,
    Lines { first: usize, last: usize },
}

#[cfg(test)]
#[path = "read_intent_tests.rs"]
mod tests;

impl std::fmt::Display for ReadRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Whole => write!(f, "whole file"),
            Self::Lines { first, last } => write!(f, "{first}–{last}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadIntent {
    pub path: String,
    pub range: ReadRange,
}

impl ReadIntent {
    pub fn parse(script: &str) -> Option<Self> {
        let script = script.trim();
        // Reject expansion, redirection, composition and additional statements, even
        // inside quotes. False negatives are preferable to guessing shell semantics.
        if script.chars().any(|c| "$`|;&<>*?[]{}~\n\r".contains(c)) {
            return None;
        }
        let words = shlex::split(script)?;
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        let (path, range) = match words.as_slice() {
            ["cat", path] | ["cat", "--", path] => (*path, ReadRange::Whole),
            ["sed", "-n", address, path] | ["sed", "-n", address, "--", path] => {
                let address = address.strip_suffix('p')?;
                let (first, last) = address.split_once(',').unwrap_or((address, address));
                let first = first.parse::<usize>().ok()?;
                let last = last.parse::<usize>().ok()?;
                if first == 0 || last < first {
                    return None;
                }
                (*path, ReadRange::Lines { first, last })
            }
            _ => return None,
        };
        if path.is_empty() || path.starts_with('-') {
            return None;
        }
        Some(Self {
            path: path.to_owned(),
            range,
        })
    }

    #[must_use]
    pub fn from_tool_input(input: &codex_protocol::tool_input::ToolInput) -> Option<Self> {
        use codex_protocol::tool_input::ToolInputPayload;

        if !input.name.is_default_namespace() || input.name.name != "fs" {
            return None;
        }
        let ToolInputPayload::Json(arguments) = &input.payload else {
            return None;
        };
        let arguments: serde_json::Value = serde_json::from_str(arguments).ok()?;
        if arguments.get("operation")?.as_str()? != "read" {
            return None;
        }
        let path = arguments.get("path")?.as_str()?;
        if path.is_empty() {
            return None;
        }
        Some(Self {
            path: path.to_owned(),
            range: ReadRange::Whole,
        })
    }

    #[must_use]
    pub fn label(&self, name: &str) -> String {
        match self.range {
            ReadRange::Whole => format!("{name} (whole file)"),
            ReadRange::Lines { .. } => format!("{name}:{}", self.range),
        }
    }
}
