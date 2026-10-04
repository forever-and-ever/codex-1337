//! Checks that tool response delivery preserves live results without rooting discarded values.

use std::collections::HashMap;
use std::sync::mpsc as std_mpsc;

use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::sync::mpsc;

use super::super::RuntimeState;
use super::super::PendingToolCall;
use super::super::value::v8_value_to_json;
use super::resolve_tool_response;
use crate::v8_init::ensure_v8_initialized;

#[test]
fn discarded_tool_response_is_collectible_before_cell_ends() {
    ensure_v8_initialized().expect("initialize V8");
    let isolate = &mut v8::Isolate::new(v8::CreateParams::default());
    v8::scope!(let scope, isolate);
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let (runtime_command_tx, _runtime_command_rx) = std_mpsc::channel();
    scope.set_slot(RuntimeState {
        event_tx,
        pending_tool_calls: HashMap::new(),
        pending_timeouts: HashMap::new(),
        stored_values: HashMap::new(),
        stored_value_writes: HashMap::new(),
        enabled_tools: Vec::new(),
        next_tool_call_id: 1,
        next_timeout_id: 1,
        tool_call_id: "cell".to_string(),
        runtime_command_tx,
        exit_requested: false,
    });

    let promise = {
        v8::scope!(let scope, scope);
        let resolver = v8::PromiseResolver::new(scope).expect("create tool promise");
        let promise = resolver.get_promise(scope);
        let resolver = v8::Global::new(scope, resolver);
        scope
            .get_slot_mut::<RuntimeState>()
            .expect("runtime state")
            .pending_tool_calls
            .insert("tool".to_string(), PendingToolCall { resolver, native_execution: false });
        v8::Global::new(scope, promise)
    };
    let response = json!({"content": [{"type": "text", "text": "tool result"}]});
    resolve_tool_response(scope, "tool", Ok(response.clone())).expect("resolve tool promise");
    scope.perform_microtask_checkpoint();
    scope.low_memory_notification();

    let weak_result = {
        v8::scope!(let scope, scope);
        let promise = v8::Local::new(scope, &promise);
        assert_eq!(promise.state(), v8::PromiseState::Fulfilled);
        let result = promise.result(scope);
        assert_eq!(v8_value_to_json(scope, result), Ok(Some(response)));
        v8::Weak::new(scope, result)
    };
    drop(promise);
    scope.low_memory_notification();

    assert!(weak_result.is_empty(), "discarded response is still rooted");
}

#[test]
fn native_result_presentation_follows_each_promise_and_current_values() {
    use codex_code_mode_protocol::CodeModeToolKind;
    use codex_code_mode_protocol::ToolDefinition;
    use codex_code_mode_protocol::enabled_tool_metadata;
    use codex_protocol::ToolName;
    use super::super::RuntimeEvent;

    ensure_v8_initialized().expect("initialize V8");
    let isolate = &mut v8::Isolate::new(v8::CreateParams::default());
    v8::scope!(let scope, isolate);
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let (runtime_command_tx, _runtime_command_rx) = std_mpsc::channel();
    let tools = [ToolName::namespaced("functions", "exec_command"), ToolName::namespaced("mcp__fake__", "exec_command"), ToolName::plain("write_stdin")];
    scope.set_slot(RuntimeState {
        event_tx,
        pending_tool_calls: HashMap::new(),
        pending_timeouts: HashMap::new(),
        stored_values: HashMap::new(),
        stored_value_writes: HashMap::new(),
        enabled_tools: tools.into_iter().map(|tool_name| enabled_tool_metadata(&ToolDefinition {
            name: tool_name.to_string(), tool_name, summary: String::new(), description: String::new(),
            kind: CodeModeToolKind::Function, input_schema: None, input_schema_max_bytes: None, output_schema: None,
        })).collect(),
        next_tool_call_id: 1,
        next_timeout_id: 1,
        tool_call_id: "cell".to_string(),
        runtime_command_tx,
        exit_requested: false,
    });
    super::super::globals::install_globals(scope).expect("install globals");
    let promise = super::evaluate_main_module(scope, r#"
const first = tools.exec_command({});
const other = tools.mcp__fake__exec_command({});
const running = tools.write_stdin({});
const result = await first;
text(result); notify(result);
text(result, {metadata:true});
text(JSON.stringify(result));
text({...result}); text(JSON.parse(JSON.stringify(result)));
store("saved", result); text(load("saved")); text({result}); text([result]);
text(await other);
text(await running);
result.output = "changed"; text(result);
result.output = " \n"; notify(result);
result.extra = "kept"; text(result); delete result.extra;
result.toJSON = () => ({custom:"own"}); text(result); delete result.toJSON;
Object.setPrototypeOf(result, {toJSON(){ return {custom:"inherited"}; }}); text(result);
Object.setPrototypeOf(result, Object.prototype);
result.exit_code = 1; text(result);
result.exit_code = null; result.session_id = 23; text(result);
result.exit_code = 0; text(result);
for (const options of [null, [], {metadata:1}, {extra:true}]) {
  try { text(result, options); } catch (error) { text(String(error).includes("options must be an object")); }
}
"#).expect("evaluate script");
    // Responses arrive in reverse order. Origin belongs to the promise, not delivery order.
    let completed = json!({"chunk_id":"abcd12", "wall_time_seconds":0.003421, "exit_code":0, "original_token_count":214, "output":"literal\n"});
    resolve_tool_response(scope, "tool-3", Ok(json!({"wall_time_seconds":0.2, "session_id":42, "output":"partial"}))).unwrap();
    resolve_tool_response(scope, "tool-2", Ok(completed.clone())).unwrap();
    resolve_tool_response(scope, "tool-1", Ok(completed.clone())).unwrap();
    scope.perform_microtask_checkpoint();
    assert!(matches!(super::completion_state(scope, promise.as_ref()), super::CompletionState::Completed {error_text: None, ..}));
    let mut text = Vec::new();
    let mut notifications = Vec::new();
    while let Ok(event) = event_rx.try_recv() {
        match event {
            RuntimeEvent::ContentItem(codex_code_mode_protocol::FunctionCallOutputContentItem::InputText {text: value}) => text.push(value),
            RuntimeEvent::Notify {text, ..} => notifications.push(text),
            _ => {}
        }
    }
    let full = completed.to_string();
    let mut modified = completed.clone(); modified["output"] = json!(" \n"); modified["extra"] = json!("kept");
    let mut contradictory = completed.clone(); contradictory["output"] = json!(" \n"); contradictory["session_id"] = json!(23);
    let text = text.into_iter().map(|text| serde_json::from_str::<serde_json::Value>(&text).map(|value| value.to_string()).unwrap_or(text)).collect::<Vec<_>>();
    assert_eq!(text, vec![
        "literal\n".to_string(), "exit=0 wall=0.003421s chunk=abcd12 tokens≈214".to_string()+"\nliteral\n",
        full.clone(), full.clone(), full.clone(), full.clone(), json!({"result":completed}).to_string(), json!([completed]).to_string(), full,
        "Running (session 42)\npartial".to_string(), "changed".to_string(), modified.to_string(),
        json!({"custom":"own"}).to_string(), json!({"custom":"inherited"}).to_string(),
        "Exit 1\n \n".to_string(), "Running (session 23)\n \n".to_string(), contradictory.to_string(),
        "true".to_string(), "true".to_string(), "true".to_string(), "true".to_string(),
    ]);
    assert_eq!(notifications, vec!["literal\n", " \n"]);
}
