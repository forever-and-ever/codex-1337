use super::*;
use crate::connection_manager::tests::catalog_refresh::MutableCatalog;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn ordinary_notifications_invalidate_cached_bindings_at_the_next_boundary()
-> anyhow::Result<()> {
    let fixture = MutableCatalog::new().await?;
    fixture.connections.list_all_tools().await;
    let mut config = crate::mcp::tests::test_mcp_config(std::env::temp_dir());
    config
        .server_permissions
        .insert("docs".to_string(), crate::McpServerPermissions::Thread);
    let published = Arc::new(PublishedMcpRuntime {
        connections: Arc::clone(&fixture.connections),
        config: Some(Arc::new(config)),
        auth: None,
        auth_token: None,
        auth_manager: None,
        auth_owner: None,
        auth_generation: Arc::new(()),
        plugins_available: false,
        ready_selected_capability_roots: Vec::new(),
        environment_selections: Arc::default(),
        ready_environments: HashMap::new(),
        cached_binding: Mutex::new(None),
    });
    let required_plugins = HashSet::new();
    let capture = || {
        McpRuntime::binding_from_published_runtime(
            Arc::clone(&published),
            /*required_servers*/ &[],
            &required_plugins,
        )
    };
    let before = capture().await.expect("initial binding");
    assert!(Arc::ptr_eq(
        &before,
        &capture().await.expect("cached binding")
    ));
    let frozen = before.tools().to_vec();

    let updated = rmcp::model::Tool::new(
        "search",
        "changed search schema",
        Arc::new(
            serde_json::json!({"type": "object", "required": ["query"]})
                .as_object()
                .expect("schema object")
                .clone(),
        ),
    );
    *fixture.tools.write().await = vec![
        updated.clone(),
        rmcp::model::Tool::new("added", "added tool", Arc::default()),
    ];
    fixture.notify().await?;
    // Host-call attribution observes current catalogs; it is not a model boundary.
    let observed = catalog_boundary::binding_from_published_runtime(
        Arc::clone(&published),
        &[],
        &required_plugins,
        crate::client_tool_catalog::CatalogReadMode::Current,
    )
    .await
    .expect("current binding");
    assert_eq!(observed.tools(), frozen);
    let after = capture().await.expect("refreshed binding");
    assert!(!Arc::ptr_eq(&before, &after));
    assert_eq!(before.tools(), frozen);
    assert_eq!(
        after
            .prepare_call("docs", "search")
            .expect("updated call")
            .tool_info()
            .tool,
        updated
    );
    assert!(after.prepare_call("docs", "added").is_some());
    assert!(after.prepare_call("docs", "removed").is_none());
    assert!(Arc::ptr_eq(
        &after,
        &capture().await.expect("cached refreshed binding")
    ));

    // Identical definitions do not invalidate a binding or its prepared calls.
    fixture.notify().await?;
    assert!(Arc::ptr_eq(
        &after,
        &capture().await.expect("equivalent catalog binding")
    ));
    fixture.client.shutdown().await;
    Ok(())
}
