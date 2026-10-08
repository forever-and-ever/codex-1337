use super::StateRuntime;
use codex_protocol::CommsGate;
use codex_protocol::ThreadId;

impl StateRuntime {
    pub async fn set_thread_comms(
        &self,
        thread_id: ThreadId,
        comms: CommsGate,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query("UPDATE threads SET comms = ? WHERE id = ?")
            .bind(comms.as_str())
            .bind(thread_id.to_string())
            .execute(self.pool.as_ref())
            .await?;
        Ok(result.rows_affected() > 0)
    }
}
