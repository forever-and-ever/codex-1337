//! Goal box requests (codex-1337): reading and setting a root goal's timebox and quotabox.

use super::ClientResponsePayload;
use super::Feature;
use super::GoalAccess;
use super::JSONRPCErrorError;
use super::StateDbHandle;
use super::ThreadGoalRequestProcessor;
use super::ThreadId;
use super::goal_service_error;
use super::invalid_request;
use super::parse_thread_id_for_request;
use codex_app_server_protocol::ThreadGoalBoxesReadParams;
use codex_app_server_protocol::ThreadGoalBoxesReadResponse;
use codex_app_server_protocol::ThreadGoalQuotaboxSetParams;
use codex_app_server_protocol::ThreadGoalQuotaboxSetResponse;
use codex_app_server_protocol::ThreadGoalTimeboxSetParams;
use codex_app_server_protocol::ThreadGoalTimeboxSetResponse;

impl ThreadGoalRequestProcessor {
    pub(crate) async fn thread_goal_boxes_read(
        &self,
        params: ThreadGoalBoxesReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let thread_id = self.boxed_thread_id(&params.thread_id)?;
        let state_db = self
            .state_db_for_materialized_thread(thread_id, GoalAccess::Read)
            .await?;
        let report = self
            .goal_service
            .read_goal_boxes(&state_db, thread_id)
            .await
            .map_err(goal_service_error)?;
        Ok(Some(
            ThreadGoalBoxesReadResponse {
                boxes: report.boxes,
                gauge: report.gauge,
            }
            .into(),
        ))
    }

    pub(crate) async fn thread_goal_timebox_set(
        &self,
        params: ThreadGoalTimeboxSetParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let thread_id = self.boxed_thread_id(&params.thread_id)?;
        let _goal_resume_guard = self.thread_state_manager.lock_goal_resume(thread_id).await;
        let state_db = self.boxed_goal_state_db(thread_id).await?;
        let report = self
            .goal_service
            .set_goal_timebox(&state_db, thread_id, params.deadline)
            .await
            .map_err(goal_service_error)?;
        Ok(Some(
            ThreadGoalTimeboxSetResponse {
                boxes: report.boxes,
            }
            .into(),
        ))
    }

    pub(crate) async fn thread_goal_quotabox_set(
        &self,
        params: ThreadGoalQuotaboxSetParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let thread_id = self.boxed_thread_id(&params.thread_id)?;
        let _goal_resume_guard = self.thread_state_manager.lock_goal_resume(thread_id).await;
        let state_db = self.boxed_goal_state_db(thread_id).await?;
        let report = self
            .goal_service
            .set_goal_quotabox(&state_db, thread_id, params.quotabox)
            .await
            .map_err(goal_service_error)?;
        Ok(Some(
            ThreadGoalQuotaboxSetResponse {
                boxes: report.boxes,
                gauge: report.gauge,
            }
            .into(),
        ))
    }

    fn boxed_thread_id(&self, thread_id: &str) -> Result<ThreadId, JSONRPCErrorError> {
        if !self.config.features.enabled(Feature::Goals) {
            return Err(invalid_request("goals feature is disabled"));
        }
        parse_thread_id_for_request(thread_id)
    }

    /// The state DB holding a thread's goal, reconciled with its rollout before a box changes.
    async fn boxed_goal_state_db(
        &self,
        thread_id: ThreadId,
    ) -> Result<StateDbHandle, JSONRPCErrorError> {
        let state_db = self
            .state_db_for_materialized_thread(thread_id, GoalAccess::Mutate)
            .await?;
        self.reconcile_thread_goal_rollout(thread_id, &state_db)
            .await?;
        Ok(state_db)
    }
}
