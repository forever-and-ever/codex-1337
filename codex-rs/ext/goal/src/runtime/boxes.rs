//! Goal box enforcement for one thread (codex-1337): a watch that reminds and halts.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::Weak;
use std::time::Duration;

use codex_protocol::goal_boxes::GoalBoxes;
use codex_protocol::goal_boxes::QuotaReading;
use tokio::task::AbortHandle;

use super::GoalRuntimeHandle;
use super::GoalRuntimeInner;
use crate::analytics::GoalEventAttribution;
use crate::boxes::GoalBoxHost;
use crate::boxes::GoalBoxHostSlot;
use crate::boxes::Halt;
use crate::boxes::QuotaError;
use crate::boxes::halt_item;
use crate::boxes::reminder_instants;
use crate::boxes::timebox_reminder_item;
use crate::boxes::unix_now;
use crate::tool::protocol_goal_from_state;

/// How often a quotabox reads its quota, in seconds; the server's cache refreshes once a minute.
const QUOTA_POLL: i64 = 120;
/// Longest sleep between looks at the wall clock, which jumps across suspends, in seconds.
const CLOCK_GLANCE: i64 = 60;

/// One thread's goal box watch: whether its goal may hold boxes, the host, the latest quota
/// reading, and the task that reminds and halts.
pub(crate) struct BoxWatch {
    host: GoalBoxHostSlot,
    root: bool,
    reading: Mutex<Option<QuotaReading>>,
    task: Mutex<Option<AbortHandle>>,
}

impl BoxWatch {
    pub(crate) fn new(host: GoalBoxHostSlot, root: bool) -> Self {
        Self {
            host,
            root,
            reading: Mutex::new(None),
            task: Mutex::new(None),
        }
    }
}

impl Drop for BoxWatch {
    fn drop(&mut self) {
        if let Some(task) = self
            .task
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
    }
}

impl GoalRuntimeHandle {
    /// Whether this thread's goal may hold boxes: only a root agent's may.
    pub(crate) fn boxes_allowed(&self) -> bool {
        self.inner.box_watch.root
    }

    pub(crate) fn box_host(&self) -> Option<Arc<dyn GoalBoxHost>> {
        self.inner.box_watch.host.host()
    }

    /// Restarts the watch over this thread's goal boxes. It ends by itself once the goal is not
    /// active or has no boxes.
    pub(crate) fn arm_boxes(&self) {
        if !self.boxes_allowed() {
            return;
        }
        let task = tokio::spawn(watch(Arc::downgrade(&self.inner))).abort_handle();
        if let Some(previous) = self
            .inner
            .box_watch
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(task)
        {
            previous.abort();
        }
    }

    pub(crate) fn disarm_boxes(&self) {
        if let Some(task) = self
            .inner
            .box_watch
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
    }

    /// Reads the quota a quotabox watches and keeps the reading; `None` without a host or for a
    /// subagent.
    pub(crate) async fn read_quota(&self) -> Option<Result<QuotaReading, QuotaError>> {
        if !self.boxes_allowed() {
            return None;
        }
        let result = self.box_host()?.quota(self.thread_id()).await;
        if let Ok(reading) = result {
            *self
                .inner
                .box_watch
                .reading
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(reading);
        }
        Some(result)
    }

    /// Refreshes the quota reading when the goal has a quotabox, so judging it is current.
    pub(crate) async fn refresh_quota_if_boxed(&self) {
        let boxed = self
            .inner
            .state_dbs
            .thread_goals()
            .goal_boxes(self.thread_id())
            .await
            .is_ok_and(|boxes| boxes.is_some_and(|boxes| boxes.quotabox.is_some()));
        if boxed {
            self.read_quota().await;
        }
    }

