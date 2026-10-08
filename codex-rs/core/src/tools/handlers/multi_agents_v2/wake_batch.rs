//! Delay a mailbox wake while multiple direct children can still contribute.
//! Watches are local to the handler future: cancellation leaves mail and agent state intact.

use super::WaitOutcome;
use crate::agent::AgentStatus;
use crate::session::InputQueueActivity;
use crate::session::session::Session;
use codex_protocol::error::Result as CodexResult;
use futures::StreamExt;
use futures::stream;
use futures::stream::BoxStream;
use futures::stream::SelectAll;
use std::num::NonZeroU64;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio::time::sleep_until;

enum EndReason {
    WindowExpired,
    WaitDeadline,
    Steered,
    ChildrenQuiet,
    ObservationFailed,
}

pub(super) async fn after_activity(
    outcome: WaitOutcome,
    session: &Session,
    window_ms: Option<NonZeroU64>,
    activity_rx: &mut watch::Receiver<InputQueueActivity>,
    wait_deadline: Instant,
) -> WaitOutcome {
    if outcome != WaitOutcome::MailboxActivity {
        return outcome;
    }
    let Some(window_ms) = window_ms else {
        return outcome;
    };
    let window_deadline = Instant::now() + Duration::from_millis(window_ms.get());
    match hold(session, activity_rx, window_deadline, wait_deadline).await {
        EndReason::Steered => WaitOutcome::Steered,
        EndReason::WindowExpired
        | EndReason::WaitDeadline
        | EndReason::ChildrenQuiet
        | EndReason::ObservationFailed => WaitOutcome::MailboxActivity,
    }
}

async fn hold(
    session: &Session,
    activity_rx: &mut watch::Receiver<InputQueueActivity>,
    window_deadline: Instant,
    wait_deadline: Instant,
) -> EndReason {
    loop {
        // Race setup too: registry/settings locks must not defer a steer or either deadline.
        let mut children = tokio::select! {
            activity = activity_rx.changed() => {
                match activity {
                    Ok(()) => match *activity_rx.borrow_and_update() {
                        InputQueueActivity::Steer => return EndReason::Steered,
                        InputQueueActivity::Mailbox => continue,
                    },
                    Err(_) => return EndReason::ObservationFailed,
                }
            }
            _ = sleep_until(wait_deadline) => return EndReason::WaitDeadline,
            _ = sleep_until(window_deadline) => return EndReason::WindowExpired,
            children = running_children(session) => match children {
                Ok(children) => children,
                Err(error) => {
                    tracing::warn!(%error, "could not observe children for wait_agent batching");
                    return EndReason::ObservationFailed;
                }
            },
        };
        if children.len() < 2 {
            return EndReason::ChildrenQuiet;
        }
        tokio::select! {
            activity = activity_rx.changed() => match activity {
                Ok(()) => match *activity_rx.borrow_and_update() {
                    InputQueueActivity::Steer => return EndReason::Steered,
                    InputQueueActivity::Mailbox => {},
                },
                Err(_) => return EndReason::ObservationFailed,
            },
            _ = sleep_until(wait_deadline) => return EndReason::WaitDeadline,
            _ = sleep_until(window_deadline) => return EndReason::WindowExpired,
            change = children.next() => {
                if let Some(Err(error)) = change {
                    tracing::warn!(%error, "child status observation failed during wait_agent batching");
                    return EndReason::ObservationFailed;
                }
            }
        }
    }
}

async fn running_children(
    session: &Session,
) -> CodexResult<SelectAll<BoxStream<'static, CodexResult<()>>>> {
    let runtime = &session.services.local_agent_runtime;
    let mut subscriptions = SelectAll::new();
    // An absent local manager, including the default test runtime, has no running children.
    if runtime.upgrade().is_err() {
        return Ok(subscriptions);
    }
    let control = runtime.control(session.session_id());
    for (id, _) in runtime
        .open_thread_spawn_children(session.thread_id)
        .await?
    {
        if !matches!(
            control.get_status(id).await,
            AgentStatus::PendingInit | AgentStatus::Running
        ) {
            continue;
        }
        let mut status = control.subscribe_status(id).await?;
        // The initial snapshot was captured after subscribing, closing the status-read race.
        let Some(initial) = status.next().await else {
            continue;
        };
        if matches!(
            initial?.status(),
            Some(AgentStatus::PendingInit | AgentStatus::Running)
        ) {
            subscriptions.push(
                status
                    .map(|snapshot| snapshot.map(|_| ()))
                    // SelectAll otherwise silently removes closed streams. Closure must
                    // also cause the running count to be re-evaluated.
                    .chain(stream::once(async { Ok(()) }))
                    .boxed(),
            );
        }
    }
    Ok(subscriptions)
}
