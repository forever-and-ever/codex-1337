use codex_protocol::AgentPath;
use codex_protocol::ResponseItemId;
use codex_protocol::ThreadId;
use codex_protocol::models::BaseInstructions;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_protocol::protocol::ThreadMemoryMode;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;
use codex_rollout::ResponseItemEnvelope;
use codex_rollout::RolloutItem;
use codex_state::AgentMailState;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::super::LocalThreadStore;
use super::super::test_support::test_config;
use crate::AppendThreadItemsParams;
use crate::ArchiveThreadParams;
use crate::CreateThreadParams;
use crate::DeleteThreadParams;
use crate::ListTurnsParams;
use crate::LocalHistoryAccount;
use crate::RevertThreadParams;
use crate::SortDirection;
use crate::StoredTurnItemsView;
use crate::ThreadMetadataPatch;
use crate::ThreadPersistenceMetadata;
use crate::ThreadStore;
use crate::UpdateThreadMetadataParams;
use crate::validate_accepted_agent_mail;

#[tokio::test]
async fn revert_keeps_thread_id_and_hides_suffix_across_repeated_reverts() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let state_db = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("initialize state database");
    let store = LocalThreadStore::new(config, Some(state_db.clone()));
    let thread_id = ThreadId::new();
    create_paginated_thread(&store, thread_id).await;
    let sender = ThreadId::new();
    let mut mail = Vec::new();
    for content in [
        "retained canonical mail",
        "discarded canonical mail",
        "not yet admitted",
    ] {
        let mut communication = InterAgentCommunication::new(
            AgentPath::root().join("sender").expect("sender path"),
            AgentPath::root(),
            Vec::new(),
            content.to_string(),
            false,
        );
        let id = ResponseItemId::with_suffix("amsg_mail", uuid::Uuid::now_v7());
        let payload = serde_json::to_string(&communication).expect("serialize accepted mail");
        state_db
            .thread_queue()
            .accept_agent_mail(
                &id,
                thread_id,
                sender,
                thread_id,
                content,
                &"0".repeat(64),
                &payload,
            )
            .await
            .expect("accept mail before process-crash boundary");
        communication.id = Some(id);
        mail.push(communication);
    }
    // Canonical append completed, but the process died before the inbox acknowledgment.
    store
        .append_items(AppendThreadItemsParams {
            thread_id,
            items: vec![
                turn_started("turn-1"),
                RolloutItem::ResponseItem(ResponseItemEnvelope::new(mail[0].to_model_input_item())),
                turn_completed("turn-1"),
                turn_started("turn-2"),
                RolloutItem::ResponseItem(ResponseItemEnvelope::new(mail[1].to_model_input_item())),
                turn_completed("turn-2"),
            ],
        })
        .await
        .expect("append turns");
    let original_path = store
        .live_rollout_path(thread_id)
        .await
        .expect("source rollout path");
    // Live metadata updates append SessionMeta; they must not hide earlier admission witnesses.
    store
        .update_thread_metadata(UpdateThreadMetadataParams {
            thread_id,
            patch: ThreadMetadataPatch {
                memory_mode: Some(ThreadMemoryMode::Enabled),
                ..Default::default()
            },
            include_archived: false,
        })
        .await
        .expect("append metadata update after canonical mail");
    store
        .shutdown_thread(thread_id)
        .await
        .expect("close source writer");
    // A failed append was newline-terminated on reopen, then durably retried with the same
    // ordinal and a new timestamp. Reconciliation must prove the retry, never skip bad mail.
    let original = codex_rollout::read_rollout_text(&original_path).expect("read canonical source");
    let mut retried = String::new();
    for line in original.lines() {
        if line.contains(mail[0].id.as_ref().expect("mail identity").as_str()) {
            let (_, item) = line.split_once(",\"ordinal\":").expect("paginated record");
            let partial =
                format!("{{\"timestamp\":\"1970-01-01T00:00:00.000Z\",\"ordinal\":{item}");
            let partial = &partial[..partial.len() - 10];
            retried.push_str(partial);
            retried.push('\n');
            retried.push_str(partial);
            retried.push('\n');
        }
        retried.push_str(line);
        retried.push('\n');
    }
    let ids: Vec<_> = mail
        .iter()
        .map(|mail| mail.id.clone().expect("mail ID"))
        .collect();
    let conflicting = retried.replacen("retained canonical mail", "conflicting canonical mail", 1);
    std::fs::write(&original_path, &conflicting).expect("write conflicting partial attempt");
    assert!(
        store
            .find_accepted_agent_mail(thread_id, &ids)
            .await
            .is_err(),
        "unmatched malformed accepted mail must remain fail-closed"
    );
    std::fs::write(&original_path, &retried).expect("write exact partial retry");
    let recovered = store
        .find_accepted_agent_mail(thread_id, &ids)
        .await
        .expect("reconcile an exact adjacent retry without storage repair");
    assert_eq!(recovered.len(), 2);
    assert_eq!(
        std::fs::read_to_string(&original_path).expect("read preserved source"),
        retried
    );
    assert_eq!(
        state_db
            .thread_queue()
            .pending_agent_mail(thread_id)
            .await
            .expect("read untouched acceptance obligations")
            .len(),
        3
    );

    // The injected historical bytes precede projection in the real failure. Rebuild this
    // fixture's disposable projection so its saved byte cutoffs describe those same bytes.
    super::super::thread_history::delete_thread(&store, thread_id)
        .await
        .expect("drop fixture projection with old byte offsets");
    super::super::thread_history_materialization::materialize_to_sqlite(
        &store,
        thread_id,
        &original_path,
    )
    .await
    .expect("rebuild projection over partial retries");
    codex_rollout::state_db::reconcile_rollout(
        Some(state_db.as_ref()),
        original_path.as_path(),
        "test-provider",
        /*builder*/ None,
        &[],
        /*archived_only*/ Some(false),
        /*new_thread_memory_mode*/ None,
    )
    .await;
    compress_rollout(original_path.as_path());

    store
        .revert_thread(RevertThreadParams {
            thread_id,
            before_turn_id: "turn-2".to_string(),
            multi_agent_version: None,
        })
        .await
        .expect("revert before second turn");
    let first_replacement_path = state_db
        .get_thread(thread_id)
        .await
        .expect("read metadata")
        .expect("thread metadata")
        .rollout_path;
    assert_ne!(first_replacement_path, original_path);
    assert_ne!(
        codex_rollout::rollout_id_from_path(first_replacement_path.as_path()),
        Some(thread_id)
    );
    let replacement_meta = codex_rollout::read_session_meta_line(first_replacement_path.as_path())
        .await
        .expect("read replacement metadata")
        .meta;
    assert_eq!(replacement_meta.id, thread_id);
    assert_eq!(
        (
            replacement_meta.account_scoped,
            replacement_meta.creator_user_id.as_deref(),
            replacement_meta.creator_account_id.as_deref()
        ),
        (true, Some("creator-user"), Some("creator-account")),
    );
    assert_eq!(replacement_meta.memory_mode, None);
    assert_eq!(
        replacement_meta.runtime_workspace_roots,
        Some(vec![home.path().join("workspace")])
    );
    assert_eq!(turn_ids(&store, thread_id).await, vec!["turn-1"]);
    for id in &ids[..2] {
        assert_eq!(
            state_db
                .thread_queue()
                .agent_mail(thread_id, id)
                .await
                .expect("read admitted mail")
                .expect("accepted identity")
                .state,
            AgentMailState::Admitted
        );
    }
    let pending = state_db
        .thread_queue()
        .pending_agent_mail(thread_id)
        .await
        .expect("read remaining inbox");
    assert_eq!(
        pending.iter().map(|record| &record.id).collect::<Vec<_>>(),
        vec![&ids[2]]
    );
    let retained = store
        .find_accepted_agent_mail(thread_id, &ids)
        .await
        .expect("scan retained lineage across compressed source");
    assert_eq!(retained.len(), 1);
    validate_accepted_agent_mail(
        retained.get(&ids[0]).expect("retained canonical mail"),
        &mail[0],
    )
    .expect("canonical attribution and content");
    assert!(
        !retained.contains_key(&ids[1]),
        "discarded suffix is not canonical merely because its file survives"
    );

    // Simulate an older binary replacing the rollout without the creator fields it does not know.
    let rollout = codex_rollout::read_rollout_text(&first_replacement_path)
        .expect("read replacement rollout");
    let (meta_line, remaining_lines) = rollout.split_once('\n').expect("session metadata line");
    let mut meta_line: serde_json::Value = serde_json::from_str(meta_line).expect("parse metadata");
    let payload = meta_line["payload"]
        .as_object_mut()
        .expect("metadata payload");
    payload.remove("account_scoped");
    payload.remove("creator_user_id");
    payload.remove("creator_account_id");
    tokio::fs::write(
        &first_replacement_path,
        format!("{meta_line}\n{remaining_lines}"),
    )
    .await
    .expect("write legacy replacement rollout");
    // The older binary's plain file replaces the framed one.
    std::fs::remove_file(first_replacement_path.with_extension("jsonl.zst"))
        .expect("remove framed rollout");
    // The older binary's replacement is a new file, so no projection of it exists yet.
    super::super::thread_history::delete_thread(
        &store,
        codex_rollout::rollout_id_from_path(first_replacement_path.as_path())
            .expect("replacement rollout id"),
    )
    .await
    .expect("drop replacement projection");

    for (user, expected) in [("creator-user", true), ("other-user", false)] {
        let account = LocalHistoryAccount {
            user_id: Some(user.to_string()),
            account_id: Some("creator-account".to_string()),
        };
        assert_eq!(
            account
                .includes(
                    home.path(),
                    thread_id,
                    Some(&first_replacement_path),
                    &[thread_id],
                    /*original_paths*/ &mut None,
                )
                .await,
            expected
        );
    }

    store
        .revert_thread(RevertThreadParams {
            thread_id,
            before_turn_id: "turn-1".to_string(),
            multi_agent_version: None,
        })
        .await
        .expect("revert before first turn");
    assert_eq!(turn_ids(&store, thread_id).await, Vec::<String>::new());
    assert!(
        store
            .find_accepted_agent_mail(thread_id, &ids)
            .await
            .expect("scan empty reverted history")
            .is_empty()
    );
    let pending = state_db
        .thread_queue()
        .pending_agent_mail(thread_id)
        .await
        .expect("read inbox after repeated revert");
    assert_eq!(
        pending.iter().map(|record| &record.id).collect::<Vec<_>>(),
        vec![&ids[2]]
    );
    let stored = state_db
        .get_thread(thread_id)
        .await
        .expect("read metadata")
        .expect("thread metadata");
    let recovered = codex_rollout::read_session_meta_line(&stored.rollout_path)
        .await
        .expect("read recovered creator metadata")
        .meta;
    assert_eq!(
        (recovered.creator_user_id, recovered.creator_account_id),
        (
            Some("creator-user".to_string()),
            Some("creator-account".to_string())
        ),
    );

    store
        .archive_thread(ArchiveThreadParams { thread_id })
        .await
        .expect("archive reverted thread");
    assert!(
        rollout_paths_for_thread(home.path(), thread_id)
            .await
            .iter()
            .all(|path| path.starts_with(home.path().join("archived_sessions")))
    );
    store
        .unarchive_thread(ArchiveThreadParams { thread_id })
        .await
        .expect("unarchive reverted thread");
    let owned_rollout_paths = rollout_paths_for_thread(home.path(), thread_id).await;
    assert_eq!(owned_rollout_paths.len(), 3);
    assert!(
        owned_rollout_paths
            .iter()
            .all(|path| path.starts_with(home.path().join("sessions")))
    );

    store
        .delete_thread(DeleteThreadParams { thread_id })
        .await
        .expect("delete reverted thread");
    for rollout_path in owned_rollout_paths {
        assert!(!rollout_path.exists());
    }
}

