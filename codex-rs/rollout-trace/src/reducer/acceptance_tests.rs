//! Accepted history retains original inference provenance across retries and resets.

use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

use super::test_support::append_completed_inference;
use super::test_support::append_inference_completion;
use super::test_support::append_inference_request;
use super::test_support::create_started_writer;
use super::test_support::message;
use super::test_support::start_thread;
use super::test_support::start_turn;
use super::test_support::start_turn_for_thread;
use super::test_support::trace_context;
use crate::ModelRequestProtocol;
use crate::ProducerRef;
use crate::RawPayloadKind;
use crate::RawTraceEventPayload;
use crate::model::ConversationPart;
use crate::replay_bundle;

#[test]
fn identical_text_from_a_discarded_attempt_does_not_gain_acceptance() -> anyhow::Result<()> {
    for request_protocol in [
        ModelRequestProtocol::Responses,
        ModelRequestProtocol::Messages,
    ] {
        let temp = TempDir::new()?;
        let writer = create_started_writer(&temp)?;
        start_turn(&writer, "turn-1")?;
        let input = message("user", "answer");
        let append_request = |inference_id: &str| -> anyhow::Result<()> {
            let request = match request_protocol {
                ModelRequestProtocol::Responses => json!({"input": [input]}),
                ModelRequestProtocol::Messages => json!({"messages": [{
                    "role": "user", "content": [{"type": "text", "text": "answer"}],
                }]}),
            };
            let request_payload =
                writer.write_json_payload(RawPayloadKind::InferenceRequest, &request)?;
            writer.append(RawTraceEventPayload::InferenceStarted {
                request_protocol,
                inference_call_id: inference_id.to_string(),
                thread_id: "thread-root".to_string(),
                codex_turn_id: "turn-1".to_string(),
                model: "test-model".to_string(),
                provider_name: "test-provider".to_string(),
                request_payload,
            })?;
            Ok(())
        };
        let mut output = vec![message("assistant", "same text")];
        if request_protocol == ModelRequestProtocol::Messages {
            // Native signatures remain opaque identity, including discarded retries.
            output.push(json!({
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "same thought"}],
                "encrypted_content": "native-signature-not-for-rendering",
            }));
        }
        for (index, item) in output.iter_mut().enumerate() {
            item["id"] = json!(format!("discarded-{index}"));
        }
        append_request("first")?;
        let response = writer.write_json_payload(
            RawPayloadKind::InferenceResponse,
            &json!({"output_items": output}),
        )?;
        append_inference_completion(&writer, "first", "resp-first", response)?;

        append_request("retry")?;
        for (index, item) in output.iter_mut().enumerate() {
            let id = format!("accepted-{index}");
            item["id"] = json!(id);
            // Acceptance can arrive before the asynchronous stream mapper's terminal record.
            writer.append_with_context(
                trace_context("turn-1"),
                RawTraceEventPayload::HistoryItemAccepted {
                    item_id: id,
                    agent_message_payload: None,
                },
            )?;
        }
        let response = writer.write_json_payload(
            RawPayloadKind::InferenceResponse,
            &json!({"output_items": output}),
        )?;
        append_inference_completion(&writer, "retry", "resp-retry", response)?;

        let trace = replay_bundle(temp.path())?;
        let first = &trace.inference_calls["first"];
        let retry = &trace.inference_calls["retry"];
        assert_ne!(first.response_item_ids, retry.response_item_ids);
        let mut expected = retry.request_item_ids.clone();
        expected.extend(retry.response_item_ids.clone());
        assert_eq!(trace.threads["thread-root"].conversation_item_ids, expected);
        assert_eq!(
            trace.conversation_items[&first.response_item_ids[0]]
                .responses_item_id
                .as_deref(),
            Some("discarded-0")
        );
        if request_protocol == ModelRequestProtocol::Messages {
            let reasoning = &trace.conversation_items[&retry.response_item_ids[1]];
            assert_eq!(reasoning.responses_item_id.as_deref(), Some("accepted-1"));
            assert!(
                reasoning
                    .body
                    .parts
                    .iter()
                    .any(|part| matches!(part, ConversationPart::SignatureRef { .. }))
            );
            assert!(!serde_json::to_string(&trace)?.contains("native-signature-not-for-rendering"));
        }
    }
    Ok(())
}

#[test]
fn late_response_reuses_a_later_request_sighting_without_rewriting_its_snapshot()
-> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let writer = create_started_writer(&temp)?;
    start_turn(&writer, "turn-1")?;
    let input = message("user", "answer");
    let mut output = message("assistant", "accepted");
    output["id"] = json!("msg-accepted");
    append_inference_request(
        &writer,
        "thread-root",
        "turn-1",
        "first",
        vec![input.clone()],
    )?;
    let later_input = vec![input, output.clone(), message("user", "continue")];
    append_inference_request(
        &writer,
        "thread-root",
        "turn-1",
        "second",
        later_input.clone(),
    )?;
    let response = writer.write_json_payload(
        RawPayloadKind::InferenceResponse,
        &json!({"output_items": [output]}),
    )?;
    append_inference_completion(&writer, "first", "resp-first", response)?;
    append_inference_request(&writer, "thread-root", "turn-1", "third", later_input)?;

    let trace = replay_bundle(temp.path())?;
    let second = &trace.inference_calls["second"];
    assert_eq!(
        trace.inference_calls["first"].response_item_ids,
        vec![second.request_item_ids[1].clone()]
    );
    assert_eq!(
        trace.inference_calls["third"].request_item_ids,
        second.request_item_ids
    );
    assert_eq!(
        trace.threads["thread-root"].conversation_item_ids,
        second.request_item_ids
    );
    assert_eq!(
        trace.conversation_items[&second.request_item_ids[1]].produced_by,
        vec![ProducerRef::Inference {
            inference_call_id: "first".into()
        }]
    );
    Ok(())
}

