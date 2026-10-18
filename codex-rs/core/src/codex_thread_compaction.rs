//! Definitive task admission for app-server compaction requests.

use super::CodexThread;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::W3cTraceContext;
use tokio::sync::oneshot;

impl CodexThread {
    pub async fn start_compaction_with_trace(
        &self,
        trace: Option<W3cTraceContext>,
    ) -> CodexResult<()> {
        let (reply, response) = oneshot::channel();
        let _ = self
            .submit_with_trace(Op::StartCompaction { reply }, trace)
            .await?;
        response.await.unwrap_or(Err(CodexErr::InternalAgentDied))
    }
}