#[tokio::test]
async fn revert_preserves_legacy_scope_and_resolved_multi_agent_version() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let state_db = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("initialize state database");
    let store = LocalThreadStore::new(config, Some(state_db.clone()));
    let other_account = LocalHistoryAccount {
        user_id: Some("other-user".to_string()),
        account_id: Some("other-account".to_string()),
    };
    let mut archived_threads = Vec::new();

    for version in [
        MultiAgentVersion::V1,
        MultiAgentVersion::V2,
        MultiAgentVersion::Disabled,
    ] {
        let thread_id = ThreadId::new();
        create_paginated_thread(&store, thread_id).await;
        store
            .append_items(AppendThreadItemsParams {
                thread_id,
                items: vec![
                    turn_started("turn-1"),
                    turn_completed("turn-1"),
                    turn_started("turn-2"),
                    turn_completed("turn-2"),
                ],
            })
            .await
            .expect("append turns");
        let path = store
            .live_rollout_path(thread_id)
            .await
            .expect("source rollout path");
        store
            .shutdown_thread(thread_id)
            .await
            .expect("close source writer");
        let contents = codex_rollout::test_support::read_rollout(&path).expect("read rollout");
        // Preserve indexed offsets while turning the fixture into pre-enforcement history.
        let marker = "\"account_scoped\":true,";
        codex_rollout::test_support::rewrite_rollout(
            &path,
            &contents.replacen(marker, &" ".repeat(marker.len()), 1),
        )
        .expect("write legacy rollout");
        codex_rollout::state_db::reconcile_rollout(
            Some(state_db.as_ref()),
            path.as_path(),
            "test-provider",
            /*builder*/ None,
            &[],
            /*archived_only*/ Some(false),
            /*new_thread_memory_mode*/ None,
        )
        .await;

        // The second revert has no live version to supply and must keep the persisted one.
        for (before_turn_id, multi_agent_version) in [("turn-2", Some(version)), ("turn-1", None)] {
            store
                .revert_thread(RevertThreadParams {
                    thread_id,
                    before_turn_id: before_turn_id.to_string(),
                    multi_agent_version,
                })
                .await
                .expect("revert thread");
            let path = state_db
                .get_thread(thread_id)
                .await
                .expect("read metadata")
                .expect("thread metadata")
                .rollout_path;
            let meta = codex_rollout::read_session_meta_line(&path)
                .await
                .expect("read replacement metadata");
            assert_eq!(
                (meta.meta.multi_agent_version, meta.meta.account_scoped),
                (Some(version), false)
            );
            assert!(
                other_account
                    .includes(
                        home.path(),
                        thread_id,
                        Some(&path),
                        &[thread_id],
                        /*original_paths*/ &mut None,
                    )
                    .await
            );
        }
        store
            .archive_thread(ArchiveThreadParams { thread_id })
            .await
            .expect("archive reverted legacy thread");
        let path = state_db
            .get_thread(thread_id)
            .await
            .expect("read archived metadata")
            .expect("archived thread")
            .rollout_path;
        archived_threads.push((thread_id, path));
    }
    let page_thread_ids: Vec<_> = archived_threads.iter().map(|(id, _)| *id).collect();
    let mut original_paths = None;
    for (thread_id, path) in archived_threads {
        assert!(
            other_account
                .includes(
                    home.path(),
                    thread_id,
                    Some(&path),
                    &page_thread_ids,
                    &mut original_paths,
                )
                .await
        );
    }
}

