//! Test-only helpers shared across the TUI crate.

#[path = "test_support/server_requests.rs"]
mod server_requests;
pub(crate) use server_requests::thread_goal_get;
pub(crate) use server_requests::thread_goal_set;

use std::sync::LazyLock;

use codex_models_manager::bundled_models_response;
use codex_protocol::openai_models::ModelPreset;
pub(crate) use codex_utils_absolute_path::test_support::PathBufExt;
pub(crate) use codex_utils_absolute_path::test_support::test_path_buf;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub(crate) async fn start_test_app_server(
    backend_id: crate::backend_id::BackendId,
    config: &crate::Config,
    target: &crate::AppServerTarget,
    cli_kv_overrides: Vec<(String, toml::Value)>,
    loader_overrides: crate::LoaderOverrides,
    state_db: Option<crate::StateDbHandle>,
    environment_manager: std::sync::Arc<crate::EnvironmentManager>,
) -> color_eyre::Result<crate::AppServerSession> {
    let mut target = target.clone();
    // The picker must connect to the backend that owns the supplied ID.
    if let crate::AppServerTarget::LocalDaemon {
        allow_embedded_fallback,
        ..
    } = &mut target
    {
        *allow_embedded_fallback = false;
    }
    let mut state_db = state_db;
    let embedded_network_policy =
        codex_app_server_client::EmbeddedNetworkPolicy::load(&loader_overrides).await;
    let app_server = crate::start_app_server(
        &mut target,
        crate::Arg0DispatchPaths::default(),
        config.clone(),
        cli_kv_overrides,
        loader_overrides,
        /*strict_config*/ false,
        crate::CloudConfigBundleLoader::default(),
        &mut state_db,
        environment_manager,
        embedded_network_policy,
    )
    .await?;
    Ok(crate::AppServerSession::new(
        backend_id,
        app_server,
        target.thread_params_mode(),
    ))
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

pub(crate) use crate::boxed_future;
