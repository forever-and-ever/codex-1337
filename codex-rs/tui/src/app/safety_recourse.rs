//! Recourse reconstructs a verified failed-turn boundary through the owned fork worker.

use super::*;
use crate::app_server_session::session_preparation::RequestPolicy;
use crate::app_server_session::session_preparation::request;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SortDirection;
use codex_app_server_protocol::ThreadReadParams;
use color_eyre::eyre::ensure;

pub(super) fn supports_server(server: &AppServerSession) -> bool {
    let minimum = semver::Version::new(0, 151, 0);
    server.server_version().map_or_else(
        || server.uses_embedded_app_server(),
        |version| semver::Version::parse(version).is_ok_and(|version| version >= minimum),
    )
}

async fn turns(
    handle: &AppServerRequestHandle,
    thread: ThreadId,
    limit: u32,
) -> Result<codex_app_server_protocol::ThreadTurnsListResponse> {
    Ok(request(
        RequestPolicy::Interactive,
        handle,
        ClientRequest::ThreadTurnsList {
            request_id: RequestId::String(format!("tui-recourse-{}", Uuid::new_v4())),
            params: codex_app_server_protocol::ThreadTurnsListParams {
                thread_id: thread.to_string(),
                cursor: None,
                limit: Some(limit),
                sort_direction: Some(SortDirection::Desc),
                items_view: Some(codex_app_server_protocol::TurnItemsView::NotLoaded),
            },
        },
    )
    .await?)
}

pub(super) async fn validate_source(
    handle: &AppServerRequestHandle,
    source: ThreadId,
    failed_turn: &str,
) -> Result<Option<String>> {
    let metadata: codex_app_server_protocol::ThreadReadResponse = request(
        RequestPolicy::Interactive,
        handle,
        ClientRequest::ThreadRead {
            request_id: RequestId::String(format!("tui-recourse-read-{}", Uuid::new_v4())),
            params: ThreadReadParams {
                thread_id: source.to_string(),
                include_turns: false,
            },
        },
    )
    .await?;
    let history = turns(handle, source, 2).await?;
    ensure!(
        !metadata.thread.ephemeral
            && history
                .data
                .first()
                .is_some_and(|turn| turn.id == failed_turn && turn.status == TurnStatus::Failed)
            && (history.data.len() > 1 || history.next_cursor.is_none()),
        "the blocked turn is no longer the latest durable turn"
    );
    Ok(history.data.get(1).map(|turn| turn.id.clone()))
}

pub(super) async fn verify_fork(
    handle: &AppServerRequestHandle,
    started: &AppServerStartedThread,
    source: ThreadId,
    failed_turn: &str,
    previous: Option<&str>,
) -> Result<()> {
    ensure!(
        !started.blocks_direct_input && started.session.forked_from_id == Some(source),
        "the recourse fork lineage could not be verified"
    );
    let fork = turns(handle, started.session.thread_id, 1).await?;
    let latest = turns(handle, source, 1).await?;
    ensure!(
        latest
            .data
            .first()
            .is_some_and(|turn| turn.id == failed_turn && turn.status == TurnStatus::Failed)
            && (!fork.data.is_empty() || fork.next_cursor.is_none())
            && fork.data.first().map(|turn| turn.id.as_str()) == previous,
        "the recourse fork boundary could not be verified"
    );
    Ok(())
}