    fn latest_reading(&self) -> Option<QuotaReading> {
        *self
            .inner
            .box_watch
            .reading
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The halt this thread's goal boxes call for now, judged by the latest quota reading.
    pub(crate) async fn due_halt(&self) -> Result<Option<Halt>, String> {
        let boxes = self
            .inner
            .state_dbs
            .thread_goals()
            .goal_boxes(self.thread_id())
            .await
            .map_err(|err| err.to_string())?;
        Ok(boxes.and_then(|boxes| Halt::due(boxes, unix_now(), self.latest_reading())))
    }

    /// Pauses the active goal when one of its boxes calls for a halt, and asks the agent to stop
    /// work. The caller holds the goal permit. Returns whether the goal halted.
    pub(crate) async fn halt_if_due_locked(&self) -> Result<bool, String> {
        let Some(halt) = self.due_halt().await? else {
            return Ok(false);
        };
        if let Err(err) = self.prepare_external_goal_mutation().await {
            tracing::warn!("failed to account goal progress before a halt: {err}");
        }
        let Some(goal) = self
            .inner
            .state_dbs
            .thread_goals()
            .pause_active_thread_goal(self.thread_id())
            .await
            .map_err(|err| err.to_string())?
        else {
            return Ok(false);
        };
        let turn_id = self.inner.accounting_state.current_turn_id();
        self.inner.analytics.status_changed(
            &goal,
            Some(codex_state::ThreadGoalStatus::Active),
            turn_id
                .as_deref()
                .map_or(GoalEventAttribution::NoTurn, GoalEventAttribution::Turn),
        );
        self.inner.accounting_state.clear_active_goal();
        let goal = protocol_goal_from_state(goal);
        self.inner.event_emitter.thread_goal_updated(
            format!("{}:goal-halt", turn_id.as_deref().unwrap_or("idle")),
            turn_id,
            goal.clone(),
        );
        if let Some(item) = halt_item(&goal, halt) {
            self.inject_active_turn_steering(item).await;
        }
        if let Some(host) = self.box_host() {
            host.announce(self.thread_id(), halt.announcement()).await;
        }
        Ok(true)
    }

    async fn halt_if_due(&self) -> Result<bool, String> {
        let _goal_state_permit = self.goal_state_permit().await?;
        self.halt_if_due_locked().await
    }

    /// The boxes of this thread's goal while it is active and has any.
    async fn active_goal_boxes(&self) -> Option<GoalBoxes> {
        let goals = self.inner.state_dbs.thread_goals();
        let goal = goals
            .get_thread_goal(self.thread_id())
            .await
            .inspect_err(|err| tracing::warn!("goal box watch cannot read its goal: {err}"))
            .ok()??;
        if goal.status != codex_state::ThreadGoalStatus::Active {
            return None;
        }
        goals
            .goal_boxes(self.thread_id())
            .await
            .inspect_err(|err| tracing::warn!("goal box watch cannot read its boxes: {err}"))
            .ok()?
            .filter(|boxes| !boxes.is_empty())
    }
}

/// Reminds as a timebox's time left halves, reads a quotabox's quota, and halts the goal when a
/// box is reached. Reminders that fall due while the watch sleeps or before it starts are skipped.
async fn watch(inner: Weak<GoalRuntimeInner>) {
    let mut reminded_through = unix_now();
    let mut next_read = reminded_through;
    let mut blind = false;
    loop {
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let runtime = GoalRuntimeHandle { inner };
        let Some(boxes) = runtime.active_goal_boxes().await else {
            return;
        };
        let now = unix_now();
        if boxes.quotabox.is_some() && now >= next_read {
            next_read = now + QUOTA_POLL;
            match runtime.read_quota().await {
                Some(Ok(_)) => blind = false,
                Some(Err(error)) if !blind => {
                    blind = true;
                    if let Some(host) = runtime.box_host() {
                        let message =
                            format!("Quotabox cannot read quota: {error}. The goal continues.");
                        host.announce(runtime.thread_id(), message).await;
                    }
                }
                Some(Err(_)) | None => {}
            }
        }
        if Halt::due(boxes, now, runtime.latest_reading()).is_some() {
            match runtime.halt_if_due().await {
                Ok(true) => return,
                Ok(false) => {}
                Err(err) => {
                    tracing::warn!("goal box halt failed: {err}");
                    return;
                }
            }
        }
        let mut wake = now + CLOCK_GLANCE;
        if let Some(timebox) = boxes.timebox {
            if reminder_instants(timebox)
                .take_while(|instant| *instant <= now)
                .any(|instant| instant > reminded_through)
                && let Some(item) = timebox_reminder_item(timebox, now)
            {
                runtime.inject_active_turn_steering(item).await;
            }
            reminded_through = now;
            let next = reminder_instants(timebox)
                .find(|instant| *instant > now)
                .unwrap_or(timebox.deadline);
            wake = wake.min(next).min(timebox.deadline);
        }
        if boxes.quotabox.is_some() {
            wake = wake.min(next_read);
        }
        drop(runtime);
        let sleep = (wake - now).clamp(1, CLOCK_GLANCE);
        tokio::time::sleep(Duration::from_secs(sleep.unsigned_abs())).await;
    }
}
