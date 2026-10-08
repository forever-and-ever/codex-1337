use super::StateRuntime;
use codex_protocol::ContextRole;
use codex_protocol::ThreadId;

impl StateRuntime {
    pub async fn set_thread_context_role(
        &self,
        thread_id: ThreadId,
        role: Option<&ContextRole>,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query("UPDATE threads SET context_role = ? WHERE id = ?")
            .bind(role.map(ContextRole::as_str))
            .bind(thread_id.to_string())
            .execute(self.pool.as_ref())
            .await?;
        Ok(result.rows_affected() > 0)
    }
}