async fn rollout_paths_for_thread(
    home: &std::path::Path,
    thread_id: ThreadId,
) -> Vec<std::path::PathBuf> {
    codex_rollout::RolloutReferenceIndex::scan(home)
        .await
        .expect("scan rollout references")
        .rollouts_for_thread(thread_id)
        .map(|(_, path)| path.to_path_buf())
        .collect()
}

async fn create_paginated_thread(store: &LocalThreadStore, thread_id: ThreadId) {
    store
        .create_thread(CreateThreadParams {
            attached_thread: false,
            creator_user_id: Some("creator-user".to_string()),
            creator_account_id: Some("creator-account".to_string()),
            session_id: thread_id.into(),
            thread_id,
            extra_config: None,
            forked_from_id: None,
            parent_thread_id: None,
            source: SessionSource::Exec,
            thread_source: None,
            originator: "test_originator".to_string(),
            base_instructions: BaseInstructions::default(),
            dynamic_tools: Vec::new(),
            selected_capability_roots: Vec::new(),
            multi_agent_version: None,
            history_mode: ThreadHistoryMode::Paginated,
            history_base: None,
            subagent_history_start_ordinal: None,
            initial_window_id: codex_protocol::ContextWindowId::new_uuid_v7(),
            runtime_workspace_roots: Some(vec![store.config.codex_home.join("workspace").abs()]),
            metadata: ThreadPersistenceMetadata {
                cwd: Some(std::env::current_dir().expect("cwd")),
                model_provider: "test-provider".to_string(),
                memory_mode: ThreadMemoryMode::Enabled,
            },
        })
        .await
        .expect("create paginated thread");
}

