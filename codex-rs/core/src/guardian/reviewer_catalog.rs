//! Review model selection and refresh custody belong to the admitted credential authority.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;

use codex_guardian_reviewer::ReviewModel;
use codex_login::AuthManager;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
use codex_models_manager::model_info::with_config_overrides;
use codex_protocol::openai_models::CatalogModel;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::ReasoningEffort;
use futures::FutureExt;
use futures::future::BoxFuture;
use futures::future::Shared;
use tokio::sync::Mutex;
use tokio_util::task::AbortOnDropHandle;

use crate::CapturedProvider;
use crate::config::Config;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

use super::GuardianReviewContext;

struct GuardianModelCatalogRefresh(Shared<BoxFuture<'static, ()>>);

struct AdmittedCatalog {
    authority: CapturedProvider,
    manager: SharedModelsManager,
    configured: Option<ModelsResponse>,
    cache_path: PathBuf,
    refresh: OnceLock<GuardianModelCatalogRefresh>,
}

#[derive(Default)]
struct ReviewCatalogs(Mutex<Vec<Arc<AdmittedCatalog>>>);

async fn admitted_catalog(
    session: &Session,
    authority: &CapturedProvider,
    config: &Config,
) -> anyhow::Result<Arc<AdmittedCatalog>> {
    authority.validate()?;
    authority.matches(&config.model_provider)?;
    let catalogs = session
        .services
        .session_extension_data
        .get_or_init(ReviewCatalogs::default);
    let mut catalogs = catalogs.0.lock().await;
    catalogs.retain(|catalog| catalog.authority.validate().is_ok());
    let cache_path = config.models_cache_path();
    if let Some(catalog) = catalogs.iter().find(|catalog| {
        catalog.authority.same_authority(authority)
            && catalog.configured == config.model_catalog
            && catalog.cache_path == cache_path
    }) {
        return Ok(Arc::clone(catalog));
    }
    let catalog = Arc::new(AdmittedCatalog {
        authority: authority.clone(),
        manager: authority.models_manager(config)?,
        configured: config.model_catalog.clone(),
        cache_path,
        refresh: OnceLock::new(),
    });
    catalogs.push(Arc::clone(&catalog));
    Ok(catalog)
}

async fn adopt_publication(target: &SharedModelsManager, source: &SharedModelsManager) {
    if !Arc::ptr_eq(target, source)
        && let Some(publication) = source.live_catalog().await
    {
        target.adopt_live_catalog(publication).await;
    }
}

pub(crate) async fn resolve_review_model(
    session: &Session,
    context: &GuardianReviewContext,
) -> anyhow::Result<(ReviewModel, Arc<CatalogModel>)> {
    let turn = context.turn();
    let authority = turn
        .extension_data
        .get::<CapturedProvider>()
        .ok_or_else(|| {
            anyhow::anyhow!("Guardian review requires the issuing turn's admitted provider")
        })?;
    let catalog = admitted_catalog(session, &authority, &turn.config).await?;
    adopt_publication(&catalog.manager, &session.services.models_manager).await;
    // A refresh completing during this lookup still requires a reread.
    let refresh_completed = catalog
        .refresh
        .get()
        .is_some_and(|refresh| refresh.0.peek().is_some());
    let resolved = resolve_review_model_from_catalog(&catalog.manager, context).await;
    let (review_model, _) = &resolved;
    if !refresh_completed
        && review_model.default_review_model_id == "codex-auto-review"
        && !review_model.model_overridden
        && !review_model.catalog_contains_auto_review
        && turn.config.model_catalog.is_none()
        && turn.provider.info().model_catalog_url.is_none()
        && !super::is_basic_session_source(&turn.session_source)
    {
        // Custody survives cancelled waiters, but is partitioned by admitted authority.
        // A failed online fetch completes the attempt and retains upstream fallback behavior.
        let refresh = catalog.refresh.get_or_init(|| {
            let manager = Arc::clone(&catalog.manager);
            let http_client_factory = turn.config.http_client_factory();
            let task = AbortOnDropHandle::new(tokio::spawn(async move {
                manager
                    .list_models(RefreshStrategy::Online, http_client_factory)
                    .await;
            }));
            GuardianModelCatalogRefresh(
                async move {
                    let _ = task.await;
                }
                .boxed()
                .shared(),
            )
        });
        refresh.0.clone().await;
        adopt_publication(&catalog.manager, &session.services.models_manager).await;
        return Ok(resolve_review_model_from_catalog(&catalog.manager, context).await);
    }
    Ok(resolved)
}

async fn resolve_review_model_from_catalog(
    manager: &SharedModelsManager,
    context: &GuardianReviewContext,
) -> (ReviewModel, Arc<CatalogModel>) {
    let turn = context.turn();
    let (review_model, model_info) = resolve_catalog_review_model(
        manager,
        &turn.config,
        &context.model_info,
        context.reasoning_effort.as_ref(),
        turn.provider.approval_review_preferred_model(),
    )
    .await;
    (
        review_model,
        model_info.map_or_else(|| Arc::clone(&context.model_info), Arc::new),
    )
}

/// Resolves policy through the issuing turn's admitted catalog, never its mutable parent.
pub async fn guardian_policy_model(
    thread: &crate::CodexThread,
    authority: &CapturedProvider,
    config: &Config,
    parent_model: Arc<ModelInfo>,
) -> anyhow::Result<Arc<ModelInfo>> {
    let manager = authority.models_manager(config)?;
    adopt_publication(&manager, &thread.session.services.models_manager).await;
    let (_, model_info) = resolve_catalog_review_model(
        &manager,
        config,
        &parent_model,
        config.model_reasoning_effort.as_ref(),
        authority.provider().approval_review_preferred_model(),
    )
    .await;
    Ok(model_info.map_or(parent_model, |model| model.model))
}

pub(crate) async fn model_for_admission(
    session: &Session,
    authority: &CapturedProvider,
    config: &Config,
    model: &str,
) -> anyhow::Result<CatalogModel> {
    let catalog = admitted_catalog(session, authority, config).await?;
    adopt_publication(&catalog.manager, &session.services.models_manager).await;
    Ok(catalog
        .manager
        .get_model_info(model, &config.to_models_manager_config())
        .await)
}

pub(super) async fn session_manager<'a>(
    session: &'a Session,
    parent: &Session,
) -> &'a SharedModelsManager {
    let manager = &session.services.models_manager;
    adopt_publication(manager, &parent.services.models_manager).await;
    if let Some(catalogs) = parent
        .services
        .session_extension_data
        .get::<ReviewCatalogs>()
    {
        let sources: Vec<_> = catalogs
            .0
            .lock()
            .await
            .iter()
            .map(|catalog| Arc::clone(&catalog.manager))
            .collect();
        for source in sources {
            // The review's admitted catalog can be newer than both disk and the now-switched
            // parent. Its full publication still passes the destination's identity/time fence.
            adopt_publication(manager, &source).await;
        }
    }
    manager
}

