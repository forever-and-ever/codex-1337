#![cfg(test)]

use super::invocation_context;
use rmcp::model::Tool;
use serde_json::json;

#[test]
fn discovery_preserves_the_connectors_opaque_invocation_context() {
    let mut declaration = json!({
        "name": "search_service.web_run",
        "inputSchema": {"type": "object"},
    });
    let tool: Tool = serde_json::from_value(declaration.clone()).unwrap();
    assert_eq!(invocation_context(&tool), None);

    let pin = json!({"revision": "connector-selected"});
    declaration["_meta"] = json!({"_codex_apps": {"invocation_context": pin}});
    let tool: Tool = serde_json::from_value(declaration).unwrap();
    assert_eq!(invocation_context(&tool), Some(&pin));
    let mut refreshed = serde_json::to_value(&tool).unwrap();
    refreshed["_meta"]["_codex_apps"]["invocation_context"] = json!({"revision": "live"});
    refreshed["_meta"]["_codex_apps"]["resource_uri"] = json!("ui://latest");
    let live: Tool = serde_json::from_value(refreshed.clone()).unwrap();
    assert!(super::same_apps_declaration(&live, &tool));
    let mut restored = live;
    super::restore_invocation_context(&mut restored, &tool);
    assert_eq!(invocation_context(&restored), Some(&pin));
    assert_eq!(
        serde_json::to_value(&restored).unwrap()["_meta"]["_codex_apps"]["resource_uri"],
        "ui://latest"
    );

    refreshed["_meta"]["_codex_apps"]["other_metadata"] = json!("changed");
    let changed: Tool = serde_json::from_value(refreshed).unwrap();
    assert!(!super::same_apps_declaration(&changed, &tool));
}
