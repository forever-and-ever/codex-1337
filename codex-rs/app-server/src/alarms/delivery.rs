use std::collections::HashMap;
use std::time::Duration;

use codex_app_server_protocol::alarm_sounding::AlarmCause;
use codex_core::TerminalWatch;
use codex_core::TerminalWatchEvent;
use codex_core::TurnInputSubmission;
use codex_protocol::turn_input::NotSubmittedReason;
use codex_state::AlarmTrigger;
use codex_state::ThreadAlarm;
use codex_state::ThreadAlarmState;
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::Alarms;

pub(super) enum Delivery {
    Done,
    Draining,
}

pub(super) async fn deliver(alarms: &Alarms, alarm: &ThreadAlarm) -> Result<Delivery, String> {
    let service = alarms
        .0
        .coordination
        .get()
        .and_then(std::sync::Weak::upgrade)
        .ok_or_else(|| "alarm host is unavailable".to_string())?;
    // Cold restoration takes the thread-list permit, which archive holds before mutation.
    let loaded = Box::pin(service.load(&alarm.thread_id.to_string())).await;
    let _mutation = alarms.0.mutate().await;
    let store = alarms.store()?;
    let saved = store
        .db
        .list_thread_alarms(store.owner, alarm.thread_id)
        .await
        .map_err(|err| err.to_string())?
        .into_iter()
        .find(|saved| saved.id == alarm.id);
    let Some(saved) = saved else {
        let mut registry = alarms.0.registry();
        registry.alarms.remove(&alarm.id);
        registry.watches.remove(&alarm.id);
        registry.refresh(alarm.thread_id, true);
        drop(registry);
        alarms.publish(alarm.thread_id, None);
        return Ok(Delivery::Done);
    };
    let ThreadAlarmState::Sounded { body } = saved.state else {
        unreachable!("only sounded alarms are delivered");
    };
    let (thread, _lease) = loaded.map_err(|error| error.message)?;
    match service.deliver_alarm(&thread, body).await? {
        TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::ServerDraining,
        } => return Ok(Delivery::Draining),
        TurnInputSubmission::NotSubmitted { reason } => tracing::warn!(
            alarm = alarm.id,
            ?reason,
            "alarm was not submitted; discarding"
        ),
        TurnInputSubmission::Accepted
        | TurnInputSubmission::Started { .. }
        | TurnInputSubmission::Steered { .. } => {}
    }
    store
        .db
        .delete_thread_alarm(store.owner, alarm.thread_id, alarm.id)
        .await
        .map_err(|err| err.to_string())?;
    let mut registry = alarms.0.registry();
    registry.alarms.remove(&alarm.id);
    registry.watches.remove(&alarm.id);
    registry.refresh(alarm.thread_id, true);
    drop(registry);
    alarms.publish(alarm.thread_id, None);
    Ok(Delivery::Done)
}

pub(super) async fn watch(
    watch: TerminalWatch,
    session_id: i32,
    deadline: Option<i64>,
) -> AlarmCause {
    let command = watch.command().to_string();
    let probe = watch.exit_probe();
    let terminal = watch.wait();
    tokio::pin!(terminal);
    loop {
        let delay = deadline.map(|at| {
            Duration::from_millis(at.saturating_sub(super::now()).max(0).unsigned_abs())
                .min(Duration::from_mins(1))
        });
        tokio::select! {
            biased;
            (observed_at, event) = &mut terminal => {
                if deadline.is_some_and(|at| observed_at > at)
                    && !probe.has_exited()
                    && !matches!(event, TerminalWatchEvent::Lost)
                {
                    return AlarmCause::Deadline { session_id, command };
                }
                return terminal_cause(event, session_id, command);
            }
            () = async {
                match delay {
                    Some(delay) => tokio::time::sleep(delay).await,
                    None => std::future::pending().await,
                }
            } => {
                if deadline.is_some_and(|at| super::now() >= at) {
                    if probe.has_exited() {
                        let (_, event) = (&mut terminal).await;
                        return terminal_cause(event, session_id, command);
                    }
                    return AlarmCause::Deadline { session_id, command };
                }
            }
        }
    }
}

