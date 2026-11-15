//! Checks code-mode timing headers, including absent and zero host measurements.

use super::*;
use codex_protocol::models::ImageReference;
use pretty_assertions::assert_eq;

#[test]
fn completed_timing_preserves_content_and_distinguishes_zero_from_unavailable() {
    let content = vec![
        FunctionCallOutputContentItem::InputText {
            text: "result".to_string(),
        },
        FunctionCallOutputContentItem::InputImage {
            image: ImageReference::Inline {
                image_url: "data:image/png;base64,image".to_string(),
            },
            detail: None,
        },
        FunctionCallOutputContentItem::inline_audio("data:audio/wav;base64,audio".to_string()),
    ];
    for (host_duration, expected_timing) in [
        (
            Some(Duration::from_millis(/*millis*/ 750)),
            "Wall time 1.250 seconds (code-mode 0.750 seconds; overhead 0.500 seconds)",
        ),
        (
            Some(Duration::ZERO),
            "Wall time 1.250 seconds (code-mode 0.000 seconds; overhead 1.250 seconds)",
        ),
        (
            Some(Duration::from_micros(/*micros*/ 1_251_400)),
            "Wall time 1.250 seconds (code-mode 1.251 seconds; overhead -0.001 seconds)",
        ),
        (None, "Wall time 0.8 seconds"),
    ] {
        for metadata in [
            ExecutionOutputMetadata::Hidden,
            ExecutionOutputMetadata::Included,
        ] {
            let mut output = CodeModeToolOutput::new(
                FunctionToolOutput::from_content(content.clone(), Some(false)),
                "Script failed".to_string(),
                Duration::from_millis(/*millis*/ 750),
                host_duration,
                metadata,
            );
            output.set_handler_duration_ms(/*handler_duration_ms*/ 1_250);
            let mut expected_content = vec![FunctionCallOutputContentItem::InputText {
                text: match metadata {
                    ExecutionOutputMetadata::Hidden => "Script failed\nOutput:\n".to_string(),
                    ExecutionOutputMetadata::Included => {
                        format!("Script failed\n{expected_timing}\nOutput:\n")
                    }
                },
            }];
            expected_content.extend(content.clone());
            let expected = FunctionToolOutput::from_content(expected_content, Some(false));
            for payload in [
                ToolPayload::Custom {
                    input: "text('result')".to_string(),
                },
                ToolPayload::Function {
                    arguments: r#"{"cell_id":"1"}"#.to_string(),
                },
            ] {
                let response = output.to_response_item("call", &payload);
                assert_eq!(response, expected.to_response_item("call", &payload));
                // Serialization must reuse the completed value, including on retries.
                assert_eq!(output.to_response_item("call", &payload), response);
            }
            assert_eq!(output.success_for_logging(), false);
            assert!(output.log_output().contains(expected_timing));
            // Recording and replay used to prefix-truncate an already-budgeted cell,
            // losing its later sections, media and recovery notice.
            let mut envelope = crate::tools::registry::AnyToolResult {
                call_id: "call".to_string(),
                payload: ToolPayload::Custom {
                    input: String::new(),
                },
                result: Box::new(output),
                post_tool_use_payload: None,
            }
            .into_response();
            let mut history = crate::context_manager::ContextManager::new(
                codex_protocol_test_support::uuid_v7_item_id_generator_for_test(),
            );
            history.record_annotated_items(
                std::slice::from_mut(&mut envelope),
                codex_protocol::protocol::TruncationPolicy::Tokens(1),
            );
            assert_eq!(history.annotated_items()[0].item, envelope.item);
            let persisted = serde_json::to_value(&envelope.metadata).unwrap();
            let replayed = codex_history::ResponseItemEnvelope {
                item: envelope.item.clone(),
                metadata: serde_json::from_value(persisted).unwrap(),
            };
            let mut history = crate::context_manager::ContextManager::new(
                codex_protocol_test_support::uuid_v7_item_id_generator_for_test(),
            );
            history.replay_annotated_item(
                &replayed,
                codex_protocol::protocol::TruncationPolicy::Bytes(1),
            );
            assert_eq!(history.annotated_items()[0].item, envelope.item);
        }
    }
}

#[test]
fn header_only_output_preserves_its_slot_without_changing_literal_text_output() {
    let header = "Script completed\nOutput:\n";
    let output = CodeModeToolOutput::new(
        FunctionToolOutput::from_content(Vec::new(), Some(true)),
        "Script completed".to_string(),
        Duration::ZERO,
        /*host_duration*/ None,
        ExecutionOutputMetadata::Hidden,
    );
    for payload in [
        ToolPayload::Custom {
            input: "".to_string(),
        },
        ToolPayload::Function {
            arguments: "{}".to_string(),
        },
    ] {
        let response = output.to_response_item("call", &payload);
        let body = match response {
            ResponseInputItem::FunctionCallOutput { output, .. }
            | ResponseInputItem::CustomToolCallOutput { output, .. } => output.body,
            _ => panic!("unexpected response"),
        };
        assert_eq!(
            body,
            FunctionCallOutputBody::ContentItems(vec![FunctionCallOutputContentItem::InputText {
                text: header.to_string()
            }])
        );
        let literal = FunctionToolOutput::from_text(header.to_string(), Some(true));
        let response = literal.to_response_item("notify", &payload);
        let body = match response {
            ResponseInputItem::FunctionCallOutput { output, .. }
            | ResponseInputItem::CustomToolCallOutput { output, .. } => output.body,
            _ => panic!("unexpected response"),
        };
        assert_eq!(body, FunctionCallOutputBody::Text(header.to_string()));
    }
}
