use super::*;
use pretty_assertions::assert_eq;

#[test]
fn undeclared_tool_preserves_stream_and_replays_as_a_paired_rejection() {
    // A model can name a nested/retired tool. Keep later blocks and usage, never
    // infer a dispatch address, and preserve the original wire name on resume.
    let tools = [AnthropicTool {
        name: "read".into(),
        description: String::new(),
        input_schema: json!({}),
        identity: AnthropicToolIdentity::Function {
            name: "read".into(),
            namespace: None,
        },
    }];
    let frames = [
        json!({"type":"message_start","message":{"id":"msg","usage":{"input_tokens":5,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"Check both.","signature":"signed/+=="}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"known","name":"read","input":{"arguments":{"path":"one"}}}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"unknown","name":"functions__exec","input":{}}}),
        json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"input\":\"text('never dispatch');\"}"}}),
        json!({"type":"content_block_stop","index":2}),
        json!({"type":"content_block_start","index":3,"content_block":{"type":"text","text":"Still receiving."}}),
        json!({"type":"content_block_stop","index":3}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}),
        json!({"type":"message_stop"}),
    ];
    let mut decoder = Decoder::new(&tools);
    let mut items = Vec::new();
    let mut completed = None;
    for frame in frames {
        for event in decoder.decode(&frame.to_string()).unwrap() {
            match event {
                ResponseEvent::OutputItemDone(item) => items.push(item),
                ResponseEvent::Completed {
                    token_usage,
                    end_turn,
                    ..
                } => completed = Some((token_usage.unwrap().total_tokens, end_turn)),
                _ => {}
            }
        }
    }
    assert_eq!(completed, Some((12, Some(false))));
    assert_eq!(
        serde_json::to_value(&items[2]).unwrap(),
        json!({"type":"rejected_tool_call","id":"msg_2","provider":"anthropic","call_id":"unknown","name":"functions__exec","input":{"input":"text('never dispatch');"}})
    );
    items.extend([
        serde_json::from_value(json!({"type":"function_call_output","call_id":"known","output":"first result"})).unwrap(),
        serde_json::from_value(json!({"type":"function_call_output","call_id":"unknown","output":"Tool is not declared."})).unwrap(),
    ]);
    // Rollout serialization drops transient success hints; rejection must retain
    // its identity and is_error flag without a live declaration or decoder.
    let restored: Vec<ResponseItem> = serde_json::from_value(json!(items)).unwrap();
    let history = crate::anthropic::history::encode_history(
        &restored,
        "",
        &[],
        &[codex_protocol::openai_models::InputModality::Text],
    )
    .unwrap();
    assert_eq!(
        json!(history.messages),
        json!([
            {"role":"assistant","content":[
                {"type":"thinking","thinking":"Check both.","signature":"signed/+=="},
                {"type":"tool_use","id":"known","name":"read","input":{"arguments":{"path":"one"}}},
                {"type":"tool_use","id":"unknown","name":"functions__exec","input":{"input":"text('never dispatch');"}},
                {"type":"text","text":"Still receiving."}
            ]},
            {"role":"user","content":[
                {"type":"tool_result","tool_use_id":"known","content":"first result","is_error":false},
                {"type":"tool_result","tool_use_id":"unknown","content":"Tool is not declared.","is_error":true}
            ]}
        ])
    );
}

