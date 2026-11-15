#![cfg(test)]

//! Checks runtime release and notification delivery around listener shutdown.

use super::super::thread_goal_processor::ThreadGoalRequestProcessor;
use super::super::thread_processor::ThreadRequestProcessor;
use super::super::thread_processor::ThreadResumeTarget;
use super::*;
use crate::config_manager::ConfigManager;
use crate::outgoing_message::OutgoingEnvelope;
use crate::outgoing_message::OutgoingMessage;
use crate::thread_sample::SampleResponseRegistry;
use crate::thread_state::ConnectionCapabilities;
use crate::thread_state::ListenerBinding;
use crate::thread_state::ListenerInstallMode;
use crate::thread_state::ListenerStateCandidate;
use crate::thread_state::PendingNetworkApproval;
use crate::thread_state::resolve_server_request_on_thread_listener;
use anyhow::Context;
use anyhow::Result;
use codex_analytics::AnalyticsEventsClient;
use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadUnsubscribeParams;
use codex_app_server_protocol::ThreadUnsubscribeResponse;
use codex_app_server_protocol::ThreadUnsubscribeStatus;
use codex_arg0::Arg0DispatchPaths;
use codex_core::ThreadEvictionOutcome;
use codex_core::ThreadRegistration;
use codex_goal_extension::GoalService;
use codex_login::AuthManager;
use codex_thread_store::PersistContext;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use std::collections::HashSet;
use test_case::test_case;
use tokio::sync::Semaphore;
use tokio::sync::mpsc;
use tokio::time::timeout;

