use super::*;
use pretty_assertions::assert_eq;

#[derive(Default)]
pub(crate) struct RefreshControl {
    pub(super) peer: tokio::sync::Mutex<Option<rmcp::Peer<RoleServer>>>,
    pub(super) requests: AtomicUsize,
    pub(super) calls: AtomicUsize,
    pub(super) fail: AtomicBool,
    pub(super) pause: AtomicBool,
    pub(super) started: Notify,
    pub(super) release: Notify,
    pub(super) pause_call: AtomicBool,
    pub(super) call_started: Notify,
    pub(super) release_call: Notify,
}

pub(crate) struct MutableCatalog {
    pub(crate) connections: Arc<McpConnectionSet>,
    pub(crate) client: Arc<RmcpClient>,
    pub(crate) tools: Arc<tokio::sync::RwLock<Vec<Tool>>>,
    control: Arc<RefreshControl>,
}

impl MutableCatalog {
    pub(crate) async fn new() -> Result<Self> {
        Self::from_tools(vec![
            create_test_tool("docs", "search").tool,
            create_test_tool("docs", "removed").tool,
        ])
        .await
    }

    pub(crate) async fn from_tools(tools: Vec<Tool>) -> Result<Self> {
        let tools = Arc::new(tokio::sync::RwLock::new(tools));
        let control = Arc::new(RefreshControl::default());
        let client = Arc::new(
            RmcpClient::new_in_process_client(Arc::new(MutableToolsTransportFactory {
                server: MutableToolsServer {
                    tools: Arc::clone(&tools),
                    block_tool_listing: Arc::new(AtomicBool::new(false)),
                    refresh_control: Some(Arc::clone(&control)),
                },
            }))
            .await?,
        );
        client
            .initialize(
                InitializeRequestParams::new(
                    ClientCapabilities::default(),
                    Implementation::new("codex-test", "0.0.0-test"),
                )
                .with_protocol_version(ProtocolVersion::V_2025_06_18),
                Some(Duration::from_secs(5)),
                Box::new(|_, _| async { Err(anyhow!("unexpected elicitation")) }.boxed()),
            )
            .await?;
        let (initial_tools, initial_meta) = list_tools_for_client_uncached(
            "docs",
            /*is_codex_apps_mcp_server*/ false,
            "test",
            &client,
            Some(Duration::from_secs(5)),
            MAX_MCP_CATALOG_ITEMS,
            /*server_instructions*/ None,
        )
        .await?;
        let managed_client = ManagedClient {
            connection_sharing: ConnectionSharing::Private,
            shared_elicitation: None,
            _auth_change_notifications: None,
            startup_tools: None,
            client: Arc::clone(&client),
            server_info: create_test_server_info("Mutable catalog"),
            tool_catalog: Arc::new(ClientToolCatalog::new(
                initial_tools,
                initial_meta,
                /*updates*/ None,
            )),
            tool_timeout: Some(Duration::from_secs(5)),
            server_instructions: None,
            server_supports_sandbox_state_meta_capability: false,
            client_supports_network_policy_meta_capability: false,
            codex_apps_tools_cache_context: None,
        };
        let mut connections = McpConnectionSet::empty(/*prefix_mcp_tool_names*/ true);
        connections.insert_test_client(
            "docs".to_string(),
            AsyncManagedClient {
                ema_authority: None,
                client: futures::future::ready(Ok(managed_client)).boxed().shared(),
                is_codex_apps_mcp_server: false,
                server_capabilities: Arc::new(std::sync::Mutex::new(None)),
                cached_server_info: None,
                codex_apps_tools_cache_context: None,
                tool_catalog_cache_context: None,
                startup_complete: Arc::new(AtomicBool::new(true)),
                startup_reconnect: None,
                connection_slot: None,
                cancel_token: CancellationToken::new(),
            },
        );
        connections
            .servers
            .get_mut("docs")
            .expect("docs view")
            .tool_filter = ToolFilter {
            enabled: None,
            disabled: HashSet::from(["disabled".to_string()]),
        };
        Ok(Self {
            connections: Arc::new(connections),
            client,
            tools,
            control,
        })
    }

    pub(crate) async fn notify(&self) -> Result<()> {
        let before = self.client.tool_list_revision().current();
        let peer = self
            .control
            .peer
            .lock()
            .await
            .as_ref()
            .expect("server peer after startup list")
            .clone();
        peer.notify_tool_list_changed().await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.client.tool_list_revision().current() == before {
                tokio::task::yield_now().await;
            }
            anyhow::Ok(())
        })
        .await??;
        Ok(())
    }
}

