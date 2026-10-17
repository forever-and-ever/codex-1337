//! Test-only helpers shared across the TUI crate.

use std::sync::LazyLock;

use codex_models_manager::bundled_models_response;
use codex_protocol::openai_models::ModelPreset;
pub(crate) use codex_utils_absolute_path::test_support::PathBufExt;
pub(crate) use codex_utils_absolute_path::test_support::test_path_buf;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub(crate) async fn thread_goal_get(
    server: &crate::app_server_session::AppServerSession,
    thread_id: codex_protocol::ThreadId,
) -> color_eyre::Result<codex_app_server_protocol::ThreadGoalGetResponse> {
    use codex_app_server_protocol::ClientRequest;
    use codex_app_server_protocol::RequestId;
    use codex_app_server_protocol::ThreadGoalGetParams;
    Ok(server
        .request_handle()
        .request_typed(ClientRequest::ThreadGoalGet {
            request_id: RequestId::String(format!("test-goal-read-{}", uuid::Uuid::new_v4())),
            params: ThreadGoalGetParams {
                thread_id: thread_id.to_string(),
            },
        })
        .await?)
}

pub(crate) async fn thread_goal_set(
    server: &crate::app_server_session::AppServerSession,
    thread_id: codex_protocol::ThreadId,
    objective: Option<String>,
    status: Option<codex_app_server_protocol::ThreadGoalStatus>,
    token_budget: Option<Option<i64>>,
) -> color_eyre::Result<codex_app_server_protocol::ThreadGoalSetResponse> {
    use codex_app_server_protocol::ClientRequest;
    use codex_app_server_protocol::RequestId;
    use codex_app_server_protocol::ThreadGoalMutationOrigin;
    use codex_app_server_protocol::ThreadGoalSetParams;
    Ok(server
        .request_handle()
        .request_typed(ClientRequest::ThreadGoalSet {
            request_id: RequestId::String(format!("test-goal-set-{}", uuid::Uuid::new_v4())),
            params: ThreadGoalSetParams {
                thread_id: thread_id.to_string(),
                objective,
                status,
                token_budget,
                origin: Some(ThreadGoalMutationOrigin::User),
            },
        })
        .await?)
}

pub(crate) static TEST_MODEL_PRESETS: LazyLock<Vec<ModelPreset>> = LazyLock::new(|| {
    let mut response = bundled_models_response()
        .unwrap_or_else(|err| panic!("bundled models.json should parse: {err}"));
    response.models.sort_by_key(|model| model.priority);
    let mut presets: Vec<ModelPreset> = response.models.into_iter().map(Into::into).collect();
    ModelPreset::mark_default_by_picker_visibility(&mut presets);
    presets
});

#[cfg(unix)]
pub(crate) fn temp_runtime_dir() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("tui-")
        .tempdir_in(codex_uds::prepare_shared_daemon_socket_directory()?)
}
pub(crate) fn test_path_display(path: &str) -> String {
    test_path_buf(path).display().to_string()
}

pub(crate) fn session_source_cli<T>() -> T
where
    T: DeserializeOwned,
{
    from_app_server_wire(codex_app_server_protocol::SessionSource::Cli)
}

pub(crate) fn skill_scope_user<T>() -> T
where
    T: DeserializeOwned,
{
    from_app_server_wire(codex_app_server_protocol::SkillScope::User)
}

pub(crate) fn skill_scope_repo<T>() -> T
where
    T: DeserializeOwned,
{
    from_app_server_wire(codex_app_server_protocol::SkillScope::Repo)
}

fn from_app_server_wire<T>(value: impl Serialize) -> T
where
    T: DeserializeOwned,
{
    serde_json::to_value(value)
        .and_then(serde_json::from_value)
        .unwrap_or_else(|err| {
            panic!("app-server wire value should map to legacy helper type: {err}")
        })
}