#[test_case(true; "listener attached before shutdown")]
#[test_case(false; "listener attached after shutdown")]
#[tokio::test]
async fn listener_shutdown_releases_runtime_and_preserves_other_subscribers(
    attach_before_shutdown: bool,
) -> Result<()> {
    let server = wiremock::MockServer::start().await;
    let test = test_codex().build_with_auto_env(&server).await?;
    let thread_id = test.session_configured.thread_id;
    test.thread_store
        .persist_thread(thread_id, PersistContext::Standard)
        .await?;
    let thread = test.codex;
    // A watch may coalesce Held back into Quiet before its observer runs. The
    // policy must retain the final release time instead of an observed bool edge.
    let holds = thread.runtime_holds();
    let mut activity = holds.subscribe();
    timeout(
        Duration::from_secs(10),
        activity.wait_for(|state| {
            matches!(
                state,
                codex_core::runtime_holds::RuntimeActivity::Quiet { .. }
            )
        }),
    )
    .await??;
    let (_subscribers_tx, detached) =
        watch::channel(crate::thread_state::SubscriberActivity::Detached {
            since: Instant::now(),
        });
    let (status_tx, status) = watch::channel(ThreadStatus::Idle);
    let mut policy = UnloadingState::new(Duration::from_millis(250), detached, activity, status);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let transient_use = holds.acquire().expect("open runtime");
    drop(transient_use); // No await: the observer cannot see the Held publication.
    assert!(!policy.should_unload_now());
    assert!(
        timeout(
            Duration::from_millis(5),
            policy.wait_for_unloading_trigger()
        )
        .await
        .is_err()
    );
    // Status-only wakeups must not erase a contention floor; a second
    // Contended result establishes a fresh floor without rewriting quiet-since.
    timeout(Duration::from_secs(5), policy.wait_for_unloading_trigger()).await?;
    let quiet = holds.activity();
    policy.defer_contention();
    status_tx.send_replace(ThreadStatus::NotLoaded);
    assert!(
        timeout(
            Duration::from_millis(200),
            policy.wait_for_unloading_trigger()
        )
        .await
        .is_err()
    );
    policy.defer_contention();
    status_tx.send_replace(ThreadStatus::Idle);
    assert!(
        timeout(
            Duration::from_millis(900),
            policy.wait_for_unloading_trigger()
        )
        .await
        .is_err()
    );
    assert_eq!(holds.activity(), quiet);
    timeout(Duration::from_secs(5), policy.wait_for_unloading_trigger()).await?;
    drop(policy);
    drop(holds);
    let runtime = Arc::downgrade(&thread);
    let config = Arc::new(test.config.clone());
    let auth =
        AuthManager::shared_from_config(config.as_ref(), /*enable_codex_api_key_env*/ false)
            .await?;
    let config_manager =
        ConfigManager::without_managed_config_for_tests(config.codex_home.to_path_buf());
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(/*buffer*/ 16);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        outgoing_tx,
        AnalyticsEventsClient::disabled(),
    ));
    let thread_state_manager = ThreadStateManager::new();
    let thread_watch_manager = ThreadWatchManager::new_with_outgoing(outgoing.clone());
    let skills_watcher = SkillsWatcher::new(
        test.thread_manager.skills_service(),
        &config.codex_home,
        outgoing.clone(),
    );
    let thread_goal_processor = ThreadGoalRequestProcessor::new(
        test.thread_manager.clone(),
        outgoing.clone(),
        config.clone(),
        thread_state_manager.clone(),
        /*state_db*/ None,
        Arc::new(GoalService::new()),
        config_manager.clone(),
    );
    let (prompt_cache, _prompt_cache_worker) =
        crate::prompt_cache::PromptCache::spawn(outgoing.clone());
    let (alarms, _alarm_worker) = crate::alarms::Alarms::spawn(
        None,
        None,
        Arc::new(std::sync::OnceLock::new()),
        outgoing.clone(),
    );
    let processor = ThreadRequestProcessor::new(
        auth,
        test.thread_manager.clone(),
        outgoing,
        Arg0DispatchPaths::default(),
        config,
        config_manager,
        test.thread_store.clone(),
        Default::default(),
        thread_state_manager.clone(),
        thread_watch_manager.clone(),
        Arc::new(Semaphore::new(/*permits*/ 1)),
        thread_goal_processor,
        /*state_db*/ None,
        /*log_db*/ None,
        skills_watcher.clone(),
        /*turn_cost_worker*/ None,
        Vec::new(),
        prompt_cache,
        alarms,
        Arc::new(SampleResponseRegistry::default()),
    );
    let connection_id = ConnectionId(1);
    let disconnecting_connection_id = ConnectionId(2);
    let remaining_connection_id = ConnectionId(3);
    let connection_ids = vec![
        connection_id,
        disconnecting_connection_id,
        remaining_connection_id,
    ];
    for &connection_id in &connection_ids {
        processor
            .connection_initialized(connection_id, ConnectionCapabilities::default())
            .await;
    }
    let network_request_id = RequestId::Integer(4);
    if attach_before_shutdown {
        // Pause after identity validation. Core capacity wins before the host
        // transaction can acquire viewer custody: installation must not run.
        let other = test_codex().build_with_auto_env(&server).await?;
        let other_id = other.session_configured.thread_id;
        let other_thread = Arc::clone(&other.codex);
        let mut quiet = other_thread.runtime_holds().subscribe();
        timeout(
            Duration::from_secs(10),
            quiet.wait_for(|state| {
                matches!(
                    state,
                    codex_core::runtime_holds::RuntimeActivity::Quiet { .. }
                )
            }),
        )
        .await??;
        let candidate = thread_state_manager.created_listener_state(other_id).await;
        let state_manager = thread_state_manager.clone();
        let manager = Arc::clone(&other.thread_manager);
        let (validated_tx, validated_rx) = oneshot::channel();
        let (install_tx, install_rx) = oneshot::channel();
        let installer = tokio::spawn(async move {
            assert!(matches!(
                manager.thread_registration(other_id, &other_thread).await,
                ThreadRegistration::Current
            ));
            validated_tx.send(()).expect("identity observer");
            install_rx.await.expect("install barrier");
            state_manager
                .subscribe_and_install_listener(
                    other_id,
                    &candidate,
                    &other_thread.runtime_holds(),
                    &[connection_id],
                    ListenerInstallMode::Live,
                    |_| -> Result<(), ()> {
                        panic!("Closing runtime installed an attached listener")
                    },
                )
                .await
        });
        validated_rx.await?;
        let eligibility = other
            .codex
            .idle_close_eligibility(Duration::ZERO)
            .expect("idle child before capacity seal");
        assert_eq!(
            other
                .thread_manager
                .try_idle_close(Arc::clone(&other.codex), eligibility)
                .await?,
            ThreadEvictionOutcome::Evicted
        );
        install_tx.send(()).expect("installation still waiting");
        assert!(matches!(
            installer.await?.expect("installation closure never ran"),
            ListenerBinding::Closing
        ));
        assert!(
            thread_state_manager
                .subscribed_connection_ids(other_id)
                .await
                .is_empty()
        );
        assert!(matches!(
            thread_state_manager.created_listener_state(other_id).await,
            ListenerStateCandidate::Unpublished(_)
        ));
    }
    // Converse ordering: install viewer custody before the same saved eligibility
    // can seal. A manually acquired core hold is not a host-subscription witness.
    let eligibility = thread
        .idle_close_eligibility(Duration::ZERO)
        .expect("idle runtime");
    let candidate = thread_state_manager.created_listener_state(thread_id).await;
    let installed = thread_state_manager
        .subscribe_and_install_listener(
            thread_id,
            &candidate,
            &thread.runtime_holds(),
            &connection_ids,
            ListenerInstallMode::Live,
            |_| Ok::<_, ()>(()),
        )
        .await
        .expect("viewer installation");
    assert!(matches!(
        installed,
        ListenerBinding::Installed { attached: true, .. }
    ));
    assert!(thread.idle_close_eligibility(Duration::ZERO).is_none());
    assert_eq!(
        test.thread_manager
            .try_idle_close(Arc::clone(&thread), eligibility)
            .await?,
        ThreadEvictionOutcome::Busy
    );
    if attach_before_shutdown {
        processor
            .try_attach_thread_listener(thread_id, Arc::clone(&thread), connection_ids.clone())
            .await;
        // Leave the prompt outstanding so shutdown runs before the proxy waiter resolves it.
        let permission_guard = thread_watch_manager
            .note_permission_requested(&thread_id.to_string())
            .await;
        thread_state_manager
            .thread_state(thread_id)
            .await
            .lock()
            .await
            .network_approvals
            .insert(
                "network-approval".to_string(),
                PendingNetworkApproval {
                    request_id: network_request_id.clone(),
                    permission_guard,
                },
            );
    }
    thread.shutdown_and_wait().await?;
    assert!(
        !thread_state_manager
            .try_add_connection_to_thread(thread_id, connection_id)
            .await
    );
    let shutdown_result = tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
        let resume = processor
            .thread_resume(
                ThreadResumeTarget::Client(ConnectionRequestId {
                    connection_id,
                    request_id: RequestId::Integer(1),
                }),
                ThreadResumeParams {
                    thread_id: thread_id.to_string(),
                    exclude_turns: true,
                    ..Default::default()
                },
                /*app_server_client_name*/ None,
                /*app_server_client_version*/ None,
                /*client_mcp_extensions*/ Default::default(),
            )
            .await;
        test.thread_manager.remove_thread(&thread_id).await;
        if !attach_before_shutdown {
            processor
                .try_attach_thread_listener(thread_id, Arc::clone(&thread), connection_ids)
                .await;
        }
        let mut network_resolved = false;
        loop {
            match outgoing_rx.recv().await.expect("outgoing channel closed") {
                OutgoingEnvelope::ToConnection {
                    connection_id: recipient,
                    message: OutgoingMessage::AppServerNotification(notification),
                    ..
                } if recipient == connection_id
                    && matches!(
                        &notification.notification,
                        ServerNotification::ServerRequestResolved(resolved)
                            if resolved.request_id == network_request_id
                    ) =>
                {
                    assert!(!network_resolved, "duplicate network prompt resolution");
                    network_resolved = true;
                }
                OutgoingEnvelope::Broadcast {
                    message: OutgoingMessage::AppServerNotification(notification),
                } if matches!(
                    &notification.notification,
                    ServerNotification::ThreadStatusChanged(status)
                        if status.status == ThreadStatus::NotLoaded
                ) =>
                {
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(network_resolved, attach_before_shutdown);
        drop(thread);
        while runtime.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
        let Some(ClientResponsePayload::ThreadUnsubscribe(unsubscribe)) = processor
            .thread_unsubscribe(
                &ConnectionRequestId {
                    connection_id,
                    request_id: RequestId::Integer(2),
                },
                ThreadUnsubscribeParams {
                    thread_id: thread_id.to_string(),
                },
            )
            .await
            .expect("unsubscribe failed")
        else {
            panic!("unsubscribe response missing");
        };
        assert_eq!(
            unsubscribe,
            ThreadUnsubscribeResponse {
                status: ThreadUnsubscribeStatus::NotLoaded,
            },
        );
        assert_eq!(
            thread_state_manager
                .subscribed_connection_ids(thread_id)
                .await
                .into_iter()
                .collect::<HashSet<_>>(),
            HashSet::from([disconnecting_connection_id, remaining_connection_id]),
        );
        processor
            .connection_closed(disconnecting_connection_id)
            .await;
        assert_eq!(
            thread_state_manager
                .subscribed_connection_ids(thread_id)
                .await,
            vec![remaining_connection_id],
        );
        resolve_server_request_on_thread_listener(
            &thread_state_manager.thread_state(thread_id).await,
            RequestId::Integer(3),
        )
        .await;
        loop {
            if let OutgoingEnvelope::ToConnection {
                connection_id: recipient,
                message: OutgoingMessage::AppServerNotification(notification),
                ..
            } = outgoing_rx.recv().await.expect("outgoing channel closed")
                && recipient == remaining_connection_id
                && matches!(
                    &notification.notification,
                    ServerNotification::ServerRequestResolved(resolved)
                        if resolved.request_id == RequestId::Integer(3)
                )
            {
                break;
            }
        }
        resume
    })
    .await;
    thread_state_manager.clear_all_listeners().await;
    skills_watcher.shutdown();

    let resume = shutdown_result.context("terminal listener did not release or relay")?;
    assert_eq!(
        resume.expect_err("resume accepted a terminated runtime"),
        invalid_request(format!(
            "thread {thread_id} is closing; retry thread/resume after the thread is closed"
        )),
    );
    Ok(())
}
