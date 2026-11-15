//! Owned history reads for cancellable frontend continuations.

use super::AppServerRequestHandle;
use super::AppServerSession;
use super::Config;
use super::HistoryHydrationScope;
use super::LocalSettings;
use super::Result;
use super::Thread;
use super::ThreadId;
use super::history::preparation::HistoryPaginationUpdate;
use super::history::preparation::{self};
use super::session_preparation::RequestPolicy;

pub(crate) struct HistoryObservation(HistoryPaginationUpdate);

pub(crate) struct PreparedHistory {
    pub(crate) result: Result<Thread>,
    pub(crate) observation: HistoryObservation,
}

impl HistoryObservation {
    pub(crate) fn apply(self, server: &mut AppServerSession) {
        server.apply_history_preparation(self.0);
    }
}

pub(crate) async fn load(
    handle: &AppServerRequestHandle,
    thread_id: ThreadId,
    through: Option<&str>,
    tracking: Option<(&Config, &LocalSettings)>,
    progress: &(dyn Fn() + Send + Sync),
    image_origin: crate::history_cell::ImagePathOrigin,
) -> PreparedHistory {
    let mut update = HistoryPaginationUpdate::Untracked;
    let result = async {
        let mut thread =
            preparation::read_thread(RequestPolicy::Interactive, handle, thread_id, false).await?;
        if !thread.ephemeral {
            let (result, observation) = preparation::prepare_history_with_progress(
                RequestPolicy::Interactive,
                handle,
                &mut thread,
                None,
                None,
                tracking.map(|(config, _)| config),
                tracking.map(|(_, local)| local),
                through.map_or(
                    HistoryHydrationScope::Complete,
                    HistoryHydrationScope::ThroughTurn,
                ),
                progress,
                false,
                image_origin,
            )
            .await;
            update = observation;
            result?;
        }
        Ok(thread)
    }
    .await;
    PreparedHistory {
        result,
        observation: HistoryObservation(update),
    }
}

pub(crate) async fn legacy(handle: &AppServerRequestHandle, thread_id: ThreadId) -> Result<Thread> {
    preparation::read_thread(RequestPolicy::Interactive, handle, thread_id, true).await
}
