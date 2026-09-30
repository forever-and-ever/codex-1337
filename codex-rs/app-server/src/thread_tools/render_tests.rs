use super::*;
use codex_protocol::models::FunctionCallOutputBody;
use pretty_assertions::assert_eq;

#[test]
fn delegated_prompts_match_desktop_xml_contract() {
    let message = DelegatedMessage {
        source_thread_id: "thread-1".to_string(),
        source_thread_name: Some("Coder <main> & tools".to_string()),
        sent_at: Some(1_800_000_000_000),
        input: "Check <main> & report > status".to_string(),
    };
    let output = FunctionCallOutputBody::Text(message.encode());
    for namespace in ["codex_tui", "codex_app"] {
        assert_eq!(
            DelegatedMessage::from_tool_output("send_message_to_thread", Some(namespace), &output),
            Some(message.clone())
        );
    }
    assert_eq!(
        DelegatedMessage::from_tool_output("send_message_to_thread", Some("untrusted"), &output),
        None
    );
    // Existing desktop and persisted messages have neither a title nor a clock.
    assert_eq!(
        DelegatedMessage::parse(
            "<codex_delegation>\n  <source_thread_id>thread-1</source_thread_id>\n  <input>Check &lt;main&gt; &amp; report &gt; status</input>\n</codex_delegation>"
        ),
        Some(DelegatedMessage {
            source_thread_name: None,
            sent_at: None,
            ..message
        })
    );
}

fn delegated_prompt(source_thread_id: &str, input: &str) -> String {
    DelegatedMessage {
        source_thread_id: source_thread_id.to_string(),
        source_thread_name: None,
        sent_at: None,
        input: input.to_string(),
    }
    .encode()
}

#[test]
fn activity_metadata_is_retained_without_including_outputs() -> anyhow::Result<()> {
    let assistant_text = "Working".repeat(400);
    let turn: Turn = serde_json::from_value(json!({
        "id": "turn-1",
        "status": "completed",
        "items": [
            {"type": "reasoning", "id": "thought-1", "summary": ["Thinking"], "content": ["Private reasoning"]},
            {"type": "commandExecution", "id": "command-1", "command": "cargo test", "cwd": "/tmp",
                "status": "completed", "commandActions": [], "aggregatedOutput": "Command output", "exitCode": 0},
            {"type": "fileChange", "id": "patch-1", "status": "completed",
                "changes": [{"path": "src/main.rs", "kind": {"type": "add"}, "diff": "+hello"}]},
            {"type": "mcpToolCall", "id": "mcp-1", "server": "docs", "tool": "search",
                "status": "completed", "arguments": {}},
            {"type": "userMessage", "id": "user-1", "content": [
                {"type": "text", "text": delegated_prompt("source-1", "Check <main> & status")},
                {"type": "skill", "name": "debug", "path": "/tmp/SKILL.md"},
                {"type": "mention", "name": "docs", "path": "app://docs"}
            ]},
            {"type": "agentMessage", "id": "assistant-1", "text": assistant_text, "phase": "commentary"},
            {"type": "webSearch", "id": "web-1", "query": "latest docs", "action": null},
            {"type": "sleep", "id": "sleep-1", "durationMs": 1000},
            {"type": "imageGeneration", "id": "image-1", "status": "completed",
                "revisedPrompt": "a cat", "result": "image bytes"},
            {"type": "enteredReviewMode", "id": "review-1", "review": "review changes"},
            {"type": "functionCallOutput", "id": "delegation-1", "name": "send_message_to_thread",
                "namespace": "codex_tui", "output": delegated_prompt("source-2", "Follow <up> & report")}
        ]
    }))?;

    let summary = response_json(
        success_response(turn_summary(
            &turn,
            /*include_outputs*/ false,
            DEFAULT_OUTPUT_CHARS,
        ))
        .expect("task response"),
    );
    assert_eq!(
        summary["items"],
        json!([
            {"type": "reasoning", "id": "thought-1", "summary": ["Thinking"]},
            {"type": "commandExecution", "id": "command-1", "command": "cargo test", "cwd": "/tmp", "exitCode": 0, "status": "completed", "durationMs": null},
            {"type": "fileChange", "id": "patch-1", "status": "completed",
                "changes": [{"path": "src/main.rs", "kind": {"type": "add"}}]},
            {"type": "mcpToolCall", "id": "mcp-1", "server": "docs", "tool": "search", "arguments": {}, "status": "completed", "durationMs": null},
            {"type": "userMessage", "id": "user-1", "content": [
                {"type": "text", "text": delegated_prompt("source-1", "Check <main> & status"),
                    "codexDelegation": {"sourceThreadId": "source-1", "sourceThreadName": null, "sentAt": null, "input": "Check <main> & status"}},
                {"type": "skill", "name": "debug", "path": "/tmp/SKILL.md"},
                {"type": "mention", "name": "docs", "path": "app://docs"}
            ]},
            {"type": "agentMessage", "id": "assistant-1", "text": assistant_text, "phase": "commentary"},
            {"type": "webSearch", "id": "web-1", "query": "latest docs", "action": null},
            {"type": "sleep", "id": "sleep-1", "durationMs": 1000},
            {"type": "imageGeneration", "id": "image-1", "status": "completed",
                "revisedPrompt": "a cat", "savedPath": null},
            {"type": "enteredReviewMode", "id": "review-1", "review": "review changes"},
            {"type": "functionCallOutput", "id": "delegation-1", "name": "send_message_to_thread",
                "namespace": "codex_tui", "codexDelegation": {
                    "sourceThreadId": "source-2", "sourceThreadName": null, "sentAt": null, "input": "Follow <up> & report"
                }}
        ])
    );

    let full = turn_summary(&turn, /*include_outputs*/ true, DEFAULT_OUTPUT_CHARS);
    assert_eq!(
        full["items"][0]["content"],
        json!([{"text": "Private reasoning", "truncated": false}])
    );
    assert_eq!(
        full["items"][1]["output"],
        json!({"text": "Command output", "truncated": false})
    );
    assert_eq!(
        full["items"][2]["changes"][0]["diff"],
        json!({"text": "+hello", "truncated": false})
    );
    assert_eq!(
        full["items"][8]["result"],
        json!({"text": "image bytes", "truncated": false})
    );

    let no_outputs = turn_summary(
        &turn, /*include_outputs*/ true, /*output_chars*/ 0,
    );
    assert_eq!(no_outputs["items"][5]["text"], assistant_text);
    assert_eq!(
        no_outputs["items"][1]["output"],
        json!({"text": "", "truncated": true, "originalChars": 14})
    );
    assert_eq!(
        no_outputs["items"][8]["result"],
        json!({"text": "", "truncated": true, "originalChars": 11})
    );
    Ok(())
}
