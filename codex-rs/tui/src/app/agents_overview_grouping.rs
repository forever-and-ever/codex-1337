//! Command Center grouping, including restoration and client-local preference persistence.
//! The view owns the live choice; ordered app events save it through the existing config writer.

use super::super::App;
use super::super::agents_overview::AgentsOverviewState;
use super::super::agents_overview_discovery::matches_query;
use super::AgentsOverviewGrouping;
use super::AgentsOverviewView;
use crate::legacy_core::config::edit::ConfigEdit;
use crate::legacy_core::config::edit::ConfigEditsBuilder;
use codex_app_server_protocol::Thread;

pub(super) fn model_name(thread: &Thread) -> &str {
    thread
        .model
        .as_deref()
        .filter(|model| !model.is_empty())
        .unwrap_or("Unknown")
}

impl AgentsOverviewView {
    pub(super) fn is_pinned(&self, index: usize) -> bool {
        self.pin_rank(index).is_some()
    }

    fn pin_rank(&self, index: usize) -> Option<usize> {
        self.pinned_thread_ranks
            .as_ref()?
            .get(&self.rows[index].thread_id)
            .copied()
    }

    pub(in crate::app) fn visible_indices(&self) -> Vec<usize> {
        let state = self.state();
        let search = state.search.to_lowercase();
        let status_group = super::command_center::TASK_FILTERS[state.status_filter];
        let visible = self.rows.iter().enumerate().filter_map(|(index, row)| {
            (state.rename_target == Some(row.thread_id)
                || (state.includes_row(row)
                    && (search.is_empty() || matches_query(&row.thread, &search))
                    && status_group.is_none_or(|group| group == row.group)))
            .then_some(index)
        });
        match state.grouping {
            AgentsOverviewGrouping::Project => {
                let mut visible = visible.collect::<Vec<_>>();
                visible.sort_by_cached_key(|&index| {
                    let row = &self.rows[index];
                    let rank = self.pin_rank(index);
                    (
                        row.group,
                        &self.project_groups[index].key,
                        rank.is_none(),
                        rank,
                        std::cmp::Reverse(row.thread.updated_at),
                    )
                });
                visible
            }
            grouping @ (AgentsOverviewGrouping::Status | AgentsOverviewGrouping::Model) => {
                let (mut pinned, mut unpinned): (Vec<_>, Vec<_>) =
                    visible.partition(|&index| self.is_pinned(index));
                pinned.sort_by_key(|&index| self.pin_rank(index));
                if grouping == AgentsOverviewGrouping::Status {
                    pinned.extend(unpinned);
                    // Stable sorting keeps pin order, then the supplied newest-first order.
                    pinned.sort_by_key(|&index| self.rows[index].group);
                } else {
                    unpinned.sort_by_key(|&index| {
                        (
                            model_name(&self.rows[index].thread),
                            std::cmp::Reverse(self.rows[index].thread.updated_at),
                        )
                    });
                    pinned.extend(unpinned);
                }
                pinned
            }
        }
    }

    pub(super) fn same_group(
        &self,
        grouping: AgentsOverviewGrouping,
        left: usize,
        right: usize,
    ) -> bool {
        match grouping {
            AgentsOverviewGrouping::Status => self.rows[left].group == self.rows[right].group,
            AgentsOverviewGrouping::Project => {
                self.rows[left].group == self.rows[right].group
                    && self.project_groups[left].key == self.project_groups[right].key
            }
            AgentsOverviewGrouping::Model if self.is_pinned(left) || self.is_pinned(right) => {
                self.is_pinned(left) && self.is_pinned(right)
            }
            AgentsOverviewGrouping::Model => {
                model_name(&self.rows[left].thread) == model_name(&self.rows[right].thread)
            }
        }
    }
}

impl AgentsOverviewState {
    pub(in super::super) fn new(grouping: codex_config::types::AgentsOverviewGrouping) -> Self {
        let state = Self::default();
        state
            .view_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .grouping = grouping;
        state
    }
}

impl App {
    pub(in super::super) async fn persist_agents_overview_grouping(
        &mut self,
        grouping: AgentsOverviewGrouping,
    ) {
        self.local_settings.tui.agents_overview_grouping = grouping;
        let edit = ConfigEdit::SetPath {
            segments: vec!["tui".to_string(), "agents_overview_grouping".to_string()],
            value: grouping.as_str().into(),
        };
        if let Err(err) =
            ConfigEditsBuilder::for_config_path(self.local_settings.user_config_path.as_path())
                .with_edits([edit])
                .apply()
                .await
        {
            self.add_agents_overview_error(format!(
                "Failed to save Command Center grouping: {err}"
            ));
        }
    }
}