// A malformed envelope for a declared tool is answerable; failing the stream would discard later blocks and usage.
#[test]
fn malformed_declared_tool_preserves_later_blocks_and_usage() {
    let tools = [AnthropicTool {
        name: "read".into(),
        description: String::new(),
        input_schema: json!({}),
        identity: AnthropicToolIdentity::Function {
            name: "read".into(),
            namespace: None,
        },
    }];
    let mut decoder = Decoder::new(&tools);
    let mut items = Vec::new();
    let mut completed = None;
    for frame in [
        json!({"type":"message_start","message":{"id":"msg","usage":{"input_tokens":5,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"malformed","name":"read","input":{"path":"one"}}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":"Still receiving."}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}),
        json!({"type":"message_stop"}),
    ] {
        for event in decoder.decode(&frame.to_string()).unwrap() {
            match event {
                ResponseEvent::OutputItemDone(item) => items.push(item),
                ResponseEvent::Completed {
                    token_usage,
                    end_turn,
                    ..
                } => {
                    completed = Some((token_usage.unwrap().total_tokens, end_turn));
                }
                _ => {}
            }
        }
    }
    assert_eq!(
        (serde_json::to_value(items).unwrap(), completed),
        (
            json!([
                {"type":"rejected_tool_call","id":"msg_0","reason":"malformed_input","provider":"anthropic","call_id":"malformed","name":"read","input":{"path":"one"}},
                {"type":"message","id":"msg_1","role":"assistant","content":[{"type":"output_text","text":"Still receiving."}]}
            ]),
            Some((12, Some(false)))
        )
    );
}

#[test]
fn optional_thinking_breakdown_does_not_corrupt_completion_accounting() {
    // Missing/invalid observability must neither become a fabricated zero nor
    // make a successful response unusable. Iterations do not define this field.
    for (details, reported) in [
        (json!(null), None),
        (json!({}), None),
        (json!({"thinking_tokens": "8"}), None),
        (json!({"thinking_tokens": -1}), None),
        (json!({"thinking_tokens": 11}), None),
        (json!({"thinking_tokens": 0}), Some(0)),
        (json!({"thinking_tokens": 8}), Some(8)),
    ] {
        let mut decoder = Decoder::new(&[]);
        let mut final_usage = json!({"output_tokens":10});
        if !details.is_null() {
            final_usage["output_tokens_details"] = details;
        }
        for frame in [
            json!({"type":"message_start","message":{"id":"msg","usage":{"input_tokens":2,"output_tokens":0,"output_tokens_details":{"thinking_tokens":0}}}}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":final_usage}),
        ] {
            decoder.decode(&frame.to_string()).unwrap();
        }
        let events = decoder
            .decode(&json!({"type":"message_stop"}).to_string())
            .unwrap();
        let [
            ResponseEvent::Completed {
                token_usage: Some(usage),
                usage_metadata: Some(metadata),
                ..
            },
        ] = events.as_slice()
        else {
            panic!("expected completed response")
        };
        assert_eq!(
            (
                usage.input_tokens,
                usage.output_tokens,
                usage.total_tokens,
                metadata.reported_reasoning_tokens
            ),
            (2, 10, 12, reported)
        );
    }
}

#[test]
fn interleaved_tools_signed_blocks_and_cumulative_usage() {
    // Tool fragments interleave, signatures append, and iterative usage replaces
    // summary counts. Losing any of these corrupts the next request's history.
    let tools = [
        AnthropicTool {
            name: "functions__exec".into(),
            description: String::new(),
            input_schema: json!({}),
            identity: AnthropicToolIdentity::Custom {
                name: "exec".into(),
                namespace: Some("functions".into()),
            },
        },
        AnthropicTool {
            name: "send".into(),
            description: String::new(),
            input_schema: crate::anthropic::anthropic_function_input_schema(
                json!({"type":"object"}),
            )
            .unwrap(),
            identity: AnthropicToolIdentity::Function {
                name: "send".into(),
                namespace: None,
            },
        },
    ];
    let frames = [
        json!({"type":"message_start","message":{"id":"msg","usage":{"input_tokens":0,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Exact α\n"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"first+"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"second/=="}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"exec1","name":"functions__exec","input":{}}}),
        json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"send1","name":"send","input":{}}}),
        json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"arguments\": { \"target\" :"}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"input\":\"text('α');\\n\"}"}}),
        json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":" \"worker\" }}"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"content_block_stop","index":2}),
        json!({"type":"content_block_start","index":3,"content_block":{"type":"compaction","content":"Exact checkpoint. ","signature":""}}),
        json!({"type":"content_block_delta","index":3,"delta":{"type":"signature_delta","signature":"signed/=="}}),
        json!({"type":"content_block_stop","index":3}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"input_tokens":42,"output_tokens":19,"output_tokens_details":{"thinking_tokens":13},"iterations":[{"input_tokens":42,"output_tokens":19,"cache_creation_input_tokens":7,"cache_read_input_tokens":11},{"input_tokens":3,"output_tokens":2,"cache_creation_input_tokens":5,"cache_read_input_tokens":13}]}}),
        json!({"type":"message_stop"}),
    ];
    let mut decoder = Decoder::new(&tools);
    let mut output = Vec::new();
    let mut tool_deltas = Vec::new();
    let mut usage = None;
    for frame in frames {
        for event in decoder.decode(&frame.to_string()).unwrap() {
            match event {
                ResponseEvent::OutputItemDone(item) => output.push(item),
                ResponseEvent::ToolCallInputDelta {
                    item_id,
                    call_id,
                    delta,
                } => tool_deltas.push(json!({"item_id":item_id,"call_id":call_id,"delta":delta})),
                ResponseEvent::Completed {
                    token_usage,
                    end_turn,
                    usage_metadata,
                    ..
                } => {
                    assert_eq!(end_turn, Some(false));
                    assert_eq!(usage_metadata.unwrap().reported_reasoning_tokens, Some(13));
                    usage = token_usage;
                }
                _ => {}
            }
        }
    }
    assert_eq!(
        tool_deltas,
        vec![
            json!({"item_id":"msg_1","call_id":"exec1","delta":"text('α');\n"}),
            json!({"item_id":"msg_2","call_id":"send1","delta":"{ \"target\" : \"worker\" }"}),
        ]
    );
    assert_eq!(
        serde_json::to_value(output).unwrap(),
        json!([
        {"type":"reasoning","id":"msg_0","summary":[{"type":"summary_text","text":"Exact α\n"}],"content":null,"encrypted_content":"first+second/=="},
            {"type":"custom_tool_call","id":"msg_1","call_id":"exec1","name":"exec","namespace":"functions","input":"text('α');\n"},
            {"type":"function_call","id":"msg_2","call_id":"send1","name":"send","arguments":"{ \"target\" : \"worker\" }"},
            {"type":"native_compaction","id":"msg_3","provider":"anthropic","content":"Exact checkpoint. ","signature":"signed/=="}
        ])
    );
    assert_eq!(
        usage.unwrap(),
        codex_protocol::protocol::TokenUsage {
            input_tokens: 81,
            cached_input_tokens: 24,
            cache_write_input_tokens: 12,
            output_tokens: 21,
            reasoning_output_tokens: 13,
            total_tokens: 102,
            codex_rollout_budget_units: None,
        }
    );

    // A truncated response still reports incurred usage. The runtime
    // adapter must classify failure only after accounting for these counts.
    for (reason, expected) in [
        ("model_context_window_exceeded", (true, false)),
        ("max_tokens", (false, true)),
    ] {
        let mut decoder = Decoder::new(&[]);
        decoder.decode(&json!({"type":"message_start","message":{"id":"limited","usage":{"input_tokens":45,"output_tokens":0}}}).to_string()).unwrap();
        decoder.decode(&json!({"type":"message_delta","delta":{"stop_reason":reason},"usage":{"output_tokens":2}}).to_string()).unwrap();
        let completed = decoder
            .decode(&json!({"type":"message_stop"}).to_string())
            .unwrap();
        let [
            ResponseEvent::Completed {
                token_usage: Some(usage),
                usage_metadata: Some(metadata),
                ..
            },
        ] = completed.as_slice()
        else {
            panic!("native truncation must retain completion accounting");
        };
        assert_eq!(usage.total_tokens, 47);
        assert_eq!(
            (
                anthropic_context_window_exceeded(metadata),
                anthropic_output_limit_exceeded(metadata)
            ),
            expected
        );
    }
}
