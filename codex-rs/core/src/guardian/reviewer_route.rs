//! Routes borrowed `ChatGPT` approval reviews without permitting hidden parent-provider billing.

use super::GuardianReviewContext;
use crate::agent::child_config::switch_agent_provider;
use crate::chatgpt_delegate::uses_chatgpt_login;
use crate::config::Config;
use crate::session::session::Session;
use codex_guardian_reviewer::GuardianReviewError;
use codex_guardian_reviewer::GuardianReviewOutcome;
use codex_guardian_reviewer::ReviewDeadline;
use codex_guardian_reviewer::ReviewModel;
use codex_model_provider::DEFAULT_APPROVAL_REVIEW_PREFERRED_MODEL;
use codex_model_provider_info::ModelProviderAdapter;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_models_manager::manager::RefreshStrategy;
use codex_protocol::ThreadId;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::WarningEvent;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

pub(super) const OPENROUTER_REVIEW_LOGIN_REQUIRED: &str =
    "Auto-review for OpenRouter conversations needs a ChatGPT login; choose one with /account.";

#[derive(Clone, Debug)]
pub(crate) struct CapturedReviewer(pub(super) Arc<crate::CapturedProvider>);

impl PartialEq for CapturedReviewer {
    fn eq(&self, other: &Self) -> bool {
        self.0.same_authority(&other.0)
    }
}
impl Eq for CapturedReviewer {}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum ReviewerRoute {
    #[default]
    ParentProvider,
    OpenRouterUnavailable,
    ChatgptAutoReview {
        account_id: String,
        captured: CapturedReviewer,
    },
}

#[derive(Clone, Copy)]
pub(crate) enum QuarantineCause {
    Ineligible,
    Unavailable,
}

#[derive(Default)]
pub(crate) struct ReviewerRouteQuarantine {
    accounts: Mutex<HashMap<String, Instant>>,
    warned: Mutex<HashSet<ThreadId>>,
}

impl ReviewerRouteQuarantine {
    async fn contains(&self, account_id: &str) -> bool {
        let mut accounts = self.accounts.lock().await;
        accounts.retain(|_, until| *until > Instant::now());
        accounts.contains_key(account_id)
    }

    async fn insert(&self, account_id: &str, cause: QuarantineCause) {
        let duration = match cause {
            QuarantineCause::Ineligible => Duration::from_hours(/*hours*/ 1),
            QuarantineCause::Unavailable => Duration::from_mins(/*mins*/ 10),
        };
        self.accounts
            .lock()
            .await
            .insert(account_id.to_owned(), Instant::now() + duration);
    }
}

impl ReviewerRoute {
    pub(crate) async fn resolve(session: &Session, context: &GuardianReviewContext) -> Self {
        let config = &context.turn().config;
        let fallback = match config.model_provider.adapter {
            ModelProviderAdapter::OpenaiCompatible => return Self::ParentProvider,
            ModelProviderAdapter::ClaudeSubscription => Self::ParentProvider,
            ModelProviderAdapter::OpenRouter => Self::OpenRouterUnavailable,
        };
        if !config.model_providers.contains_key(OPENAI_PROVIDER_ID) {
            return fallback;
        }
        Box::pin(tokio::time::timeout(
            Duration::from_secs(/*secs*/ 5),
            async {
                let borrowed = Box::pin(crate::chatgpt_delegate::borrow_chatgpt(session, config))
                    .await
                    .ok()?;
                let account_id = borrowed.account_id?;
                if session
                    .services
                    .local_agent_runtime
                    .reviewer_route_quarantine
                    .contains(&account_id)
                    .await
                {
                    return None;
                }
                let models = borrowed
                    .manager
                    .list_models(
                        RefreshStrategy::OnlineIfUncached,
                        borrowed.config.http_client_factory(),
                    )
                    .await;
                models
                    .iter()
                    .any(|model| model.model == DEFAULT_APPROVAL_REVIEW_PREFERRED_MODEL)
                    .then_some(Self::ChatgptAutoReview {
                        account_id,
                        captured: CapturedReviewer(Arc::new(borrowed.captured)),
                    })
            },
        ))
        .await
        .ok()
        .flatten()
        .unwrap_or(fallback)
    }

    pub(crate) fn apply(&self, config: &mut Config) {
        if matches!(self, Self::ChatgptAutoReview { .. }) {
            switch_agent_provider(config, OPENAI_PROVIDER_ID);
        }
    }

    pub(crate) fn credentials(
        &self,
        turn: &crate::session::turn_context::TurnContext,
    ) -> anyhow::Result<crate::CapturedProvider> {
        let captured = match self {
            Self::ChatgptAutoReview { captured, .. } => captured.0.as_ref().clone(),
            Self::ParentProvider => turn
                .extension_data
                .get::<crate::CapturedProvider>()
                .ok_or_else(|| {
                    anyhow::anyhow!("Guardian review requires the issuing turn's admitted provider")
                })?
                .as_ref()
                .clone(),
            Self::OpenRouterUnavailable => anyhow::bail!(OPENROUTER_REVIEW_LOGIN_REQUIRED),
        };
        captured.validate()?;
        Ok(captured)
    }

