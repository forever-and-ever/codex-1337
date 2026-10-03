use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

fn examples() -> Vec<Value> {
    serde_json::from_str(include_str!("../mcp-file-changes/v1.examples.json")).unwrap()
}

#[test]
fn receipts_admit_contract_examples_and_reject_inconsistent_captured_effects() {
    for example in examples() {
        assert_eq!(
            FileChangeReceipt::parse(&example["receipt"]).is_some(),
            example["schemaValid"].as_bool().unwrap(),
            "{}",
            example["name"]
        );
    }
    let valid = examples()[0]["receipt"].clone();
    // The schema cannot express URI identity or complete hunks; enforce both together.
    for (field, value) in [
        (
            "unifiedDiff",
            json!("--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1 @@\n-old\n+new\n"),
        ),
        (
            "unifiedDiff",
            json!(
                "--- a/a.txt\n+++ b/a.txt\n@@ -18446744073709551615 +1 @@\n-old\n+new\n@@ -1 +2 @@\n-old\n+new\n"
            ),
        ),
        (
            "unifiedDiff",
            json!(
                "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-old\n+new\n"
            ),
        ),
        ("before", json!(null)),
    ] {
        let mut receipt = valid.clone();
        receipt["files"][0][field] = value;
        assert!(FileChangeReceipt::parse(&receipt).is_none(), "{field}");
    }
    for uri in [
        "file:///workspace/b.txt",
        "file:///a%zz",
        "file://user@host/a",
        "file:///a?x=1",
        "file:///a\u{1b}",
    ] {
        let mut receipt = valid.clone();
        receipt["files"][0]["after"]["uri"] = json!(uri);
        assert!(FileChangeReceipt::parse(&receipt).is_none());
    }
    let mut receipt = valid.clone();
    receipt["files"][0]["after"] = receipt["files"][0]["before"].clone();
    assert!(FileChangeReceipt::parse(&receipt).is_none());
    let mut receipt = valid.clone();
    receipt["files"]
        .as_array_mut()
        .unwrap()
        .push(valid["files"][0].clone());
    assert!(FileChangeReceipt::parse(&receipt).is_none());
    let mut receipt = valid;
    receipt["files"][0]
        .as_object_mut()
        .unwrap()
        .remove("before");
    assert!(FileChangeReceipt::parse(&receipt).is_none());
}

#[test]
fn receipt_capture_is_private_and_rejection_does_not_change_tool_failure() {
    for receipt in [
        examples()[0]["receipt"].clone(),
        json!({"version": 2, "source": "private source"}),
    ] {
        let mut result = crate::mcp::CallToolResult {
            content: vec![json!({"type":"text", "text":"publication failed; do not retry"})],
            structured_content: None,
            is_error: Some(true),
            meta: Some(json!({FILE_CHANGES_META_KEY: receipt, "other": "kept"})),
        };
        let captured = take_file_changes(&mut result);
        assert_eq!(captured.is_some(), receipt["version"] == 1);
        assert_eq!(result.meta, Some(json!({"other":"kept"})));
        assert_eq!(result.is_error, Some(true));
        let projected = serde_json::to_string(&result.as_function_call_output_payload()).unwrap();
        assert!(!projected.contains("private source") && !projected.contains("unifiedDiff"));
        assert!(projected.contains("publication failed; do not retry"));
    }
    let mut oversized = examples()[0]["receipt"].clone();
    oversized["files"][0]["unifiedDiff"] = json!("é".repeat(32_769));
    assert!(FileChangeReceipt::parse(&oversized).is_none());
    oversized["files"][0]["unifiedDiff"] = json!("x".repeat(262_144));
    assert!(FileChangeReceipt::parse(&oversized).is_none());
}