async fn turn_ids(store: &LocalThreadStore, thread_id: ThreadId) -> Vec<String> {
    store
        .list_turns(ListTurnsParams {
            thread_id,
            include_archived: false,
            cursor: None,
            page_size: 10,
            sort_direction: SortDirection::Asc,
            items_view: StoredTurnItemsView::NotLoaded,
        })
        .await
        .expect("list turns")
        .turns
        .into_iter()
        .map(|turn| turn.turn_id)
        .collect()
}

fn turn_started(turn_id: &str) -> RolloutItem {
    RolloutItem::EventMsg(EventMsg::TurnStarted(TurnStartedEvent {
        turn_attribution: None,
        turn_id: turn_id.to_string(),
        root_turn_id: None,
        trace_id: None,
        started_at: Some(10),
        model_context_window: None,
        collaboration_mode_kind: Default::default(),
    }))
}

fn turn_completed(turn_id: &str) -> RolloutItem {
    RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
        root_turn_id: None,
        turn_id: turn_id.to_string(),
        last_agent_message: None,
        error: None,
        started_at: Some(10),
        completed_at: Some(20),
        duration_ms: Some(10_000),
        time_to_first_token_ms: None,
    }))
}

/// Re-encodes a rollout as upstream's single zstd frame, the encoding `codex rollouts compress`
/// converts, and returns its path.
fn compress_rollout(path: &std::path::Path) -> std::path::PathBuf {
    let contents = codex_rollout::read_rollout_text(path).expect("read rollout");
    let compressed =
        zstd::stream::encode_all(contents.as_bytes(), /*level*/ 3).expect("compress rollout");
    let plain_path = codex_rollout::plain_rollout_path(path);
    let compressed_path = plain_path.with_extension("jsonl.zst");
    std::fs::write(&compressed_path, compressed).expect("write compressed rollout");
    if plain_path.exists() {
        std::fs::remove_file(plain_path).expect("remove plain rollout");
    }
    compressed_path
}
