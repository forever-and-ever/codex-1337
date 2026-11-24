use std::collections::HashSet;
use std::sync::Arc;

use super::CachedMcpBinding;
use super::McpBinding;
use super::PublishedMcpRuntime;
use crate::McpServerKey;
use tracing::Span;

#[tracing::instrument(name = "mcp.binding.capture", skip_all, fields(cache_hit))]
pub(super) async fn binding_from_published_runtime(
    current: Arc<PublishedMcpRuntime>,
    required_servers: &[McpServerKey],
    required_plugins: &HashSet<String>,
    mode: crate::client_tool_catalog::CatalogReadMode,
) -> Option<Arc<McpBinding>> {
    let config = Arc::clone(current.config.as_ref()?);
    let stable_catalog_revisions = crate::connection_manager::boundary::catalog_revisions(
        &current.connections,
        required_servers,
        required_plugins,
        mode,
    )
    .await;
    if let Some(catalog_revisions) = &stable_catalog_revisions {
        let cached = tracing::info_span!("mcp.binding.cache_lock_wait").in_scope(|| {
            current
                .cached_binding
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        });
        if let Some(cached) = cached.as_ref()
            && &cached.catalog_revisions == catalog_revisions
            && let Some(binding) = cached.binding.upgrade()
        {
            Span::current().record("cache_hit", true);
            return Some(binding);
        }
    }

    Span::current().record("cache_hit", false);
    // Keep the instrumented catalog fan-out out of its callers' future layouts.
    let binding = Arc::new(
        Box::pin(
            crate::connection_manager::boundary::capture_binding_with_mode(
                &current.connections,
                config,
                current.plugins_available,
                required_servers,
                required_plugins,
                mode,
            ),
        )
        .await,
    );
    if let Some(catalog_revisions) = stable_catalog_revisions
        && crate::connection_manager::boundary::catalog_revisions(
            &current.connections,
            required_servers,
            required_plugins,
            mode,
        )
        .await
        .as_ref()
            == Some(&catalog_revisions)
    {
        let mut cached = current
            .cached_binding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cached) = cached.as_ref()
            && cached.catalog_revisions == catalog_revisions
            && let Some(binding) = cached.binding.upgrade()
        {
            return Some(binding);
        }
        *cached = Some(CachedMcpBinding {
            catalog_revisions,
            binding: Arc::downgrade(&binding),
        });
    }
    Some(binding)
}
