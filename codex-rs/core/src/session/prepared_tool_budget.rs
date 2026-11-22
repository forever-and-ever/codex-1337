use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ResponseItem;
use codex_utils_output_truncation::approx_token_count;

pub(super) fn retain_growth(original_tokens: usize, prepared: &ResponseItem, limit: &mut usize) {
    *limit = limit.saturating_add(text_tokens(prepared).saturating_sub(original_tokens));
}

pub(super) fn text_tokens(item: &ResponseItem) -> usize {
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
