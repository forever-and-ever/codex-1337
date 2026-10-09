//! `write_stdin` inside Code Mode cells.
//!
//! An empty write already returns the moment its process exits. In a cell it never reports a
//! process as still running sooner than [`EMPTY_WRITE_FLOOR_MS`]: each such report costs the model
//! a round trip, while the wait costs only wall-clock time, and the cell still returns control to
//! the model when news arrives.

use codex_tools::JsonSchema;
use codex_tools::ToolSpec;

use super::WriteStdinHandler;

const EMPTY_WRITE_FLOOR_MS: u64 = 60_000;
const CHARS_DESCRIPTION: &str =
    "Bytes to write to stdin. Defaults to empty, which waits for output without writing.";
const YIELD_TIME_DESCRIPTION: &str = "Longest wait for output; returns as soon as the process exits. Non-empty writes default to 250 ms and cap at 30000 ms; empty writes wait 60000-300000 ms by default.";

/// Who calls `write_stdin`.
#[derive(Clone, Copy, Debug, Default)]
pub(super) enum Caller {
    #[default]
    Model,
    Cell,
}

impl WriteStdinHandler {
    /// `write_stdin` for a Code Mode Only session, where every call comes from a cell.
    pub(crate) fn cell() -> Self {
        Self {
            caller: Caller::Cell,
        }
    }
}

impl Caller {
    pub(super) fn yield_time_ms(self, chars: &str, requested_ms: u64) -> u64 {
        match self {
            Self::Cell if chars.is_empty() => requested_ms.max(EMPTY_WRITE_FLOOR_MS),
            Self::Model | Self::Cell => requested_ms,
        }
    }

    pub(super) fn spec(self, spec: ToolSpec) -> ToolSpec {
        match self {
            Self::Model => spec,
            Self::Cell => {
                let ToolSpec::Function(mut spec) = spec else {
                    unreachable!("write_stdin has a function schema");
                };
                let properties = spec.parameters.properties.get_or_insert_default();
                properties.insert(
                    "chars".to_string(),
                    JsonSchema::string(Some(CHARS_DESCRIPTION.to_string())),
                );
                properties.insert(
                    "yield_time_ms".to_string(),
                    JsonSchema::number(Some(YIELD_TIME_DESCRIPTION.to_string())),
                );
                ToolSpec::Function(spec)
            }
        }
    }
}
