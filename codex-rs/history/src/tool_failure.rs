//! Tool failure across rollout persistence.
//!
//! Native Claude encodes `FunctionCallOutputPayload::success == Some(false)` as
//! `tool_result.is_error`, but the payload persists in the Responses wire shape, which omits
//! `success`. The rollout marks the failure in the output's harness metadata instead, and
//! loading moves the mark back into the payload, so a resumed or forked turn encodes exactly as
//! it did live. In memory the payload alone owns the fact.

use std::borrow::Cow;

use codex_protocol::models::ResponseItem;

use crate::CodexHarnessMetadata;

/// Persisted metadata for `item`, marked when it is a failed tool output.
pub(crate) fn mark<'a>(
    item: &ResponseItem,
    metadata: Option<&'a CodexHarnessMetadata>,
) -> Option<Cow<'a, CodexHarnessMetadata>> {
    let failed = matches!(
        item,
        ResponseItem::FunctionCallOutput { output, .. }
            | ResponseItem::CustomToolCallOutput { output, .. }
            if output.success == Some(false)
    );
    if !failed {
        return metadata.map(Cow::Borrowed);
    }
    let mut metadata = metadata.cloned().unwrap_or_default();
    metadata.tool_failed = true;
    Some(Cow::Owned(metadata))
}

/// Moves a persisted failure mark from loaded metadata into its output.
pub(crate) fn restore(item: &mut ResponseItem, metadata: Option<&mut CodexHarnessMetadata>) {
    if let Some(metadata) = metadata
        && std::mem::take(&mut metadata.tool_failed)
        && let ResponseItem::FunctionCallOutput { output, .. }
        | ResponseItem::CustomToolCallOutput { output, .. } = item
    {
        output.success = Some(false);
    }
}
