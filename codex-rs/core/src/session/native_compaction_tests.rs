use super::make_session_and_context;
use super::user_message;
use crate::compact::CompactedHistoryMetadata;
use crate::compact::CompactionWindow;
use codex_history::ResponseItemEnvelope;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::BTreeSet;
use std::collections::HashSet;

use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn native_checkpoint_swap_retains_concurrent_tail_and_rejects_stale_prefix_atomically() {
    let (session, context) = Box::pin(make_session_and_context()).await;
    let session = Arc::new(session);
    let context = Arc::new(context);
    let step = session
        .capture_step_context(Arc::clone(&context), &CancellationToken::new())
        .await
        .unwrap();
    let world_state = session
        .build_world_state_for_step(&step, true)
        .await
        .unwrap();
    session
        .record_conversation_items(
            &context,
            context.model_info(),
            &[user_message("summarized prefix")],
        )
        .await;
    let source = session.clone_history().await.into_shared_annotated_items();
    session
        .record_conversation_items(
            &context,
            context.model_info(),
            &[user_message("concurrent tail")],
        )
        .await;
    let tail = session.clone_history().await.annotated_items()[source.len()..].to_vec();
    let checkpoint: ResponseItem = serde_json::from_value(json!({
        "type":"native_compaction", "id":"cmp_native", "provider":"anthropic",
        "content":"summary", "signature":"exact-signed+/block=",
    }))
    .unwrap();
    let metadata = || CompactedHistoryMetadata {
        input_goal_ids: HashSet::default(),
        source_history: Some(source.clone()),
        message: String::new(),
        window: CompactionWindow::Advance,
        compaction_response_id: Some("compact".into()),
        compaction_model_hash: None,
        reviewer_compaction_hash: None,
    };
    let (initial_context, baseline) =
        Box::pin(crate::compact::build_compaction_replacement_history(
            &session,
            &step,
            &world_state,
            Vec::new(),
            crate::context::CompactionContinuation::capture(source.iter().map(|item| &item.item))
                .unwrap(),
        ))
        .await;
    let continuation = initial_context.last().expect("captured continuation");
    let ResponseItem::Message { role, content, .. } = &continuation.item else {
        panic!("continuation message")
    };
    assert_eq!(role, "developer");
    let text = crate::content_items_to_text(content).unwrap();
    assert!(text.contains("summarized prefix"));
    assert!(
        !text.contains("concurrent tail"),
        "capture only the frozen compaction input"
    );
    // Retained evidence is data: guidance wording can change, but malformed
    // authenticated observations must fail without silently losing handling history.
    let mut persisted = continuation.item.clone();
    if let ResponseItem::Message { content, .. } = &mut persisted
        && let [ContentItem::InputText { text }] = content.as_mut_slice()
    {
        let guidance = text.lines().nth(1).unwrap().to_owned();
        *text = text.replacen(&guidance, "Reworded guidance is not record framing.", 1);
    } else {
        panic!("text continuation");
    }
    let recaptured = crate::context::CompactionContinuation::capture([&persisted])
        .unwrap()
        .into_item()
        .unwrap();
    let ResponseItem::Message { content, .. } = recaptured else {
        panic!("recaptured continuation");
    };
    assert_eq!(
        crate::content_items_to_text(&content).as_deref(),
        Some(text.as_str())
    );
    if let ResponseItem::Message { content, .. } = &mut persisted
        && let [ContentItem::InputText { text }] = content.as_mut_slice()
    {
        *text = text.replace("\"user_request\"", "\"invalid_observation\"");
    }
    assert!(crate::context::CompactionContinuation::capture([&persisted]).is_err());

    let replacement = std::iter::once(ResponseItemEnvelope::new(checkpoint.clone()))
        .chain(initial_context)
        .collect::<Vec<_>>();
    session
        .replace_compacted_history(
            replacement.clone(),
            context.to_turn_context_item(),
            baseline,
            /*rejected_audio_file_ids*/ BTreeSet::default(),
            metadata(),
        )
        .await
        .unwrap();
    let installed = session.clone_history().await;
    // Refreshed context receives fresh IDs; the concurrent tail keeps its exact identities.
    assert_eq!(
        installed.annotated_items().len(),
        replacement.len() + tail.len()
    );
    assert_eq!(
        &installed.annotated_items()[replacement.len()..],
        tail.as_slice()
    );
    let ResponseItem::Message { content, .. } =
        &installed.annotated_items()[replacement.len() - 1].item
    else {
        panic!("installed continuation message")
    };
    assert_eq!(
        crate::content_items_to_text(content).as_deref(),
        Some(text.as_str())
    );
    assert_eq!(
        installed.raw_items().next(),
        Some(&checkpoint),
        "signed checkpoint stays first"
    );
    let window_before = {
        let state = session.state.lock().await;
        (
            state.auto_compact_window_number(),
            state.auto_compact_window_ids().clone(),
        )
    };
    // Reusing the old captured prefix after a successful swap must not install
    // another checkpoint, advance the window, or discard its concurrent tail.
    assert!(
        session
            .replace_compacted_history(
                vec![ResponseItemEnvelope::new(checkpoint)],
                context.to_turn_context_item(),
                world_state.render_full().0,
                /*rejected_audio_file_ids*/ BTreeSet::default(),
                metadata()
            )
            .await
            .is_err()
    );
    assert_eq!(
        session.clone_history().await.annotated_items(),
        installed.annotated_items()
    );
    let state = session.state.lock().await;
    assert_eq!(
        (
            state.auto_compact_window_number(),
            state.auto_compact_window_ids()
        ),
        (window_before.0, &window_before.1)
    );
}
