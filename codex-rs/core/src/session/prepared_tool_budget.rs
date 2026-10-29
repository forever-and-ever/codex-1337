use codex_history::CodexHarnessMetadata;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ResponseItem;
use codex_utils_output_truncation::approx_token_count;

pub(super) fn retain_growth(
    original: &ResponseItem,
    prepared: &[ResponseItem],
    metadata: &mut Option<CodexHarnessMetadata>,
) {
    let Some(limit) = metadata
        .as_mut()
        .and_then(|metadata| metadata.history_truncation_token_limit.as_mut())
    else {
        return;
    };
    // Keep the original cap, adding only text introduced by media preparation.
    let prepared_tokens = prepared
        .iter()
        .map(text_tokens)
        .fold(0, usize::saturating_add);
    *limit = limit.saturating_add(prepared_tokens.saturating_sub(text_tokens(original)));
}

fn text_tokens(item: &ResponseItem) -> usize {
    let (ResponseItem::FunctionCallOutput { output, .. }
    | ResponseItem::CustomToolCallOutput { output, .. }) = item
    else {
        return 0;
    };
    match &output.body {
        FunctionCallOutputBody::Text(text) => approx_token_count(text),
        FunctionCallOutputBody::ContentItems(items) => items
            .iter()
            .map(|item| match item {
                FunctionCallOutputContentItem::InputText { text } => approx_token_count(text),
                _ => 0,
            })
            .fold(0, usize::saturating_add),
    }
}
