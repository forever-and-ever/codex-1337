use crate::ensure_layout;
use crate::extensions::seed_extension_instructions;
use crate::guard;
use crate::metrics::MEMORY_STARTUP;
use crate::phase1;
use crate::phase2;
use crate::runtime::MemoryStartupContext;
use codex_core::CodexThread;
use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_features::Feature;
use codex_protocol::MemoryVersion;
use codex_protocol::ThreadId;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::SessionSource;
use std::sync::Arc;
use tracing::warn;

/// Starts the asynchronous startup memory pipeline for an eligible root session.
///
/// The pipeline is skipped for ephemeral sessions, disabled feature flags, and
/// subagent sessions.
pub async fn start_memories_startup_task(
    thread_manager: Arc<ThreadManager>,
    thread_id: ThreadId,
    thread: Arc<CodexThread>,
    config: Arc<Config>,
    parent_permission_profile: PermissionProfile,
    source: &SessionSource,
) {
    if config.ephemeral
        || !config.features.enabled(Feature::MemoryTool)
        || source.is_non_root_agent()
    {
        return;
    }

    if config.model_provider.adapter == codex_model_provider_info::ModelProviderAdapter::OpenRouter
    {
        return;
    }
    // Admission captures under the conversation settings permit, not in the worker.
    // A failed auxiliary admission cannot reject the already accepted primary turn.
    let provider = match thread.capture_provider().await {
        Ok(provider) => provider,
        Err(error) => {
            warn!(%error, "cannot capture memory-job credentials; skipping");
            return;
        }
    };

    // Dual-write warms v2 while v1 is selected; it is no longer needed after cutover.
    let versions = if config.memories.dual_write && config.memories.version == MemoryVersion::V1 {
        vec![MemoryVersion::V1, MemoryVersion::V2]
    } else {
        vec![config.memories.version]
    };
    for version in versions {
        let mut pipeline_config = config.as_ref().clone();
        pipeline_config.memories.version = version;
        let config = Arc::new(pipeline_config);
        let parent_permission_profile = parent_permission_profile.clone();
        let context = Arc::new(MemoryStartupContext::new(
            Arc::clone(&thread_manager),
            provider.clone(),
            thread_id,
            Arc::clone(&thread),
            config.as_ref(),
            source.clone(),
        ));
        if context
            .provider()
            .memory_extraction_preferred_model()
            .is_none()
            && context
                .provider()
                .memory_consolidation_preferred_model()
                .is_none()
        {
            continue;
        }
        tokio::spawn(async move {
            if context.memory_store().await.is_none() {
                warn!("state db unavailable for memories startup pipeline; skipping");
                return;
            }
            let root = config
                .codex_home
                .join(config.memories.version.directory_name());
            if let Err(err) = ensure_layout(&root).await {
                warn!("failed preparing memories root: {err}");
                return;
            }
            if let Err(err) = seed_extension_instructions(&root).await {
                warn!("failed seeding memory extension instructions: {err}");
            }

            // Clean memories to make preserve DB size. This does not consume tokens so can be
            // done before the quota check.
            phase1::prune(context.as_ref(), &config).await;

            if !guard::rate_limits_ok(context.provider(), &config).await {
                context.counter(
                    MEMORY_STARTUP,
                    /*inc*/ 1,
                    &[("status", "skipped_rate_limit")],
                );
                return;
            }

            // Run phase 1.
            phase1::run(Arc::clone(&context), Arc::clone(&config)).await;
            // Run phase 2.
            phase2::run(context, config, parent_permission_profile).await;
        });
    }
}
