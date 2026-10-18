//! `exec_command` inside Code Mode cells.
//!
//! A cell blocks until its script finishes and returns control to the model when news arrives or
//! its own budget runs out, so a command it starts needs no yield of its own: the command runs
//! until it exits unless the script backgrounds it. An escalation is the bare request: the
//! reviewer judges the command and its conversation, not the model's case for it, and nothing
//! persists a model-proposed prefix. The schema therefore drops `yield_time_ms`,
//! `justification`, and `prefix_rule`; one passed out of habit is ignored rather than refused.

use codex_tools::JsonSchema;
use codex_tools::ToolSpec;
use serde::Deserialize;

use super::super::ExecCommandArgs;
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
const DROPPED: [&str; 3] = ["yield_time_ms", "justification", "prefix_rule"];

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
        crate::tools::fence::Prerequisite::detached();
        InitialWait::Yield(MIN_YIELD_TIME_MS)
    } else {
        InitialWait::Exit
    })
}

/// Forgets the rationale a habitual call still carries.
pub(super) fn forget_rationale(args: &mut ExecCommandArgs) {
    args.justification = None;
    args.prefix_rule = None;
}

pub(super) fn spec(spec: ToolSpec) -> ToolSpec {
    let ToolSpec::Function(mut spec) = spec else {
        unreachable!("exec_command has a function schema");
    };
    spec.description = spec
        .description
        .replacen(UPSTREAM_DESCRIPTION, DESCRIPTION, 1);
    let properties = spec.parameters.properties.get_or_insert_default();
    for dropped in DROPPED {
        let _ = properties.remove(dropped);
    }
    let _ = properties.insert(
        "background".to_string(),
        JsonSchema::boolean(Some(BACKGROUND_DESCRIPTION.to_string())),
    );
    ToolSpec::Function(spec)
}
