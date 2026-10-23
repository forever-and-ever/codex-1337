//! The server's goal box host (codex-1337): the quota a root thread's requests draw on, and
//! warnings to the thread's clients.

use std::sync::Arc;
use std::sync::Weak;

use codex_goal_extension::BoxFuture;
use codex_goal_extension::GoalBoxHost;
use codex_goal_extension::QuotaError;
use codex_model_provider_info::ModelProviderAdapter;
use codex_protocol::KeyName;
use codex_protocol::ThreadId;
use codex_protocol::goal_boxes::QuotaReading;

use super::account_processor::QuotaReader;
use super::turn_processor::openrouter::KeyStatusReader;
use crate::outgoing_message::OutgoingMessageSender;
use crate::thread_state::ThreadStateManager;
use codex_core::ThreadManager;

pub(crate) struct AppServerGoalBoxHost {
    // Weak: the thread manager's extensions own the goal service that owns this host.
    threads: Weak<ThreadManager>,
    quota: QuotaReader,
    keys: KeyStatusReader,
    outgoing: Arc<OutgoingMessageSender>,
    thread_state_manager: ThreadStateManager,
}

impl AppServerGoalBoxHost {
    pub(crate) fn new(
        threads: Weak<ThreadManager>,
        quota: QuotaReader,
        keys: KeyStatusReader,
        outgoing: Arc<OutgoingMessageSender>,
        thread_state_manager: ThreadStateManager,
    ) -> Self {
        Self {
            threads,
            quota,
            keys,
            outgoing,
            thread_state_manager,
        }
    }

    async fn read_quota(&self, thread_id: ThreadId) -> Result<QuotaReading, QuotaError> {
        let unloaded = || QuotaError::Unreadable("The thread is not loaded.".to_string());
        let thread = self
            .threads
            .upgrade()
            .ok_or_else(unloaded)?
            .get_thread(thread_id)
            .await
            .map_err(|_| unloaded())?;
        let account = || {
            thread.committed_provider_account_id().ok_or_else(|| {
                QuotaError::Unreadable("The thread has not chosen an account yet.".to_string())
            })
        };
        let snapshot = thread.config_snapshot().await;
        match snapshot.model_provider_adapter {
            ModelProviderAdapter::OpenRouter => {
                let name: KeyName = account()?.parse().map_err(|_| {
                    QuotaError::Unreadable(
                        "The thread's OpenRouter key name is invalid.".to_string(),
                    )
                })?;
                Box::pin(self.keys.usd_available(&name)).await
            }
            ModelProviderAdapter::ClaudeSubscription => self.quota.claude_weekly(account()?).await,
            ModelProviderAdapter::OpenaiCompatible if snapshot.requires_openai_auth => {
                let id = thread.auth_manager().selected_account_id().ok_or_else(|| {
                    QuotaError::Unsupported(
                        "This thread's OpenAI login has no plan quota.".to_string(),
                    )
                })?;
                self.quota.openai_weekly(id).await
            }
            ModelProviderAdapter::OpenaiCompatible => Err(QuotaError::Unsupported(
                "This thread's provider reports no quota a quotabox can watch.".to_string(),
            )),
        }
    }
}

impl GoalBoxHost for AppServerGoalBoxHost {
    fn quota(&self, thread_id: ThreadId) -> BoxFuture<'_, Result<QuotaReading, QuotaError>> {
        Box::pin(self.read_quota(thread_id))
    }

    fn announce(&self, thread_id: ThreadId, message: String) -> BoxFuture<'_, ()> {
        Box::pin(crate::extensions::send_thread_warning(
            &self.outgoing,
            &self.thread_state_manager,
            thread_id,
            message,
        ))
    }
}
