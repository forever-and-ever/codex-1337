use crate::function_tool::FunctionCallError;
use crate::sandboxing::SandboxPermissions;
use serde::Deserialize;
use serde::Serialize;

const MAX_SOURCE_BYTES: usize = 64 * 1024;

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ScriptMetadata {
    #[serde(default, deserialize_with = "optional")]
    pub(super) workdir: Option<String>,
    #[serde(default, deserialize_with = "optional")]
    pub(super) environment_id: Option<String>,
    #[serde(default, deserialize_with = "optional")]
    pub(super) max_output_tokens: Option<usize>,
    #[serde(default)]
    pub(super) background: bool,
    #[serde(default, deserialize_with = "optional")]
    pub(super) sandbox_permissions: Option<SandboxPermissions>,
    #[serde(default, deserialize_with = "optional")]
    pub(super) justification: Option<String>,
}

pub(super) fn parse(source: &str) -> Result<ScriptMetadata, FunctionCallError> {
    let reject = |message: &str| FunctionCallError::RespondToModel(message.to_string());
    if source.len() > MAX_SOURCE_BYTES {
        return Err(reject(
            "Script source exceeds 64 KiB; use a script file through exec_command instead.",
        ));
    }
    if source.trim().is_empty() {
        return Err(reject(
            "Script source must not be empty or whitespace-only.",
        ));
    }
    if source.contains('\0') {
        return Err(reject("Script source must not contain NUL bytes."));
    }
    let mut lines = source.lines();
    let first = lines.next().unwrap_or_default();
    let leading = if first.starts_with("#!") {
        lines.next().unwrap_or_default()
    } else {
        first
    };
    let Some(directive) = leading
        .trim_start_matches([' ', '\t'])
        .strip_prefix("# @exec:")
    else {
        return Ok(ScriptMetadata::default());
    };
    let metadata: ScriptMetadata = serde_json::from_str(directive)
        .map_err(|err| reject(&format!("Invalid # @exec metadata: {err}")))?;
    if metadata.sandbox_permissions == Some(SandboxPermissions::WithAdditionalPermissions) {
        return Err(reject(
            "Invalid # @exec metadata: with_additional_permissions requires a permission profile; use exec_command instead.",
        ));
    }
    if metadata
        .workdir
        .as_ref()
        .is_some_and(|value| value.trim().is_empty() || value.contains('\0'))
        || metadata
            .environment_id
            .as_ref()
            .is_some_and(|value| value.trim().is_empty() || value.contains('\0'))
        || metadata
            .justification
            .as_ref()
            .is_some_and(|value| value.trim().is_empty() || value.contains('\0'))
    {
        return Err(reject(
            "# @exec string values must be nonempty and must not contain NUL bytes.",
        ));
    }
    Ok(metadata)
}

fn optional<'de, T: Deserialize<'de>, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}
