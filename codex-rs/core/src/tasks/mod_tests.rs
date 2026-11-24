use super::SessionTask;
use super::SessionTaskResult;
use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::tests::make_session_and_context_with_rx;
use crate::session::turn_context::TurnContext;
use crate::state::TaskKind;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

struct UncooperativeTimedTask {
    entered: Arc<tokio::sync::Notify>,
    finish: Arc<tokio::sync::Notify>,
}

impl SessionTask for UncooperativeTimedTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }
    fn span_name(&self) -> &'static str {
        "session_task.uncooperative_timing"
    }
    async fn run(
        self: Arc<Self>,
        _session: Arc<Session>,
        context: Arc<TurnContext>,
        _input: Vec<TurnInput>,
        _cancellation_token: CancellationToken,
    ) -> SessionTaskResult {
        let timing = &context.turn_timing_state;
        timing.enable_critical_path();
        let batch = codex_direct_tool_timing::ToolBatchTiming::default();
        let now = std::time::Instant::now();
        // Drop the call timing before ending its encompassing blocking interval.
        let _blocking;
        let _work = timing.direct_tool_timings.start_turn_direct_call(
            "uncooperative",
            &context.sub_id,
            batch.clone(),
            now,
            timing.snapshot_path(now),
        );
        _blocking = timing.begin_tool_blocking(Some(batch));
        let _review = _work.start_review();
        self.entered.notify_one();
        self.finish.notified().await;
        Ok(None)
    }
}

#[test_case::test_case(false; "forced teardown")]
#[test_case::test_case(true; "finished before wait")]
#[tokio::test(flavor = "current_thread")]
async fn abort_drops_timing_guards_before_profile_completion(finished_before_wait: bool) {
    use codex_analytics::IncompleteReason::ForcedTeardown;
    use codex_analytics::TurnCriticalPath;
    use codex_protocol::protocol::TurnAbortReason;

    let (session, context, _receiver) = make_session_and_context_with_rx().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let finish = Arc::new(tokio::sync::Notify::new());
    session
        .spawn_task(
            Arc::clone(&context),
            Vec::new(),
            UncooperativeTimedTask {
                entered: Arc::clone(&entered),
                finish: Arc::clone(&finish),
            },
        )
        .await
        .expect("register run");
    entered.notified().await;
    let turn = session
        .active_turn
        .lock()
        .await
        .take()
        .expect("active turn");
    if finished_before_wait {
        // The task exits after the abort claims the turn but before it waits.
        let task = turn.task.as_ref().expect("running task");
        finish.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !task.handle.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached task terminates normally");
    }
    session
        .abort_taken_tasks(Some(turn), TurnAbortReason::Interrupted)
        .await;

    let timing = &context.turn_timing_state;
    assert!(
        timing
            .direct_tool_timings
            .owner_for_turn("uncooperative", &context.sub_id)
            .is_none()
    );
    let (_, _, profile) = timing.complete_profile_and_duration_ms().await;
    if finished_before_wait {
        assert!(matches!(
            profile.critical_path,
            TurnCriticalPath::Complete(_)
        ));
    } else {
        assert_eq!(
            profile.critical_path,
            TurnCriticalPath::Incomplete(vec![ForcedTeardown])
        );
    }
}
