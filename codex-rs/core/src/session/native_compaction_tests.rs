use super::make_session_and_context;
use super::user_message;
use crate::compact::CompactedHistoryMetadata;
use crate::compact::CompactionWindow;
use codex_history::ResponseItemEnvelope;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;
use serde_json::json;

#[tokio::test]
async fn native_checkpoint_swap_retains_concurrent_tail_and_rejects_stale_prefix_atomically() {
    let (session, context) = make_session_and_context().await;
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
        source_history: Some(source.clone()),
        message: String::new(),
        window: CompactionWindow::Advance,
        compaction_response_id: Some("compact".into()),
        compaction_model_hash: None,
        reviewer_compaction_hash: None,
    };
    session
        .replace_compacted_history(
            vec![ResponseItemEnvelope::new(checkpoint.clone())],
            /*reference_context_item*/ None,
            /*world_state_baseline*/ None,
            metadata(),
        )
        .await
        .unwrap();
    let installed = session.clone_history().await;
    assert_eq!(
        installed.raw_items().cloned().collect::<Vec<_>>(),
        std::iter::once(checkpoint.clone())
            .chain(tail.iter().map(|item| item.item.clone()))
            .collect::<Vec<_>>()
    );
    let window_before = {
        let state = session.state.lock().await;
        (
            state.auto_compact_window_number(),
            state.auto_compact_window_ids(),
        )
    };
    // Reusing the old captured prefix after a successful swap must not install
    // another checkpoint, advance the window, or discard its concurrent tail.
    assert!(
        session
            .replace_compacted_history(
                vec![ResponseItemEnvelope::new(checkpoint)],
                /*reference_context_item*/ None,
                /*world_state_baseline*/ None,
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
        window_before
    );
}
