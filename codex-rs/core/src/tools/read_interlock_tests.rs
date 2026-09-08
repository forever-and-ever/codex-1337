use super::*;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_tools::ToolOutput;
use codex_tools::ToolPayload;
use codex_utils_output_truncation::TruncationPolicy;
use pretty_assertions::assert_eq;

fn output(text: &str) -> ExecCommandToolOutput {
    ExecCommandToolOutput {
        event_call_id: "read".into(),
        chunk_id: "chunk".into(),
        wall_time: std::time::Duration::ZERO,
        raw_output: text.as_bytes().to_vec(),
        truncation_policy: TruncationPolicy::Bytes(100_000),
        max_output_tokens: Some(20_000),
        process_id: None,
        exit_code: Some(0),
        original_token_count: None,
        output_omitted_bytes: None,
        hook_command: None,
    }
}

fn attempt(history: &ContextManager, command: &str) -> ReadAttempt {
    let intent = ReadIntent::parse(command).unwrap();
    ReadAttempt {
        generation: history.history_version(),
        key: Some(Key {
            environment: "local".into(),
            path: "/file".into(),
            range: intent.range.clone(),
        }),
        intent,
    }
}

fn deliver(history: &mut ContextManager, text: String, budget: usize) {
    let mut item: ResponseItem = serde_json::from_value(serde_json::json!({
        "type": "function_call_output", "call_id": "read", "output": ""
    }))
    .unwrap();
    if let ResponseItem::FunctionCallOutput { output, .. } = &mut item {
        *output = FunctionCallOutputPayload::from_text(text);
    }
    history.record_items([&item], TruncationPolicy::Bytes(budget));
}

#[test]
fn read_interlock_requires_retained_evidence_and_survives_neither_rewrites_nor_context_changes() {
    let body = "a moderately long source line\n".repeat(30);
    let mut history = ContextManager::new();
    let mut bank = Bank::default();
    let mut first = output(&body);
    assert_eq!(
        attempt(&history, "cat file")
            .process(&mut bank, &history, &mut first)
            .0,
        "pending_delivery"
    );
    // Execution or a truncated delivery must not make unseen content suppressible.
    deliver(
        &mut history,
        String::from_utf8(first.raw_output).unwrap(),
        90,
    );
    let mut full = output(&body);
    assert_eq!(
        attempt(&history, "cat file")
            .process(&mut bank, &history, &mut full)
            .0,
        "pending_delivery"
    );
    // Exercise the same JSON-escaped shape emitted by text(await exec_command(...)).
    let text = serde_json::to_string(&full.code_mode_result(&ToolPayload::Function {
        arguments: "{}".into(),
    }))
    .unwrap();
    deliver(&mut history, text, 100_000);
    let suppressed = attempt(&history, "cat file").process(&mut bank, &history, &mut output(&body));
    assert_eq!(suppressed.0, "suppressed");
    assert!(suppressed.2 > 0);
    assert_eq!(
        attempt(&history, "cat file")
            .process(&mut Bank::default(), &history, &mut output(&body))
            .0,
        "pending_delivery"
    );
    assert_eq!(
        attempt(&history, "sed -n '1,30p' file")
            .process(&mut bank, &history, &mut output(&body))
            .0,
        "pending_delivery"
    );
    assert_eq!(
        attempt(&history, "# force_reread\ncat file")
            .process(&mut bank, &history, &mut output(&body))
            .0,
        "forced"
    );

    let mut forced = output(&body);
    attempt(&history, "# force_reread\ncat file").process(&mut bank, &history, &mut forced);
    deliver(
        &mut history,
        String::from_utf8(forced.raw_output).unwrap(),
        100_000,
    );
    let changed = body.replace("source", "edited");
    assert_eq!(
        attempt(&history, "cat file")
            .process(&mut bank, &history, &mut output(&changed))
            .0,
        "changed"
    );

    let mut current = output(&body);
    attempt(&history, "cat file").process(&mut bank, &history, &mut current);
    deliver(
        &mut history,
        String::from_utf8(current.raw_output).unwrap(),
        100_000,
    );
    let retained = history.clone().into_annotated_items();
    history.replace_compacted(retained);
    assert_eq!(
        attempt(&history, "cat file")
            .process(&mut bank, &history, &mut output(&body))
            .0,
        "pending_delivery"
    );
    assert_eq!(
        attempt(&history, "cat file")
            .process(&mut bank, &history, &mut output("tiny\n"))
            .0,
        "small_exempt"
    );
    assert_eq!(
        attempt(&history, "cat file")
            .process(&mut bank, &history, &mut output(&"x".repeat(2048)))
            .0,
        "pending_delivery"
    );
    let mut failed = output(&body);
    failed.exit_code = Some(1);
    assert_eq!(
        attempt(&history, "cat file")
            .process(&mut bank, &history, &mut failed)
            .0,
        "incomplete_or_failed"
    );
}