// Replay has no issuing task. Admit this dedicated replay turn explicitly before optional
// checkpoint compatibility work; failure must not prevent installing the recorded history.
pub(crate) async fn reconstruction_compaction_hash(
    session: &Session,
    turn: &Arc<TurnContext>,
) -> Option<String> {
    if turn.extension_data.get::<CapturedProvider>().is_none() {
        session.capture_turn_provider(turn).ok()?;
    }
    let context = GuardianReviewContext::from(turn);
    resolve_review_model(session, &context)
        .await
        .ok()?
        .1
        .comp_hash
        .clone()
}

async fn resolve_catalog_review_model(
    models_manager: &SharedModelsManager,
    config: &Config,
    parent_model: &ModelInfo,
    reasoning_effort: Option<&ReasoningEffort>,
    default_review_model_id: &str,
) -> (ReviewModel, Option<CatalogModel>) {
    // Adopt newer publications for this same admitted identity without network work.
    // The manager rejects mismatched identities and older cache entries, preserving a
    // successful in-memory refresh even when its cache write failed.
    let mut catalog = models_manager
        .model_catalog_with_origins(RefreshStrategy::Offline, config.http_client_factory())
        .await;
    let uses_codex_backend = models_manager
        .auth_manager()
        .is_some_and(AuthManager::current_auth_uses_codex_backend);
    catalog.retain(|entry| {
        (uses_codex_backend || entry.supported_in_api)
            && (parent_model.auto_review_model_override.is_some()
                || matches!(
                    entry.origin,
                    codex_models_manager::ModelOrigin::Provider
                        | codex_models_manager::ModelOrigin::ExplicitConfig
                ))
    });
    let (review_model, reviewer) = codex_guardian_reviewer::select_review_model(
        parent_model,
        reasoning_effort,
        default_review_model_id,
        catalog,
    );
    let guardian_model_info = if review_model.model_overridden {
        // Explicit names can resolve through aliases or synthetic metadata.
        Some(
            models_manager
                .get_model_info(&review_model.model, &config.to_models_manager_config())
                .await,
        )
    } else {
        reviewer.map(|entry| CatalogModel {
            model: with_config_overrides(
                Arc::unwrap_or_clone(entry.model),
                &config.to_models_manager_config(),
            )
            .into(),
            origin: entry.origin,
        })
    };
    (review_model, guardian_model_info)
}
