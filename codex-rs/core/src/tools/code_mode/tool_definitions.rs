//! Prepares nested tool definitions for execution and search.
//! Cached contracts and their executable schemas stay borrowed until dispatch.

use crate::tools::registry::CoreToolRuntime;
use codex_code_mode::ToolDefinition;
use codex_tools::ToolSpec;
use std::borrow::Cow;

pub(crate) fn prepare_code_mode_tool_definitions<'a>(
    runtime: Option<&'a dyn CoreToolRuntime>,
    spec: impl FnOnce() -> Cow<'a, ToolSpec>,
) -> Cow<'a, [ToolDefinition]> {
    if let Some(definitions) = runtime.and_then(|runtime| runtime.cached_code_mode_definitions()) {
        return Cow::Borrowed(definitions);
    }
    let spec = spec();
    Cow::Owned(codex_tools::collect_code_mode_tool_definitions(
        std::iter::once(spec.as_ref()),
    ))
}
