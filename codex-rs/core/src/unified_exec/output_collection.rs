//! Interjection ends an observation, never a draining future or its terminal.

use std::sync::atomic::Ordering;

use tokio::sync::watch;
use tokio::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::process::OutputHandles;
use codex_utils_head_tail_buffer::HeadTailBuffer;

#[tracing::instrument(
    name = "unified_exec.collect_output",
    level = "info",
    skip_all,
    fields(
        outcome = tracing::field::Empty,
        stop_reason = tracing::field::Empty,
        exit_signaled = tracing::field::Empty,
        output_closed = tracing::field::Empty,
    )
)]
pub(super) async fn collect<const MAX_BYTES: usize>(
    output: &OutputHandles<MAX_BYTES>,
    mut pause_state: Option<watch::Receiver<bool>>,
    mut deadline: Instant,
    interjection: Option<&CancellationToken>,
) -> HeadTailBuffer<MAX_BYTES> {
    const POST_EXIT_CLOSE_WAIT_CAP: Duration = Duration::from_millis(50);

    enum StopReason {
        OutputClosed,
        Deadline,
        PostExitDeadline,
        Interjection,
    }

    let OutputHandles {
        output_buffer,
        output_notify,
        output_closed,
        output_closed_notify,
        cancellation_token,
        ..
    } = output;
    let mut collected = HeadTailBuffer::default();
    let mut exit_signal_received = cancellation_token.is_cancelled();
    let mut post_exit_deadline: Option<Instant> = None;
    let stop_reason = loop {
        tokio::select! {
            biased;
            () = wait_for_interjection(interjection) => {}
            () = extend_deadlines_while_paused(
                &mut pause_state,
                &mut deadline,
                &mut post_exit_deadline,
            ) => {}
        }
        let drained_output: HeadTailBuffer<MAX_BYTES>;
        let has_drained_output: bool;
        let mut wait_for_output = None;
        {
            let mut guard = output_buffer.lock().await;
            drained_output = std::mem::take(&mut guard.pending);
            has_drained_output =
                drained_output.retained_bytes() > 0 || drained_output.omitted_bytes() > 0;
            if !has_drained_output {
                wait_for_output = Some(output_notify.notified());
            }
        }

        // Return both accumulated and pending bytes. Cancelling the collector itself
        // would discard output it has already drained from the shared buffer.
        if interjection.is_some_and(CancellationToken::is_cancelled) {
            collected.push_buffer(drained_output);
            break StopReason::Interjection;
        }
        if !has_drained_output {
            exit_signal_received |= cancellation_token.is_cancelled();
            if exit_signal_received && output_closed.load(Ordering::Acquire) {
                break StopReason::OutputClosed;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining == Duration::ZERO {
                break StopReason::Deadline;
            }

            if exit_signal_received {
                let now = Instant::now();
                let close_wait_deadline = *post_exit_deadline
                    .get_or_insert_with(|| now + remaining.min(POST_EXIT_CLOSE_WAIT_CAP));
                let close_wait_remaining = close_wait_deadline.saturating_duration_since(now);
                if close_wait_remaining == Duration::ZERO {
                    break StopReason::PostExitDeadline;
                }
                let notified = wait_for_output.unwrap_or_else(|| output_notify.notified());
                let closed = output_closed_notify.notified();
                tokio::pin!(notified);
                tokio::pin!(closed);
                tokio::select! {
                    () = &mut notified => {}
                    () = &mut closed => {}
                    () = wait_for_interjection(interjection) => {}
                    () = tokio::time::sleep(close_wait_remaining) => break StopReason::PostExitDeadline,
                    () = wait_for_pause_change(pause_state.as_ref()) => {}
                }
                continue;
            }

            let notified = wait_for_output.unwrap_or_else(|| output_notify.notified());
            tokio::pin!(notified);
            let exit_notified = cancellation_token.cancelled();
            tokio::pin!(exit_notified);
            tokio::select! {
                () = &mut notified => {}
                () = &mut exit_notified => exit_signal_received = true,
                () = wait_for_interjection(interjection) => {}
                () = tokio::time::sleep(remaining) => break StopReason::Deadline,
                () = wait_for_pause_change(pause_state.as_ref()) => {}
            }
            continue;
        }

        collected.push_buffer(drained_output);
        exit_signal_received |= cancellation_token.is_cancelled();
        if Instant::now() >= deadline {
            break StopReason::Deadline;
        }
    };

    let span = tracing::Span::current();
    span.record(
        "stop_reason",
        match stop_reason {
            StopReason::OutputClosed => "output_closed",
            StopReason::Deadline => "deadline",
            StopReason::PostExitDeadline => "post_exit_deadline",
            StopReason::Interjection => "interjection",
        },
    );
    span.record(
        "exit_signaled",
        exit_signal_received || cancellation_token.is_cancelled(),
    );
    span.record("output_closed", output_closed.load(Ordering::Acquire));
    span.record("outcome", "completed");
    collected
}

async fn wait_for_interjection(signal: Option<&CancellationToken>) {
    if let Some(signal) = signal {
        signal.cancelled().await;
    } else {
        std::future::pending::<()>().await;
    }
}

async fn extend_deadlines_while_paused(
    pause_state: &mut Option<watch::Receiver<bool>>,
    deadline: &mut Instant,
    post_exit_deadline: &mut Option<Instant>,
) {
    let Some(receiver) = pause_state.as_mut() else {
        return;
    };
    if !*receiver.borrow() {
        return;
    }
    let paused_at = Instant::now();
    while *receiver.borrow() {
        if receiver.changed().await.is_err() {
            break;
        }
    }
    let paused_for = paused_at.elapsed();
    *deadline += paused_for;
    if let Some(post_exit_deadline) = post_exit_deadline.as_mut() {
        *post_exit_deadline += paused_for;
    }
}

async fn wait_for_pause_change(pause_state: Option<&watch::Receiver<bool>>) {
    if let Some(pause_state) = pause_state {
        let mut receiver = pause_state.clone();
        let _ = receiver.changed().await;
    } else {
        std::future::pending::<()>().await;
    }
}
