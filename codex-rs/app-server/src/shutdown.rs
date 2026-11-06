//! Explicit shutdown drains immediately; installation waits for idle without denying service.

use crate::turn_admission::Admissions;
use crate::turn_admission::TurnAdmission;
use std::io;
use tokio::sync::watch;
use tracing::info;

#[derive(Default)]
pub(crate) enum ShutdownState {
    #[default]
    Running,
    #[cfg(unix)]
    UpgradePending,
    Draining {
        last_logged_running_turn_count: Option<usize>,
    },
    Forced,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ShutdownAction {
    Noop,
    Finish,
}

#[derive(Clone, Copy)]
pub(crate) enum ShutdownSignal {
    Forceable,
    #[cfg(unix)]
    WhenIdle,
}

pub(crate) async fn shutdown_signal() -> io::Result<ShutdownSignal> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::SignalKind;
        use tokio::signal::unix::signal;

        let mut term = signal(SignalKind::terminate())?;
        let mut hangup = signal(SignalKind::hangup())?;
        tokio::select! {
            ctrl_c_result = tokio::signal::ctrl_c() => ctrl_c_result.map(|()| ShutdownSignal::Forceable),
            _ = term.recv() => Ok(ShutdownSignal::Forceable),
            _ = hangup.recv() => Ok(ShutdownSignal::WhenIdle),
        }
    }

    #[cfg(windows)]
    {
        if tokio::signal::ctrl_c().await.is_err() {
            // A detached daemon has no console; keep its local control path active.
            std::future::pending::<()>().await;
        }
        Ok(ShutdownSignal::Forceable)
    }
}

impl ShutdownState {
    pub(crate) fn requested(&self) -> bool {
        !matches!(self, Self::Running)
    }

    pub(crate) fn forced(&self) -> bool {
        matches!(self, Self::Forced)
    }

    pub(crate) fn on_signal(
        &mut self,
        signal: ShutdownSignal,
        connection_count: usize,
        running_turn_count: usize,
        turn_admission: &TurnAdmission,
    ) {
        match signal {
            #[cfg(unix)]
            ShutdownSignal::WhenIdle => {
                if matches!(self, Self::Running) {
                    *self = Self::UpgradePending;
                    info!(
                        "upgrade pending: waiting for idle without rejecting new work (connections={connection_count}, runningAssistantTurns={running_turn_count})"
                    );
                }
            }
            ShutdownSignal::Forceable => {
                if matches!(self, Self::Draining { .. } | Self::Forced) {
                    *self = Self::Forced;
                    return;
                }
                turn_admission.begin_drain();
                *self = Self::Draining {
                    last_logged_running_turn_count: None,
                };
                info!(
                    "received shutdown signal; entering graceful restart drain (connections={connection_count}, runningAssistantTurns={running_turn_count}, new client turns rejected)"
                );
            }
        }
    }

    pub(crate) fn update(
        &mut self,
        running_turns: &mut watch::Receiver<usize>,
        active_admissions: &mut watch::Receiver<Admissions>,
        connection_count: usize,
        turn_admission: &TurnAdmission,
    ) -> ShutdownAction {
        #[cfg(unix)]
        if matches!(self, Self::UpgradePending) && turn_admission.begin_idle_drain(running_turns) {
            *self = Self::Draining {
                last_logged_running_turn_count: None,
            };
        }
        #[cfg(not(unix))]
        let _ = turn_admission;

        // Read admissions first: once admission closes they only fall, and Core
        // releases an admitted turn's permit only as that turn ends.
        let active_admissions = *active_admissions.borrow_and_update();
        let running_turn_count = *running_turns.borrow_and_update();
        match self {
            Self::Running => ShutdownAction::Noop,
            #[cfg(unix)]
            Self::UpgradePending => ShutdownAction::Noop,
            Self::Forced => {
                info!(
                    "received second shutdown signal; forcing restart with {running_turn_count} running assistant turn(s) and {connection_count} connection(s)"
                );
                ShutdownAction::Finish
            }
            Self::Draining {
                last_logged_running_turn_count,
            } => {
                if running_turn_count == 0 && active_admissions.is_empty() {
                    info!(
                        "shutdown signal restart: no assistant turns running; stopping acceptor and disconnecting {connection_count} connection(s)"
                    );
                    return ShutdownAction::Finish;
                }
                if *last_logged_running_turn_count != Some(running_turn_count) {
                    info!(
                        "shutdown signal restart: waiting for {running_turn_count} running assistant turn(s), {} admitted turn(s) and {} admitted request(s) to finish",
                        active_admissions.turns, active_admissions.requests
                    );
                    *last_logged_running_turn_count = Some(running_turn_count);
                }
                ShutdownAction::Noop
            }
        }
    }
}

#[cfg(test)]
#[path = "shutdown_tests.rs"]
mod tests;
