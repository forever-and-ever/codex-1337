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
        (
            "unifiedDiff",
            json!("--- a/a.txt\n+++ b/a.txt\n@@ -0 +0 @@\n-old\n+new\n"),
        ),
        (
            "unifiedDiff",
            json!("--- a/a.txt\n+++ b/a.txt\n@@ -99 +99 @@\n-old\n+new\n"),
        ),
        (
            "unifiedDiff",
            json!("--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-long source\n+new\n"),
        ),
        (
            "unifiedDiff",
            json!(
                "diff --git a/b.bin b/b.bin\nGIT binary patch\nliteral 1\nx\ndiff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n"
            ),
        ),
    ] {
        let mut receipt = valid.clone();
        receipt["files"][0][field] = value;
        assert!(FileChangeReceipt::parse(&receipt).is_none(), "{field}");
    }
    let mut mismatched = valid.clone();
    mismatched["files"][0]["after"]["uri"] = json!("file:///workspace/b.txt");
    assert!(FileChangeReceipt::parse(&mismatched).is_none());
    for uri in [
        "file:///a%zz",
        "file://user@host/a",
        "file:///a?x=1",
        "file:///a\u{1b}",
    ] {
        let mut receipt = valid.clone();
        receipt["files"][0]["before"]["uri"] = json!(uri);
        receipt["files"][0]["after"]["uri"] = json!(uri);
        assert!(FileChangeReceipt::parse(&receipt).is_none());
    }
    let mut encoded = valid.clone();
    for endpoint in ["before", "after"] {
        encoded["files"][0][endpoint]["uri"] = json!("file://remote/workspace/a%20b.rs");
    }
    assert_eq!(
        FileChangeReceipt::parse(&encoded).unwrap().into_value(),
        encoded
    );
    // Empty-range anchors and one-byte unterminated lines have different coordinates.
    for (operation, before_bytes, after_bytes, diff) in [
        (
            "modify",
            Some(4),
            Some(4),
            "diff --git a/a.txt b/a.txt\nindex 1234567..7654321 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
        ),
        (
            "modify",
            Some(1),
            Some(2),
            "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n",
        ),
        (
            "modify",
            Some(1),
            Some(3),
            "--- a/a.txt\n+++ b/a.txt\n@@ -1,0 +2 @@\n+x\n",
        ),
        (
            "create",
            None,
            Some(1),
            "--- /dev/null\n+++ b/a.txt\n@@ -0,0 +1 @@\n+x\n\\ No newline at end of file\n",
        ),
        (
            "delete",
            Some(1),
            None,
            "--- a/a.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n\\ No newline at end of file\n",
        ),
    ] {
        let mut receipt = valid.clone();
        receipt["files"][0]["operation"] = json!(operation);
        for (endpoint, bytes) in [("before", before_bytes), ("after", after_bytes)] {
            if let Some(bytes) = bytes {
                receipt["files"][0][endpoint]["bytes"] = json!(bytes);
            } else {
                receipt["files"][0][endpoint] = Value::Null;
            }
        }
        receipt["files"][0]["unifiedDiff"] = json!(diff);
        assert!(
            FileChangeReceipt::parse(&receipt).is_some(),
            "{operation}: {diff}"
        );
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