    pub(crate) async fn resolve_chatgpt_model(
        &self,
        context: &GuardianReviewContext,
    ) -> anyhow::Result<(ReviewModel, Arc<codex_models_manager::CatalogModel>)> {
        let mut config = context.turn().config.as_ref().clone();
        switch_agent_provider(&mut config, OPENAI_PROVIDER_ID);
        let Self::ChatgptAutoReview { captured, .. } = self else {
            anyhow::bail!("ChatGPT review requires a captured route");
        };
        captured
            .0
            .validate()
            .map_err(|error| ReviewerRouteUnavailable(error.to_string()))?;
        let manager = captured
            .0
            .models_manager(&config)
            .map_err(|error| ReviewerRouteUnavailable(error.to_string()))?;
        let available_models = manager
            .model_catalog_with_origins(RefreshStrategy::InMemory, config.http_client_factory())
            .await;
        let mut model = manager
            .get_model_info(
                DEFAULT_APPROVAL_REVIEW_PREFERRED_MODEL,
                &config.to_models_manager_config(),
            )
            .await;
        Arc::make_mut(&mut model.model).auto_review_model_override = None;
        let (review_model, reviewer) = codex_guardian_reviewer::select_review_model(
            &model,
            context.reasoning_effort.as_ref(),
            DEFAULT_APPROVAL_REVIEW_PREFERRED_MODEL,
            available_models,
        );
        Ok((review_model, Arc::new(reviewer.unwrap_or(model))))
    }

    pub(crate) fn guard(
        &self,
        session: &Session,
        provider: &ModelProviderInfo,
    ) -> anyhow::Result<()> {
        if matches!(self, Self::OpenRouterUnavailable) {
            anyhow::bail!(OPENROUTER_REVIEW_LOGIN_REQUIRED);
        }
        if let Self::ChatgptAutoReview { captured, .. } = self {
            captured.0.validate()?;
        }
        if let Self::ChatgptAutoReview { account_id, .. } = self
            && (!uses_chatgpt_login(provider)
                || session.provider_id != OPENAI_PROVIDER_ID
                || session.services.auth_manager.selected_account_id().as_ref() != Some(account_id)
                || !session
                    .services
                    .auth_manager
                    .auth_cached()
                    .is_some_and(|auth| auth.is_chatgpt_auth()))
        {
            anyhow::bail!("OpenAI auto-review login changed before reviewer startup");
        }
        Ok(())
    }

    pub(crate) async fn fallback(
        &mut self,
        session: &Session,
        outcome: &GuardianReviewOutcome,
    ) -> Option<&'static str> {
        let Self::ChatgptAutoReview { account_id, .. } = self else {
            return None;
        };
        let GuardianReviewOutcome::Error(error) = outcome else {
            return None;
        };
        let (cause, reason) = match error {
            GuardianReviewError::Cancelled | GuardianReviewError::StaleAuthorization => {
                return None;
            }
            GuardianReviewError::Session {
                error_info: Some(CodexErrorInfo::Unauthorized),
                ..
            } => (Some(QuarantineCause::Ineligible), "unauthorized"),
            GuardianReviewError::Session {
                error_info: Some(CodexErrorInfo::UsageLimitExceeded),
                ..
            } => (Some(QuarantineCause::Ineligible), "usage limit"),
            GuardianReviewError::Session { .. } => {
                (Some(QuarantineCause::Unavailable), "session error")
            }
            GuardianReviewError::Timeout => (Some(QuarantineCause::Unavailable), "timeout"),
            GuardianReviewError::PromptBuild { .. }
            | GuardianReviewError::Parse { .. }
            | GuardianReviewError::InputBudgetExceeded => (None, "review failed"),
        };
        let quarantine = &session
            .services
            .local_agent_runtime
            .reviewer_route_quarantine;
        if let Some(cause) = cause {
            quarantine.insert(account_id, cause).await;
        }
        // OpenRouter must never receive the OpenAI reviewer id or hidden fallback spend.
        if session.get_config().await.model_provider.adapter == ModelProviderAdapter::OpenRouter {
            return None;
        }
        *self = Self::ParentProvider;
        Some(reason)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("OpenAI reviewer route unavailable: {0}")]
pub(super) struct ReviewerRouteUnavailable(String);

pub(super) async fn run_review_attempt<Run, Attempt>(
    session: &Session,
    context: &GuardianReviewContext,
    deadline: ReviewDeadline,
    route_state: &Mutex<ReviewerRoute>,
    mut run: Run,
) -> (
    GuardianReviewOutcome,
    codex_analytics::GuardianReviewAnalyticsResult,
)
where
    Run: FnMut(ReviewerRoute) -> Attempt,
    Attempt: Future<
        Output = (
            GuardianReviewOutcome,
            codex_analytics::GuardianReviewAnalyticsResult,
        ),
    >,
{
    let mut route = route_state.lock().await.clone();
    let mut result = run(route.clone()).await;
    if let Some(reason) = route.fallback(session, &result.0).await {
        *route_state.lock().await = route.clone();
        if matches!(deadline, ReviewDeadline::At(at) if Instant::now() >= at) {
            return result;
        }
        let warn = session
            .services
            .local_agent_runtime
            .reviewer_route_quarantine
            .warned
            .lock()
            .await
            .insert(session.thread_id);
        if warn {
            session.send_event(context.turn(), EventMsg::GuardianWarning(WarningEvent {
                message: format!("Auto-review: OpenAI reviewer unavailable ({reason}); using the Claude reviewer."),
            })).await;
        }
        result = run(route).await;
    }
    result
}

#[cfg(test)]
#[path = "reviewer_route_tests.rs"]
mod tests;