fn terminal_cause(event: TerminalWatchEvent, session_id: i32, command: String) -> AlarmCause {
    match event {
        TerminalWatchEvent::Matched(line) => AlarmCause::Matched {
            session_id,
            command,
            line,
        },
        TerminalWatchEvent::Exited { code, tail } => AlarmCause::Exited {
            session_id,
            command,
            code,
            tail,
        },
        TerminalWatchEvent::Lost => AlarmCause::Lost {
            session_id,
            command,
        },
    }
}

enum Next {
    Sound(i64, AlarmCause),
    Deliver(ThreadAlarm),
    Wait(Option<Duration>),
}

enum Pending {
    Sound(AlarmCause),
    Deliver,
}

struct Retry {
    at: Instant,
    pending: Pending,
}

fn next(alarms: &Alarms, retries: &HashMap<i64, Retry>) -> Next {
    let now = super::now();
    let registry = alarms.0.registry();
    let mut wait: Option<Duration> = None;
    for alarm in registry.alarms.values() {
        if let Some(retry) = retries.get(&alarm.id) {
            let remaining = retry.at.saturating_duration_since(Instant::now());
            if !remaining.is_zero() {
                wait = Some(wait.map_or(remaining, |wait| wait.min(remaining)));
                continue;
            }
            if let Pending::Sound(cause) = &retry.pending {
                return Next::Sound(alarm.id, cause.clone());
            }
        }
        match &alarm.state {
            ThreadAlarmState::Sounded { .. } => return Next::Deliver(alarm.clone()),
            ThreadAlarmState::Armed(AlarmTrigger::Clock { at }) => {
                if *at <= now {
                    return Next::Sound(
                        alarm.id,
                        AlarmCause::Clock {
                            late_by_seconds: None,
                        },
                    );
                }
                let delay = Duration::from_millis(at.saturating_sub(now).unsigned_abs());
                wait = Some(wait.map_or(delay, |wait| wait.min(delay)));
            }
            // Live watch tasks arbitrate terminal events against their own deadlines.
            ThreadAlarmState::Armed(AlarmTrigger::Watch { .. }) => {}
        }
    }
    Next::Wait(wait.map(|delay| delay.min(Duration::from_mins(1))))
}

pub(super) async fn run(alarms: Alarms, mut sounds: mpsc::UnboundedReceiver<(i64, AlarmCause)>) {
    while let Err(error) = alarms.initialize().await {
        tracing::warn!(%error, "alarm recovery will retry");
        tokio::time::sleep(Duration::from_mins(1)).await;
    }
    let mut retries = HashMap::<i64, Retry>::new();
    let mut pending_sound = None;
    loop {
        {
            let registry = alarms.0.registry();
            retries.retain(|id, _| registry.alarms.contains_key(id));
        }
        // A buffered terminal observation always precedes a fresh clock scan.
        let work = match pending_sound.take().or_else(|| sounds.try_recv().ok()) {
            Some((id, cause)) => Next::Sound(id, cause),
            None => next(&alarms, &retries),
        };
        let (id, pending, result) = match work {
            Next::Sound(id, cause) => {
                let _mutation = alarms.0.mutate().await;
                let result = alarms.sound_locked(id, cause.clone()).await;
                (id, Pending::Sound(cause), result)
            }
            Next::Deliver(alarm) => {
                let result = match Box::pin(deliver(&alarms, &alarm)).await {
                    Ok(Delivery::Draining) => return,
                    Ok(Delivery::Done) => Ok(()),
                    Err(error) => Err(error),
                };
                (alarm.id, Pending::Deliver, result)
            }
            Next::Wait(delay) => {
                tokio::select! {
                    biased;
                    sound = sounds.recv() => pending_sound = sound,
                    () = alarms.0.changed.notified() => {},
                    () = async {
                        match delay {
                            Some(delay) => tokio::time::sleep(delay).await,
                            None => std::future::pending().await,
                        }
                    } => {},
                }
                continue;
            }
        };
        match result {
            Ok(()) => {
                retries.remove(&id);
            }
            Err(error) => {
                tracing::warn!(alarm = id, %error, "alarm scheduler will retry");
                retries.insert(
                    id,
                    Retry {
                        at: Instant::now() + Duration::from_mins(1),
                        pending,
                    },
                );
            }
        }
    }
}
