//! Goal box requests (codex-1337): reading and setting a root goal's timebox and quotabox.

use std::sync::Arc;

use codex_protocol::ThreadId;
use codex_protocol::goal_boxes::GoalTimebox;
use codex_protocol::goal_boxes::Quotabox;

use super::GoalService;
use super::GoalServiceError;
use crate::boxes::GoalBoxHost;
use crate::boxes::GoalBoxHostSlot;
use crate::boxes::GoalBoxesReport;
use crate::boxes::QuotaError;
use crate::boxes::gauge;
use crate::boxes::unix_now;
use crate::runtime::GoalRuntimeHandle;

/// Shortest timebox a user may set, in seconds.
const SHORTEST_TIMEBOX: i64 = 60;

impl GoalService {
    /// Installs the server's goal box host; only the first installation takes.
    pub fn install_goal_box_host(&self, host: Arc<dyn GoalBoxHost>) {
        self.box_host.install(host);
    }

    pub(crate) fn box_host_slot(&self) -> GoalBoxHostSlot {
        self.box_host.clone()
    }

    /// The boxes on a root thread's unfinished goal, with the quota a quotabox would watch.
    pub async fn read_goal_boxes(
        &self,
        state_db: &codex_state::StateRuntime,
        thread_id: ThreadId,
    ) -> Result<GoalBoxesReport, GoalServiceError> {
        let runtime = self.boxable_runtime(thread_id)?;
        let boxes = state_db
            .thread_goals()
            .goal_boxes(thread_id)
            .await
            .map_err(internal)?
            .ok_or_else(no_goal)?;
        Ok(GoalBoxesReport {
            boxes,
            gauge: runtime.read_quota().await.map(gauge),
        })
    }

    /// Sets the timebox on a root thread's unfinished goal to end at `deadline`, or clears it.
    pub async fn set_goal_timebox(
        &self,
        state_db: &codex_state::StateRuntime,
        thread_id: ThreadId,
        deadline: Option<i64>,
    ) -> Result<GoalBoxesReport, GoalServiceError> {
        let runtime = self.boxable_runtime(thread_id)?;
        let now = unix_now();
        let timebox = deadline.map(|deadline| GoalTimebox {
            set_at: now,
            deadline,
        });
        if timebox.is_some_and(|timebox| timebox.deadline - now < SHORTEST_TIMEBOX) {
            return Err(GoalServiceError::InvalidRequest(
                "A timebox must end at least a minute from now.".to_string(),
            ));
        }
        let boxes = {
            let _goal_state_permit = runtime
                .goal_state_permit()
                .await
                .map_err(GoalServiceError::Internal)?;
            state_db
                .thread_goals()
                .set_goal_timebox(thread_id, timebox)
                .await
                .map_err(internal)?
                .ok_or_else(no_goal)?
        };
        runtime.arm_boxes();
        Ok(GoalBoxesReport { boxes, gauge: None })
    }

    /// Sets the quotabox on a root thread's unfinished goal, or clears it. A quotabox must
    /// measure the quota its agent reads, and the quota must not already have reached it.
    pub async fn set_goal_quotabox(
        &self,
        state_db: &codex_state::StateRuntime,
        thread_id: ThreadId,
        quotabox: Option<Quotabox>,
    ) -> Result<GoalBoxesReport, GoalServiceError> {
        let runtime = self.boxable_runtime(thread_id)?;
        let reading = match quotabox {
            Some(quotabox) => {
                let reading = runtime.read_quota().await.ok_or_else(|| {
                    GoalServiceError::InvalidRequest(
                        "This server cannot read quota for a quotabox.".to_string(),
                    )
                })?;
                match reading {
                    Ok(reading) => match quotabox.reached_by(reading) {
                        Some(false) => {}
                        Some(true) => {
                            return Err(GoalServiceError::InvalidRequest(format!(
                                "Quota is already at {reading}, within a quotabox of {quotabox}."
                            )));
                        }
                        None => {
                            return Err(GoalServiceError::InvalidRequest(format!(
                                "This goal's agent has {reading}; give the quotabox the same unit."
                            )));
                        }
                    },
                    Err(QuotaError::Unsupported(message)) => {
                        return Err(GoalServiceError::InvalidRequest(message));
                    }
                    // The watch keeps reading; an unreadable quota does not block a quotabox.
                    Err(QuotaError::Unreadable(_)) => {}
                }
                Some(reading)
            }
            None => None,
        };
        let boxes = {
            let _goal_state_permit = runtime
                .goal_state_permit()
                .await
                .map_err(GoalServiceError::Internal)?;
            state_db
                .thread_goals()
                .set_goal_quotabox(thread_id, quotabox)
                .await
                .map_err(internal)?
                .ok_or_else(no_goal)?
        };
        runtime.arm_boxes();
        Ok(GoalBoxesReport {
            boxes,
            gauge: reading.map(gauge),
        })
    }

    fn boxable_runtime(
        &self,
        thread_id: ThreadId,
    ) -> Result<Arc<GoalRuntimeHandle>, GoalServiceError> {
        let runtime = self.runtime_for_thread(thread_id).ok_or_else(|| {
            GoalServiceError::InvalidRequest("Goal boxes need the thread loaded.".to_string())
        })?;
        if !runtime.boxes_allowed() {
            return Err(GoalServiceError::InvalidRequest(
                "Goal boxes apply only to a root agent's goal.".to_string(),
            ));
        }
        Ok(runtime)
    }
}

fn internal(err: impl std::fmt::Display) -> GoalServiceError {
    GoalServiceError::Internal(format!("failed to update goal boxes: {err}"))
}

fn no_goal() -> GoalServiceError {
    GoalServiceError::InvalidRequest("There is no unfinished goal to box.".to_string())
}
