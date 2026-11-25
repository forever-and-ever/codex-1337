//! The nested-tool roster as model requests state and amend it.

use serde_json::Value;

/// Every roster fragment among the input items, in order: the roster, then its amendments.
pub fn fragments(input: &[Value]) -> Vec<String> {
    input
        .iter()
        .filter(|item| item["role"] == "developer")
        .filter_map(|item| item["content"].as_array())
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .filter(|text| text.starts_with("<nested_tools>"))
        .map(str::to_owned)
        .collect()
}
