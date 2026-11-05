//! Human-only edit evidence does not enter source-bearing telemetry.
use codex_protocol::mcp::CallToolResult;
use codex_protocol::mcp_file_changes::FILE_CHANGES_META_KEY;
use std::borrow::Cow;

pub(crate) fn without_human_receipts(
    result: &Result<CallToolResult, String>,
) -> Cow<'_, Result<CallToolResult, String>> {
    if result
        .as_ref()
        .ok()
        .and_then(|result| result.meta.as_ref())
        .is_none_or(|meta| meta.get(FILE_CHANGES_META_KEY).is_none())
    {
        return Cow::Borrowed(result);
    }
    let mut sanitized = result.clone();
    if let Ok(result) = &mut sanitized
        && let Some(meta) = result
            .meta
            .as_mut()
            .and_then(serde_json::Value::as_object_mut)
    {
        meta.remove(FILE_CHANGES_META_KEY);
    }
    Cow::Owned(sanitized)
}
