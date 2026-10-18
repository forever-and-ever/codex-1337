//! Submission gating and accepted-state projection for deferred model settings.

use super::AutomaticModelSwitchState;
use super::ChatWidget;
use super::ReserveReturnModel;

impl ChatWidget {
    pub(super) fn model_settings_block_submission(&self) -> bool {
        self.waiting_for_luna_reserve() || self.input_queue_frontend.automatic_model_switch_pending
    }

    pub(super) fn commit_automatic_model_switch(
        &mut self,
        reserve_return: Option<ReserveReturnModel>,
        previous_model: String,
    ) {
        self.automatic_model_switch_state = AutomaticModelSwitchState {
            reserve_return,
            replaced_model: Some(previous_model),
        };
    }
}