#[test]
fn late_response_keeps_compaction_replacement_distinct() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let writer = create_started_writer(&temp)?;
    start_turn(&writer, "turn-1")?;
    let input = message("user", "answer");
    let mut output = message("assistant", "accepted");
    output["id"] = json!("msg-accepted");
    append_inference_request(
        &writer,
        "thread-root",
        "turn-1",
        "first",
        vec![input.clone()],
    )?;
    let checkpoint = writer.write_json_payload(
        RawPayloadKind::CompactionCheckpoint,
        &json!({
            "input_history": [input, output], "replacement_history": [output]
        }),
    )?;
    writer.append_with_context(
        trace_context("turn-1"),
        RawTraceEventPayload::CompactionInstalled {
            compaction_id: "reset".into(),
            checkpoint_payload: checkpoint,
        },
    )?;
    let response = writer.write_json_payload(
        RawPayloadKind::InferenceResponse,
        &json!({"output_items": [output]}),
    )?;
    append_inference_completion(&writer, "first", "resp-first", response)?;
    append_inference_request(&writer, "thread-root", "turn-1", "second", vec![output])?;

    let trace = replay_bundle(temp.path())?;
    let checkpoint = &trace.compactions["reset"];
    assert_eq!(
        trace.inference_calls["first"].response_item_ids,
        vec![checkpoint.input_item_ids[1].clone()]
    );
    assert_eq!(
        trace.inference_calls["second"].request_item_ids,
        checkpoint.replacement_item_ids
    );
    assert_ne!(
        checkpoint.input_item_ids[1],
        checkpoint.replacement_item_ids[0]
    );
    assert_eq!(
        trace.conversation_items[&checkpoint.replacement_item_ids[0]].produced_by,
        vec![ProducerRef::Compaction {
            compaction_id: "reset".into()
        }]
    );
    let mut expected = checkpoint.input_item_ids.clone();
    expected.push(checkpoint.marker_item_id.clone());
    expected.extend(checkpoint.replacement_item_ids.clone());
    assert_eq!(trace.threads["thread-root"].conversation_item_ids, expected);
    Ok(())
}

#[test]
fn inherited_output_is_child_prefill_without_copying_parent_sampling_provenance()
-> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let writer = create_started_writer(&temp)?;
    start_turn(&writer, "turn-parent")?;
    let mut output = message("assistant", "inherited");
    output["id"] = json!("msg-parent");
    append_completed_inference(
        &writer,
        "thread-root",
        "turn-parent",
        "parent",
        vec![message("user", "answer")],
        vec![output.clone()],
    )?;
    start_thread(&writer, "child", "/root/child")?;
    start_turn_for_thread(&writer, "child", "turn-child")?;
    append_inference_request(&writer, "child", "turn-child", "child", vec![output])?;

    let trace = replay_bundle(temp.path())?;
    let parent = &trace.inference_calls["parent"];
    let child = &trace.inference_calls["child"];
    assert_ne!(child.request_item_ids, parent.response_item_ids);
    assert_eq!(
        trace.threads["child"].conversation_item_ids,
        child.request_item_ids
    );
    assert_eq!(
        trace.conversation_items[&child.request_item_ids[0]].produced_by,
        Vec::<ProducerRef>::new()
    );
    // A sighting in the child must not imply acceptance in the parent's history.
    assert_eq!(
        trace.threads["thread-root"].conversation_item_ids,
        parent.request_item_ids
    );
    let accepted = RawTraceEventPayload::HistoryItemAccepted {
        item_id: "msg-parent".into(),
        agent_message_payload: None,
    };
    let context = trace_context("turn-parent");
    let first = writer.append_with_context(context.clone(), accepted.clone())?;
    let mut duplicate = writer.append_with_context(context, accepted)?;
    drop(writer);
    // Give the later event an earlier clock reading: sequence determines which pair wins.
    let path = temp.path().join("trace.jsonl");
    let original = serde_json::to_string(&duplicate)?;
    duplicate.wall_time_unix_ms = first.wall_time_unix_ms - 1;
    let events = std::fs::read_to_string(&path)?;
    assert!(events.contains(&original));
    std::fs::write(
        path,
        events.replace(&original, &serde_json::to_string(&duplicate)?),
    )?;
    let replayed = replay_bundle(temp.path())?;
    let local = &replayed.conversation_items[&parent.response_item_ids[0]];
    let inherited = &replayed.conversation_items[&child.request_item_ids[0]];
    assert_eq!(
        (local.accepted_at_unix_ms, local.accepted_seq),
        (Some(first.wall_time_unix_ms), Some(first.seq))
    );
    assert_eq!(
        (inherited.accepted_at_unix_ms, inherited.accepted_seq),
        (None, None)
    );
    Ok(())
}

#[test]
fn legacy_bundles_cannot_claim_accepted_history() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let writer = create_started_writer(&temp)?;
    start_turn(&writer, "turn-1")?;
    append_completed_inference(
        &writer,
        "thread-root",
        "turn-1",
        "call",
        vec![message("user", "answer")],
        vec![message("assistant", "unproven")],
    )?;
    let manifest_path = temp.path().join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_reader(std::fs::File::open(&manifest_path)?)?;
    manifest["schema_version"] = json!(1);
    std::fs::write(manifest_path, serde_json::to_vec(&manifest)?)?;
    assert!(
        replay_bundle(temp.path())
            .unwrap_err()
            .to_string()
            .contains("cannot establish accepted history")
    );
    Ok(())
}
