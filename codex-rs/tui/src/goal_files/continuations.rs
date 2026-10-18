//! Background file and transcript preparation.

use super::AppServerFileSystem;
use super::GoalDraft;
use super::GoalFilePath;
use super::Result;
use super::materialize_goal_objective;

pub(super) async fn materialize_goal_draft_continuation(
    app_server: &AppServerFileSystem,
    codex_home: Option<&GoalFilePath>,
    draft: GoalDraft,
) -> Result<(String, Option<GoalFilePath>)> {
    let mut output_dir = None;
    let result = materialize_goal_objective(app_server, codex_home, draft, &mut output_dir).await;
    match result {
        Ok(objective) => Ok((objective, output_dir)),
        Err(error) => {
            if let Some(output_dir) = output_dir
                && let Err(cleanup_error) = app_server.fs_remove_path(&output_dir).await
            {
                tracing::warn!(%output_dir, %cleanup_error, "failed to clean up rejected goal draft");
            }
            Err(error)
        }
    }
}
