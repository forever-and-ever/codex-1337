//! `exec_command` inside Code Mode cells.
//!
//! A cell blocks until its script finishes and returns control to the model when news arrives or
//! its own budget runs out, so a command it starts needs no yield of its own: the command runs
//! until it exits unless the script backgrounds it. The schema drops `yield_time_ms`; one passed
//! out of habit is ignored rather than refused.

use codex_tools::JsonSchema;
use codex_tools::ToolSpec;
use serde::Deserialize;

use super::ExecCommandHandler;
use super::ExecCommandHandlerOptions;
use super::ExecCommandLifetime;
use crate::function_tool::FunctionCallError;
use crate::tools::handlers::parse_arguments;
use crate::unified_exec::InitialWait;
use crate::unified_exec::MIN_YIELD_TIME_MS;

const UPSTREAM_DESCRIPTION: &str =
    "Runs a command, returning output or a session ID for ongoing interaction.";
const DESCRIPTION: &str = "Runs a command until it exits and returns its output. Set `background` for servers or to keep working while it runs; the call then returns a session ID for `write_stdin`.";
const BACKGROUND_DESCRIPTION: &str =
    "True starts the command and returns its session ID at once instead of waiting for it to exit.";

impl ExecCommandHandler {
    /// `exec_command` for a Code Mode Only session, where every call comes from a cell.
    pub(crate) fn cell(options: ExecCommandHandlerOptions) -> Self {
        Self {
            options,
            lifetime: ExecCommandLifetime::Cell,
        }
    }
}

#[derive(Deserialize)]
struct CellArgs {
    #[serde(default)]
    background: bool,
}

/// How long a cell's command keeps its script waiting.
pub(super) fn initial_wait(arguments: &str) -> Result<InitialWait, FunctionCallError> {
    let CellArgs { background } = parse_arguments(arguments)?;
    Ok(if background {
        InitialWait::Yield(MIN_YIELD_TIME_MS)
    } else {
        InitialWait::Exit
    })
}

pub(super) fn spec(spec: ToolSpec) -> ToolSpec {
    let ToolSpec::Function(mut spec) = spec else {
        unreachable!("exec_command has a function schema");
    };
    spec.description = spec
        .description
        .replacen(UPSTREAM_DESCRIPTION, DESCRIPTION, 1);
    let properties = spec.parameters.properties.get_or_insert_default();
    properties.remove("yield_time_ms");
    properties.insert(
        "background".to_string(),
        JsonSchema::boolean(Some(BACKGROUND_DESCRIPTION.to_string())),
    );
    ToolSpec::Function(spec)
}