#[tokio::test]
async fn ordinary_catalog_refresh_coalesces_and_preserves_failed_and_racing_notifications()
-> Result<()> {
    let fixture = MutableCatalog::new().await?;
    // A startup notification after the initial list must not be consumed by readiness.
    fixture.notify().await?;
    let before = capture_binding(&fixture.connections).await;
    assert_eq!(fixture.control.requests.load(Ordering::Acquire), 2);
    let old_tools = before.tools().to_vec();
    let stale_call = before
        .prepare_call("docs", "search")
        .expect("initial search");
    let reconnected = MutableCatalog::new().await?;
    assert!(!Arc::ptr_eq(&fixture.client, &reconnected.client));
    let equivalent = reconnected
        .connections
        .prepare_call_for_tool(Arc::clone(before.config()), stale_call.tool_info())
        .await
        .expect("equivalent definition on a replacement connection");
    Box::pin(equivalent.call(
        /*arguments*/ None, /*meta*/ None, /*timeout*/ None,
    ))
    .await?;
    reconnected.client.shutdown().await;

    let mut search = create_test_tool("docs", "search").tool;
    search.input_schema = Arc::new(
        serde_json::json!({"type": "object", "properties": {"query": {"type": "string"}}})
            .as_object()
            .expect("schema object")
            .clone(),
    );
    *fixture.tools.write().await = vec![
        search.clone(),
        create_test_tool("docs", "added").tool,
        create_test_tool("docs", "disabled").tool,
    ];
    fixture.notify().await?;
    fixture.notify().await?;
    let (after, concurrent) = tokio::join!(
        capture_binding(&fixture.connections),
        capture_binding(&fixture.connections),
    );
    assert_eq!(fixture.control.requests.load(Ordering::Acquire), 3);
    assert!(
        fixture
            .connections
            .prepare_call_for_tool(Arc::clone(before.config()), stale_call.tool_info(),)
            .await
            .is_err()
    );
    assert_eq!(after.tools(), concurrent.tools());
    assert_eq!(before.tools(), old_tools);
    assert_eq!(
        after
            .tools()
            .iter()
            .map(|tool| tool.tool.clone())
            .collect::<Vec<_>>(),
        vec![create_test_tool("docs", "added").tool, search],
    );
    for tool in after.tools() {
        let call = after
            .prepare_call("docs", &tool.tool.name)
            .expect("listed call");
        assert_eq!(call.tool_info(), tool);
        assert!(
            fixture
                .connections
                .prepare_call_for_tool(Arc::clone(after.config()), tool,)
                .await
                .is_ok()
        );
        assert_eq!(
            Box::pin(call.call(
                /*arguments*/ None, /*meta*/ None, /*timeout*/ None
            ))
            .await?
            .content,
            vec![serde_json::json!({"type": "text", "text": tool.tool.name})],
        );
    }
    assert!(after.prepare_call("docs", "removed").is_none());
    assert!(after.prepare_call("docs", "disabled").is_none());
    let error = Box::pin(
        stale_call.call_with_preparation(/*requested_timeout*/ None, || async {
            panic!("stale call must reject before preparation")
        }),
    )
    .await
    .expect_err("old definition must not reach the changed server");
    assert!(error.to_string().contains("catalog changed"));

    // An annotation-only edit changes call authority even when the schema is identical.
    fixture.tools.write().await[0].annotations =
        Some(rmcp::model::ToolAnnotations::new().read_only(true));
    fixture.notify().await?;
    assert!(
        fixture
            .connections
            .prepare_call_for_tool(
                Arc::clone(after.config()),
                after
                    .prepare_call("docs", "search")
                    .expect("search")
                    .tool_info(),
            )
            .await
            .is_err()
    );
    let after = capture_binding(&fixture.connections).await;

    // A failed list must preserve both the last-good definitions and the dirty revision.
    fixture.control.fail.store(true, Ordering::Release);
    *fixture.tools.write().await = vec![create_test_tool("docs", "recovered").tool];
    fixture.notify().await?;
    assert!(
        fixture
            .connections
            .prepare_call_for_tool(
                Arc::clone(after.config()),
                after
                    .prepare_call("docs", "search")
                    .expect("search")
                    .tool_info(),
            )
            .await
            .is_err()
    );
    let failed = capture_binding(&fixture.connections).await;
    assert_eq!(failed.tools(), after.tools());
    fixture.control.fail.store(false, Ordering::Release);
    let recovered = capture_binding(&fixture.connections).await;
    assert!(recovered.prepare_call("docs", "recovered").is_some());
    assert!(recovered.prepare_call("docs", "added").is_none());
    assert_eq!(fixture.control.requests.load(Ordering::Acquire), 7);

    // Capture the response before pausing: a second notification during the fetch
    // cannot be acknowledged by publishing the first response.
    fixture.control.pause.store(true, Ordering::Release);
    *fixture.tools.write().await = vec![create_test_tool("docs", "first_revision").tool];
    fixture.notify().await?;
    let connections = Arc::clone(&fixture.connections);
    let fetching = tokio::spawn(async move { capture_binding(&connections).await });
    fixture.control.started.notified().await;
    *fixture.tools.write().await = vec![create_test_tool("docs", "second_revision").tool];
    fixture.notify().await?;
    fixture.control.pause.store(false, Ordering::Release);
    fixture.control.release.notify_one();
    let first = fetching.await?;
    assert!(first.prepare_call("docs", "first_revision").is_some());
    let second = capture_binding(&fixture.connections).await;
    assert!(second.prepare_call("docs", "second_revision").is_some());
    assert!(second.prepare_call("docs", "first_revision").is_none());
    assert_eq!(fixture.control.requests.load(Ordering::Acquire), 9);

    // Publication must defer rather than wait for a live call that a future step
    // may need to release. Dirty catalogs must also deny newly prepared invocations.
    let old_call = second
        .prepare_call("docs", "second_revision")
        .expect("second revision");
    fixture.control.pause_call.store(true, Ordering::Release);
    let running_call = old_call.clone();
    let running = tokio::spawn(async move {
        Box::pin(running_call.call(
            /*arguments*/ None, /*meta*/ None, /*timeout*/ None,
        ))
        .await
    });
    fixture.control.call_started.notified().await;
    *fixture.tools.write().await = vec![create_test_tool("docs", "after_call").tool];
    fixture.notify().await?;
    let deferred = tokio::time::timeout(
        Duration::from_secs(1),
        capture_binding(&fixture.connections),
    )
    .await?;
    assert_eq!(deferred.tools(), second.tools());
    let prepared = tokio::time::timeout(
        Duration::from_secs(1),
        fixture
            .connections
            .prepare_call_for_tool(Arc::clone(second.config()), old_call.tool_info()),
    )
    .await?;
    assert!(prepared.is_err());
    let error = Box::pin(
        old_call.call_with_preparation(/*requested_timeout*/ None, || async {
            panic!("pending notification must reject before preparation")
        }),
    )
    .await
    .expect_err("dirty catalog must deny a new invocation");
    assert!(error.to_string().contains("catalog update pending"));
    assert_eq!(fixture.control.requests.load(Ordering::Acquire), 9);
    fixture.control.pause_call.store(false, Ordering::Release);
    fixture.control.release_call.notify_one();
    running.await??;
    let published = capture_binding(&fixture.connections).await;
    assert!(published.prepare_call("docs", "after_call").is_some());
    assert!(published.prepare_call("docs", "second_revision").is_none());
    assert_eq!(fixture.control.requests.load(Ordering::Acquire), 10);

    // Preparation may have side effects, but a notification during its awaits
    // must still prevent the subsequent MCP request without replaying preparation.
    let call = published
        .prepare_call("docs", "after_call")
        .expect("after call");
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let entered_preparation = Arc::clone(&entered);
    let release_preparation = Arc::clone(&release);
    let calls = fixture.control.calls.load(Ordering::Acquire);
    let preparing = tokio::spawn(async move {
        Box::pin(
            call.call_with_preparation(/*requested_timeout*/ None, || async move {
                entered_preparation.notify_one();
                release_preparation.notified().await;
                Ok((None, None))
            }),
        )
        .await
    });
    entered.notified().await;
    *fixture.tools.write().await = vec![create_test_tool("docs", "after_preparation").tool];
    fixture.notify().await?;
    release.notify_one();
    let error = preparing
        .await?
        .expect_err("notification during preparation must prevent dispatch");
    assert!(error.to_string().contains("no MCP call was dispatched"));
    assert_eq!(fixture.control.calls.load(Ordering::Acquire), calls);
    fixture.client.shutdown().await;
    Ok(())
}
